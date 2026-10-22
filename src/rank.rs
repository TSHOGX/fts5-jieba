//! Query-side ranking helpers that sit on top of FTS5's `bm25()`.
//!
//! BM25 itself is computed by SQLite. What a search box still needs is
//!
//! 1. a **planner** that turns a raw query into an FTS5 `MATCH` expression, so a
//!    multi-word query does not demand its words be adjacent, and
//! 2. a **re-ranker** that mixes normalized relevance with a recency prior.
//!
//! Both are pure functions with no database access, so adopting them needs no reindex.
//!
//! ```
//! use fts5jieba::rank::{plan, rerank, Candidate, Weights};
//!
//! assert_eq!(plan("deploy netlify"), r#""deploy netlify" OR ("deploy" AND "netlify")"#);
//!
//! // `bm25` is FTS5's raw score: negative, smaller is better.
//! let hits = [
//!     Candidate { bm25: -10.0, age_days: Some(400.0) }, // strong match, stale
//!     Candidate { bm25: -6.0, age_days: Some(400.0) },  // close, stale
//!     Candidate { bm25: -5.5, age_days: Some(1.0) },    // close, fresh
//! ];
//! let order = rerank(&hits, Weights::default());
//! assert_eq!(order[0].index, 0); // a much better match still wins
//! assert_eq!(order[1].index, 2); // among near-ties, the fresh one comes first
//! ```

/// Characters that mark a query as hand-written FTS5 syntax.
const SYNTAX_CHARS: &[char] = &['"', ':', '*', '(', ')'];
/// Bare words that are FTS5 operators when they stand alone.
const OPERATORS: &[&str] = &["OR", "AND", "NOT", "NEAR"];

/// Quote one string as an FTS5 phrase, doubling embedded quotes.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Plan an FTS5 `MATCH` expression from a raw query.
///
/// Quoting the whole query as one phrase makes `deploy netlify` match only when the two
/// words are adjacent, which is brittle for long documents. The plan ORs two arms:
///
/// - the whole query as a phrase, which bm25 rewards for being adjacent (precision), and
/// - an AND of one phrase per word, which allows them to be scattered (recall).
///
/// A single word, or a run of Chinese with no spaces, collapses to one phrase. If the
/// query already contains FTS5 syntax (quotes, `*`, `:`, parentheses, or a bare
/// `OR`/`AND`/`NOT`/`NEAR`) it is returned untouched so power users keep full control.
/// An empty query yields `""`, which matches nothing.
pub fn plan(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        return "\"\"".to_string();
    }
    if q.contains(SYNTAX_CHARS) || q.split_whitespace().any(|w| OPERATORS.contains(&w)) {
        return q.to_string();
    }
    let terms: Vec<&str> = q.split_whitespace().collect();
    if terms.len() <= 1 {
        return quote(q);
    }
    let all = terms
        .iter()
        .map(|t| quote(t))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!("{} OR ({all})", quote(q))
}

/// Exponential recency bonus in `[0, 1]`.
///
/// `exp(-ln2 * age / half_life)`: a document one half-life old scores 0.5. A missing age
/// or a non-positive half-life returns the neutral 0.5, so a missing timestamp is never
/// punished. A negative age (a future timestamp) is clamped to zero and scores 1.0.
pub fn recency_bonus(age_days: Option<f64>, half_life_days: f64) -> f64 {
    match age_days {
        Some(age) if half_life_days > 0.0 && age.is_finite() => {
            (-std::f64::consts::LN_2 * age.max(0.0) / half_life_days).exp()
        }
        _ => 0.5,
    }
}

/// Blend weights for [`rerank`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    /// Weight of normalized bm25 relevance.
    pub relevance: f64,
    /// Weight of the recency bonus. Zero restores pure bm25 order.
    pub recency: f64,
    /// Days for the recency bonus to halve.
    pub half_life_days: f64,
}

impl Default for Weights {
    /// Recency is a light nudge (0.3) on top of relevance (1.0), with a 30-day half-life.
    fn default() -> Self {
        Self {
            relevance: 1.0,
            recency: 0.3,
            half_life_days: 30.0,
        }
    }
}

/// One bm25 candidate to re-rank.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// FTS5's raw `bm25()` score: negative, and smaller means a better match.
    pub bm25: f64,
    /// Age of the document in days, if known.
    pub age_days: Option<f64>,
}

/// A re-ranked candidate: its position in the input slice and its final score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ranked {
    pub index: usize,
    pub score: f64,
}

