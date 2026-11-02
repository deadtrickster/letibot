//! Room for the descriptors a test binary's own parallelism needs - a fixture, so that a test
//! measures its subject instead of the shell's soft open-file limit.
//!
//! Where the descriptors go: every harness opens the store AND a corpus connection, and sqlite in
//! WAL keeps the db, the `-wal` and the `-shm` open - ~6 per harness, and a test binary runs its
//! tests in parallel by default. Darwin's default SOFT limit is 256 and Linux's is 1024, which is
//! why none of this has ever been red in CI.
//!
//! A soft limit is the RUN's and not the test's subject, and the HARD limit is what bounds it - so a
//! caller asks for what it needs, this asks the kernel for `min(want, hard)`, prints what it got
//! and a box that cannot give it says so in those words (through `apparatus::present`, so
//! `LETIBOT_REQUIRE_APPARATUS=1` refuses the run rather than letting it measure the wrong thing).
//!
//! ```ignore
//! let Some(_) = room::room_for("fifty concurrent subagents", 512) else { return };
//! ```

/// The most this fixture will ever ask for: enough for a binary's parallel tests, and a number
/// the kernel does not have to honour - the hard limit is the ceiling.
const WANT: libc::rlim_t = 4_096;

/// Raise this process's soft open-file limit to `need`: `Some` when the process may open
/// that many, `None` after announcing why not (and a failure under
/// `LETIBOT_REQUIRE_APPARATUS`). `what` names the demand in a person's words
pub fn room_for(what: &str, need: u64) -> Option<()> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit`/`setrlimit` take a pointer to this local `rlimit` and write through it
    // nothing else in this process reads or writes that struct.
    let (before, hard, now) = unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            eprintln!(
                "  {what}: the open-file limit could not be read, so this run has whatever it \
                 started with"
            );
            // Nothing was measured, so nothing is claimed: the test runs as it always did rather
            // than turning a failed reading into a refusal.
            return Some(());
        }
        let (before, hard) = (lim.rlim_cur, lim.rlim_max);
        let want = WANT.min(hard);
        let now = if before < want {
            lim.rlim_cur = want;
            match libc::setrlimit(libc::RLIMIT_NOFILE, &lim) {
                0 => want,
                _ => {
                    eprintln!(
                        "  {what}: the open-file limit could not be raised from {before} (hard \
                         {})",
                        said(hard)
                    );
                    before
                }
            }
        } else {
            before
        };
        (before, hard, now)
    };
    eprintln!(
        "  {what}: open files {before} → {now} (hard {}), {need} asked for",
        said(hard)
    );
    letibot_tokencore::apparatus::present(
        &format!("a soft open-file limit of {need} for {what} (this process may open {now})"),
        now >= need,
    )
}

/// `RLIM_INFINITY` in words, since it prints as a very large number.
fn said(limit: libc::rlim_t) -> String {
    if limit == libc::RLIM_INFINITY {
        "unlimited".to_string()
    } else {
        limit.to_string()
    }
}
