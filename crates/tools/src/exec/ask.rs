//! Is this run waiting for an answer? A question about the process, not its text.
//! [`waiting_for_an_answer`]: a process of the run has this daemon's device on fd 0, blocked reading.
//! [`last_line`] only displays; [`Waiting::Unreadable`] is a third answer. See [`blocked_reading_fd0`].

use letibot_transcript::text::clip_front_chars;
use std::os::fd::RawFd;
// `is_char_device` comes from `FileTypeExt`, not `FileType`, so the trait must be in scope
// where `stdin_of` asks whether fd 0's target is a character device.
#[cfg(not(target_os = "macos"))]
use std::os::unix::fs::FileTypeExt;

/// Which descriptor the daemon holds for a run, and the identity a process's fd 0 is
/// compared against. A `grep` blocked on `ls`'s pipe is blocked in a read too, so what makes
/// the read ours is the device, and each kind has one name the kernel gives both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEnd {
    /// The inode of the pipe's write end, which both ends share, so the process's
    /// `pipe:[<inode>]` and ours are comparable. The pty-less fallback; see
    /// [`super::jobs::Stdin`].
    Pipe(u64),
    /// The slave's own name, unique per pty. The master is `/dev/ptmx` for every pty on
    /// the box and answers nothing.
    Terminal(String),
}

/// Is the run waiting for an answer? Three answers, and the third is not the second: "it is
/// not asking" and "this daemon cannot see whether it is" are different facts, and rendering
/// the second as the first reports a capability the daemon does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// The signal was read, and it says yes: a process of this run has our device on its
    /// fd 0 and is blocked in a read on it.
    Yes,
    /// The signal was read, and it says no.
    No,
    /// The signal could not be read: `/proc` refused for every process of the run, or the
    /// host cannot list them at all. Not the same answer as `No`.
    Unreadable,
}

/// The whole of condition 3, as one call: `pids` are the run's processes and `ours` is the
/// device this daemon holds for the run. `Yes` short-circuits; `No` only when every process
/// was looked at, since one it could not open is one it cannot say is not asking.
#[cfg(target_os = "macos")]
pub fn waiting_for_an_answer(_pids: &[u32], _ours: Option<&InputEnd>) -> Waiting {
    Waiting::Unreadable
}

#[cfg(not(target_os = "macos"))]
pub fn waiting_for_an_answer(pids: &[u32], ours: Option<&InputEnd>) -> Waiting {
    let Some(ours) = ours else {
        return Waiting::Unreadable;
    };
    if pids.is_empty() {
        return Waiting::Unreadable;
    }
    let mut unseen = false;
    for pid in pids {
        // `Unreadable` here means this process could not be looked at: a uid the daemon is
        // not, a process that left since the cgroup read, a `/proc` a confined daemon may
        // not open. It is not a `No`.
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
    // `No` only if nothing is left unlooked-at: one process the daemon could not open is one
    // it cannot say is not asking.
    if unseen {
        Waiting::Unreadable
    } else {
        Waiting::No
    }
}

/// The inode of the pipe on this descriptor, or `None` when it is not a pipe. `fstat` and not
/// a `/proc` walk, since the daemon holds this end itself; the type is checked as well as the
/// number, because an inode compared across two kinds of object is a coincidence.
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

/// What a process's fd 0 is. Four answers, because not our kind of device, not a device at
/// all and could not look are three different facts, and only the last keeps
/// [`waiting_for_an_answer`] from saying `No`.
#[cfg(not(target_os = "macos"))]
enum StdinRead {
    /// fd 0 is a pipe, and this is its inode.
    Pipe(u64),
    /// fd 0 is a character device, and this is the name the kernel gives it: `/dev/pts/N`
    /// for the run's own terminal. The comparison against ours is on the whole name.
    Device(String),
    /// Read, and fd 0 is neither: a regular file, a socket, `/dev/null`.
    Other,
    /// This process could not be looked at: `EACCES` for a uid the daemon is not, or a
    /// process that left between the cgroup read and this one.
    Unreadable,
}

/// `/proc/<pid>/fd/0` read as a link: `pipe:[<inode>]` for a pipe, the device's path for a
/// character device. A failed `stat` is [`StdinRead::Other`], because a synthesised name
/// (`socket:[N]`) and a `(deleted)` path were both read and neither can be our terminal.
#[cfg(not(target_os = "macos"))]
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
            // `pipe:[` then a non-number, which no kernel does: `Other` rather than
            // `Unreadable`, because the link was read and reading it is what makes an
            // answer possible.
            None => StdinRead::Other,
        };
    }
    // The only devices a run's fd 0 can be ours through are character ones. `stat` on the
    // target follows the link and is answered in the daemon's own namespace.
    match std::fs::metadata(&target) {
        Ok(m) if m.file_type().is_char_device() => StdinRead::Device(text.to_string()),
        // Read, and not a character device: a regular file, a socket's backing store, a
        // `/dev/null` this run is not on.
        Ok(_) => StdinRead::Other,
        // Read, and naming something that is not there: `socket:[N]`, `anon_inode:[N]`, a
        // file deleted out from under the run. Not `Unreadable` - the link was read.
        Err(_) => StdinRead::Other,
    }
}

