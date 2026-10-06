//! **A pseudo-terminal, so a colour-aware program colourises** — the mechanism
//! behind the operator's own `! ls -la` looking like their console.
//!
//! # The defect, in the operator's own words
//!
//! *"i run `! ls -la` and the output is plain, while in a proper terminal directory
//! names are highlighted."* Every command this harness runs gets `Stdio::piped()` for
//! stdout and stderr, because the capture ring needs bytes. A pipe is not a terminal,
//! so `isatty(1)` is false, and `ls` — which colourises only when its output is a
//! terminal (`--color=auto`, the default on every distribution here) — prints plain
//! text and is **right** to: that is the behaviour every well-behaved program has, and
//! the reason `ls | cat` is not a wall of escapes.
//!
//! So the program is not broken and the pipe is not broken. What is missing is the
//! one thing a pipe cannot be told: *a person is reading this*.
//!
//! # Why a pty and not the environment
//!
//! The other route is to set `LS_COLORS`, `TERM`, `COLORTERM`, `CLICOLOR_FORCE` and
//! `FORCE_COLOR` and hope. Measured on this box, 2026-09-25: with all of them set,
//! `ls --color=auto` still prints plain, because coreutils consults `isatty` and
//! nothing else — `LS_COLORS` says *which* colours, never *whether*. `CARGO_TERM_COLOR`
//! and `CLICOLOR_FORCE` do cover `cargo` and `ripgrep`, so the environment route
//! colours **some** tools and, by construction, never `ls` — which is the operator's
//! own example and the one that was reported. A pty makes `--color=auto` fire for the
//! same reason it fires in their console, and covers every program that asks the same
//! question rather than the ones somebody has listed.
//!
//! # What it costs, stated rather than discovered
//!
//! - **stdout and stderr become one stream.** They already were: [`super::host`] drains
//!   both pipes into one [`super::jobs::Capture`] and `HostBackend::run` reports an
//!   empty `stderr` for every job, so a pty merges two things this substrate had
//!   already merged. Nothing is lost that was not lost before.
//! - **A program changes how it buffers.** stdout on a terminal is line-buffered
//!   rather than block-buffered, so bytes arrive sooner. For `wait_with_progress`,
//!   whose progress is *bytes produced*, that is a finer tick and not a worse one.
//! - **A program may believe it is interactive.** Only the output side is a terminal:
//!   stdin stays `/dev/null` (see [`Pty::stdio`] and the call site), so a command that
//!   reads stdin gets the EOF it always got rather than blocking on a terminal nobody
//!   is typing at. `sudo` asks `letibot-askpass` through the `-A` shim and not a tty,
//!   which is unchanged.
//! - **`\n` is translated to `\r\n` by the line discipline unless it is told not to.**
//!   A pty's default output post-processing does exactly that, and `\r` in the capture
//!   would be a stray byte in every payload the model reads. So the slave's `ONLCR` is
//!   cleared at open (see [`Pty::plain_newlines`]) — one `tcsetattr`, once, on the
//!   device rather than `stty` in every command.
//!
//! # Where it is used, and why only there
//!
//! [`super::host::SpawnRequest::tty`] is set for the **operator's own run** — the `!`
//! line and the door's calls, which are the same ungated path — and for nothing else.
//! A model's `bash` call keeps its pipe, because the model reads the payload as
//! tokens: `ESC [ 0 1 ; 3 4 m` around every directory name is bytes it pays for and
//! cannot see, and the colour is for a person looking at a screen. The same argument
//! is why this is not a property of the host or of the session: it is a property of
//! **who is going to read the bytes**.
//!
//! # Why `libc`, in a crate whose manifest said it had none
//!
//! There is no pty in `std`. The alternatives were read and rejected: `script(1)`
//! allocates one for us but adds a program to every operator run, re-shells the
//! command through its own `sh -c`, and needs `stty` inside the pty to undo the
//! `\r\n` — two external dependencies and a quoting layer for something four
//! `libc` calls do exactly. The manifest's note is amended where it stood, with this
//! reason, rather than left to disagree with the dependency list.

use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::Stdio;

