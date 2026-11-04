//! `letibot-render`: the GLM renderer as its own executable.
//! The work is `letibot_dialect_glm::cli::run`, shared with the `letibot` multicall.
//! Exists because the fidelity gate execs the name `letibot-render` from PATH.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_dialect_glm::cli::run(&args));
}
