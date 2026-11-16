//! Who is on the other end of the head socket: a head or a model's command. A command this
//! daemon started runs as the operator's user, so the kernel is asked which process connected;
//! a connection from inside this daemon's tree is a tool call, never a head.

use std::os::unix::net::UnixStream;

use crate::protocol::ClientFrame;

/// The `kind` the askpass helper attaches with: the one seat an inside peer may take.
pub const ASKPASS_KIND: &str = "askpass";

/// The process the kernel named for this connection, and what that makes the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    /// The pid on the other end, as the kernel filled it in at `connect`. `None` when it would not
    /// say - another platform, or a process that exited before the question could be asked.
    pub pid: Option<i32>,
    /// True when this connection came out of a tool call. It is a descendant of this daemon,
    /// or inside a cgroup this daemon opened, so it is a command on the model's behalf.
    pub inside: bool,
}

impl Peer {
    /// Classify a connection. An unreadable peer is `inside`: a platform that cannot answer, or a
    /// process that exited under the question, must fall on the side that refuses.
    pub fn of(stream: &UnixStream, tree_root: i32) -> Peer {
        let pid = peer_pid(stream);
        Peer {
            pid,
            inside: inside(pid, tree_root),
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
/// kernel would not give is treated as a tool call, never as a head. `tree_root` is the pid whose
/// tree is the daemon's: the daemon's own pid in production.
fn inside(pid: Option<i32>, tree_root: i32) -> bool {
    match pid {
        Some(p) => descended_from(p, tree_root) || in_daemon_tree(p, tree_root),
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
        ClientFrame::DriverTodos { .. } => "writing a driver's todo rows",
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
        ClientFrame::Github { .. } => "the GitHub reviewer seat",
        ClientFrame::RoomChat { .. } => "chat in the rooms as the operator",
        ClientFrame::NewSession { .. } => "/new",
        ClientFrame::ResumeSession { .. } => "resuming a session",
        ClientFrame::RenameSession { .. } => "/rename",
        ClientFrame::ArchiveSession { .. } => "archiving a session",
        ClientFrame::UnarchiveSession { .. } => "unarchiving a session",
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
    let seq = wire
        .get("expected_seq")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    (id, seq)
}

/// What a connection is, fixed when it attaches and never read off a frame: a peer that says it is
/// the operator is still a peer. A person's own seat is the terminal's head and the bridge's remote
/// head; every other kind - the askpass helper, an editor's ACP seat, an unknown one - is a
/// program's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Operator,
    Peer,
}

impl Role {
    /// The kind a head attached as is the one thing asked. A frame's `from` names which peer is
    /// speaking; it never makes a connection a person's.
    pub fn of(kind: &str) -> Role {
        match kind.trim() {
            "tui" | "remote" => Role::Operator,
            _ => Role::Peer,
        }
    }

    pub fn is_operator(self) -> bool {
        matches!(self, Role::Operator)
    }
}

/// Who a prompt's words belong to, decided by the connection and never by the frame. A person's own
/// seat may type with no sender at all; every other connection speaks as its own seat, and a `from`
/// that reads as the person is refused that signature and spoken as the connection that sent it.
pub fn resolve_from(role: Role, identity: &str, from: Option<String>) -> Option<String> {
    let claimed = from.map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
    match (role, claimed) {
        // The one place the operator is still the default: this connection is the person's.
        (Role::Operator, None) => None,
        (Role::Operator, Some(handle)) => Some(signed_as(identity, &handle)),
        (Role::Peer, handle) => Some(signed_as(identity, handle.as_deref().unwrap_or_default())),
    }
}

/// The handle a peer's words are filed under: what it claimed, unless that reads as the person.
fn signed_as(identity: &str, claimed: &str) -> String {
    if claimed.is_empty() || reads_as_operator(claimed) {
        identity.trim().to_string()
    } else {
        claimed.to_string()
    }
}

/// The handles that would let a frame sign as the person. Every other word is a handle.
fn reads_as_operator(handle: &str) -> bool {
    matches!(handle, "operator" | "human" | "me" | "user") || handle.starts_with("human:")
}

/// The pid the kernel recorded for this connection at `connect`. `None` when it will not say.
#[cfg(target_os = "macos")]
pub fn peer_pid(stream: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // `LOCAL_PEERPID`, not `getpeereid`: the uid is already known to be the operator's, and it is
    // the process that decides between a head and a command.
    // SAFETY: `pid` and `len` outlive the call and `len` is the size of `pid`.
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
pub fn peer_pid(stream: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `ucred` is plain data, zero is a valid value.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` outlive the call and `len` is the size of `cred`.
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
/// user's process. macOS has no `/proc`: `PROC_PIDT_SHORTBSDINFO` answers for a root-owned
/// ancestor such as `login`, where `PROC_PIDTBSDINFO` refuses and every head would read as inside.
#[cfg(target_os = "macos")]
fn parent_of(pid: i32) -> Option<i32> {
    // SAFETY: `proc_bsdshortinfo` is plain data, zero is a valid value.
    let mut bi: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: `bi` is `size` bytes and outlives the call.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            &mut bi as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got == size {
        Some(bi.pbsi_ppid as i32)
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

/// Whether the process sits in a tool scope of ANY daemon, `letibot.<pid>.<n>` and everything under
/// it: a tool of another daemon is no head of this one. This is the arm a double-forked child cannot
/// leave. On macOS the scope is a process group, and a child that calls `setsid` leaves it.
#[cfg(target_os = "macos")]
fn in_daemon_tree(pid: i32, _daemon: i32) -> bool {
    // SAFETY: `getpgid` takes a pid and returns -1 on failure.
    let pgid = unsafe { libc::getpgid(pid) };
    // A peer whose group cannot be read is not placed, so it falls on the refusing side.
    pgid <= 0 || recorded_group(&std::env::temp_dir().join("letibot-scopes"), pgid)
}

/// Whether `pgid` is listed in a `cgroup.procs` under any daemon's `letibot.<pid>.<n>` root in
/// `parent`: the file the tool launcher writes its group leader's pid into.
#[cfg(any(test, target_os = "macos"))]
fn recorded_group(parent: &std::path::Path, pgid: i32) -> bool {
    fn walk(dir: &std::path::Path, pgid: i32) -> bool {
        if let Ok(s) = std::fs::read_to_string(dir.join("cgroup.procs"))
            && s.lines().any(|l| l.trim().parse::<i32>() == Ok(pgid))
        {
            return true;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        rd.flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .any(|e| walk(&e.path(), pgid))
    }
    let Ok(rd) = std::fs::read_dir(parent) else {
        return false;
    };
    // Any daemon's scopes: a tool of another daemon is no head of this one.
    rd.flatten()
        .filter(|e| is_tool_root(&e.file_name().to_string_lossy()))
        .any(|e| walk(&e.path(), pgid))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn in_daemon_tree(pid: i32, daemon: i32) -> bool {
    let read = |p: i32| std::fs::read_to_string(format!("/proc/{p}/cgroup")).ok();
    cgroup_inside(read(pid).as_deref(), read(daemon).as_deref())
}

/// Whether a peer's `/proc/<pid>/cgroup` puts it in ANY daemon's tool scope that this daemon is
/// not itself inside. Any daemon, because a tool of another daemon is no head of this one; an
/// unreadable file is inside, because no answer must fall on the refusing side.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn cgroup_inside(peer: Option<&str>, daemon: Option<&str>) -> bool {
    let Some(peer) = peer else {
        return true;
    };
    // The launcher's daemon steps into a `daemon` leaf of its scope (`DAEMON_LEAF` in tools).
    let daemon = daemon.and_then(cgroup_path).unwrap_or("");
    let daemon = daemon.strip_suffix("/daemon").unwrap_or(daemon);
    // No unified line is a v1 host, where no tool scope can exist to be inside of.
    let Some(path) = cgroup_path(peer) else {
        return false;
    };
    // A daemon started from a tool shares that tool's scope with its own heads, so only the
    // part of the peer's path below the daemon's own counts.
    let below = path
        .strip_prefix(daemon)
        .filter(|rest| daemon.is_empty() || rest.is_empty() || rest.starts_with('/'))
        .unwrap_or(path);
    below.split('/').any(is_tool_root)
}

/// The unified (v2) path of a `/proc/<pid>/cgroup`, the `0::` line.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn cgroup_path(contents: &str) -> Option<&str> {
    contents.lines().find_map(|l| l.strip_prefix("0::"))
}

/// `letibot.<pid>.<n>`, the root a daemon opens per tool tree. Not `letibot.slice`, which is
/// where daemons themselves run.
fn is_tool_root(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("letibot.") else {
        return false;
    };
    let mut parts = rest.split('.');
    let num =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    num(parts.next()) && num(parts.next()) && parts.next().is_none()
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
            inside(None, std::process::id() as i32),
            "an unreadable peer is a tool call: a platform that cannot answer, or a process that \
             exited under the question, must never be handed the operator's doors"
        );
        assert!(
            !inside(Some(std::process::id() as i32), std::process::id() as i32),
            "the daemon's own pid is not inside its own tree"
        );
    }

    /// A tool of daemon A connecting to daemon B's socket is a tool call for B as well.
    #[test]
    fn a_peer_in_another_daemons_cgroup_is_inside() {
        let me = std::process::id();
        let daemon = "0::/user.slice/user@1000.service/letibot.slice/letibot-b.scope\n";
        let other = format!(
            "0::/user.slice/user@1000.service/letibot.slice/letibot-a.scope/letibot.{}.0/session.s\n",
            me + 1
        );
        assert!(cgroup_inside(Some(&other), Some(daemon)));
        assert!(
            cgroup_inside(None, Some(daemon)),
            "an unreadable cgroup is the refusing side"
        );
        let head = "0::/user.slice/user@1000.service/tmux-spawn-1.scope\n";
        assert!(!cgroup_inside(Some(head), Some(daemon)), "a head is not");
        assert!(
            !cgroup_inside(Some(daemon), Some(daemon)),
            "the daemon's own slice is not a tool scope"
        );
        // A daemon run from a tool: its own heads share the tool's scope, its tools are below it.
        let nested = format!("0::/u/letibot.{me}.3/session.s\n");
        assert!(!cgroup_inside(Some(&nested), Some(&nested)));
        let its_tool = format!("0::/u/letibot.{me}.3/session.s/letibot.{}.0/x\n", me + 7);
        assert!(cgroup_inside(Some(&its_tool), Some(&nested)));
        let leaf = "0::/u/letibot.slice/letibot-b.scope/daemon\n";
        let own_tool = format!("0::/u/letibot.slice/letibot-b.scope/letibot.{me}.0/s\n");
        assert!(cgroup_inside(Some(&own_tool), Some(leaf)), "its own tool");
    }

    #[test]
    fn stat_ppid_is_read_from_the_last_bracket() {
        // The program name holds a space and a bracket, which a field count from the left misses.
        let stat = "4242 (a program (x)) S 31337 4242 4242 0 -1";
        assert_eq!(stat_ppid(stat), Some(31337));
    }

    /// A double-forked child of a tool: the shell exits, the grandchild is reparented to init, and
    /// only its process group still ties it to the scope the daemon recorded.
    #[test]
    fn a_double_forked_child_of_a_tool_is_in_the_recorded_group() {
        use std::os::unix::process::CommandExt;
        let dir = std::env::temp_dir().join(format!("peer-groups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let daemon = std::process::id() as i32;
        let scope = dir.join(format!("letibot.{daemon}.0")).join("session.s1");
        std::fs::create_dir_all(&scope).unwrap();
        let mut leader = Command::new("/bin/sh")
            .arg("-c")
            .arg("nohup sleep 30 >/dev/null 2>&1 & echo $!")
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .expect("a tool shell");
        let mut out = String::new();
        std::io::Read::read_to_string(leader.stdout.as_mut().unwrap(), &mut out).unwrap();
        let _ = leader.wait();
        let grandchild: i32 = out.trim().parse().expect("the grandchild pid");
        std::fs::write(scope.join("cgroup.procs"), format!("{}\n", leader.id())).unwrap();
        // SAFETY: `getpgid` takes a pid and returns -1 on failure.
        let pgid = unsafe { libc::getpgid(grandchild) };
        assert_eq!(pgid, leader.id() as i32, "the grandchild kept the group");
        assert!(
            recorded_group(&dir, pgid),
            "a double-forked child is inside through its recorded group"
        );
        assert!(
            !recorded_group(&dir, std::process::id() as i32),
            "this process's own group is not recorded, so a head is not inside"
        );
        // SAFETY: `kill` with a pid this test spawned.
        unsafe { libc::kill(grandchild, libc::SIGKILL) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tool_call_cannot_speak_as_the_operator_in_the_rooms() {
        let frame = ClientFrame::RoomChat {
            client_request_id: "c1".into(),
            op: serde_json::json!({"type": "rooms"}),
        };
        let tool = Peer {
            pid: Some(7),
            inside: true,
        };
        let head = Peer {
            pid: Some(8),
            inside: false,
        };
        assert!(tool.refuses(&frame).is_some(), "a tool call is refused");
        assert!(head.refuses(&frame).is_none(), "a head is served");
    }
}