/// A pty pair: the master end this process reads, the slave end the command writes to.
///
/// **One handle per end, and the slave handle is dropped after `spawn`.** A slave this
/// process still holds open is a slave that never closes, so a read on the master would
/// never report the child's exit — the drain thread would block for ever and the waiter
/// behind it would never reap the job. [`Pty::into_master`] is that drop, named.
pub struct Pty {
    master: File,
    slave: File,
}

impl Pty {
    /// Open a pty pair, with the slave's output post-processing already off.
    ///
    /// `O_NOCTTY` on both ends, and it is not decoration: without it a session leader
    /// that opens a terminal acquires it as its **controlling** terminal, and the
    /// daemon must not be able to acquire one — `SIGHUP` on a pty that goes away would
    /// then reach the process that owns every session on this box.
    pub fn open() -> io::Result<Pty> {
        // `/dev/ptmx`, and the fd is owned from here on: every early return below
        // closes it through `File`'s own `Drop` rather than by hand.
        let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { File::from_raw_fd(master_fd) };
        // The same marking on the master, for the same reason — see the note on the slave
        // below, which is the end that actually bit.
        if unsafe { libc::fcntl(master_fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::grantpt(master_fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::unlockpt(master_fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // `ptsname_r` rather than `ptsname`: the latter returns a pointer into a
        // static buffer, and a static that two sessions on one daemon race for is a
        // name that occasionally belongs to somebody else's terminal.
        let mut name = [0 as libc::c_char; 128];
        if unsafe { libc::ptsname_r(master_fd, name.as_mut_ptr(), name.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let path = unsafe { CStr::from_ptr(name.as_ptr()) };
        let slave_fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
        if slave_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // **`FD_CLOEXEC` on both ends, and the slave is the one that matters.** Neither
        // `posix_openpt` nor `open` sets it, and an unmarked descriptor is inherited by
        // every process this one forks — including the children of *other* jobs, because
        // `Command::spawn` forks the whole process from whichever thread calls it. A
        // leaked slave is a terminal somebody else holds open, and the master only reports
        // the end when the last one closes: measured, 2026-09-25, a pty test in this crate
        // reading its master took **30.0 seconds** — the lifetime of an unrelated test's
        // `sleep 30` that had inherited the slave — and the production shape of the same
        // leak is a job that never leaves `Running`.
        //
        // The child that is *meant* to have it still does: [`Pty::stdio`] hands it over as
        // a `Stdio`, which std dups onto the child's fd 1 and 2, and `dup2` clears
        // `FD_CLOEXEC` on the descriptor it creates. Marking the original is what stops it
        // reaching anybody else.
        if unsafe { libc::fcntl(slave_fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let pty = Pty {
            master,
            slave: unsafe { File::from_raw_fd(slave_fd) },
        };
        pty.plain_newlines()?;
        Ok(pty)
    }

    /// **`\n` stays `\n`.** A pty's line discipline applies `ONLCR` by default, which
    /// turns every newline the command writes into `\r\n` on the way out. That is right
    /// for a terminal and wrong for a capture: `\r` is a control byte this tree's
    /// sanitiser turns into a space and would otherwise leave a stray byte on the end
    /// of every line of every operator payload.
    ///
    /// Cleared on the **device**, before anything runs, rather than by an `stty` inside
    /// the command — an `stty` would be a program that has to exist, that has to be on
    /// the pinned `PATH`, and that has to run before the first byte the command writes.
    fn plain_newlines(&self) -> io::Result<()> {
        let fd = self.slave.as_raw_fd();
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        t.c_oflag &= !libc::ONLCR;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The slave end, for `Command::stdout` / `Command::stderr`.
    ///
    /// **stdout and stderr, never stdin.** stdin is a terminal here would mean a
    /// command that reads it waits for a person who is not there, where a pipe gave it
    /// EOF — the change from *a command that answers* to *a command that hangs until
    /// its deadline*, for a colour nobody asked for on fd 0.
    pub fn stdio(&self) -> io::Result<Stdio> {
        Ok(Stdio::from(self.slave.try_clone()?))
    }

    /// The master end, with this process's own slave handle closed.
    ///
    /// Call it **after** `spawn`, once the child holds its own copy: see the type's
    /// note on why an open slave in the parent is a drain that never ends.
    pub fn into_master(self) -> File {
        self.master
    }
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty")
            .field("master", &self.master.as_raw_fd())
            .field("slave", &self.slave.as_raw_fd())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Read a pty's master until the last slave closes.
    ///
    /// **`Err` is the ending, not a failure**: on Linux a read on the master once every
    /// slave is closed returns `EIO`, which is the pty's EOF. This is the same rule
    /// `host::drain` follows for the same reason, and a test that treated it as an error
    /// would be asserting against the mechanism rather than about it.
    fn read_to_close(mut master: File) -> String {
        let mut got = String::new();
        let mut buf = [0u8; 512];
        loop {
            match master.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => got.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
        got
    }

    fn run_on(stdout: Stdio, stderr: Stdio, script: &str) -> std::process::Child {
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("sh starts")
    }

    /// **A slave handle the parent still holds is a master read that never ends** — the
    /// defect the first cut of the `host` wiring shipped, kept as a test so the discipline
    /// is measured rather than remembered.
    ///
    /// A `Command` keeps every `Stdio` it is given for as long as it lives, so
    /// `host::spawn` has to drop its own copies explicitly after `spawn`. This is what
    /// happens when it does not: the child has exited, and the master reports neither EOF
    /// nor the `EIO` that is this platform's end-of-pty, because two slaves are open — in
    /// the parent. The `host` symptom was not a hang in a test but a job that stayed
    /// `Running` for ever: the drain never ended, so the waiter never reaped, so a finished
    /// child stayed a **zombie**, and a zombie is not in `cgroup.procs` — the membership
    /// check timed out at five seconds and killed a command that had already run.
    ///
    /// **The other half is [`the_master_reports_the_child_leaving_once_the_parents_own_slave_is_dropped`]**,
    /// which reads the master blocking and gets the end. It is two tests rather than one
    /// because the second half has to *wait* for a hangup and a test that waits is a test
    /// that can hang; the assertion that matters here is the one that cannot be waited out.
    #[test]
    fn a_stdio_copy_the_parent_keeps_alive_hides_the_childs_exit() {
        let p = Pty::open().expect("a pty on this box");
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("echo done")
            .stdin(Stdio::null())
            .stdout(p.stdio().unwrap())
            .stderr(p.stdio().unwrap());
        let mut child = cmd.spawn().expect("sh starts");
        let master = p.into_master();
        let _ = child.wait();
        // Non-blocking, so "would block" and "the far end is gone" are two answers rather
        // than one hang.
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut buf = [0u8; 64];
        let err = loop {
            match (&master).read(&mut buf) {
                Ok(0) => panic!("the master reported EOF with a slave still open"),
                Ok(_) => continue,
                Err(e) => break e,
            }
        };
        assert_eq!(
            err.kind(),
            io::ErrorKind::WouldBlock,
            "with the Command's own slave copies open the master must NOT report the exit, \
             or the drain in `host::spawn` would end before the child does: {err}"
        );
    }

    /// **Neither end of the pair leaks into a child that is not the one it is for.**
    ///
    /// `posix_openpt` and `open` both hand back a descriptor without `FD_CLOEXEC`, and
    /// this process forks for every job it starts — from whichever thread calls
    /// `Command::spawn`, so a pty one job is holding is inherited by *another job's*
    /// child. A leaked slave is somebody else holding a terminal open, and the master
    /// reports the end only when the last one closes: measured, this cost a pty test 30
    /// seconds of wall clock and, in production, is a job that never leaves `Running`.
    ///
    /// The assertion is on the flag rather than on a spawned child's fd table, because
    /// the flag is the mechanism and the fd table is the symptom — and the flag is what a
    /// future edit to this file is most likely to drop.
    #[test]
    fn neither_end_of_the_pair_is_inherited_across_an_exec() {
        let p = Pty::open().expect("a pty on this box");
        for (what, fd) in [
            ("master", p.master.as_raw_fd()),
            ("slave", p.slave.as_raw_fd()),
        ] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(
                flags >= 0,
                "F_GETFD on the {what}: {}",
                io::Error::last_os_error()
            );
            assert_eq!(
                flags & libc::FD_CLOEXEC,
                libc::FD_CLOEXEC,
                "the {what} would be inherited by every process this one forks"
            );
        }
        // And the child that IS meant to have it still does — the flag must not have
        // closed the terminal on the one descriptor the command runs on.
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("test -t 1 && echo yes || echo no")
            .stdin(Stdio::null())
            .stdout(p.stdio().unwrap())
            .stderr(p.stdio().unwrap())
            .spawn()
            .expect("sh starts");
        let master = p.into_master();
        let seen = read_to_close(master);
        let _ = child.wait();
        assert_eq!(seen, "yes\n", "the intended child lost its terminal");
    }

    /// **The property this module exists for.** A child writing to the slave is writing
    /// to a terminal, which is the whole of what `ls --color=auto` asks.
    ///
    /// The control is in the same test on purpose: the identical script on a pipe says
    /// `pipe`, so a green run cannot be a `sh` that answered `tty` for another reason.
    /// The assertion is on `"tty\n"` and not on a `contains`, because the missing `\r`
    /// is the other half — [`Pty::plain_newlines`] is what makes it true.
    #[test]
    fn a_child_on_the_slave_sees_a_terminal_and_its_newlines_are_not_translated() {
        let script = "if [ -t 1 ]; then echo tty; else echo pipe; fi";
        let p = Pty::open().expect("a pty on this box");
        let child = run_on(p.stdio().unwrap(), p.stdio().unwrap(), script);
        let master = p.into_master();
        let seen = read_to_close(master);
        let mut child = child;
        let _ = child.wait();
        assert_eq!(
            seen, "tty\n",
            "the slave must be a terminal, and `\\n` must stay one byte"
        );

        // The control: the same script, the same shell, a pipe instead of a pty.
        let piped = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .output()
            .expect("sh starts");
        assert_eq!(
            String::from_utf8_lossy(&piped.stdout),
            "pipe\n",
            "a pipe must still be a pipe, or this test measures nothing"
        );
    }

    /// **The slave end is a terminal, and a pipe is not.** `isatty` is the syscall `ls`,
    /// `grep` and `cargo` all make; asserting it directly is asserting the mechanism
    /// rather than one program's use of it, and the pipe is the contrast that makes the
    /// assertion mean something — it is what the spawn used before this module existed.
    ///
    /// The master is a tty on Linux too (both ends of a Unix98 pty are character devices
    /// with tty operations), so it is not the control here; a pipe is.
    #[test]
    fn the_slave_end_is_a_terminal_and_a_pipe_is_not() {
        let p = Pty::open().expect("a pty on this box");
        assert_eq!(
            unsafe { libc::isatty(p.slave.as_raw_fd()) },
            1,
            "the slave must be a tty"
        );
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2)");
        assert_eq!(
            unsafe { libc::isatty(fds[0]) },
            0,
            "a pipe must not be a tty, or `isatty` is answering something else"
        );
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    /// **A slave the parent still holds open is a drain that never ends.**
    ///
    /// This is the failure the type's own note warns about, measured: the child exits,
    /// and the master read returns only once the parent's copy is gone — so
    /// [`Pty::into_master`] dropping it is what makes `host`'s drain thread finish and
    /// the job settle. A pty whose parent keeps the slave would leave every operator
    /// command `Running` until its deadline.
    #[test]
    fn the_master_reports_the_child_leaving_once_the_parents_own_slave_is_dropped() {
        let p = Pty::open().expect("a pty on this box");
        let child = run_on(p.stdio().unwrap(), p.stdio().unwrap(), "echo done");
        let master = p.into_master();
        let mut child = child;
        let _ = child.wait();
        // If the parent still held a slave this would block for ever; the test's own
        // completion is the assertion, and the text says the read ended at the exit.
        assert_eq!(read_to_close(master), "done\n");
    }
}
