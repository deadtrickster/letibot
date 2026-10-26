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

/// **Every allocation counted, and a large one logged** — see `letibot_harnessd::memwatch`.
#[global_allocator]
static ALLOC: letibot_harnessd::memwatch::Counting = letibot_harnessd::memwatch::Counting;

fn main() {
    if let Some(b) = std::env::var("LETIBOT_LARGE_ALLOC")
        .ok()
        .and_then(|v| letibot_tools::exec::memory::Amount::parse(&v).ok())
    {
        letibot_harnessd::memwatch::set_large_threshold(b.resolve(0) as usize);
    }
    // **Step into the daemon's own leaf before anything else can start a process** (TODO.md
    // R64/R65) — now inside `cli::run`, which both this binary and `letibot harnessd` go through,
    // and said there only once the process turns out to be a DAEMON: this binary is also what
    // `--latest-session` and the other verbs run as, and each of those used to print the
    // memory-caps line on the operator's terminal before the real daemon printed it again.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match letibot_harnessd::cli::run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("harnessd: {e}");
            std::process::exit(1);
        }
    }
}
