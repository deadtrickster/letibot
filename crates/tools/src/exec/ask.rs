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
//! The run's input is **the terminal this daemon holds the master of**
//! ([`super::jobs::Stdin`], and [`super::pty`]'s header for why it is a terminal and no
//! longer a pipe beside one), so *"the program is waiting for an answer"* is a **fact about
//! the process** and not a guess about its English. Four conditions, and every one of them
//! is a reading of `/proc` or of a clock:
//!
//! 1. **The run is still alive.** A program that printed something and exited is not
//!    waiting for anything, and a card for it would be a card nobody can answer.
//! 2. **It has written nothing for a beat.** A program that is asking and still drawing is
//!    a program that is not blocked; see the misses below.
//! 3. **Some process of the run has OUR terminal on fd 0 and is blocked in a read on that
//!    same descriptor.** `/proc/<pid>/fd/0` names the device — `pipe:[<inode>]` for the
//!    pty-less fallback, `/dev/pts/N` for the terminal — and it is compared with what the
//!    daemon holds, so *a process reading somebody else's descriptor* (the `grep` in
//!    `! ls | grep foo`, waiting on `ls`) is not this, and neither is a process reading a
//!    file. `/proc/<pid>/wchan` then says what it is blocked **in**, and
//!    `/proc/<pid>/task/<tid>/syscall` says **which descriptor** — the two together are the
//!    whole of it, and the second is what keeps `sudo` from reading as a question (see
//!    miss 1).
//! 4. **Nothing has been typed at it for a beat** ([`super::jobs::Stdin::since_last_input`]).
//!    This one is new with the terminal and it is not decoration: a program that has just
//!    been answered is blocked reading its terminal again within microseconds, so without
//!    this the daemon would raise a card about a question the person answered a moment ago
//!    — every time they answered one.
//!
//! Conditions 1, 2 and 4 are the caller's, because the caller is the one holding the clocks
//! and the job state; this module is 3, plus [`last_line`], which is the *display* half and
//! decides nothing.
//!
//! # The wchan names, and the fact that they are kernel-version-specific
//!
//! A process blocked in `read(2)` sits in the kernel's read for that device, and `wchan`
//! names it. Two families, and they are different names because they are different code:
//!
//! * **A pipe**: measured on this box (2026-09-25, `anon_pipe_read`) — the modern name.
//!   Older kernels call it `pipe_read`, and older still `pipe_wait`.
//! * **A terminal**: measured on this box (2026-10-06, **`wait_woken`**) — and it is the
//!   same name for `sh -c 'read x'`, for `python3 -c 'os.read(0,1)'` and for `cat` with no
//!   arguments. Named because it is *not* obvious and it is not a pipe read: this is the
//!   measurement the whole rule was rebuilt on, and a reader who assumed `n_tty_read` would
//!   have written a rule that never fires.
//!
//! [`blocked_reading_fd0`] accepts the three pipe names and the one terminal name by suffix
//! and **nothing else**, and the reason it does not simply look for `read` is that
//! `filemap_read`, `unix_stream_read_generic` and `tcp_recvmsg` all end in a read and none
//! of them is a program waiting for a person.
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
//!
//!    **And the near miss inside this one, which is `sudo`.** `sudo -A` forks the askpass
//!    helper with a pipe on the child's stdout and then blocks in `read(2)` on that pipe —
//!    with its own fd 0 untouched, which in an operator's `!` run is the pipe the daemon
//!    holds. So it satisfies both halves of a reading that only compares fd 0 and `wchan`,
//!    and the daemon would raise a card for a question nobody asked while the person is
//!    typing their password. `syscall`'s first argument is what tells the two apart —
//!    measured on this box, the substitution that has sudo's shape is `read(3, …)` and a
//!    program genuinely waiting is `read(0, …)`.
//! 2. **A program that asks and keeps drawing.** A build with a spinner, `apt` between two
//!    of its own progress lines: the process is not blocked, so nothing is raised. *What is
//!    on the screen* is the whole question and this reads a process.
//! 3. **A thread, not a process.** `wchan` is per thread and `/proc/<pid>/wchan` is the
//!    thread group leader's. [`blocked_reading_fd0`] reads **every** thread under
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
//!
//!    **And it is not only the case where NOTHING could be read — that was this module's own
//!    defect.** A `! sudo apt install mc` is `bash` (this uid, readable, in `wait(2)`) over
//!    `sudo` and then `apt` (**root**, so `/proc/<pid>/fd/0` is `EACCES` for the daemon, and
//!    `wchan` reads `0` — the kernel's own *could not look*). Reading the shell and reporting
//!    `No` says *nobody is asking* about a process that was never looked at, which is the
//!    empty-haystack mistake this enum exists to prevent; `apt` waiting at `Continue? [Y/n]`
//!    is then invisible, no card is raised, and the run sits on the pipe until its deadline.
//!    **`No` requires having looked at EVERY process** — see [`waiting_for_an_answer`].
//! 5. **A kernel whose wchan name is none of the four.** Then nothing is raised and the
//!    reason is invisible. Named here because it is the one miss that would look like the
//!    feature never firing.
//! 6. **A wait that is named `wait_woken` and whose first syscall argument happens to be
//!    `0` without being a read of the run's terminal.** The terminal's name is the generic
//!    one — `wait_woken` is where *many* blocking reads sit — so on this path the descriptor
//!    carries the weight the name used to: the process must have OUR terminal on fd 0 **and**
//!    be blocked with `0` as its first argument. A blocked `splice(0, …)`, `read(0, …)` or
//!    `readv(0, …)` is a read of that terminal; a `pselect6(0, …)` with no descriptors, if a
//!    kernel ever parked one in `wait_woken`, would be a false card. Named rather than
//!    claimed away, and the remedy is the same as every other miss's: `!send`.
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
// `is_char_device` is a `FileTypeExt` method and not a `FileType` one, so the trait has to be
// in scope where `stdin_of` asks whether fd 0's target is a character device.
use std::os::unix::fs::FileTypeExt;