/// Is any thread of this process blocked in a read on ITS fd 0? Every thread, since
/// `/proc/<pid>/wchan` is the leader's only; `syscall`'s first argument supplies the
/// descriptor it lacks, and an unreadable thread is not counted - the caller's `Unreadable`.
#[cfg(not(target_os = "macos"))]
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

/// The descriptor a blocked thread is in a syscall on, from `/proc/<tid>/syscall`. `None` when
/// the file cannot be read or holds no syscall: the kernel answers `running` for a task not in
/// one and `-1` for a task it will not describe, and both are could not look.
#[cfg(not(target_os = "macos"))]
fn blocked_on_fd(syscall: &std::path::Path) -> Option<i64> {
    let text = std::fs::read_to_string(syscall).ok()?;
    let mut fields = text.split_whitespace();
    let _nr = fields.next()?;
    // A descriptor is an index and never negative: the kernel's `-1` parses as a number, so
    // the sign is checked and not only the parse.
    let arg0 = fields.next()?;
    let value = i64::from_str_radix(arg0.trim_start_matches("0x"), 16).ok()?;
    (value >= 0).then_some(value)
}

/// The three names a blocked pipe read has had, and nothing else. Suffix matching, because a
/// kernel that prefixes the symbol names the same place; and not a match on `read`, because
/// `filemap_read` and `tcp_recvmsg` end in one and neither waits for a person.
pub fn is_a_pipe_read(wchan: &str) -> bool {
    wchan.ends_with("pipe_read") || wchan.ends_with("pipe_wait")
}

/// The one name a blocked read of a terminal has here, and nothing else. It is the generic
/// one - many blocking reads sit in `wait_woken` - so the descriptor carries the
/// distinguishing, and narrowing the name to a guess would make the rule kernel-specific.
pub fn is_a_tty_read(wchan: &str) -> bool {
    wchan.ends_with("wait_woken")
}

/// The last line of what the run has written, to SHOW and never to decide: the person is
/// answering the program, so its own last words are what they need. `None` when nothing was
/// written, and `\r` ends a line as well as `\n` because a progress bar repaints.
pub fn last_line(tail: &str) -> Option<String> {
    let line = tail.rsplit(['\n', '\r']).find(|l| !l.trim().is_empty())?;
    let line = line.trim_end();
    (!line.is_empty()).then(|| clip_front_chars(line, MAX_SHOWN))
}

/// The most of the program's own line the card carries: a card is one line of a conversation,
/// not a page. Clipped from the front, because the tail of a prompt is where the answer's own
/// shape lives and the head is context a person can live without.
pub const MAX_SHOWN: usize = 200;

#[cfg(test)]
mod tests {
    /// macOS has no reading of the signal, and says so rather than answering `No`.
    #[test]
    #[cfg(target_os = "macos")]
    fn on_macos_the_signal_is_unreadable_and_never_a_no() {
        let ours = InputEnd::Pipe(1);
        assert_eq!(
            waiting_for_an_answer(&[std::process::id()], Some(&ours)),
            Waiting::Unreadable
        );
    }

    use super::*;
    use std::os::fd::AsRawFd;

