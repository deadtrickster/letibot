//! `letibot-render-qwen` — the Qwen renderer, as its own executable.
//!
//! The work lives in `letibot_dialect_qwen::cli::run` so that this binary and the
//! `letibot` multicall are the same implementation rather than two copies. This
//! exists because the fidelity gate execs a PATH.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_dialect_qwen::cli::run(&args));
}