/// **Which descriptor the daemon is holding for a run** — the identity a process's fd 0 is
/// compared against, and the two kinds are not interchangeable.
///
/// The comparison exists because *"blocked reading"* is not the question on its own: a
/// `grep` blocked on `ls`'s pipe in `! ls | grep foo` is blocked in a read, and a card for it
/// would be the daemon claiming the run was waiting for a line. What makes it ours is the
/// **device**, and each kind of device has one name the kernel will give both sides:
/// an inode for a pipe, the slave's path for a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEnd {
    /// The inode of the pipe's write end — both ends share it, so the process's
    /// `pipe:[<inode>]` and ours are comparable. The pty-less fallback; see
    /// [`super::jobs::Stdin`].
    Pipe(u64),
    /// **The slave's own name**, `/dev/pts/N` on this box. The master is `/dev/ptmx` for
    /// every pty on the box and answers nothing; the slave's name is unique per pty.
    Terminal(String),
}

/// **Is the run waiting for an answer?** — three answers, and the third is not the second.
///
/// `No` and `Unreadable` are not interchangeable and are not merged: *"it is not asking"*
/// and *"this daemon cannot see whether it is"* are different facts, and a caller that
/// rendered the second as the first would be reporting a capability it does not have — the
/// same distinction [`super::host::ProcessHost::monitors`] keeps for an empty list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// The signal was read, and it says yes: a process of this run has our device on its
    /// fd 0 and is blocked in a read on it.
    Yes,
    /// The signal was read, and it says no.
    No,
    /// **The signal could not be read** — `/proc` refused for every process of the run, or
    /// the host cannot list them at all. See the module header's miss 4.
    Unreadable,
}