    /// The signal is about the process and not about its words: a child blocked on a pipe this
    /// test holds is reported waiting whatever it wrote first, and the two cases are the same
    /// program with different output.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
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
        // The answer releases it, so the child can exit.
        {
            use std::io::Write;
            w.write_all(b"J\n").expect("the answer reaches it");
            w.flush().ok();
        }
        let _ = child.wait();
    }

    /// A process that is not reading this pipe is not waiting on it: the `grep` in
    /// `! ls | grep foo` is blocked in a pipe read, on ls's pipe and not on the one the daemon
    /// holds. The fd-0 comparison is what tells them apart.
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

    /// A process blocked on a pipe that is NOT ours is not waiting on ours, even when its fd 0
    /// is: `sudo -A` forks the askpass helper with a pipe on the child's stdout and blocks in
    /// `read(2)` on that pipe, so fd 0 and `wchan` match and only the descriptor does not.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
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

        // ---- The control: the same shell, genuinely blocked on the descriptor we hold.
        let (seen, mut child, _w) = ours("read x");
        assert_eq!(
            seen,
            Waiting::Yes,
            "a program reading ITS STDIN — which is our pipe — is still waiting for a line"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// A process the daemon may not look at is not a `No`: `bash` over `sudo` and `apt` is
    /// readable while the root processes under it are `EACCES`, and reading only the shell says
    /// nobody is asking about a process never opened. A hole is not an empty list.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
    fn a_process_that_could_not_be_looked_at_is_not_a_no() {
        // This process is readable and is not blocked on the pipe inode named here, so it alone
        // would answer `No`. A pid that does not exist cannot be looked at, the same
        // `Unreadable` a root-owned `sudo` gives, without needing one.
        assert_eq!(
            waiting_for_an_answer(
                &[std::process::id(), u32::MAX - 1],
                Some(&InputEnd::Pipe(1234))
            ),
            Waiting::Unreadable,
            "one process of the run could not be looked at, so the run's answer is not `No`"
        );
        // And with every process readable it is `No`, which keeps the rule above from being
        // "never say no".
        assert_eq!(
            waiting_for_an_answer(&[std::process::id()], Some(&InputEnd::Pipe(1234))),
            Waiting::No,
            "every process was looked at and none is reading this pipe"
        );
    }

    /// `Yes` is knowledge, and a sibling that could not be read does not take it back: one
    /// process seen blocked on our pipe is the answer, whatever else in the run the daemon
    /// could not open.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
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

    /// No `/proc` reading is a fact, not a `No`: a daemon that cannot look at the run must not
    /// report that the run is not asking. Both ways it happens are here - no device to compare
    /// against, and no process list to walk.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
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
        // And this process, whose stdin is not a pipe at all: read, and definitively not
        // waiting - the difference between this case and the two above.
        assert_eq!(
            waiting_for_an_answer(&[std::process::id()], Some(&InputEnd::Pipe(1234))),
            Waiting::No
        );
    }

    /// A run whose fd 0 is a socket is `No`, not `Unreadable`: `socket:[N]` is a synthesised
    /// name, so `stat` fails with `ENOENT` - but the link WAS read, and a socket is not the
    /// character device this daemon holds. The child is blocked, so only the device saves it.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
    fn a_socket_on_fd_zero_is_a_no_and_not_an_unreadable() {
        let (child_end, _keep) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let mut child = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
                child_end,
            )))
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("cat starts");
        // A moment to reach its read, so this is not green because the child had not started.
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

    /// A program blocked reading the run's own terminal is waiting. The assertion is on the
    /// device as well as the block: the same child, asked about somebody else's terminal, must
    /// not be waiting on ours, which keeps the rule from being any tty will do.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
    fn a_child_blocked_on_the_runs_own_terminal_is_waiting() {
        let p = super::super::pty::Pty::open().expect("a pty on this box");
        let mut child = on_the_terminal(&p, "read x");
        let ours = InputEnd::Terminal(p.slave_path().to_string());
        assert_eq!(
            poll(&[child.id()], Some(&ours)),
            Waiting::Yes,
            "a program blocked reading the terminal this daemon holds is waiting for a line"
        );

        // A second pty, because "the name is what makes it ours" is the claim: a rule that
        // matched any terminal would pass the half above.
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

    /// `sudo`'s shape under the terminal: fd 0 is our terminal and the process is blocked
    /// reading a pipe it made. `wchan` is `wait_woken` for both, so `read(7, ...)` against
    /// `read(0, ...)` is the whole of the difference. `pw=$(sleep 30)` is that shape.
    #[test]
    #[cfg(target_os = "linux")] // the signal is `/proc`'s; macOS answers `Unreadable` (see `waiting_for_an_answer`)
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

        // ---- The control: the same shell on the same terminal, blocked on the descriptor we
        //      hold.
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

    /// The kernel names, and the near misses that must not pass: `anon_pipe_read`, `pipe_read`
    /// and `pipe_wait` are pipe reads, where `tcp_recvmsg` and `unix_stream_read_generic` are
    /// not, and a match on `read` would take them all.
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

    /// The terminal's own name, and the same near misses. `wait_woken` is the generic one, so
    /// it must count as a terminal read and not as a pipe read: the two lists are disjoint,
    /// which keeps a pipe's `wchan` from answering a question about a terminal.
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

    /// The text is shown and never read: `last_line` returns the program's own words, including
    /// a question mark and a password prompt, and none of them is treated differently. This is
    /// what keeps a `Continue?` matcher from growing back.
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
        // Nothing written is `None` and not an empty string: `! cat` blocked before it printed
        // anything is a real case.
        assert_eq!(last_line(""), None);
        assert_eq!(last_line("\n"), None);
        // A long line keeps its end.
        let long = format!("{} Proceed [y/N]", "x".repeat(400));
        let got = last_line(&long).expect("a last line");
        assert!(got.starts_with('…'), "{got}");
        assert!(got.ends_with("Proceed [y/N]"), "{got}");
    }

    /// There is no `Continue?` in this file: the module must not be able to grow a list of
    /// question shapes back into it, because that is the change that looks reasonable and
    /// fails on the next program.
    #[test]
    fn no_text_is_matched_anywhere_in_this_module() {
        let src = include_str!("ask.rs");
        // Built with `concat!` so this test's own needles do not count as hits: spelled as
        // literals they match this file, and the false positive would be silenced by deleting
        // the test rather than by fixing it.
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
