//! `harnessd` — the daemon, as its own executable.
//!
//! The work lives in `letibot_harnessd::cli::run` so that this binary and the
//! `letibot` multicall are the same implementation rather than two copies. This exists
//! because `scripts/letibot` execs it by path (and checks that pid is a daemon), and
//! because the daemon is started by whatever the operator has — systemd, a shell, or
//! the launcher.
//!
//! The `harnessd:` prefix on an error is unchanged by the move: it is the role's name,
//! and this is still the daemon role.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match letibot_harnessd::cli::run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("harnessd: {e}");
            std::process::exit(1);
        }
    }
}
