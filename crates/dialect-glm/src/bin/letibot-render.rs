//! `letibot-render` — the GLM renderer, as its own executable.
//!
//! The work lives in `letibot_dialect_glm::cli::run` so that this binary and the
//! `letibot` multicall are the same implementation rather than two copies. This
//! exists because the fidelity gate execs a PATH: it knows a filename and gives it
//! arguments, and the name it uses is `letibot-render`.
//!
//! See `src/cli.rs` for why the body moved and what changed in the moving.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_dialect_glm::cli::run(&args));
}
