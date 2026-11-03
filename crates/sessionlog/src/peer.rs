//! Who is on the other end of the head socket, and whether that is a head or a model's command.
//!
//! The socket's permission is the auth and it says *the operator's user*, not *the operator*: a
//! command this daemon started on a model's behalf runs as that same user and is handed
//! `LETIBOT_SOCKET` for `sudo`'s password prompt. So the kernel is asked which process connected,
//! and a connection from inside this daemon's own tree is a tool call, never a head.

use std::os::unix::net::UnixStream;

use crate::protocol::ClientFrame;

/// The process the kernel named for this connection, and what that makes the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    /// The pid on the other end, as the kernel filled it in at `connect`. `None` when it would not
    /// say - another platform, or a process that exited before the question could be asked.
    pub pid: Option<i32>,
    /// **True when this connection came out of a tool call.** It is a descendant of this daemon,
    /// or inside a cgroup this daemon opened, so it is a command on the model's behalf.
    pub inside: bool,
}

impl Peer {
    /// Classify a connection. An unreadable peer is `inside`: a platform that cannot answer, or a
    /// process that exited under the question, must fall on the side that refuses.
    pub fn of(stream: &UnixStream) -> Peer {
        let pid = peer_pid(stream);
        Peer {
            pid,
            inside: inside(pid),
        }
    }

    /// The door this frame asks for, when this connection may not have it. `None` is a frame served
    /// on any connection: the password ask, the bookkeeping of a seat, or a head's own socket.
    pub fn refuses(&self, frame: &ClientFrame) -> Option<&'static str> {
        if !self.inside {
            return None;
        }
        door(frame)
    }
}

/// The rule on its own, so the refusing side of an unreadable peer is one line to see: a pid the
/// kernel would not give is treated as a tool call, never as a head.
fn inside(pid: Option<i32>) -> bool {
    let me = std::process::id() as i32;
    match pid {
        Some(p) => descended_from(p, me) || in_daemon_tree(p, me),
        None => true,
    }
}

/// The operator's door this frame reaches, in the operator's own words; `None` is served on any
/// connection. The list of `None`s is short on purpose: a connection from inside a tool tree is
/// here for one thing, the password card `sudo` raises, and the bookkeeping of being attached.
pub fn door(frame: &ClientFrame) -> Option<&'static str> {
    let door = match frame {
        ClientFrame::Ack(_) | ClientFrame::Resync | ClientFrame::Detach => return None,
        // The one door a tool call exists to reach: `sudo`'s `SUDO_ASKPASS`.
        ClientFrame::Askpass { .. } => return None,
        // Reaching the helper above takes a seat; a second one is refused by the loop itself.
        ClientFrame::Attach { .. } => return None,
        ClientFrame::Prompt { .. } => "typing a prompt to the model",
        ClientFrame::WithdrawPrompts { .. } => "taking back a queued prompt",
        ClientFrame::SetOperatorTodos { .. } => "writing the operator's todo rows",
        ClientFrame::Stop { .. } => "stopping the daemon",
        ClientFrame::CloseSession { .. } => "closing the session",
        ClientFrame::Interrupt { .. } => "interrupting the running turn",
        ClientFrame::Promote { .. } => "promoting a run to the background",
        ClientFrame::CompactSession { .. } => "/compact",
        ClientFrame::ReseatSession { .. } => "a re-seat",
        ClientFrame::Mode { .. } => "/mode",
        ClientFrame::ModeConfirm { .. } => "/mode asking a person to confirm",
        ClientFrame::ModeConsent { .. } => "a person's consent to a mode",
        ClientFrame::Slash { .. } => "a / verb",
        ClientFrame::Feedback { .. } => "a mark on the conversation (/fb)",
        ClientFrame::Secret { .. } => "answering a password card",
        ClientFrame::Screen { .. } => "this head's screen",
        ClientFrame::Answer { .. } => "a decision",
        ClientFrame::AnswerQuestion { .. } => "an answer to a question",
        ClientFrame::AnswerTodo { .. } => "an answer to a todo row",
        ClientFrame::ListSessions => "the session list",
        ClientFrame::ListTodos => "the todo board",
        ClientFrame::ListNotes => "the standing-notes pane",
        ClientFrame::ListSkills => "the skills pane",
        ClientFrame::LoadSkill { .. } => "/skills, handing a skill to the session",
        ClientFrame::ListJobs => "the jobs pane",
        ClientFrame::ListMergeQueue => "the merge queue",
        ClientFrame::NewSession { .. } => "/new",
        ClientFrame::ResumeSession { .. } => "resuming a session",
        ClientFrame::RenameSession { .. } => "/rename",
        ClientFrame::Switch { .. } => "switching sessions",
        ClientFrame::Peek { .. } => "reading another session",
        ClientFrame::FetchRow { .. } | ClientFrame::FetchRows { .. } => "reading the conversation",
        ClientFrame::OperatorCall { .. } => "running a tool as the operator",
        ClientFrame::OperatorResult { .. } => "an operator call's result",
        ClientFrame::OperatorShell { .. } => "a ! line",
        ClientFrame::SuggestShell { .. } => "a ! completion",
        ClientFrame::FetchDiagnostic { .. } => "a decision's evidence",
        ClientFrame::Settings => "the settings rows",
        ClientFrame::ReadJobOutput { .. } => "a job's output",
        ClientFrame::TermOpen { .. } => "!term opening a pane",
        ClientFrame::TermInput { .. } => "!term keys",
        ClientFrame::TermResize { .. } => "!term resizing a pane",
        ClientFrame::TermStatus => "!term status",
        ClientFrame::TermClose { .. } => "!term closing a pane",
        ClientFrame::PromptAnswer { .. } => "an answer to a running command's question",
        ClientFrame::SendLine { .. } => "a line to the running command",
        ClientFrame::PaneOpen { .. } => "opening a plugin pane",
        ClientFrame::PaneClose { .. } => "closing a plugin pane",
        ClientFrame::PaneAction { .. } => "a plugin pane's key",
    };
    Some(door)
}

