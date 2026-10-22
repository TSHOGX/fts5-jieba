# Changelog

## 0.1.0

First standalone release, extracted from the tokenizers vendored in several projects.

- Mixed Chinese/English FTS5 tokenizer: jieba segmentation, NFD folding, colocated Porter stem, `nostem` option.
- Loadable extension (`sqlite3_extension_init`, `sqlite3_fts5jieba_init`) and in-process `register_on_connection`, the latter behind the off-by-default `link` feature so the extension builds on Windows.
- One process-wide engine per stemming setting, instead of one per table per connection.
- `rank` module: `plan` (FTS5 query planner), `recency_bonus` and `rerank` (bm25 plus recency).
- Cross-host smoke script, throughput example, and 37 tests.
