# fts5-jieba

A SQLite FTS5 tokenizer for mixed Chinese and English text, plus the query-side ranking helpers that sit on top of `bm25()`. Written in Rust, with no cgo, no C++ toolchain and no system dependencies beyond SQLite itself.

- **Chinese** is segmented with [jieba](https://github.com/messense/jieba-rs).
- **English** is Unicode-normalized (NFD diacritic stripping), lowercased, and emitted together with a colocated Porter stem, so `running` and `run` meet in the index.
- **Mixed text** is split into CJK and non-CJK runs first. Only the CJK runs go to jieba, because feeding everything to it fragments accented Latin (`Café` would become `Caf` + `é`).
- **Offsets are correct.** FTS5 takes byte offsets into the original text, so a mistake there silently corrupts `snippet()` and `highlight()`. Tests check this for Chinese, English and mixed input.
- **Documents and queries are cut differently.** Documents use jieba's search mode (finer, for recall); `MATCH` queries use default mode (coarser, for precision).

```sql
.load ./libfts5jieba sqlite3_fts5jieba_init
CREATE VIRTUAL TABLE docs USING fts5(title, body, tokenize = 'jieba');
INSERT INTO docs VALUES ('部署笔记', 'we deploy the 站点 to Netlify nightly');
SELECT title FROM docs WHERE docs MATCH 'deploy 站点';
```

Pass `'jieba nostem'` to turn English stemming off.

## Do you need this?

If you only need Chinese plus pinyin, with highlighting and snippets and no English stemming, prefer [wangfenjin/simple](https://github.com/wangfenjin/simple): it is mature and does pinyin. This project exists for the case where the same column holds both languages and English inflections should match (`running` finds `run`). `jieba-rs` and `gojieba` are segmentation libraries, not tokenizers.

## Use it

### As a loadable extension

```sh
cargo build --release
```

This produces `target/release/libfts5jieba.dylib` (macOS) or `.so` (Linux). Two entry points are exported, so any host can load it by filename: `sqlite3_extension_init` (the default) and `sqlite3_fts5jieba_init`.

`scripts/smoke.sh` loads the same built artifact into every SQLite host it finds and runs one mixed Chinese/English `MATCH`. It passes for the Homebrew `sqlite3` CLI, Python's `sqlite3` module and `bun:sqlite`.

Loading gotchas on macOS:

- The system `sqlite3` CLI has `.load` compiled out. Use `/opt/homebrew/opt/sqlite/bin/sqlite3`.
- `bun:sqlite` needs `Database.setCustomSQLite('/opt/homebrew/opt/sqlite/lib/libsqlite3.dylib')` before opening a database.
- System Python's `sqlite3` often has extension loading disabled. Use a Homebrew or pyenv Python.
- Go: `mattn/go-sqlite3` needs the build tag `sqlite_load_extension`.

### Linked into a Rust program

```toml
[dependencies]
fts5-jieba = { git = "https://github.com/TSHOGX/fts5-jieba", features = ["link"] }
```

```rust
// Once per connection, before any query that names tokenize = 'jieba'.
unsafe { fts5jieba::register_on_connection(conn.handle().cast()) }?;
```

This calls SQLite's own symbols instead of going through an extension loader's routines table, so there is no file to locate at runtime. That is why it sits behind the off-by-default `link` feature: those symbols must resolve at link time, which a plain loadable extension should not require (a Windows `cdylib` with unresolved externals fails to link). It is worth preferring when you control the binary: an index built with one tokenizer and queried with another silently returns nothing, so a missing loadable extension would make a program answer *wrongly* rather than merely worse. With linking, a registration failure is an error you can make fatal.

The jieba dictionary takes about 600 ms to build, and FTS5 creates a tokenizer per table per connection. The engine is therefore built once per process (one for stemming, one without) and shared, which is safe because it is immutable after construction.

## Ranking helpers

BM25 itself comes from SQLite's `bm25()`. What a search box still needs lives in `fts5jieba::rank`, as pure functions with no database access, so adopting them needs no reindex.

**`plan(query)`** turns a raw query into a `MATCH` expression. Quoting the whole query as one phrase makes `deploy netlify` match only when the words are adjacent. The plan ORs two arms: the whole query as a phrase (bm25 rewards adjacency) and an AND of one phrase per word (recall, words may be scattered). Hand-written FTS5 syntax passes through untouched.

```rust
use fts5jieba::rank::plan;
assert_eq!(plan("deploy netlify"), r#""deploy netlify" OR ("deploy" AND "netlify")"#);
assert_eq!(plan("中华人民共和国"), r#""中华人民共和国""#);   // one phrase
assert_eq!(plan("alpha OR beta"), "alpha OR beta");          // passed through
```

**`rerank(candidates, weights)`** mixes relevance with a recency prior. Fetch candidates in `bm25` order and over-fetch, then re-rank: relevance `-bm25` is min-max normalized within the set, and the recency bonus is `exp(-ln2 · age / half_life)`, so a document one half-life old scores 0.5.

```
score = relevance · normalized(-bm25) + recency · exp(-ln2 · age_days / half_life_days)
```

The defaults are a light nudge: relevance 1.0, recency 0.3, half-life 30 days. Setting `recency` to zero restores pure bm25 order. A missing age scores a neutral 0.5 rather than being punished.

Normalization is relative to the candidate set, so pass a realistic number of rows. With only two candidates any gap becomes the full 0 to 1 range, however small it is.

SQLite's `bm25()` also takes per-column weights, which is usually the cheapest relevance win:

```sql
SELECT title FROM docs WHERE docs MATCH ?1 ORDER BY bm25(docs, 10.0, 1.0);  -- title counts 10x body
```

Note that FTS5 auxiliary functions such as `bm25()` cannot be used inside an aggregate query. Stream the ranked rows and aggregate in your own code.

## Platforms

CI builds and tests on Linux, macOS and Windows, and on the minimum supported Rust version, 1.82. The `link` feature is tested on Linux and macOS; on Windows only the loadable extension is built and tested, and the cross-host smoke script has only been run on macOS.

## Tests

```sh
cargo test                        # 37 tests: unit, integration, doctest
cargo clippy --all-targets -- -D warnings
bash scripts/smoke.sh             # after cargo build --release
```

The integration tests run against real FTS5 tables (`rusqlite` with a bundled SQLite) and cover Chinese segmentation, CJK/English boundaries, UTF-8 byte offsets, diacritic folding, stemming and `nostem`, `snippet()` highlights, both registration paths, and the planner and re-ranker against actual `bm25()` scores.

## Performance

`cargo run --release --example throughput` measures the segmentation engine alone, on a mixed Chinese/English corpus of about 8.8 MB. On an Apple M1 Pro:

| Mode | Throughput |
|---|---|
| with stemming | about 14.9 MB/s |
| `nostem` | about 22.1 MB/s |

This is indicative, single-machine, and engine-only. It is not an end-to-end comparison with other tokenizers, and nothing here claims to be faster than `simple`.

## Attribution

The `sqlite3_api_routines` approach, describing enough of that table by field order to reach `fts5_api`, is the one documented way for a loadable extension to obtain the FTS5 API. `src/ffi.rs` notes [Signal-FTS5-Extension](https://github.com/signalapp/Signal-FTS5-Extension) (AGPL) as where the approach was read; the bindings here are an independent reimplementation of the published C ABI, not copied code.

## License

[MIT](LICENSE)
