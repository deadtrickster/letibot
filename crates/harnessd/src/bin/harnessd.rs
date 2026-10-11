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
    // **Step into the daemon's own leaf before anything else can start a process** (TODO.md
    // R64/R65). `delegate_memory` refuses when the daemon's cgroup holds any other pid, so it
    // must run while this process is still the only one in it — a `git` the startup ran first
    // would turn every tool call of this daemon's life uncapped. Said once, in the log.
    match letibot_tools::exec::delegate_memory() {
        Ok(scope) => eprintln!(
            "harnessd: memory caps on — the tool calls' cgroups live in {}",
            scope.display()
        ),
        Err(why) => eprintln!("harnessd: memory caps OFF — {why}"),
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match letibot_harnessd::cli::run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("harnessd: {e}");
            std::process::exit(1);
        }
    }
}
