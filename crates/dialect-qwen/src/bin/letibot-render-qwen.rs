//! `letibot-render-qwen`: the Qwen renderer as its own executable.
//! The work is `letibot_dialect_qwen::cli::run`, shared with the `letibot` multicall.
//! The fidelity gate execs it by name from PATH.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_dialect_qwen::cli::run(&args));
}
