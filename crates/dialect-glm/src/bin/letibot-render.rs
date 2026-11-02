//! `letibot-render`: the GLM renderer as its own executable.
//!
//! The work lives in `letibot_dialect_glm::cli::run` so this binary and the `letibot`
//! multicall are one implementation rather than two copies. It exists because the fidelity
//! gate execs a PATH: it knows a filename, gives it arguments, and the name it uses is
//! `letibot-render`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(letibot_dialect_glm::cli::run(&args));
}
