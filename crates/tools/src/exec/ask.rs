//! **Is this run waiting for an answer?** — asked of the process, not of its words.
//!
//! # Why not the text
//!
//! The first cut of this looked for a question in the run's output: a line ending in `?`,
//! or in a bracketed choice like `[Y/n]`. The operator killed it in one sentence —
//! *"i think `Continue?` is an overfit"* — and the reason is `expect`'s mistake, which this
//! tree has already paid for once: **a rule drawn around the program you had in front of
//! you works for that program and fails silently for the next one.** Question wording is
//! per-program, per-locale and per-version: `Continue?`, `[y/N]`, `Overwrite?`,
//! `Are you sure…`, `Weiter? [J/n]`, `Voulez-vous continuer`, a program that asks in words
//! with no marker at all. A matcher for those is a list that is always one program behind,
//! and it fails in the direction that looks like success — no card, no error, nothing on
//! the screen.
//!
//! # What is asked instead
//!
//! The run's stdin is a pipe **this daemon holds** ([`super::jobs::Stdin`]), so *"the
//! program is waiting for an answer"* is a **fact about the process** and not a guess about
//! its English. Three conditions, and every one of them is a reading of `/proc`:
//!
//! 1. **The run is still alive.** A program that printed something and exited is not
//!    waiting for anything, and a card for it would be a card nobody can answer.
//! 2. **It has written nothing for a beat.** A program that is asking and still drawing is
//!    a program that is not blocked; see the misses below.
//! 3. **Some process of the run has its stdin on OUR pipe and is blocked in a pipe read.**
//!    `/proc/<pid>/fd/0` is a link to `pipe:[<inode>]` and the inode is compared with the
//!    one `fstat` gives for the write end we hold — so *a process reading some other pipe*
//!    (the `grep` in `! ls | grep foo`, waiting on `ls`) is not this, and neither is a
//!    process reading a file. `/proc/<pid>/wchan` then says what it is blocked **in**.
//!
//! Conditions 1 and 2 are the caller's, because the caller is the one holding the clock and
//! the job state; this module is 3, plus [`last_line`], which is the *display* half and
//! decides nothing.
//!
//! # The wchan names, and the fact that they are kernel-version-specific
//!
//! A process blocked in `read(2)` on a pipe sits in the kernel's pipe read, and `wchan`
//! names it. Measured on this box (2026-09-25, `anon_pipe_read`): the modern name. Older
//! kernels call it `pipe_read`, and older still `pipe_wait`. [`blocked_reading`] accepts all
//! three by suffix and **nothing else**, and the reason it does not simply look for `read`
//! is that `filemap_read`, `unix_stream_read_generic` and `tcp_recvmsg` all end in a read
//! and none of them is a program waiting for a person.
//!
//! # What it MISSES, named here rather than found later
//!
//! A heuristic with unnamed misses reads as a guarantee, and this one is not one. Every
//! miss below has the same remedy and it is not a better heuristic: **`!send` — one line to
//! the running command, on demand — needs no signal at all**, and the card is a
//! convenience beside it rather than the way in.
//!
//! 1. **A program blocked on something other than its stdin.** A `y/n` loop that reads a
//!    config file before it prints its prompt, a program waiting on a network socket, a
//!    program blocked writing to a full pipe — none of them is in a pipe read on fd 0, so
//!    no card is raised even though the screen is asking.
//! 2. **A program that asks and keeps drawing.** A build with a spinner, `apt` between two
//!    of its own progress lines: the process is not blocked, so nothing is raised. *What is
//!    on the screen* is the whole question and this reads a process.
//! 3. **A thread, not a process.** `wchan` is per thread and `/proc/<pid>/wchan` is the
//!    thread group leader's. [`blocked_reading`] reads **every** thread under
//!    `/proc/<pid>/task`, which is what makes a program whose reader is a spawned thread
//!    visible — but a program blocked in a thread this process cannot see (another user, a
//!    PID namespace it is not in) is invisible.
//! 4. **A `/proc` this daemon may not read.** This is the miss that matters, and it is
//!    reported rather than guessed at: [`waiting_for_an_answer`] answers [`Waiting::Unreadable`]
//!    when it could read **nothing** — a confined session whose command runs in a user
//!    namespace, where `/proc/<pid>/fd/0` is `EACCES` for this uid. A host that cannot list
//!    the job's processes says the same thing by listing none. **No card is raised on an
//!    unreadable signal**, because a card raised on a guess is worse than a person deciding,
//!    and the person's way in is `!send`.
//! 5. **A kernel whose wchan name is none of the three.** Then nothing is raised and the
//!    reason is invisible. Named here because it is the one miss that would look like the
//!    feature never firing.
//!
//! # What is deliberately not here
//!
//! * **No card and no event.** This module answers a question; the daemon decides what to
//!   put in front of a person.
//! * **No text matching at all.** [`last_line`] takes the last line of the output and does
//!   not care what it says — the module header's whole point is that this file must not be
//!   able to grow a `Continue?` back into it. `no_text_is_matched_anywhere_in_this_module`
//!   is the test that keeps it that way.
//! * **No clock.** The beat between "quiet" and "waiting" belongs to the wait loop that
//!   already has one.

