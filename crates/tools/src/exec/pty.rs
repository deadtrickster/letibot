//! A pseudo-terminal, so a colour-aware program colourises: `isatty(1)` is what
//! `--color=auto` asks, and a pipe is not a terminal.
//!
//! Only the operator's own run gets one; a model's `bash` call keeps its pipe, because
//! escapes around every name are bytes it pays for and cannot see.
//!
//! The slave's `ONLCR` and `ECHO` are cleared at open, so the capture sees `\n` and the
//! daemon's own writes do not echo back into the run's output.

use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::Stdio;

/// A pty pair: the master end this process reads, the slave end the command writes to.
///
/// One handle per end, and every slave handle is dropped after `spawn`: a slave still open here
/// is a master read that never reports the child's exit. [`Pty::into_master`] is that drop.
pub struct Pty {
    master: File,
    slave: File,
    /// The slave's own name, `/dev/pts/N`, from `ptsname_r` at open: the only thing that can
    /// answer whether a process's fd 0 is the terminal we hold, since the master is `/dev/ptmx`
    /// for every pty on the box.
    slave_path: String,
}

impl Pty {
    /// Open a pty pair, with the slave's output post-processing already off. `O_NOCTTY` on both
    /// ends is not decoration: a session leader that opens a terminal acquires it as its
    /// controlling terminal, and the daemon must not be able to acquire one.
    pub fn open() -> io::Result<Pty> {
        // `/dev/ptmx`, and the fd is owned from here on: every early return below
        // closes it through `File`'s own `Drop` rather than by hand.
        let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { File::from_raw_fd(master_fd) };
        // The same marking on the master, for the same reason as the slave below, which is the
        // end that matters.
        if unsafe { libc::fcntl(master_fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::grantpt(master_fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::unlockpt(master_fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // `ptsname_r` rather than `ptsname`: the latter returns a pointer into a static buffer
        // that two sessions on one daemon race for. macOS has no `ptsname_r`; `TIOCPTYGNAME` is
        // what it is built on and fills a caller's buffer the same way.
        let mut name = [0 as libc::c_char; 128];
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::ioctl(master_fd, libc::TIOCPTYGNAME as _, name.as_mut_ptr()) };
        #[cfg(not(target_os = "macos"))]
        let rc = unsafe { libc::ptsname_r(master_fd, name.as_mut_ptr(), name.len()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let path = unsafe { CStr::from_ptr(name.as_ptr()) };
        let slave_fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
        if slave_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // `FD_CLOEXEC` on both ends, and the slave is the one that matters: an unmarked
        // descriptor is inherited by every process this one forks, and a leaked slave keeps the
        // master from ever reporting the end. [`Pty::stdio`]'s `dup2` clears it on the copy the
        // child is meant to have.
        if unsafe { libc::fcntl(slave_fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let pty = Pty {
            master,
            slave: unsafe { File::from_raw_fd(slave_fd) },
            slave_path: path.to_string_lossy().into_owned(),
        };
        pty.plain_newlines()?;
        Ok(pty)
    }

    /// `\n` stays `\n`: a pty's line discipline applies `ONLCR` by default, so every newline the
    /// command writes arrives as `\r\n`, and `\r` is a control byte the capture sanitises into a
    /// space. Cleared on the device, before anything runs, rather than by an `stty`.
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

    /// The slave end, for a `Command`'s stdin, stdout or stderr: one clone per call, because a
    /// `Stdio` takes ownership of the descriptor. All three, since stdin is what makes the pty
    /// the child's controlling terminal.
    pub fn stdio(&self) -> io::Result<Stdio> {
        Ok(Stdio::from(self.slave.try_clone()?))
    }

    /// The slave's own descriptor number, for [`controlling_terminal`]. The field stays private
    /// because nothing outside should do I/O on this end: what a caller needs is the number to
    /// hand to the `pre_exec` that acquires the terminal.
    pub fn slave_fd(&self) -> std::os::fd::RawFd {
        self.slave.as_raw_fd()
    }

    /// A second handle on the master, for writing to the run: what `!send` and the prompt
    /// card's answer travel down. A `dup` rather than a mutex, because the other handle is blocked
    /// in `read` for the whole run; a master rather than a slave, so holding it delays nothing.
    pub fn input(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    /// The name of the device this pty's slave is, `/dev/pts/N`. The reader is [`super::ask`]:
    /// "the run is blocked reading its terminal" is only a fact if the terminal is this one, so a
    /// `grep` blocked on `ls`'s pipe in `! ls | grep foo` must not raise that card.
    pub fn slave_path(&self) -> &str {
        &self.slave_path
    }

    /// `ECHO` off, and `ECHONL` with it: the terminal stops repeating what is written to it back
    /// into its own output, which is a transcript row and a model's prompt. A pane keeps the echo,
    /// because there a person types; `ICANON` and `ISIG` stay, so `^C` still reaches the program.
    pub fn no_echo(&self) -> io::Result<()> {
        let fd = self.slave.as_raw_fd();
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        t.c_lflag &= !(libc::ECHO | libc::ECHONL);
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The master end, with this process's own slave handle closed. Call it after `spawn`, once
    /// the child holds its own copy: an open slave here is a drain that never ends.
    pub fn into_master(self) -> File {
        self.master
    }

    /// macOS: a hold on the run's last words, taken before [`Pty::into_master`]. XNU discards what
    /// is unread in a pty when its last slave closes, where Linux keeps it readable, so a quick
    /// command's tail raced the drain. The job's waiter releases the guard after the reap.
    #[cfg(target_os = "macos")]
    pub fn tail_guard(&self) -> io::Result<TailGuard> {
        Ok(TailGuard {
            slave: self.slave.try_clone()?,
            master: self.master.try_clone()?,
        })
    }
}

/// See [`Pty::tail_guard`]. Dropping it without [`TailGuard::release`] just closes the
/// handles, so a waiter that panics cannot leave a slave open for ever.
#[cfg(target_os = "macos")]
pub struct TailGuard {
    slave: File,
    master: File,
}

#[cfg(target_os = "macos")]
impl TailGuard {
    /// Wait, bounded, until the master has nothing pending (`FIONREAD` is 0, so the drain has
    /// taken every byte), then let the slave go.
    pub fn release(self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let mut pending: libc::c_int = 0;
            let rc = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::FIONREAD, &mut pending) };
            if rc != 0 || pending == 0 || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        drop(self.slave);
    }
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty")
            .field("master", &self.master.as_raw_fd())
            .field("slave", &self.slave.as_raw_fd())
            .field("slave_path", &self.slave_path)
            .finish()
    }
}

/// Make the child a session leader whose controlling terminal is the pty slave on `fd`. The
/// order matters: `setsid` gives it no controlling terminal and `TIOCSCTTY` takes one, and the
/// reverse fails. `fd` rather than `0`, because the ioctl acts on the session, not the descriptor.
///
/// A failure is swallowed, and is not silent in effect: the child still runs here, it just cannot
/// open `/dev/tty` and bash prints its two lines about job control.
pub fn controlling_terminal(cmd: &mut std::process::Command, fd: std::os::fd::RawFd) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 {
                // Already a group leader, which a forked child is not, so this is a failure we
                // do not understand and the command still runs.
            }
            let _ = libc::ioctl(fd, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Read a pty's master until the last slave closes. `Err` is the ending, not a failure: on
    /// Linux a read on the master once every slave is closed returns `EIO`, which is the pty's
    /// EOF.
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
            // The shell-behaviour variables a parallel test plants process-wide are not this
            // test's subject; macOS's `/bin/sh` is bash and would trace every line into the
            // capture.
            .env_remove("SHELLOPTS")
            .env_remove("BASH_ENV")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("sh starts")
    }

    /// A slave handle the parent still holds is a master read that never ends: the child has
    /// exited and the master reports neither EOF nor `EIO`, because two slaves are open in the
    /// parent. A `Command` keeps every `Stdio` it is given, so `host::spawn` drops its copies.
    #[test]
    fn a_stdio_copy_the_parent_keeps_alive_hides_the_childs_exit() {
        let p = Pty::open().expect("a pty on this box");
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.env_remove("SHELLOPTS").env_remove("BASH_ENV");
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

    /// Neither end of the pair leaks into a child that is not the one it is for: an unmarked
    /// descriptor is inherited by every process this one forks, and a leaked slave keeps the
    /// master from reporting the end. The assertion is on the flag, which is the mechanism.
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
        // And the child that is meant to have it still does: the flag must not close the
        // terminal on the descriptor the command runs on.
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("test -t 1 && echo yes || echo no")
            .env_remove("SHELLOPTS")
            .env_remove("BASH_ENV")
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

    /// The property this module exists for: a child writing to the slave is writing to a
    /// terminal, which is what `ls --color=auto` asks. The identical script on a pipe says
    /// `pipe` in the same test, so a green run cannot be `sh` answering for another reason.
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
            .env_remove("SHELLOPTS")
            .env_remove("BASH_ENV")
            .stdin(Stdio::null())
            .output()
            .expect("sh starts");
        assert_eq!(
            String::from_utf8_lossy(&piped.stdout),
            "pipe\n",
            "a pipe must still be a pipe, or this test measures nothing"
        );
    }

    /// The slave end is a terminal and a pipe is not: `isatty` is the syscall `ls`, `grep` and
    /// `cargo` all make, and the pipe is the contrast that makes the assertion mean something.
    /// The master is a tty on Linux too, so a pipe and not the master is the control here.
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

    /// A slave the parent still holds open is a drain that never ends: the child exits and the
    /// master read returns only once the parent's copy is gone, so [`Pty::into_master`] dropping
    /// it is what lets `host`'s drain finish and the job settle.
    #[test]
    // macOS discards a pty's unread bytes when its last slave closes, so reading only after the
    // exit cannot see them there; `Pty::tail_guard` is the mechanism, tested below.
    #[cfg(not(target_os = "macos"))]
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

    /// The macOS half of the test above: with the guard held across the exit, a drain that
    /// starts late still gets every byte, and the master reports the end once it is released.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_tail_guard_keeps_a_quick_commands_output_until_it_is_read() {
        for _ in 0..20 {
            let p = Pty::open().expect("a pty on this box");
            let child = run_on(p.stdio().unwrap(), p.stdio().unwrap(), "echo done");
            let guard = p.tail_guard().expect("a guard");
            let master = p.into_master();
            let mut child = child;
            let _ = child.wait();
            // The child is gone and nothing has read yet: the case that lost `done`.
            let reader = std::thread::spawn(move || read_to_close(master));
            guard.release();
            assert_eq!(reader.join().unwrap(), "done\n");
        }
    }

    /// Spawn `script` with all three descriptors on the pty, and with [`controlling_terminal`]
    /// called or not: the two halves of one question.
    fn run_three(pty: &Pty, script: &str, controlling: bool) -> std::process::Child {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        // As in `run_on`: not this test's subject, and bash would trace into the capture.
        cmd.env_remove("SHELLOPTS").env_remove("BASH_ENV");
        cmd.stdin(pty.stdio().unwrap())
            .stdout(pty.stdio().unwrap())
            .stderr(pty.stdio().unwrap());
        if controlling {
            controlling_terminal(&mut cmd, pty.slave.as_raw_fd());
        }
        cmd.spawn().expect("sh starts")
    }

    /// The child's terminal IS its controlling terminal, which is what makes bash a job-control
    /// shell and silences its two lines about job control. Not "a controlling terminal exists",
    /// which a test run from an operator's terminal would satisfy anyway, but `/dev/tty` on fd 0.
    #[test]
    #[cfg(target_os = "linux")] // the check reads `/proc/self/fd/0` and expects `/dev/pts/N`
    fn the_childs_terminal_is_its_controlling_terminal() {
        const ASK: &str = "exec 9</dev/tty 2>/dev/null || { echo no-ctty; exit 0; }; \
                           a=$(ps -o tty= -p $$ 2>/dev/null); \
                           b=$(readlink /proc/self/fd/0 2>/dev/null); \
                           if [ \"$a\" != \"?\" ] && [ -n \"$a\" ] && [ \"/dev/$a\" = \"$b\" ]; \
                           then echo same; else echo \"diff:a=$a:b=$b\"; fi";

        let p = Pty::open().expect("a pty on this box");
        let mut child = run_three(&p, ASK, true);
        let master = p.into_master();
        let seen = read_to_close(master);
        let _ = child.wait();
        assert_eq!(
            seen, "same\n",
            "the child must get THIS pty as its controlling terminal, and must be able to \
             open /dev/tty on it"
        );

        // The control: the same pair, the same shell, no `setsid`, no `TIOCSCTTY`. Its own
        // error IS the answer: the open fails with `ENXIO` and `dash` aborts the script there
        // rather than taking the `||` arm, so the assertion is on the error.
        let p = Pty::open().expect("a pty");
        let mut child = run_three(&p, ASK, false);
        let master = p.into_master();
        let seen = read_to_close(master);
        let _ = child.wait();
        assert!(
            !seen.contains("same"),
            "without the two calls /dev/tty must NOT be openable, or this test measures \
             nothing: {seen:?}"
        );
        assert!(
            seen.contains("No such device or address"),
            "the control's open must fail with ENXIO, which is what makes /dev/tty \
             unopenable rather than merely unused: {seen:?}"
        );
    }

    /// `no_echo` is what keeps the daemon's own writes out of the run's output: the program
    /// reads one line and prints it, so what the master returns is the answer plus, where the
    /// terminal echoes, a copy of what was typed. The control is the same pty without the call.
    #[test]
    fn a_line_written_to_the_master_is_not_echoed_back_when_echo_is_off() {
        const EAT: &str = "IFS= read -r x; printf '%s\\n' \"$x\"";

        let p = Pty::open().expect("a pty on this box");
        p.no_echo().expect("the slave takes a termios");
        let mut child = run_three(&p, EAT, true);
        let mut master = p.into_master();
        {
            use std::io::Write;
            master
                .write_all(b"hello\n")
                .expect("the line reaches the pty");
            master.flush().ok();
        }
        let seen = read_to_close(master);
        let _ = child.wait();
        assert_eq!(
            seen, "hello\n",
            "with ECHO off the master must carry the program's answer and NOT a copy of \
             what was written to it"
        );

        // The control: the same write, the same program, echo left on.
        let p = Pty::open().expect("a pty");
        let mut child = run_three(&p, EAT, true);
        let mut master = p.into_master();
        {
            use std::io::Write;
            master
                .write_all(b"hello\n")
                .expect("the line reaches the pty");
            master.flush().ok();
        }
        let seen = read_to_close(master);
        let _ = child.wait();
        assert_eq!(
            seen, "hello\nhello\n",
            "a default pty DOES echo, or `no_echo` would be asserting about nothing"
        );
    }

    /// The name on the child's fd 0 is the name of the device we hold the master of, which is
    /// the whole of the comparison [`super::ask`] makes. The control is a second pty, whose
    /// slave name must differ.
    #[test]
    #[cfg(target_os = "linux")] // the check reads `/proc/self/fd/0` and expects `/dev/pts/N`
    fn the_slave_path_names_the_device_the_child_is_on() {
        let p = Pty::open().expect("a pty on this box");
        let other = Pty::open().expect("a second pty");
        assert_ne!(
            p.slave_path(),
            other.slave_path(),
            "two ptys must not share a name, or asking whose terminal this is answers nothing"
        );
        assert!(
            p.slave_path().starts_with("/dev/pts/"),
            "this box names pty slaves /dev/pts/N: {:?}",
            p.slave_path()
        );

        let mut child = run_three(&p, "readlink /proc/self/fd/0", true);
        // Read the name before the pty is consumed: `into_master` takes the handle.
        let named = p.slave_path().to_string();
        let master = p.into_master();
        let seen = read_to_close(master);
        let _ = child.wait();
        assert_eq!(
            seen.trim_end(),
            named,
            "the child's fd 0 must be the device this Pty opened"
        );
    }
}
