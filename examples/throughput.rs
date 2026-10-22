//! Rough throughput probe for our own tokenizer engine. NOT a comparison vs
//! simple (that needs simple built); just grounds our side with a real number.
//! Run: cargo run --release --example throughput

use fts5jieba::segment::{Engine, Reason};
use std::time::Instant;

fn main() {
    // A mixed CN/EN paragraph repeated to a meaningful corpus size.
    let unit = "中华人民共和国成立于1949年，人工智能 (artificial intelligence) \
                正在快速发展。The quick brown fox was running through the forest \
                while researchers were optimizing large language models 苹果手机 iPhone15。";
    let corpus = unit.repeat(2000);
    let bytes = corpus.len();

    for (label, stem) in [("stem", true), ("nostem", false)] {
        let engine = Engine::new(stem);
        // warm up (dict already built in new(); this warms code paths)
        let mut count = 0usize;
        engine.segment(&corpus, Reason::Document, |_| {
            count += 1;
            true
        });

        let iters = 20;
        let start = Instant::now();
        let mut total = 0usize;
        for _ in 0..iters {
            engine.segment(&corpus, Reason::Document, |_| {
                total += 1;
                true
            });
        }
        let elapsed = start.elapsed();
        let mb = (bytes * iters) as f64 / 1_048_576.0;
        let secs = elapsed.as_secs_f64();
        println!(
            "[{label:>6}] {:.1} MB in {:.3}s = {:.1} MB/s, {} tokens/pass, {:.1}M tokens/s",
            mb,
            secs,
            mb / secs,
            count,
            (total as f64 / secs) / 1_000_000.0
        );
    }
}