use std::os::fd::RawFd;

/// **Is the run waiting for an answer?** — three answers, and the third is not the second.
///
/// `No` and `Unreadable` are not interchangeable and are not merged: *"it is not asking"*
/// and *"this daemon cannot see whether it is"* are different facts, and a caller that
/// rendered the second as the first would be reporting a capability it does not have — the
/// same distinction [`super::host::ProcessHost::monitors`] keeps for an empty list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// The signal was read, and it says yes: a process of this run has its stdin on the
    /// pipe this daemon holds and is blocked in a pipe read.
    Yes,
    /// The signal was read, and it says no.
    No,
    /// **The signal could not be read** — `/proc` refused for every process of the run, or
    /// the host cannot list them at all. See the module header's miss 4.
    Unreadable,
}

/// **The whole of condition 3**, as one call: `pids` are the run's processes and `pipe` is
/// the inode of the write end this daemon holds, or `None` when there is no such end.
///
/// `None` for `pipe` is [`Waiting::Unreadable`] rather than `No`, and so is an empty `pids`
/// — a host that cannot say which processes a job has has not said *nothing is running*.
pub fn waiting_for_an_answer(pids: &[u32], pipe: Option<u64>) -> Waiting {
    let Some(ino) = pipe else {
        return Waiting::Unreadable;
    };
    if pids.is_empty() {
        return Waiting::Unreadable;
    }
    let mut saw_one = false;
    for pid in pids {
        // `Unreadable` here is *this process could not be looked at* — a uid the daemon
        // is not, a process that left between the cgroup read and this one. It is not a
        // `No`, and the only way the whole call is `Unreadable` is if EVERY process was
        // unreadable.
        match stdin_of(*pid) {
            StdinRead::Unreadable => continue,
            StdinRead::NotAPipe => saw_one = true,
            StdinRead::Pipe(their_ino) => {
                saw_one = true;
                if their_ino == ino && blocked_reading(*pid) {
                    return Waiting::Yes;
                }
            }
        }
    }
    if saw_one {
        Waiting::No
    } else {
        Waiting::Unreadable
    }
}

/// **The inode of the pipe on this descriptor**, or `None` when it is not a pipe.
///
/// `fstat` and not a `/proc` walk: this is the write end the daemon itself holds, so the
/// kernel will answer about it even where `/proc/<pid>/fd` is closed to us. A regular file
/// has an inode too, which is why the *type* is checked rather than only the number — an
/// inode compared across two different kinds of object is a coincidence waiting to happen.
pub fn pipe_inode(fd: RawFd) -> Option<u64> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return None;
    }
    Some(st.st_ino as u64)
}

