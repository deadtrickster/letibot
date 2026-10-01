//! `letibot-tui` — the terminal head, as its own executable.
//!
//! The work lives in `letibot_tui::head::run` so that this binary and the `letibot`
//! multicall are the same implementation rather than two copies. This exists because
//! `scripts/letibot` execs a PATH — `$LETIBOT_HEAD`, defaulting to `letibot-tui` —
//! and a substitute head is reached the same way.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_tui::head::run(&args));
}
