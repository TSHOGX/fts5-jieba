# Agent conventions

- Commit messages use [Conventional Commits](https://www.conventionalcommits.org/), in English, with a detailed body when the change is not trivial.
- Docs, code (including error strings) and comments are English only.
- This crate has no `unsafe` beyond the FFI boundary in `src/ffi.rs`, `src/lib.rs` and `src/tokenizer.rs`. Keep new code safe, and document the safety contract of any new `unsafe fn`.
- FTS5 takes **byte** offsets into the original text. Any change to segmentation needs a test that checks offsets through `snippet()`, not only the token list.
- An index built with one tokenizer and queried with another silently returns nothing. Never add a fallback tokenizer; let registration failure be an error.
- `rank` stays pure: no database access, no clock, no I/O. Callers pass ages in days.
- Before committing, run `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test`, then the same with `--features link`.
- Anything that references SQLite symbols directly (not through the routines table) must stay behind the `link` feature, or the Windows build breaks.
