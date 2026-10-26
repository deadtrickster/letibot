//! **Room for the descriptors a test binary's own parallelism needs** — a fixture, so that a
//! test measures its subject instead of the shell's soft open-file limit.
//!
//! MEASURED 2026-10-26 on the Mac, `main`'s gate at `2b51cc71` and then `b79cd87c`: the whole of
//! its step 3 red was this, in two targets at once.
//!
//! * `session_lifecycle::fifty_finished_subagents_leave_no_thread_and_no_registry_entry` — the
//!   model stub's own thread panicked
//!   *`clone the socket: Os { code: 24, kind: TooManyOpenFiles, message: "Too many open files" }`*,
//!   the stub went with it and all fifty children stayed unanswered: `waited 30s for all fifty
//!   children to answer`, 0 of 50, `stub turns: 0`. With the limit raised the same test passes in
//!   1.9 s, `stub turns: 50`.
//! * `restart_restore` — two to four of its eighteen tests failed per run and *different* ones
//!   each time, at `.expect("the parent opens")`, at a `wait_until` timeout and at
//!   `task_start did not start`: a daemon that could not open its store, a child that could not
//!   be started. With the limit raised: **18 passed**, twice in a row.
//!
//! **Where the descriptors go.** Every harness opens the store AND a corpus connection
//! (`Harness::open_with_registry` — *"Two connections is the store's declared shape"*), and
//! sqlite in WAL keeps the db, the `-wal` and the `-shm` open: ~6 per harness, and a test binary
//! runs its tests in parallel by default. Darwin's default SOFT limit is **256** (`getrlimit`
//! reports `rlim_cur: 256, rlim_max: RLIM_INFINITY` here); Linux's is 1024, which is why none of
//! this has ever been red in CI. Live proof while `session_lifecycle` hung: `lsof -p <test pid>`
//! showed 102 `sessions.db` descriptors, fd numbers up to 254.
//!
//! A soft limit is the RUN's and not the test's subject, and the HARD limit is what bounds it —
//! so a caller asks for what it needs, this asks the kernel for `min(want, hard)`, prints what it
//! got, and a box that cannot give it says so in those words (through `apparatus::present`, so
//! `LETIBOT_REQUIRE_APPARATUS=1` refuses the run rather than letting it measure the wrong thing).
//!
//! ```ignore
//! let Some(_) = room::room_for("fifty concurrent subagents", 512) else { return };
//! ```

/// The most this fixture will ever ask for: enough for a binary's parallel tests, and a number
/// the kernel does not have to honour — the hard limit is the ceiling.
const WANT: libc::rlim_t = 4_096;

/// **Raise this process's soft open-file limit to `need`** — `Some(())` when the process may open
/// that many, `None` after announcing why not (and a failure under `LETIBOT_REQUIRE_APPARATUS`).
///
/// `what` names the demand in a person's words, for the line the run prints and the sentence a
/// box that cannot meet it says. `need` is the caller's own reading of its workload: what the
/// tests in its binary will hold at once, and not a round number chosen for it.
pub fn room_for(what: &str, need: u64) -> Option<()> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit`/`setrlimit` take a pointer to this local `rlimit` and write through it;
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