/// **What a process's fd 0 is** — three answers, because *not a pipe* and *could not look*
/// are different facts and the caller treats them differently.
///
/// This is the distinction that makes [`waiting_for_an_answer`] able to say `No` at all: a
/// process whose stdin is a terminal has been **looked at** and is definitively not reading
/// the pipe this daemon holds, where a process whose `/proc/<pid>/fd/0` this uid may not
/// read has not been looked at and nothing can be concluded. Rendering the second as the
/// first would be the empty-haystack bug with a card on it.
enum StdinRead {
    /// fd 0 is a pipe, and this is its inode.
    Pipe(u64),
    /// Read, and fd 0 is not a pipe at all — a terminal, a file, a socket.
    NotAPipe,
    /// **This process could not be looked at**: `EACCES` for a uid the daemon is not (the
    /// confined-session case), or a process that left between the cgroup read and this one.
    Unreadable,
}

/// `/proc/<pid>/fd/0` is a symlink whose target the kernel writes as `pipe:[<inode>]`, and
/// the comparison is on the number.
fn stdin_of(pid: u32) -> StdinRead {
    let Ok(target) = std::fs::read_link(format!("/proc/{pid}/fd/0")) else {
        return StdinRead::Unreadable;
    };
    let Some(rest) = target.to_str().and_then(|t| t.strip_prefix("pipe:[")) else {
        return StdinRead::NotAPipe;
    };
    match rest.strip_suffix(']').and_then(|n| n.parse().ok()) {
        Some(ino) => StdinRead::Pipe(ino),
        // It said `pipe:[` and then something that is not a number, which no kernel does.
        // `NotAPipe` rather than `Unreadable`: the link was READ, and reading it is what
        // makes an answer possible at all.
        None => StdinRead::NotAPipe,
    }
}

/// **Is any thread of this process blocked in a pipe read?**
///
/// Every thread, not the group leader: `/proc/<pid>/wchan` is the leader's, and a program
/// whose reader is a thread it spawned would be invisible through that one file. A process
/// this daemon cannot read has no threads it can read either, and `false` is the answer —
/// [`waiting_for_an_answer`] is where that becomes `Unreadable` rather than `No`.
fn blocked_reading(pid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    let mut any = false;
    for e in entries.flatten() {
        any = true;
        if let Ok(wchan) = std::fs::read_to_string(e.path().join("wchan"))
            && is_a_pipe_read(wchan.trim())
        {
            return true;
        }
    }
    let _ = any;
    false
}

/// **The three names a blocked pipe read has had**, and nothing else.
///
/// Measured on this box: `anon_pipe_read`. Older kernels: `pipe_read`, and older still
/// `pipe_wait`. Suffix matching rather than equality, because a kernel that prefixes the
/// symbol (as this one does with `anon_`) is still naming the same place, and equality
/// would make the feature silently kernel-dependent.
///
/// **Not `ends_with("read")`**, and that is the whole of the care here: `filemap_read`,
/// `unix_stream_read_generic` and `tcp_recvmsg` all end in a read and none of them is a
/// program waiting for a person to type a line.
pub fn is_a_pipe_read(wchan: &str) -> bool {
    wchan.ends_with("pipe_read") || wchan.ends_with("pipe_wait")
}

/// **The last line of what the run has written — to SHOW, never to decide.**
///
/// This is the display half of the card: the person is being asked to answer the program,
/// so the program's own last words are what they need to see. There is no test on their
/// content and there must never be one — see the module header. `None` when the run has
/// written nothing, which is a real case (`! cat`, blocked before it prints anything) and
/// is reported as *nothing to show* rather than as an empty line.
///
/// `\r` ends a line as well as `\n`: a progress bar repaints with a carriage return and no
/// newline, so splitting on `\n` alone would put a whole download on one line.
pub fn last_line(tail: &str) -> Option<String> {
    let line = tail.rsplit(['\n', '\r']).find(|l| !l.trim().is_empty())?;
    let line = line.trim_end();
    (!line.is_empty()).then(|| clip_front(line, MAX_SHOWN))
}

/// **The most of the program's own line the card carries.**
///
/// A card is one line of a conversation and not a page. A line longer than this is clipped
/// from the **front**, because the tail of a prompt is where the answer's own shape lives
/// (`Continue? [Y/n]`) and the head of it is context a person can live without.
pub const MAX_SHOWN: usize = 200;

