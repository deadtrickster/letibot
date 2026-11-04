//! Per-embed latency of a real checkpoint, to compare builds (Accelerate on macOS, gemm elsewhere).
//! Not a pass/fail test: `LETIBOT_BENCH_MODEL=<model dir> cargo test --release -p letibot-embeddings
//! --test embed_latency -- --ignored --nocapture`.

use std::time::Instant;

use letibot_embeddings::Model;

#[test]
#[ignore = "bench: needs LETIBOT_BENCH_MODEL"]
fn embed_latency() {
    let Ok(dir) = std::env::var("LETIBOT_BENCH_MODEL") else {
        eprintln!("SKIPPED embed_latency: LETIBOT_BENCH_MODEL names no checkpoint directory");
        return;
    };
    let model = Model::open(std::path::Path::new(&dir)).expect("open the checkpoint");
    let text =
        "the offer must live at the tail of the request, out of the log, composed per turn. "
            .repeat(16);
    model.embed(&[text.as_str()]).expect("warm up");
    let n = 50;
    let start = Instant::now();
    for _ in 0..n {
        model.embed(&[text.as_str()]).expect("embed");
    }
    let one = start.elapsed() / n;
    let batch: Vec<&str> = (0..32).map(|_| text.as_str()).collect();
    let start = Instant::now();
    model.embed(&batch).expect("embed batch");
    eprintln!(
        "embed_latency: {:?} per text alone, {:?} per text in a batch of 32",
        one,
        start.elapsed() / 32
    );
}