/// **The whole of condition 3**, as one call: `pids` are the run's processes and `ours` is
/// the device this daemon holds for the run, or `None` when it holds none.
///
/// # `Yes`, `No`, and what each one costs to say
///
/// `Yes` is **knowledge** and short-circuits: one process of the run was *read*, its fd 0 is
/// this device, and it is blocked in a read on that descriptor. Nothing another process could
/// say makes that untrue, so an unreadable sibling does not turn it into a maybe.
///
/// `No` is knowledge too, and it is the one that has to be earned: **it is returned only when
/// every process of the run was looked at** and none of them is reading our device. That is
/// the correction this function needed. It used to answer `No` when *some* process had been
/// read, which made the two facts the caller cares about — *nobody is asking* and *I cannot
/// see whether anybody is* — into one, and the operator's own report is what that costs: a
/// `! sudo apt install mc` runs `bash` (this uid, readable, in `wait(2)`) over `sudo` and then
/// `apt`, both **root**, and `/proc/<pid>/fd/0` is `EACCES` for a uid that is not theirs. The
/// shell was read, the process that was actually waiting on the pipe was not, and the answer
/// came back `No` — so no card was ever raised and the run sat until its deadline. **A list
/// with a hole in it is not an empty list**, which is the rule [`super::host::ProcessHost::job_pids`]
/// states for the list itself and which this now keeps one layer up.
///
/// `None` for `ours` is [`Waiting::Unreadable`] rather than `No`, and so is an empty `pids`
/// — a host that cannot say which processes a job has has not said *nothing is running*.
pub fn waiting_for_an_answer(pids: &[u32], ours: Option<&InputEnd>) -> Waiting {
    let Some(ours) = ours else {
        return Waiting::Unreadable;
    };
    if pids.is_empty() {
        return Waiting::Unreadable;
    }
    let mut unseen = false;
    for pid in pids {
        // `Unreadable` here is *this process could not be looked at* — a uid the daemon
        // is not (the `sudo` case), a process that left between the cgroup read and this
        // one, a `/proc` a confined session's daemon may not open. It is not a `No`.
        match stdin_of(*pid) {
            StdinRead::Unreadable => unseen = true,
            StdinRead::Other => {}
            StdinRead::Pipe(their_ino) => {
                if matches!(ours, InputEnd::Pipe(ino) if *ino == their_ino)
                    && blocked_reading_fd0(*pid)
                {
                    return Waiting::Yes;
                }
            }
            StdinRead::Device(path) => {
                if matches!(ours, InputEnd::Terminal(name) if *name == path)
                    && blocked_reading_fd0(*pid)
                {
                    return Waiting::Yes;
                }
            }
        }
    }
    // **`No` only if there is nothing left unlooked-at.** See the docs above: one process the
    // daemon could not open is one it cannot say *is not* asking.
    if unseen {
        Waiting::Unreadable
    } else {
        Waiting::No
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

/// **What a process's fd 0 is** — four answers, because *not our kind of device*, *not a
/// device at all* and *could not look* are three different facts and the caller treats them
/// differently.
///
/// This is the distinction that makes [`waiting_for_an_answer`] able to say `No` at all: a
/// process whose stdin is a terminal has been **looked at** and is definitively not reading
/// the device this daemon holds, where a process whose `/proc/<pid>/fd/0` this uid may not
/// read has not been looked at and nothing can be concluded. Rendering the second as the
/// first would be the empty-haystack bug with a card on it.
///
/// `Pipe` and `Device` are the two kinds [`InputEnd`] can be. `Other` is the rest — a
/// regular file, a socket, `/dev/null` — and it is its own arm rather than folded into
/// `Device` because a path that is not a device has nothing to compare with either.
enum StdinRead {
    /// fd 0 is a pipe, and this is its inode.
    Pipe(u64),
    /// fd 0 is a character device, and this is the name the kernel gives it — `/dev/pts/N`
    /// for the run's own terminal. The comparison against ours is on the whole name.
    Device(String),
    /// Read, and fd 0 is neither: a regular file, a socket, `/dev/null`.
    Other,
    /// **This process could not be looked at**: `EACCES` for a uid the daemon is not (the
    /// confined-session case), or a process that left between the cgroup read and this one.
    Unreadable,
}

/// `/proc/<pid>/fd/0` is a symlink whose target the kernel writes as `pipe:[<inode>]` for a
/// pipe and as the device's path for anything else, and the comparison is on the whole of it.
///
/// **A character device and not "anything with a slash in it".** `S_IFCHR` is asked of the
/// link's target rather than of the link, which is why the `Device` arm carries the path: the
/// answer has to be comparable with [`super::pty::Pty::slave_path`], and a name that was
/// merely *not a pipe* would match a regular file the run had redirected its stdin to.
///
/// # The shapes the target takes, measured on this box 2026-10-07
///
/// A live child with each kind of descriptor on fd 0, `readlink /proc/<pid>/fd/0`:
///
/// | fd 0 is | the link says | `stat` |
/// |---|---|---|
/// | the run's terminal | `/dev/pts/N` | character device |
/// | `/dev/null` | `/dev/null` | character device |
/// | a regular file | `/tmp/x` | not a device |
/// | a socket | `socket:[N]` | **`ENOENT`** |
/// | a deleted file | `/tmp/#N (deleted)` | **`ENOENT`** |
/// | a pipe | `pipe:[N]` | handled above, never reached |
///
/// **The two `ENOENT`s are why a failed `stat` is [`StdinRead::Other`] and not
/// [`StdinRead::Unreadable`].** A kernel-synthesised name (`socket:[N]`, `anon_inode:[N]`)
/// is not a path, and `(deleted)` is a path to nothing; both were **read**, and neither can be
/// the terminal this daemon holds — a pty slave is a character device at a path that exists
/// for as long as the run does, and this daemon named it itself. So `Unreadable` is exactly
/// *`read_link` failed*, which is the one case where nothing about fd 0 was established: an
/// `EACCES` for a uid the daemon is not, or a process that left between the cgroup read and
/// this one. Reporting the two `ENOENT`s as `Unreadable` would be the daemon saying *I cannot
/// tell* about a descriptor it had just read — the empty-haystack mistake with the sign
/// flipped, and it would put a sentence about the daemon's own reach on a run whose stdin is
/// a socket.
fn stdin_of(pid: u32) -> StdinRead {
    let Ok(target) = std::fs::read_link(format!("/proc/{pid}/fd/0")) else {
        return StdinRead::Unreadable;
    };
    let Some(text) = target.to_str() else {
        return StdinRead::Other;
    };
    if let Some(rest) = text.strip_prefix("pipe:[") {
        return match rest.strip_suffix(']').and_then(|n| n.parse().ok()) {
            Some(ino) => StdinRead::Pipe(ino),
            // It said `pipe:[` and then something that is not a number, which no kernel
            // does. `Other` rather than `Unreadable`: the link was READ, and reading it is
            // what makes an answer possible at all.
            None => StdinRead::Other,
        };
    }
    // A device, and the only devices a run's fd 0 can be *ours* through are character ones.
    // `stat` on the target follows the link and is answered in the daemon's own namespace.
    match std::fs::metadata(&target) {
        Ok(m) if m.file_type().is_char_device() => StdinRead::Device(text.to_string()),
        // Read, and not a character device — a regular file, a socket's backing store, a
        // `/dev/null` this run is not on.
        Ok(_) => StdinRead::Other,
        // Read, and naming something that is not there: `socket:[N]`, `anon_inode:[N]`, a
        // file deleted out from under the run. See the table above — **not `Unreadable`**.
        Err(_) => StdinRead::Other,
    }
}

/// **Is any thread of this process blocked in a read on ITS fd 0?**
///
/// Every thread, not the group leader: `/proc/<pid>/wchan` is the leader's, and a program
/// whose reader is a thread it spawned would be invisible through that one file. A process
/// this daemon cannot read has no threads it can read either, and `false` is the answer —
/// [`waiting_for_an_answer`] is where that becomes `Unreadable` rather than `No`.
///
/// # The descriptor, which `wchan` alone does not give
///
/// `wchan` says *a* read and not **whose**. `sudo -A` is the case that made this load
/// bearing: it forks the askpass helper with a pipe on the child's stdout and blocks in
/// `read(2)` on that pipe, while its own fd 0 is the device this daemon holds. Compared on
/// `wchan` and fd 0 alone, that is indistinguishable from a program waiting for a person, and
/// the card it raised was for a question nobody asked — MEASURED 2026-10-06, on a run of
/// sudo's exact shape: `PromptRequested question=Some("bash: no job control in this shell")`
/// while the person was still typing their password.
///
/// So the descriptor is read too. `/proc/<tid>/syscall`'s first field is the syscall number
/// and the second is its first argument, which for `read(2)`, `readv(2)` and `splice(2)`
/// **is the fd** — and those are the three a blocked read of a terminal or a pipe can be.
/// Measured on this box: the substitution with sudo's shape is `0 0x3 …` (`read(3, …)`),
/// a program genuinely waiting for a line is `0 0x0 …` (`read(0, …)`), and a shell whose
/// command substitution is slow is `0 0x7 …` (`read(7, …)`) — the same `wchan` as a real
/// question, which is why this second reading exists at all.
///
/// **The name is the weaker half of the pair now, and that is a change the terminal made.**
/// A pipe read has its own `wchan`; a terminal read is `wait_woken`, which is where *many*
/// blocking reads sit. So the name is asked only *is this a read at all* (it excludes
/// `do_wait`, `hrtimer_nanosleep`, `pipe_write`), and the descriptor does the distinguishing.
/// The miss that leaves is named in the module header, item 6.
///
/// **A thread whose `syscall` cannot be read is not counted**, which is the conservative
/// direction and the same one miss 4 takes: a card raised on a guess is worse than a person
/// deciding, and the person's way in is `!send`. It costs nothing on a box where the two files
/// move together — measured here, `/proc/<pid>/fd/0` and `/proc/<pid>/task/<tid>/syscall` are
/// both readable for this uid and both `EACCES` for a uid that is not.
fn blocked_reading_fd0(pid: u32) -> bool {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    for e in entries.flatten() {
        let Ok(wchan) = std::fs::read_to_string(e.path().join("wchan")) else {
            continue;
        };
        let wchan = wchan.trim();
        if !(is_a_pipe_read(wchan) || is_a_tty_read(wchan)) {
            continue;
        }
        if blocked_on_fd(&e.path().join("syscall")) == Some(0) {
            return true;
        }
    }
    false
}

/// **The descriptor a blocked thread is in a syscall on**, from `/proc/<tid>/syscall`.
///
/// `None` when the file cannot be read or does not hold a syscall: the kernel answers
/// `running` for a task that is not in one, and `-1` with zeroes for a task it will not
/// describe. Both are *could not look*, and both are answered by not counting the thread.
fn blocked_on_fd(syscall: &std::path::Path) -> Option<i64> {
    let text = std::fs::read_to_string(syscall).ok()?;
    let mut fields = text.split_whitespace();
    let _nr = fields.next()?;
    // `0x3`, and never a negative: a descriptor is an index. A `-1` here is the kernel's
    // *not described* and it parses as a number, so the sign is checked and not only the
    // parse — otherwise a process the kernel declined to describe would read as fd `-1`,
    // which is not fd 0 and is therefore already the safe answer.
    let arg0 = fields.next()?;
    let value = i64::from_str_radix(arg0.trim_start_matches("0x"), 16).ok()?;
    (value >= 0).then_some(value)
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
///
/// Still asked, although the run's fd 0 is a terminal now: the pty-less fallback puts a pipe
/// there ([`super::jobs::Stdin`]), and a *program* may put one there itself. Both are reads of
/// the descriptor, and which device the descriptor is stays the other half of the question.
pub fn is_a_pipe_read(wchan: &str) -> bool {
    wchan.ends_with("pipe_read") || wchan.ends_with("pipe_wait")
}

/// **The one name a blocked read of a TERMINAL has on this box**, and nothing else.
///
/// Measured 2026-10-06, three programs on a pty whose slave is their fd 0 and whose
/// controlling terminal it is: `/bin/sh -c 'read x'`, `python3 -c 'os.read(0,1)'` and
/// `/bin/cat` with no arguments all read **`wait_woken`** — and the third of them is blocked
/// in `splice(2)`, not `read(2)`, so this name is not a name for one syscall either.
///
/// **It is the generic one and that is the honest weakness**: `wait_woken` is where many
/// blocking reads sit, where `anon_pipe_read` names the pipe read exactly. The descriptor
/// carries the distinguishing now — see [`blocked_reading_fd0`], and the module header's
/// item 6 for the miss that leaves. It is not narrowed to a guess: a name that was *not*
/// measured here would be a rule that fires on the kernel it was written on and nowhere
/// else, which is the defect this whole section is about.
pub fn is_a_tty_read(wchan: &str) -> bool {
    wchan.ends_with("wait_woken")
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
            waiting = waiting_for_an_answer(&[child.id()], Some(&InputEnd::Pipe(ino)));
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
            seen = waiting_for_an_answer(&[other.id()], Some(&InputEnd::Pipe(ino)));
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

    /// **A process blocked on a pipe that is NOT ours is not waiting on ours** — even when its
    /// fd 0 *is* ours.
    ///
    /// This is `sudo`'s exact shape, and it is the false card the operator's own report was
    /// first read as. `sudo -A` forks the askpass helper with a pipe on the child's stdout and
    /// blocks in `read(2)` on **that** pipe, while its own fd 0 is the pipe the daemon holds —
    /// so fd 0 matches and `wchan` says `anon_pipe_read`, and a reading that stops there raises
    /// a card claiming the run is waiting for a line while the person is still typing their
    /// password. MEASURED 2026-10-06 on a run of that shape: the daemon published
    /// `PromptRequested question=Some("bash: no job control in this shell")` — a card for a
    /// question nobody asked, which also takes the run's ONE open card, so the question the
    /// command really asks later can never be raised.
    ///
    /// `pw=$(sleep 30)` is that shape: the shell forks the substitution with a pipe on its
    /// stdout and blocks reading it, and its fd 0 is untouched. The control is in the same
    /// test — the identical shell with `read x`, which is `read(0, …)` and must still be `Yes`.
    #[test]
    fn a_process_blocked_on_its_own_pipe_is_not_waiting_on_ours() {
        let ours = |command: &str| {
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("sh starts");
            let w = child.stdin.take().expect("the pipe this test holds");
            let ino = pipe_inode(w.as_raw_fd()).expect("the write end is a pipe");
            let mut seen = Waiting::No;
            for _ in 0..100 {
                seen = waiting_for_an_answer(&[child.id()], Some(&InputEnd::Pipe(ino)));
                if seen != Waiting::No {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            (seen, child, w)
        };

        // ---- sudo's shape: a pipe of its own, and fd 0 left alone.
        let (seen, mut child, _w) = ours("pw=$(sleep 30)");
        assert_ne!(
            seen,
            Waiting::Yes,
            "a process blocked reading a pipe it made itself is not waiting for a line from us, \
             whatever its fd 0 is"
        );
        let _ = child.kill();
        let _ = child.wait();

        // ---- The control, which the correction must not have cost: the same shell, genuinely
        //      blocked on the descriptor we hold.
        let (seen, mut child, _w) = ours("read x");
        assert_eq!(
            seen,
            Waiting::Yes,
            "a program reading ITS STDIN — which is our pipe — is still waiting for a line"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// **A process the daemon may not look at is not a `No`.**
    ///
    /// The correction that the operator's report needed, and the reason is the shape of
    /// `! sudo apt install mc` as a process tree: `bash` (this uid, readable, in `wait(2)`) over
    /// `sudo` and then `apt`, which run as **root**. `/proc/<pid>/fd/0` is `EACCES` for a uid
    /// that is not theirs — MEASURED 2026-10-06: `readlink /proc/1/fd/0` is `PermissionDenied`,
    /// and so is the same read of a process of this uid that has made itself undumpable, which
    /// is the flag a setuid exec sets. `wchan` for such a process reads `0` — the kernel's own
    /// *could not look* — so it is not a pipe read either.
    ///
    /// Read as `No`, the daemon says *nobody is asking* about a process it never opened, `apt`
    /// waits at `Continue? [Y/n]` invisible, no card is raised and the run sits on the pipe
    /// until its deadline. A list with a hole in it is not an empty list.
    #[test]
    fn a_process_that_could_not_be_looked_at_is_not_a_no() {
        // This process is readable and is not blocked on the pipe inode named here, so it
        // alone would answer `No`. A pid that does not exist cannot be looked at at all — the
        // same `Unreadable` a root-owned `sudo` gives, without needing one.
        assert_eq!(
            waiting_for_an_answer(
                &[std::process::id(), u32::MAX - 1],
                Some(&InputEnd::Pipe(1234))
            ),
            Waiting::Unreadable,
            "one process of the run could not be looked at, so the run's answer is not `No`"
        );
        // And the same call with every process readable is `No`, which is what keeps the rule
        // above from being *never say no*.
        assert_eq!(
            waiting_for_an_answer(&[std::process::id()], Some(&InputEnd::Pipe(1234))),
            Waiting::No,
            "every process was looked at and none is reading this pipe"
        );
    }

    /// **`Yes` is knowledge, and a sibling that could not be read does not take it back.**
    ///
    /// The direction the correction must not break: one process *seen* blocked on our pipe is
    /// the answer, whatever else in the run the daemon could not open. A card is raised on this
    /// and it is right to raise it.
    #[test]
    fn a_process_seen_waiting_is_yes_even_beside_one_that_could_not_be_read() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("read x")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sh starts");
        let w = child.stdin.take().expect("the pipe this test holds");
        let ino = pipe_inode(w.as_raw_fd()).expect("the write end is a pipe");
        let mut seen = Waiting::No;
        for _ in 0..100 {
            seen = waiting_for_an_answer(&[child.id(), u32::MAX - 1], Some(&InputEnd::Pipe(ino)));
            if seen == Waiting::Yes {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            seen,
            Waiting::Yes,
            "a process seen reading our pipe is the answer, whatever else could not be opened"
        );
        let _ = child.kill();
        let _ = child.wait();
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
            waiting_for_an_answer(&[], Some(&InputEnd::Pipe(1234))),
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
            waiting_for_an_answer(&[u32::MAX - 1], Some(&InputEnd::Pipe(1234))),
            Waiting::Unreadable
        );
        // And this process, whose stdin is a terminal or `/dev/null` and not a pipe at
        // all: **read, and definitively not waiting** — which is the difference between
        // this case and the two above.
        assert_eq!(
            waiting_for_an_answer(&[std::process::id()], Some(&InputEnd::Pipe(1234))),
            Waiting::No
        );
    }

    /// **A run whose fd 0 is a socket is `No`, not `Unreadable`** — the arm this module had
    /// wrong, pinned so it cannot come back.
    ///
    /// `socket:[N]` is a kernel-synthesised name and not a path, so `stat` on it fails with
    /// `ENOENT` — measured, and the table is on [`stdin_of`]. Reading that failure as *could
    /// not look* would be the daemon saying **I cannot tell** about a descriptor it had just
    /// read, and it would put the `Unreadable` sentence on every run that hands its own fd 0
    /// to a socket. The link WAS read, and a socket is definitively not the character device
    /// this daemon holds — so the answer is `No`, and this is the one arm where *looked at and
    /// not ours* and *could not look* were rendered as each other.
    ///
    /// The child is genuinely blocked in a read on its own fd 0, which is what makes the
    /// second assertion worth something: nothing but the **device** keeps this from being a
    /// card, so the test would fail if the comparison were dropped as well.
    #[test]
    fn a_socket_on_fd_zero_is_a_no_and_not_an_unreadable() {
        let (child_end, _keep) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let mut child = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
                child_end,
            )))
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("cat starts");
        // A moment to reach its read, so this is not green because the process had not
        // started yet.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let ours = InputEnd::Terminal("/dev/pts/999".to_string());
        let seen = waiting_for_an_answer(&[child.id()], Some(&ours));
        let _ = child.kill();
        let _ = child.wait();
        assert_ne!(
            seen,
            Waiting::Unreadable,
            "fd 0 was READ and named a socket — the daemon looked, and must not report that \
             it could not"
        );
        assert_eq!(
            seen,
            Waiting::No,
            "a socket is definitively not the terminal this daemon holds"
        );
    }

    /// **A program blocked reading the run's own terminal is waiting** — the shape every
    /// operator run has now, and the rule this module's whole third condition was rebuilt
    /// for.
    ///
    /// The measurement behind it is in the header: a tty read is `wait_woken` on this box,
    /// and for `cat` it is not even a `read(2)` — it is `splice(2)`. So the assertion is on
    /// the *device* as well as the block: the same child, asked about **somebody else's**
    /// terminal, must not be waiting on ours. That second pty is what keeps the rule from
    /// being *any tty will do*.
    #[test]
    fn a_child_blocked_on_the_runs_own_terminal_is_waiting() {
        let p = super::super::pty::Pty::open().expect("a pty on this box");
        let mut child = on_the_terminal(&p, "read x");
        let ours = InputEnd::Terminal(p.slave_path().to_string());
        assert_eq!(
            poll(&[child.id()], Some(&ours)),
            Waiting::Yes,
            "a program blocked reading the terminal this daemon holds is waiting for a line"
        );

        // **A second pty in the same test**, because *the name is what makes it ours* is the
        // claim — and a rule that matched any terminal would pass the half above.
        let other = super::super::pty::Pty::open().expect("a second pty");
        let not_ours = InputEnd::Terminal(other.slave_path().to_string());
        assert_ne!(
            poll(&[child.id()], Some(&not_ours)),
            Waiting::Yes,
            "a process reading a terminal that is not ours is not waiting for a line from us"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// **`sudo`'s shape under the terminal** — a process whose fd 0 is our terminal and which
    /// is blocked reading a pipe **it** made.
    ///
    /// This is the near miss the descriptor reading exists for, and the terminal made it
    /// harder rather than easier: `wchan` is `wait_woken` for the pipe and the terminal alike
    /// now, so `read(7, …)` against `read(0, …)` is the *whole* of the difference. `pw=$(sleep
    /// 30)` is that shape — the shell forks the substitution with a pipe on its stdout and
    /// blocks reading it, with fd 0 untouched. The control is in the same test: the identical
    /// shell with `read x`, which is `read(0, …)` and must still be `Yes`.
    #[test]
    fn a_process_blocked_on_its_own_pipe_is_not_waiting_on_the_terminal() {
        let p = super::super::pty::Pty::open().expect("a pty on this box");
        let ours = InputEnd::Terminal(p.slave_path().to_string());

        // ---- sudo's shape: a pipe of its own, and fd 0 left alone.
        let mut child = on_the_terminal(&p, "pw=$(sleep 30)");
        assert_ne!(
            poll(&[child.id()], Some(&ours)),
            Waiting::Yes,
            "a process blocked reading a pipe it made itself is not waiting for a line from us, \
             whatever its fd 0 is"
        );
        let _ = child.kill();
        let _ = child.wait();

        // ---- The control, which the correction must not have cost: the same shell on the
        //      same terminal, genuinely blocked on the descriptor we hold.
        let mut child = on_the_terminal(&p, "read x");
        assert_eq!(
            poll(&[child.id()], Some(&ours)),
            Waiting::Yes,
            "a program reading ITS STDIN — which is our terminal — is still waiting for a line"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// A child with `script` as its command, on the pty the way `host::spawn` puts it there:
    /// all three descriptors, and the pty as its controlling terminal.
    fn on_the_terminal(pty: &super::super::pty::Pty, script: &str) -> std::process::Child {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        cmd.stdin(pty.stdio().unwrap())
            .stdout(pty.stdio().unwrap())
            .stderr(pty.stdio().unwrap());
        super::super::pty::controlling_terminal(&mut cmd, pty.slave_fd());
        cmd.spawn().expect("sh starts")
    }

    /// Poll a run's answer the way the wait loop does, up to two seconds: a child needs a
    /// moment to reach its read, and a single sample would be a test that fails on a loaded box.
    fn poll(pids: &[u32], ours: Option<&InputEnd>) -> Waiting {
        let mut seen = Waiting::No;
        for _ in 0..100 {
            seen = waiting_for_an_answer(pids, ours);
            if seen != Waiting::No {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        seen
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

    /// **The terminal's own name, and the same near misses.**
    ///
    /// `wait_woken` is the one name a blocked tty read has here (measured 2026-10-06, three
    /// programs), and the two directions are both in this test because the name is the
    /// *generic* one: it must count as a terminal read, and it must **not** count as a pipe
    /// read — the two lists are disjoint, which is what keeps a pipe's `wchan` from answering
    /// a question about a terminal and the other way round.
    #[test]
    fn only_a_terminal_read_counts_as_blocked_on_a_terminal() {
        assert!(is_a_tty_read("wait_woken"));
        for no in [
            "0",
            "",
            "anon_pipe_read",
            "pipe_read",
            "pipe_wait",
            "do_wait",
            "hrtimer_nanosleep",
            "pipe_write",
            "tcp_recvmsg",
            "n_tty_read",
        ] {
            assert!(!is_a_tty_read(no), "{no} is not a blocked terminal read");
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