/// The id and the seq a frame carries, for the refusal that answers it: a head matches a refusal to
/// what it sent, and a frame that carries neither gets the empty id and zero, which matches nothing
/// and therefore cannot settle a card that is not there.
pub fn request_of(frame: &ClientFrame) -> (String, u64) {
    let Ok(wire) = serde_json::to_value(frame) else {
        return (String::new(), 0);
    };
    let id = wire
        .get("client_request_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let seq = wire.get("expected_seq").and_then(|v| v.as_u64()).unwrap_or(0);
    (id, seq)
}

/// The pid the kernel recorded for this connection at `connect`. `None` when it will not say.
#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // `LOCAL_PEERPID`, not `getpeereid`: the uid is already known to be the operator's, and it is
    // the process that decides between a head and a command.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && pid > 0).then_some(pid)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn peer_pid(stream: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid)
}

/// The parent of a process, or `None` when `/proc` will not say - gone, a zombie, or another
/// user's process. macOS has no `/proc`, so the same fact comes from `PROC_PIDTBSDINFO`.
#[cfg(target_os = "macos")]
fn parent_of(pid: i32) -> Option<i32> {
    let mut bi: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut bi as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got == size {
        Some(bi.pbi_ppid as i32)
    } else {
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn parent_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat_ppid(&stat)
}

/// The `ppid` out of one `/proc/<pid>/stat` line: field 4, read from the LAST `)`, because field 2
/// is the program's name and may itself hold spaces and brackets.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn stat_ppid(stat: &str) -> Option<i32> {
    let after = &stat[stat.rfind(')')? + 1..];
    let mut fields = after.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// Whether `pid` is a descendant of `ancestor`, walking the parent chain up. A process is not its
/// own descendant, and an unreadable link answers `true`: `Peer::of` keeps the two questions apart
/// by asking this only when the pid was known, and by treating no answer as the refusing side.
fn descended_from(pid: i32, ancestor: i32) -> bool {
    // A chain longer than this is a loop or a fork bomb, and either way the answer is the same one
    // an unreadable link gives.
    const DEPTH: usize = 64;
    let mut at = pid;
    for _ in 0..DEPTH {
        if at <= 1 {
            return false;
        }
        let Some(parent) = parent_of(at) else {
            return true;
        };
        if parent == ancestor {
            return true;
        }
        if parent == at {
            return false;
        }
        at = parent;
    }
    true
}

/// Whether the process sits in a cgroup this daemon opened: `letibot.<daemon pid>.<n>` and
/// everything under it, which is where every tool call and job runs. This is the arm a
/// double-forked child cannot leave, and it is empty on a host with no cgroup v2.
#[cfg(target_os = "macos")]
fn in_daemon_tree(_pid: i32, _daemon: i32) -> bool {
    false
}

#[cfg(all(unix, not(target_os = "macos")))]
fn in_daemon_tree(pid: i32, daemon: i32) -> bool {
    let Ok(contents) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
        return false;
    };
    let root = format!("/letibot.{daemon}.");
    contents
        .lines()
        .any(|line| line.split("::").nth(1).is_some_and(|path| path.contains(&root)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    /// A child of this process, holding still until it is killed: the shape of every tool call the
    /// daemon starts.
    fn child() -> Child {
        let c = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a child process");
        // The spawn returns before the child has necessarily run, but the pid exists from `fork`,
        // and the parent link is set before the child is returned.
        c
    }

    #[test]
    fn a_child_of_this_process_is_inside_and_this_process_is_not() {
        let mut c = child();
        let me = std::process::id() as i32;
        assert!(
            descended_from(c.id() as i32, me),
            "a process this one spawned is inside its tree"
        );
        assert!(
            !descended_from(me, me),
            "a process is not inside its own tree: the daemon asking itself is the daemon"
        );
        assert!(
            !descended_from(me, c.id() as i32),
            "the parent is not a descendant of its child"
        );
        let _ = c.kill();
        let _ = c.wait();
    }

    #[test]
    fn a_peer_the_kernel_would_not_name_is_on_the_refusing_side() {
        assert!(
            inside(None),
            "an unreadable peer is a tool call: a platform that cannot answer, or a process that \
             exited under the question, must never be handed the operator's doors"
        );
        assert!(
            !inside(Some(std::process::id() as i32)),
            "the daemon's own pid is not inside its own tree"
        );
    }

    #[test]
    fn stat_ppid_is_read_from_the_last_bracket() {
        // The program name holds a space and a bracket, which a field count from the left misses.
        let stat = "4242 (a program (x)) S 31337 4242 4242 0 -1";
        assert_eq!(stat_ppid(stat), Some(31337));
    }
}