/// Re-rank bm25 candidates, best first.
///
/// Fetch candidates in bm25 order (over-fetching), then pass them here. Relevance
/// `-bm25` is min-max normalized *within the candidate set* so it is comparable with the
/// recency bonus, which is already in `[0, 1]`:
///
/// `score = relevance * norm_rel + recency * recency_bonus`
///
/// With `recency == 0` the order collapses to bm25 order. A single candidate, or several
/// with identical bm25, all get a normalized relevance of 1.0 rather than dividing by
/// zero. Ties keep their input order.
///
/// Because normalization is relative to the set, the *same* bm25 gap means more in a
/// tight set than in a wide one: with only two candidates the gap is always the full
/// range, so the better match gets 1.0 and the other 0.0 however close they are. Pass a
/// realistic over-fetched set (tens of rows), not just the top two.
pub fn rerank(candidates: &[Candidate], weights: Weights) -> Vec<Ranked> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let rel: Vec<f64> = candidates.iter().map(|c| -c.bm25).collect();
    let min = rel.iter().copied().fold(f64::INFINITY, f64::min);
    let max = rel.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = if max > min { max - min } else { 1.0 };
    let use_recency = weights.recency > 0.0 && weights.half_life_days > 0.0;

    let mut ranked: Vec<Ranked> = candidates
        .iter()
        .enumerate()
        .map(|(index, c)| {
            let norm = (-c.bm25 - min) / span;
            let rec = if use_recency {
                recency_bonus(c.age_days, weights.half_life_days)
            } else {
                0.0
            };
            Ranked {
                index,
                score: weights.relevance * norm + weights.recency * rec,
            }
        })
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(bm25: f64, age: Option<f64>) -> Candidate {
        Candidate {
            bm25,
            age_days: age,
        }
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert_eq!(plan(""), "\"\"");
        assert_eq!(plan("   "), "\"\"");
    }

    #[test]
    fn single_word_is_one_phrase() {
        assert_eq!(plan("netlify"), "\"netlify\"");
    }

    #[test]
    fn spaceless_chinese_is_one_phrase() {
        assert_eq!(plan("中华人民共和国"), "\"中华人民共和国\"");
    }

    #[test]
    fn several_words_get_a_phrase_arm_and_a_recall_arm() {
        assert_eq!(
            plan("deploy netlify"),
            r#""deploy netlify" OR ("deploy" AND "netlify")"#
        );
        assert_eq!(
            plan("netlify 部署 站点"),
            r#""netlify 部署 站点" OR ("netlify" AND "部署" AND "站点")"#
        );
    }

    #[test]
    fn fts5_syntax_passes_through() {
        for q in [
            "a OR b",
            "a AND b",
            "NOT a",
            "a NEAR b",
            "\"exact phrase\"",
            "pre*",
            "col:val",
            "(a b)",
        ] {
            assert_eq!(plan(q), q);
        }
    }

    #[test]
    fn operators_are_only_recognised_as_whole_uppercase_words() {
        assert_eq!(
            plan("order android"),
            r#""order android" OR ("order" AND "android")"#
        );
        assert!(plan("a or b").contains("AND"));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(plan("  netlify  "), "\"netlify\"");
    }

    #[test]
    fn recency_halves_at_the_half_life() {
        assert!((recency_bonus(Some(30.0), 30.0) - 0.5).abs() < 1e-12);
        assert!((recency_bonus(Some(60.0), 30.0) - 0.25).abs() < 1e-12);
        assert!((recency_bonus(Some(0.0), 30.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn recency_is_neutral_when_unknown() {
        assert_eq!(recency_bonus(None, 30.0), 0.5);
        assert_eq!(recency_bonus(Some(10.0), 0.0), 0.5);
        assert_eq!(recency_bonus(Some(f64::NAN), 30.0), 0.5);
    }

    #[test]
    fn a_future_timestamp_scores_one() {
        assert_eq!(recency_bonus(Some(-5.0), 30.0), 1.0);
    }

    #[test]
    fn zero_recency_weight_keeps_bm25_order() {
        let w = Weights {
            recency: 0.0,
            ..Weights::default()
        };
        let hits = [
            cand(-3.0, Some(0.0)),
            cand(-9.0, Some(999.0)),
            cand(-6.0, Some(1.0)),
        ];
        let order: Vec<usize> = rerank(&hits, w).iter().map(|r| r.index).collect();
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn fresh_documents_can_overtake_near_ties() {
        // The strong hit widens the range, so the -6.0 vs -5.5 gap is small after
        // normalization and the recency bonus decides between them.
        let hits = [
            cand(-10.0, Some(400.0)),
            cand(-6.0, Some(400.0)),
            cand(-5.5, Some(1.0)),
        ];
        let order: Vec<usize> = rerank(&hits, Weights::default())
            .iter()
            .map(|r| r.index)
            .collect();
        assert_eq!(order, vec![0, 2, 1]);
    }

    #[test]
    fn two_candidates_always_span_the_full_relevance_range() {
        // Documented behaviour: min-max over two rows turns any gap into 1.0 vs 0.0.
        let hits = [cand(-6.0, Some(400.0)), cand(-5.999, Some(0.0))];
        let r = rerank(&hits, Weights::default());
        assert_eq!(r[0].index, 0);
    }

    #[test]
    fn a_much_better_match_still_wins() {
        let hits = [cand(-12.0, Some(400.0)), cand(-2.0, Some(0.0))];
        assert_eq!(rerank(&hits, Weights::default())[0].index, 0);
    }

    #[test]
    fn degenerate_sets_do_not_divide_by_zero() {
        assert!(rerank(&[], Weights::default()).is_empty());
        let one = rerank(&[cand(-4.0, None)], Weights::default());
        assert!(one[0].score.is_finite());
        let same = rerank(&[cand(-4.0, None), cand(-4.0, None)], Weights::default());
        assert!(same.iter().all(|r| r.score.is_finite()));
        assert_eq!(same[0].index, 0, "ties keep input order");
    }
}