/// Keep the end, and say a front was dropped rather than pretending the line began there.
fn clip_front(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let skip = line.chars().count() - max;
    let mut s = String::from("…");
    s.extend(line.chars().skip(skip));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// **The signal is about the process and not about its words.**
    ///
    /// A child whose stdin is a pipe this test holds is blocked in a pipe read, and that is
    /// what is reported — **whatever it wrote first**. The two cases are the same program
    /// with different output, one of them the operator's own `Continue? [Y/n]` and the other
    /// a language with no marker this file has ever heard of: both are `Yes`, which is the
    /// whole point of asking the process instead.
    #[test]
    fn a_child_blocked_on_the_pipe_we_hold_is_waiting() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("printf 'Weiter? [J/n] '; read x")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sh starts");
        let mut w = child.stdin.take().expect("a pipe this test holds");
        let ino = pipe_inode(w.as_raw_fd()).expect("the write end is a pipe");
        // Give it a moment to reach the read.
        let mut waiting = Waiting::No;
        for _ in 0..100 {
            waiting = waiting_for_an_answer(&[child.id()], Some(ino));
            if waiting == Waiting::Yes {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            waiting,
            Waiting::Yes,
            "a process blocked reading the pipe this test holds must be reported as waiting, \
             and the words it printed are not what decided it"
        );
        // The answer releases it, and the same call then says no.
        {
            use std::io::Write;
            w.write_all(b"J\n").expect("the answer reaches it");
            w.flush().ok();
        }
        let _ = child.wait();
    }

    /// **A process that is not reading this pipe is not waiting on it.**
    ///
    /// The control, and the one that keeps the card honest in a pipeline: a `grep` in
    /// `! ls | grep foo` is blocked in a pipe read — on *ls*'s pipe, not on the one this
    /// daemon holds. The fd-0 comparison is what tells them apart, and without it a slow
    /// `ls` would raise a card claiming the run was waiting for a line.
    #[test]
    fn a_process_reading_some_other_pipe_is_not_waiting_on_ours() {
        // Our pipe: the daemon's end, held here and never written to.
        let mut ours = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("sh starts");
        let w = ours.stdin.take().expect("our pipe");
        let ino = pipe_inode(w.as_raw_fd()).expect("a pipe");

        // A child whose stdin is a DIFFERENT pipe, blocked reading it.
        let mut other = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("read x")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("sh starts");
        let _keep = other.stdin.take().expect("the other pipe");
        let mut seen = Waiting::No;
        for _ in 0..100 {
            seen = waiting_for_an_answer(&[other.id()], Some(ino));
            if seen == Waiting::Yes {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_ne!(
            seen,
            Waiting::Yes,
            "a process blocked on somebody else's pipe is not waiting for a line from us"
        );

        let _ = ours.kill();
        let _ = other.kill();
        let _ = ours.wait();
        let _ = other.wait();
    }

    /// **No `/proc` reading is a fact, not a `No`.**
    ///
    /// The confined-session case, and the one that decides the shape of the answer: a
    /// daemon that cannot look at the run must not report that the run is not asking. The
    /// two ways it happens are both here — no pipe inode to compare against, and no process
    /// list to walk — and both must be `Unreadable` rather than `No`.
    #[test]
    fn a_signal_that_cannot_be_read_is_not_a_no() {
        assert_eq!(
            waiting_for_an_answer(&[], Some(1234)),
            Waiting::Unreadable,
            "a host that cannot list the job's processes has not said nothing is running"
        );
        assert_eq!(
            waiting_for_an_answer(&[std::process::id()], None),
            Waiting::Unreadable,
            "with no pipe inode there is nothing to compare fd 0 against"
        );
        // A pid that does not exist: nothing could be read about it at all.
        assert_eq!(
            waiting_for_an_answer(&[u32::MAX - 1], Some(1234)),
            Waiting::Unreadable
        );
        // And this process, whose stdin is a terminal or `/dev/null` and not a pipe at
        // all: **read, and definitively not waiting** — which is the difference between
        // this case and the two above.
        assert_eq!(
            waiting_for_an_answer(&[std::process::id()], Some(1234)),
            Waiting::No
        );
    }

    /// **The kernel names, and the near misses that must not pass.**
    ///
    /// `anon_pipe_read` is what this box says (measured 2026-09-25); `pipe_read` and
    /// `pipe_wait` are older kernels. Everything else in the list is a read that is **not**
    /// a program waiting for a person, and `tcp_recvmsg` and `unix_stream_read_generic` are
    /// the two that would fire on every network wait if the test were `ends_with("read")`.
    #[test]
    fn only_a_pipe_read_counts_as_blocked_on_a_pipe() {
        for yes in ["anon_pipe_read", "pipe_read", "pipe_wait"] {
            assert!(is_a_pipe_read(yes), "{yes} is a blocked pipe read");
        }
        for no in [
            "0",
            "",
            "filemap_read",
            "generic_file_read_iter",
            "unix_stream_read_generic",
            "tcp_recvmsg",
            "do_wait",
            "hrtimer_nanosleep",
            "pipe_write",
            "wait_woken",
        ] {
            assert!(!is_a_pipe_read(no), "{no} is not a blocked pipe read");
        }
    }

    /// **The text is shown and never read.** `last_line` returns the program's own words —
    /// including a question mark, including a language nobody here speaks, including a
    /// password prompt — and none of those is treated differently, because nothing in this
    /// module looks at what the line says.
    ///
    /// This is the test that keeps the `Continue?` matcher from growing back: the four
    /// lines below are the operator's own case, a Welsh one, a bare `?` and a password
    /// prompt, and every one of them is answered exactly the same way.
    #[test]
    fn the_text_is_shown_and_never_decided_on() {
        assert_eq!(
            last_line("Reading...\nDo you want to continue? [Y/n] ").as_deref(),
            Some("Do you want to continue? [Y/n]")
        );
        assert_eq!(
            last_line("A ydych yn siwr? [Y/n]").as_deref(),
            Some("A ydych yn siwr? [Y/n]")
        );
        assert_eq!(last_line("just a ?").as_deref(), Some("just a ?"));
        assert_eq!(
            last_line("[sudo] password for dead: ").as_deref(),
            Some("[sudo] password for dead:"),
            "the card may SHOW this; whether it is a question is the process's answer"
        );
        // `\r` ends a line: a progress bar repaints with a carriage return and no newline.
        assert_eq!(
            last_line("50%\r100%\rProceed [y/N] ").as_deref(),
            Some("Proceed [y/N]")
        );
        // Nothing written is `None` and not an empty string — `! cat` blocked before it
        // printed anything is a real case and says so.
        assert_eq!(last_line(""), None);
        assert_eq!(last_line("\n"), None);
        // A long line keeps its end.
        let long = format!("{} Proceed [y/N]", "x".repeat(400));
        let got = last_line(&long).expect("a last line");
        assert!(got.starts_with('…'), "{got}");
        assert!(got.ends_with("Proceed [y/N]"), "{got}");
    }

    /// **There is no `Continue?` in this file.** The operator's correction, as an assertion
    /// about the source rather than about a behaviour: the module must not be able to grow
    /// a list of question shapes back into it, because that is the change that looks
    /// reasonable and fails on the next program.
    #[test]
    fn no_text_is_matched_anywhere_in_this_module() {
        let src = include_str!("ask.rs");
        // **Built with `concat!` so this test's own needles do not count as hits.** The
        // first cut spelled them as literals and the test failed against itself, which is
        // a false positive that would have been silenced by deleting the test rather than
        // by fixing it — and the test is the thing worth keeping.
        let needles = [
            concat!("contains", "(\"?\")"),
            concat!("ends_with", "('?')"),
            concat!("ends_with", "(\"?\")"),
            concat!("contains", "(\"[Y/n]\")"),
            concat!("is_some_and(|l| l", ".ends_with"),
        ];
        for forbidden in needles {
            assert!(
                !src.contains(forbidden),
                "`{forbidden}` is a text match on the question: the detection is the \
                 process's state, and the text is only ever shown"
            );
        }
    }
}
