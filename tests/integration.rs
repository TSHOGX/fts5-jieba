//! End-to-end: load the built extension into a bundled SQLite, register the
//! `jieba` tokenizer, and verify real FTS5 behavior for Chinese, English
//! (with Porter stemming), and mixed text — including that offset-dependent
//! snippet() works, which proves byte offsets survive the round trip.

use rusqlite::Connection;

fn extension_path() -> String {
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let prefix = if cfg!(target_os = "windows") {
        ""
    } else {
        "lib"
    };
    let executable = std::env::current_exe().expect("resolve integration test executable");
    let profile_dir = executable
        .parent()
        .and_then(std::path::Path::parent)
        .expect("resolve Cargo profile directory");
    let candidates = [
        profile_dir.join(format!("{prefix}fts5jieba.{ext}")),
        profile_dir
            .join("deps")
            .join(format!("{prefix}fts5jieba.{ext}")),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return candidate.display().to_string();
        }
    }
    panic!("built extension not found; run `cargo build` first. tried: {candidates:?}");
}

fn open() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    unsafe {
        conn.load_extension_enable().unwrap();
        conn.load_extension(extension_path(), Some("sqlite3_fts5jieba_init"))
            .unwrap();
        conn.load_extension_disable().unwrap();
    }
    conn
}

fn count(conn: &Connection, query: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM docs WHERE docs MATCH ?1",
        [query],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn chinese_word_segmentation() {
    let conn = open();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('中华人民共和国国歌');",
    )
    .unwrap();
    // jieba segments into words; sub-words should match.
    assert_eq!(count(&conn, "中华"), 1);
    assert_eq!(count(&conn, "共和国"), 1);
    // a substring that is NOT a jieba word boundary should not match
    assert_eq!(count(&conn, "国国"), 0);
}

#[test]
fn english_stemming_matches_inflections() {
    let conn = open();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('He was running quickly');",
    )
    .unwrap();
    // document 'running' indexes both 'running' and stem 'run'
    assert_eq!(count(&conn, "running"), 1, "exact form");
    assert_eq!(count(&conn, "run"), 1, "stem of document term matches");
    // query 'runs' -> stem 'run' -> matches the doc's 'run' posting
    assert_eq!(count(&conn, "runs"), 1, "query inflection matches via stem");
    assert_eq!(count(&conn, "quick"), 1, "quickly -> quick");
}

#[test]
fn mixed_chinese_english_numbers() {
    let conn = open();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('Apple 苹果手机 iPhone 15');",
    )
    .unwrap();
    assert_eq!(count(&conn, "apple"), 1, "case-folded english");
    assert_eq!(count(&conn, "苹果"), 1, "chinese word");
    assert_eq!(count(&conn, "手机"), 1, "chinese word");
    assert_eq!(count(&conn, "iphone"), 1, "case-folded");
    assert_eq!(count(&conn, "15"), 1, "number token");
}

#[test]
fn nostem_arg_disables_stemming() {
    let conn = open();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba nostem');
         INSERT INTO docs(body) VALUES ('running');",
    )
    .unwrap();
    assert_eq!(count(&conn, "running"), 1, "exact still matches");
    assert_eq!(count(&conn, "run"), 0, "no stem posting when nostem");
}

#[test]
fn diacritics_are_folded() {
    let conn = open();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('Café résumé');",
    )
    .unwrap();
    assert_eq!(count(&conn, "cafe"), 1);
    assert_eq!(count(&conn, "resume"), 1);
}

#[test]
fn snippet_offsets_are_correct() {
    // Wrong byte offsets would slice mid-codepoint inside SQLite. A clean
    // highlight of the ORIGINAL-cased token proves offsets point at the source.
    let conn = open();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('the Quick 棕色 Fox');",
    )
    .unwrap();
    let s: String = conn
        .query_row(
            "SELECT snippet(docs, 0, '[', ']', '...', 8) FROM docs WHERE docs MATCH 'quick'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        s.contains("[Quick]"),
        "expected highlighted original token, got: {s}"
    );

    // And highlight a Chinese word to be sure multibyte offsets are right too.
    let s: String = conn
        .query_row(
            "SELECT snippet(docs, 0, '[', ']', '...', 8) FROM docs WHERE docs MATCH '棕色'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        s.contains("[棕色]"),
        "expected highlighted chinese word, got: {s}"
    );
}

// ---------------------------------------------------------------------------
// Linked registration: no loadable extension, as a Rust program would use it.
// ---------------------------------------------------------------------------

