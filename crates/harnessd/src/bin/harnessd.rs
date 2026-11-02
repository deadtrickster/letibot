//! `harnessd` - the daemon, as its own executable. The work lives in
//! `letibot_harnessd::cli::run` so that this binary and the `letibot` multicall are the
//! same implementation rather than two copies. This exists because `scripts/letibot` execs
//! it by path (and checks that pid is a daemon), and because the daemon is started by
//! whatever the operator has: systemd, a shell, or the launcher. The `harnessd:` prefix on
//! an error is the role's name, and this is the daemon role.

/// Every allocation is counted and a large one logged; see `letibot_harnessd::memwatch`.
#[global_allocator]
static ALLOC: letibot_harnessd::memwatch::Counting = letibot_harnessd::memwatch::Counting;

fn main() {
    if let Some(b) = std::env::var("LETIBOT_LARGE_ALLOC")
        .ok()
        .and_then(|v| letibot_tools::exec::memory::Amount::parse(&v).ok())
    {
        letibot_harnessd::memwatch::set_large_threshold(b.resolve(0) as usize);
    }
    // The leaf is entered and the memory caps are said in `cli::run`, which every verb here
    // goes through, and only once the process turns out to be a DAEMON: `--latest-session`
    // and the other one-shot verbs must not print the caps line on the operator's terminal.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match letibot_harnessd::cli::run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("harnessd: {e}");
            std::process::exit(1);
        }
    }
}
