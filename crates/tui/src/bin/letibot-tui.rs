//! `letibot-tui` - the terminal head, as its own executable.
//!
//! The work lives in `letibot_tui::head::run` so this binary and the `letibot` multicall are one implementation; `scripts/letibot` execs `$LETIBOT_HEAD`, so a substitute head is reached the same way.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_tui::head::run(&args));
}