#[test]
fn register_on_connection_works_without_loading_an_extension() {
    let conn = Connection::open_in_memory().unwrap();
    unsafe { fts5jieba::register_on_connection(conn.handle().cast()) }.expect("register");
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('running 中华人民共和国');",
    )
    .unwrap();
    assert_eq!(count(&conn, "run"), 1);
    assert_eq!(count(&conn, "中华"), 1);
}

#[test]
fn registration_is_per_connection() {
    let a = Connection::open_in_memory().unwrap();
    let b = Connection::open_in_memory().unwrap();
    unsafe { fts5jieba::register_on_connection(a.handle().cast()) }.unwrap();
    assert!(
        b.execute_batch("CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');")
            .is_err(),
        "an unregistered connection must not know the tokenizer"
    );
}

// ---------------------------------------------------------------------------
// Ranking on top of real bm25().
// ---------------------------------------------------------------------------

use fts5jieba::rank::{plan, rerank, Candidate, Weights};

fn linked() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    unsafe { fts5jieba::register_on_connection(conn.handle().cast()) }.unwrap();
    conn
}

#[test]
fn planner_finds_scattered_words_that_a_single_phrase_misses() {
    let conn = linked();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('we deploy the site to netlify nightly');",
    )
    .unwrap();
    assert_eq!(
        count(&conn, "\"deploy netlify\""),
        0,
        "naive phrase needs adjacency"
    );
    assert_eq!(
        count(&conn, &plan("deploy netlify")),
        1,
        "planned query recalls it"
    );
}

#[test]
fn planner_handles_mixed_chinese_and_english() {
    let conn = linked();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('把站点部署到 Netlify 上');
         INSERT INTO docs(body) VALUES ('今天天气很好');",
    )
    .unwrap();
    assert_eq!(count(&conn, &plan("netlify 部署")), 1);
}

#[test]
fn fts5_syntax_survives_planning() {
    let conn = linked();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         INSERT INTO docs(body) VALUES ('alpha'), ('beta'), ('gamma');",
    )
    .unwrap();
    assert_eq!(count(&conn, &plan("alpha OR beta")), 2);
    assert_eq!(count(&conn, &plan("gam*")), 1);
}

#[test]
fn column_weights_let_a_title_hit_outrank_a_body_hit() {
    let conn = linked();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(title, body, tokenize='jieba');
         INSERT INTO docs(title, body) VALUES ('release notes', 'misc text about other things');
         INSERT INTO docs(title, body) VALUES ('misc', 'the release is out');",
    )
    .unwrap();
    // bm25 arguments are per-column weights; smaller scores are better.
    let first: String = conn
        .query_row(
            "SELECT title FROM docs WHERE docs MATCH 'release' ORDER BY bm25(docs, 10.0, 1.0)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(first, "release notes");
}

#[test]
fn recency_rerank_promotes_a_fresher_document_over_real_bm25_scores() {
    let conn = linked();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body, tokenize='jieba');
         CREATE TABLE meta(id INTEGER PRIMARY KEY, age_days REAL);
         INSERT INTO docs(rowid, body) VALUES (1, 'cache invalidation strategy cache');
         INSERT INTO docs(rowid, body) VALUES (2, 'cache invalidation strategy notes');
         INSERT INTO meta VALUES (1, 400.0), (2, 1.0);",
    )
    .unwrap();

    let mut stmt = conn
        .prepare(
            "SELECT docs.rowid, bm25(docs), meta.age_days
             FROM docs JOIN meta ON meta.id = docs.rowid
             WHERE docs MATCH ?1 ORDER BY bm25(docs) LIMIT 50",
        )
        .unwrap();
    let rows: Vec<(i64, f64, f64)> = stmt
        .query_map([plan("cache strategy")], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|(_, bm25, _)| *bm25 < 0.0),
        "bm25 is negative"
    );

    let cands: Vec<Candidate> = rows
        .iter()
        .map(|(_, bm25, age)| Candidate {
            bm25: *bm25,
            age_days: Some(*age),
        })
        .collect();

    // Pure relevance order versus the recency-aware order.
    let pure = rerank(
        &cands,
        Weights {
            recency: 0.0,
            ..Weights::default()
        },
    );
    let blended = rerank(
        &cands,
        Weights {
            recency: 5.0,
            ..Weights::default()
        },
    );
    assert_eq!(
        rows[pure[0].index].0, rows[0].0,
        "recency off keeps bm25 order"
    );
    assert_eq!(
        rows[blended[0].index].0, 2,
        "heavy recency promotes the fresh row"
    );
}
