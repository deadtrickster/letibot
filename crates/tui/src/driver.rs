//! The loop: frames in, screen out, ack after the screen is out.
//!
//! Small, because the order of three operations is the whole of it and burying
//! that order in a binary would make it unreviewable:
//!
//! ```text
//!   1. drain every frame that has arrived, classifying each
//!   2. draw
//!   3. ack `batch.last_seq()` with (rendered, filtered)
//! ```
//!
//! Step 3 comes after step 2, always. §13.2b: *"a crash then costs a duplicate,
//! never a silence"*. And the seq acked is the last one **read** in step 1, not the
//! last one drawn — a head at `Terse` renders almost nothing and must still
//! advance, or it rereads its own output forever.
//!
//! # The socket is not the session
//!
//! A daemon goes away for ordinary reasons — it is restarted, `--stop` lands, the box
//! has a moment — and a head whose loop treats that as the end of the program takes
//! the operator's view of a conversation that is still on disk with it. So the
//! connection lives in [`Link`], which owns the socket, the channel the frames arrive
//! on and the thread reading it, and whose failure mode is to **mark the link down**
//! rather than to return an error:
//!
//! ```text
//!   a write fails        -> Link::tick tells the head; the head draws the line
//!   the pump goes away   -> same, from Disconnected
//!   Link::reconnect      -> a new socket, ATTACH with the seq we had
//! ```
//!
//! The `since_seq` is the whole of the recovery: the daemon answers a resume with the
//! gap as events, or with a `Resync` when the gap is larger than it still holds. So
//! getting the socket back *is* getting the conversation back, out of the protocol
//! that was already there.

use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::JoinHandle;

use letibot_metrics::now_ms;
use letibot_sessionlog::client::{ClientError, HeadClient, Inbound, pump};
use letibot_sessionlog::protocol::{Ack, Caps, ServerFrame};
use letibot_sessionlog::wire::FrameReader;

use crate::app::{Action, App, Disposition, ForceOutcome, ForceWhy, Key, WAY_OUT};
use crate::head::{ForceTarget, Signaller, force_stop, kill_signaller};

/// Where the terminal's caret belongs: `(row, column)`, zero-based, or nowhere.
pub type Caret = Option<(usize, usize)>;

/// What puts a frame on a screen. A closure, so the same loop serves a terminal
/// and a test that renders to a `Vec<String>`.
pub type Draw<'a> = dyn FnMut(&[String], Caret) + 'a;

/// A connection that can die and come back: the client, the channel the reader pushes frames into, and the
/// thread. Kept together so the recovery is a two-line act for the caller rather than four pieces of state
/// swapped in the right order.
pub struct Link {
    client: HeadClient,
    rx: Receiver<Inbound>,
    /// `None` only while [`Link::close`] is running. A test never sees it, and keeping
    /// it an `Option` is what lets `join` take the handle out of `self`.
    reader: Option<JoinHandle<()>>,
    /// The session this head is IN, not the one it was opened for: a reconnect that named what the launcher
    /// knew would resume into whatever the daemon calls current, since a `Switch` moves the default.
    session: String,
    kind: String,
    identity: String,
    /// The socket this connection is on, kept so a head that asked the daemon to stop can see the file go: the
    /// daemon unlinks it after it has joined its accept loop.
    socket_path: std::path::PathBuf,
    /// **The process at the other end of this socket**, from `SO_PEERCRED` — the daemon
    /// itself, not a number read out of a file that may be stale or belong to an older
    /// daemon with the same workspace. `None` when the kernel would not say.
    daemon_pid: Option<i32>,
    /// **The force path, while it runs** — `head::force_stop` on its own thread, its answer
    /// arriving here. `Some` from the moment the head takes the force path until the answer
    /// has been handed to the app.
    force: Option<Receiver<ForceOutcome>>,
    /// When the daemon the head is stopping started (`process_started`), read when the stop
    /// began: what the force path checks the pid against before every signal.
    stop_started: Option<u64>,
    /// How the force path sends a signal. `kill(2)`, unless a test stood something else in.
    signaller: Signaller,
}

/// Which process is at the other end of this socket: `SO_PEERCRED`, filled in from the `connect`, so it is
/// the daemon this head is attached to and not a pid from a file. `None` on any failure, and the farewell says
/// *pid unknown* rather than filling in a zero.
#[cfg(target_os = "macos")]
fn peer_pid(s: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && pid > 0).then_some(pid)
}

#[cfg(not(target_os = "macos"))]
fn peer_pid(s: &UnixStream) -> Option<i32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid)
}

/// Where the process this head is watching came from. The daemon is a child of the head, and a child that
/// exits is a zombie holding its `/proc` entry until its parent waits, so *is it alive* was wrong about every
/// daemon that had stopped properly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parentage {
    /// **A direct child of this head.** The head owes it a wait, and until that wait happens
    /// the child is a zombie that `/proc` still lists. A `waitpid` is the only test that
    /// answers correctly here.
    Ours,
    /// **Somebody else's process.** Its own parent or init reaps it, so a dead one is gone
    /// from `/proc` and the wrapper's test is the right one — which is what an attached head
    /// that did not spawn the daemon is looking at.
    NotOurs,
}

/// Which of the two it is, read from `/proc` rather than assumed from how this head was launched: a head that
/// inherited its daemon through an `exec` never called `spawn`, so the kernel answers *is it mine* and not
/// *did I start it*. macOS cannot describe a zombie, so such a pid goes to the targeted `waitpid`.
#[cfg(target_os = "macos")]
fn parentage(pid: i32) -> Parentage {
    match bsdinfo(pid) {
        Ok((ppid, _)) if ppid == std::process::id() => Parentage::Ours,
        Ok(_) => Parentage::NotOurs,
        Err(libc::ESRCH) => Parentage::Ours,
        Err(_) => Parentage::NotOurs,
    }
}

/// `PROC_PIDTBSDINFO` — macOS's `/proc/<pid>/stat` — as `(ppid, status)`, or the errno:
/// `ESRCH` for a pid that is gone **or a zombie**, `EPERM` for another user's process.
#[cfg(target_os = "macos")]
fn bsdinfo(pid: i32) -> Result<(u32, u32), i32> {
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
        Ok((bi.pbi_ppid, bi.pbi_status))
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

#[cfg(not(target_os = "macos"))]
fn parentage(pid: i32) -> Parentage {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => match stat_ppid(&stat) {
            Some(ppid) if ppid == std::process::id() as i32 => Parentage::Ours,
            _ => Parentage::NotOurs,
        },
        // Gone, or unreadable: not something this head is the parent of, and the `/proc`
        // test below is the honest one either way.
        Err(_) => Parentage::NotOurs,
    }
}

/// The `ppid` out of one `/proc/<pid>/stat` line: field 4, read from the LAST `)`, since field 2 is the
/// executable's name and may contain spaces and brackets.
#[cfg(any(not(target_os = "macos"), test))]
fn stat_ppid(stat: &str) -> Option<i32> {
    let after = &stat[stat.rfind(')')? + 1..];
    let mut fields = after.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// The state char out of one `/proc/<pid>/stat` line — `R` running, `S` sleeping, **`Z` a
/// zombie**, `X` dead.
#[cfg(any(not(target_os = "macos"), test))]
fn stat_state(stat: &str) -> Option<char> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().next()?.chars().next()
}

/// Has that process gone, asked the way the relationship makes true. `Some(true)` lets the head leave early,
/// `Some(false)` is still there, and `None` is cannot tell, where the deadline decides. The four facts stay
/// four: nothing here touches `sent`, `acked` or `closed`.
pub(crate) fn process_gone(pid: i32) -> Option<bool> {
    match parentage(pid) {
        Parentage::Ours => match reap_own_child(pid) {
            Some(gone) => Some(gone),
            // `ECHILD`: the kernel says this is not our child after all — or somebody has
            // already reaped it, which is the same answer as reaping it here. Ask `/proc`.
            None => process_gone_by_proc(pid),
        },
        Parentage::NotOurs => process_gone_by_proc(pid),
    }
}

/// When that process started, the same for one process for its whole life and different for the next process
/// given the same pid, which is what the force path needs before it signals a pid read earlier. `None` when the
/// process is gone, and the caller then sends nothing.
#[cfg(not(target_os = "macos"))]
pub(crate) fn process_started(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat_starttime(&stat)
}

#[cfg(target_os = "macos")]
pub(crate) fn process_started(pid: i32) -> Option<u64> {
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
    (got == size).then(|| bi.pbi_start_tvsec * 1_000_000 + bi.pbi_start_tvusec)
}

/// Field 22 (`starttime`) of one `/proc/<pid>/stat` line: the 20th field after the last `)`.
#[cfg(any(not(target_os = "macos"), test))]
fn stat_starttime(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(19)?.parse().ok()
}

/// Reap our own child if it has exited, and answer whether it is gone: `waitpid(pid, WNOHANG)` and never
/// `waitpid(-1, …)`, since a head that reaped another child's exit would be corrupting a process table it does
/// not own. An unreaped zombie is a leak either way, so this runs every tick.
fn reap_own_child(pid: i32) -> Option<bool> {
    let mut status: libc::c_int = 0;
    let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if r == pid {
        // It had exited, and it is reaped now — the zombie this whole function exists for.
        Some(true)
    } else if r == 0 {
        // Our child, and still running. `0` is the only answer that means that.
        Some(false)
    } else {
        None
    }
}

/// The `/proc` test, for a process this head is not the parent of and for the fallback: a zombie counts as
/// gone, and macOS has no `/proc`, so `PROC_PIDTBSDINFO` is asked instead. With the parent's `waitpid` this is
/// what answers when the kernel declines to.
#[cfg(target_os = "macos")]
fn process_gone_by_proc(pid: i32) -> Option<bool> {
    match bsdinfo(pid) {
        Ok(_) => Some(false),
        Err(libc::ESRCH) => Some(true),
        Err(libc::EPERM) => Some(false),
        Err(_) => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn process_gone_by_proc(pid: i32) -> Option<bool> {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => match stat_state(&stat) {
            Some('Z') | Some('X') => Some(true),
            Some(_) => Some(false),
            None => None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(true),
        // PermissionDenied means the entry IS there; anything else is a `/proc` that cannot
        // answer, which is not the same fact.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(false),
        Err(_) => None,
    }
}

impl Link {
    /// Connect and send the `ATTACH`. The `Hello` arrives on [`Link::frames`] like any
    /// other frame — which is also what makes a reconnect look like the first attach.
    pub fn open(
        path: impl AsRef<std::path::Path>,
        session: &str,
        since_seq: u64,
        kind: &str,
        identity: &str,
    ) -> Result<Link, ClientError> {
        let socket_path = path.as_ref().to_path_buf();
        let (client, reader) =
            HeadClient::start_attach(path, session, since_seq, kind, identity, Caps::default())?;
        Ok(Link::spawn(
            client,
            reader,
            session.to_string(),
            kind.to_string(),
            identity.to_string(),
            socket_path,
        ))
    }

    fn spawn(
        mut client: HeadClient,
        reader: FrameReader<UnixStream>,
        session: String,
        kind: String,
        identity: String,
        socket_path: std::path::PathBuf,
    ) -> Link {
        let (tx, rx) = std::sync::mpsc::channel();
        // **Read once, at connect.** `SO_PEERCRED` answers for the socket, so it cannot
        // go stale while the connection is up, and a reconnect is a new connection to
        // whatever daemon is there now — see `Link::reconnect`.
        let daemon_pid = peer_pid(client.socket());
        Link {
            client,
            rx,
            reader: Some(std::thread::spawn(move || pump(reader, tx))),
            session,
            kind,
            identity,
            socket_path,
            daemon_pid,
            force: None,
            stop_started: None,
            signaller: kill_signaller(),
        }
    }

    /// **Stand in how the force path signals.** For a test that needs a daemon which
    /// survives SIGTERM and SIGKILL — the one case the head's last farewell is for — and
    /// cannot have one from the kernel. Kept across [`Link::reconnect`].
    pub fn set_signaller(&mut self, signaller: Signaller) {
        self.signaller = signaller;
    }

    /// **Take the force path** — once — on its own thread, against the pid the HEAD holds.
    fn start_force(&mut self, pid: Option<i32>) {
        if self.force.is_some() {
            return;
        }
        let target = ForceTarget {
            socket: self.socket_path.clone(),
            pid,
            started: self.stop_started,
        };
        let signal = self.signaller.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(force_stop(&target, &signal));
        });
        self.force = Some(rx);
    }

    /// The process at the other end of this socket, or `None`. Read once by the caller that opened the
    /// connection and handed to the head, so this is the seed and not a second home for the fact.
    pub fn daemon_pid(&self) -> Option<i32> {
        self.daemon_pid
    }

    /// The frames, in order, as the reader hands them over.
    pub fn frames(&self) -> &Receiver<Inbound> {
        &self.rx
    }

    /// Take the seat the daemon named in a `Hello` this caller read itself: the first attach of a head that
    /// has not been through [`Link::tick`], since a refused attach must never claim the screen. The session
    /// comes with it, since the daemon may have seated the connection somewhere else.
    pub fn seated_by(&mut self, hello: &ServerFrame) {
        self.note_session(hello);
        self.client.seated_by(hello);
    }

    /// Where this head is, from the `Hello` that said so: the one writer of [`Link::session`] after the first
    /// attach. A `Hello` is an attach, a switch, a re-attach or a resync, and all four leave the head in the
    /// session it names.
    fn note_session(&mut self, frame: &ServerFrame) {
        if let ServerFrame::Hello { session_id, .. } = frame {
            self.session = session_id.clone();
        }
    }

    /// The session this connection is for.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Drop the dead socket and try for a new one, resuming where this head got to. `since_seq` is passed in
    /// because the read mark belongs to the head that read the frames. The old connection goes first, since its
    /// reader thread is blocked in a `read` and only a channel with no receiver ends it.
    pub fn reconnect(
        &mut self,
        path: impl AsRef<std::path::Path>,
        since_seq: u64,
    ) -> Result<(), ClientError> {
        self.close();
        // `self.session` is the session this head is in, so the retry resumes the same conversation rather
        // than attaching to the daemon's current one, and `since_seq` is a mark in that log and no other.
        let (client, reader) = HeadClient::start_attach(
            path,
            &self.session,
            since_seq,
            &self.kind,
            &self.identity,
            Caps::default(),
        )?;
        let (session, kind, identity) = (
            self.session.clone(),
            self.kind.clone(),
            self.identity.clone(),
        );
        // **The force path is the head's, not the connection's**: a stop in flight keeps its
        // thread, its answer and the identity it checks across a new socket.
        let signaller = self.signaller.clone();
        let force = self.force.take();
        let stop_started = self.stop_started;
        *self = Link::spawn(
            client,
            reader,
            session,
            kind,
            identity,
            self.socket_path.clone(),
        );
        self.signaller = signaller;
        self.force = force;
        self.stop_started = stop_started;
        Ok(())
    }

    /// A `Bye` is final and a dropped socket is not, so this is the caller's way of saying goodbye when it is
    /// leaving: send the `Detach` if the socket is still there, then stop the reader. Best effort throughout.
    pub fn detach(&mut self) {
        let _ = self.client.detach();
        self.close();
    }

    /// Stop reading and wait for the reader to actually stop. The channel ends a reader that is awake, since
    /// the next `send` fails; the socket ends one that is asleep in `read`. Neither is enough alone.
    fn close(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        drop(std::mem::replace(&mut self.rx, rx));
        let _ = self.client.shut_down();
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
    }

    /// One pass: drain, draw, ack, with every way the socket can fail turned into a fact the head draws rather
    /// than an error it dies of. It returns nothing, which is the requirement stated as a type: no failure of
    /// the connection is a failure of this loop.
    pub fn tick(
        &mut self,
        app: &mut App,
        size: (usize, usize),
        keys: &[Key],
        raw: &[u8],
        draw: &mut Draw<'_>,
    ) {
        app.clock(now_ms());
        // The workspace's branch, read from the loop and never from a paint: it spawns a process, and a
        // measurement never runs where the frame is drawn. Two seconds between readings keeps the rate low.
        app.refresh_git();
        app.refresh_github_refs();
        // **And the model check a held message waits on** (`app::reach`) — collected here for the
        // same reason: a probe is a network round trip, and the loop never waits on one.
        app.poll_reach();

        // 1. Drain. Nothing is sent in this phase.
        let mut rendered = 0u64;
        let mut filtered = 0u64;
        let mut last_seq = 0u64;
        loop {
            match self.rx.try_recv() {
                Ok(Inbound::Frame(frame)) => {
                    if let ServerFrame::Event(env) = &frame {
                        // The read mark, taken from what was *read*. There is no
                        // "last rendered seq" variable here, on purpose.
                        last_seq = env.seq;
                    }
                    // And where this head is, from the frame that says so: this is the mark a reconnect sends.
                    // See [`Link::reconnect`] for what it cost to leave it out.
                    self.note_session(&frame);
                    match app.apply(frame) {
                        Disposition::Rendered => rendered += 1,
                        Disposition::Filtered => filtered += 1,
                        Disposition::Control => {}
                    }
                }
                // A frame this head cannot read is said, counted and stepped over: it carries no seq, so it
                // moves neither counter the `Ack` carries. It reaches the transcript and the `unreadable`
                // counter, which is what makes a daemon this head cannot read distinguishable from a quiet one.
                Ok(Inbound::Unreadable(u)) => {
                    app.unreadable(u);
                }
                Err(TryRecvError::Empty) => break,
                // The reader is gone, and that is how a clean death is noticed: a daemon killed with a signal
                // closes the socket, the pump reads EOF and the sender is dropped, with nothing failing to write.
                // Without this arm the loop would tick for ever over a dead channel.
                Err(TryRecvError::Disconnected) => {
                    app.link_down("the daemon closed the connection");
                    break;
                }
            }
        }

        // A `Hello` in the drain above may have seated this connection as a different
        // head — that is what a session switch is — and a client still acking under the
        // old id would ack into a session it has left, which the hub ignores in silence.
        if let Some(id) = app.take_seated() {
            self.client.seated(&id);
        }

        // Actions a *frame* produced, not a key: the switch that follows a session
        // being created. Ahead of the key actions, because they are the answer to
        // something the operator already asked for.
        let mut actions = app.take_actions();
        // The pane owns the keyboard while it is open, and this is where that is decided: `raw` is what the
        // program gets and the `Key`s are this head's reading, so the two paths are exclusive rather than
        // layered. `App::pane_keys` finds the way out in the bytes before anything is forwarded.
        if app.pane_open() {
            actions.extend(app.pane_keys(raw));
        } else {
            for k in keys {
                // Cloned rather than copied: `Key::Paste` carries the paste, because the
                // point of bracketed paste is that a 3 KB stack trace is one key.
                if let Some(a) = app.key(k.clone()) {
                    actions.push(a);
                }
            }
            // The same read can carry the keystroke that opens the pane and the way out of it, and `app::key_of`
            // maps `0x1c` to nothing, so without this arm the pane would open with no way out at all.
            if app.pane_open()
                && let Some(at) = raw.iter().position(|b| *b == WAY_OUT)
            {
                actions.extend(app.pane_keys(&raw[at..]));
            }
        }
        // The pane's rectangle is not asked for here: `compose_screen` resizes it, being the only layer that
        // knows how many rows it got, and the frame it queues is sent on the next tick. Asking here would send
        // the terminal's size as the pane's.

        // 2. Draw. Every frame is built; whether any of it reaches the terminal is
        //    `Terminal::draw`'s business, and for an unchanged frame the answer is no
        //    bytes at all.
        let screen = app.screen(size.0, size.1);
        draw(&screen, app.cursor());

        // Answer with what was actually drawn: this is the only place that knows what the person is looking at,
        // since it is the place that put the bytes on the terminal at this head's real size. A failure here is
        // the socket, reported like every other write.
        for req_id in app.take_screen_requests() {
            if let Err(e) = self.client.screen(&req_id, size.0, size.1, screen.clone()) {
                app.link_down(&e.to_string());
            }
        }

        // 3. Ack — after the frame is out, and only when a frame was read. An idle
        //    tick has no seq and must not invent one.
        if last_seq > 0
            && !app.detached()
            && let Err(e) = self.client.ack(Ack {
                seq: last_seq,
                rendered,
                filtered,
            })
        {
            app.link_down(&e.to_string());
        }

        for a in actions {
            // A link opens whether or not the daemon is there: the URL is the person's desktop and this head is
            // the process that has it. Taken ahead of the link-down and read-only arms, since neither has
            // anything to say about a URL.
            if let Action::OpenUrl(url) = &a {
                if let Err(why) = open_url(url) {
                    app.say(&why);
                }
                continue;
            }
            // **Nothing leaves a head whose link is down**, and it says so once rather
            // than failing once per action. Every write below would fail the same way,
            // and a batch of them would bury the one sentence that matters.
            if app.detached() {
                // A pane whose open never left is not a pane: the link can go down between the keystroke and
                // this send, leaving a rectangle with no program in it and no ending coming. See
                // [`App::drop_pane`].
                app.drop_pane();
                app.refused_while_detached();
                break;
            }
            // Nothing leaves a read-only attach either, and this is the floor under it: the arm makes *nothing
            // the person types can kill the session* true for the keys that do not pass through `App::submit`,
            // a decision card's answer and a pane's bytes among them. Say once, send nothing, keep reading.
            if matches!(a, Action::StopDaemon)
                && let Some(daemon) = app.daemon_protocol()
                && daemon < letibot_sessionlog::protocol::PROTOCOL_VERSION
            {
                let who = self.client.identity().to_string();
                let pid = app.daemon_pid();
                self.stop_started = pid.and_then(process_started);
                app.force_stop_began(
                    &who,
                    pid,
                    ForceWhy::OlderDaemon {
                        daemon,
                        head: letibot_sessionlog::protocol::PROTOCOL_VERSION,
                    },
                    now_ms(),
                );
                self.start_force(pid);
                continue;
            }
            if app.skew_locked() {
                app.refused_while_locked();
                break;
            }
            // The whole match is one `Result` so that the *only* thing a failed write
            // does is tell the head. Each arm used to end in `?`, which propagated out
            // of this loop and out of `main`.
            let sent = (|| -> Result<(), ClientError> {
                match a {
                    // `expected_seq` is what this head was looking at when the operator
                    // acted. The daemon decides what that means per command; the head's
                    // job is to report it honestly.
                    Action::Prompt(p) => {
                        self.client
                            .prompt_attached(app.seq, &p.text, p.attachments)?;
                    }
                    Action::WithdrawPrompts => {
                        self.client.withdraw_prompts(app.seq)?;
                    }
                    // A snapshot announced a row it never filled, so the head asks the daemon for that row by
                    // name: the store is the truth and the answer arrives as `RowFetched` and fills the position
                    // the head named.
                    Action::FetchRow {
                        session,
                        row,
                        at,
                        len,
                    } => {
                        self.client.fetch_row(&session, row, at, len)?;
                    }
                    // The person's half of the board, on its way out: the head sends the WHOLE list, since
                    // there is no per-row frame and no delta, so a head cannot accumulate a difference between
                    // its copy and the store. A state move for the model's rows rides the same frame.
                    Action::SetOperatorTodos { items, moved } => {
                        self.client.set_operator_todos(app.seq, items, moved)?;
                    }
                    Action::Interrupt(reason) => {
                        self.client.interrupt(app.seq, &reason)?;
                    }
                    Action::Promote => {
                        self.client.promote(app.seq)?;
                    }
                    Action::Answer {
                        req_id,
                        option_id,
                        pattern,
                        note,
                    } => {
                        let request = self.client.answer_with(
                            &req_id,
                            &option_id,
                            pattern.as_deref(),
                            note.as_deref(),
                        )?;
                        app.answer_sent(&req_id, request);
                    }
                    // **The frame this head had no way to send** (§1.7). `Answer`
                    // above grants a permission; this answers a question, and the
                    // client method has existed since protocol 5 with no caller.
                    Action::AnswerQuestion { req_id, answer } => {
                        let request = self.client.answer_question(&req_id, answer)?;
                        app.answer_sent(&req_id, request);
                    }
                    Action::AnswerTodo {
                        content,
                        question,
                        answer,
                    } => {
                        self.client.answer_todo(&content, &question, answer)?;
                    }
                    Action::Resync => {
                        self.client.request_resync()?;
                    }
                    Action::ListSessions => {
                        self.client.list_sessions()?;
                    }
                    Action::ListJobs => {
                        self.client.list_jobs()?;
                    }
                    // The standing-notes pane's bootstrap read. The jobs pane's shape: a read
                    // whose answer arrives as a `StandingNotes` frame on the pump, and the
                    // head draws what comes back rather than deciding what is in it.
                    Action::ListNotes => {
                        self.client.list_notes()?;
                    }
                    // The skills pane's bootstrap read — rows of names, never bodies.
                    Action::ListSkills => {
                        self.client.list_skills()?;
                    }
                    // **The operator hands the session a skill.** Stale-tolerant like a prompt:
                    // somebody who asked while the screen moved still meant it.
                    Action::LoadSkill { name, then } => {
                        self.client.load_skill(app.seq, &name, &then)?;
                    }
                    Action::ListTodos => {
                        self.client.list_todos()?;
                    }
                    // The queue pane's bootstrap read, the same shape as the two above: the answer arrives as a
                    // `MergeQueue` frame on the pump. It is the one ask not scoped to this session, since there
                    // is one queue, which is the daemon's business.
                    Action::ListMergeQueue => {
                        self.client.list_merge_queue()?;
                    }
                    Action::NewSession(title) => {
                        // The head's own working directory, read here rather than carried
                        // through `App`: the app is the same object under `--replay`, where
                        // there is no daemon and no session to make.
                        let cwd = std::env::current_dir()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        self.client.new_session(&title, &cwd)?;
                    }
                    // The answer is a second `Hello` on the pump, applied like the first. Nothing is torn down
                    // here: the switch happens inside the daemon on this same socket.
                    Action::Switch(id) => {
                        self.client.switch(&id, 0)?;
                    }
                    Action::PaneOpen { pane, w, h } => {
                        self.client.pane_open(&pane, w, h)?;
                    }
                    // **The pane left the screen**: the daemon takes it off its refresh clock,
                    // so a closed dashboard stops reading files every `every_s`. Silent in
                    // answer — the head that closed it is the one that said so.
                    Action::PaneClose { pane } => {
                        self.client.pane_close(&pane)?;
                    }
                    // **A stop's action, back to the pane that drew it** — the same shape the
                    // open has: a read the daemon answers with a new view on the log, not a
                    // command queued behind a turn.
                    Action::PaneAction { pane, item, action } => {
                        self.client.pane_action(&pane, &item, &action)?;
                    }
                    Action::Settings => {
                        self.client.settings()?;
                    }
                    // A read, not a move: the answer arrives as a `Peeked` frame and the pane is built from it,
                    // so nothing changes which session this connection is in and nothing is read until the
                    // person asked. A child is a session, so its pane is drawn by the rows renderer.
                    Action::Peek(id) => {
                        self.client
                            .peek(&id, letibot_sessionlog::protocol::PeekShape::Rows)?;
                    }
                    // The same shape as `Peek`: a read that comes back as an event on the log, with the
                    // `client_request_id` dropped since the answer is addressed to the session, not to the ask.
                    Action::ReadJobOutput { job, offset } => {
                        self.client.read_job_output(&job, offset)?;
                    }
                    // Same shape as `NewSession`: the daemon answers with `Sessions` naming it as `created`,
                    // and `App::apply` turns that into the switch. One path for going to a session that was not
                    // here a moment ago, minted or restored.
                    Action::ResumeSession(id) => {
                        self.client.resume_session(&id)?;
                    }
                    Action::Rename { session_id, title } => {
                        self.client.rename_session(&session_id, &title)?;
                    }
                    // The compaction itself is disclosed on the session's own log: the
                    // summary turn streams like any turn, and the forked transcript's
                    // first item says what replaced the history. Nothing to apply here.
                    Action::Compact => {
                        self.client.compact(app.seq)?;
                    }
                    Action::Reseat { summarise } => {
                        self.client.reseat(app.seq, summarise)?;
                    }
                    Action::Mode { name, consented } => {
                        self.client.set_mode(app.seq, &name, consented)?;
                    }
                    Action::ModeConsent { req_id, yes } => {
                        self.client.mode_consent(app.seq, &req_id, yes)?;
                    }
                    Action::Slash { line } => {
                        self.client.slash(app.seq, &line)?;
                    }
                    Action::Feedback {
                        at_ms,
                        polarity,
                        note,
                        item_id,
                    } => {
                        self.client
                            .feedback(at_ms, &polarity, &note, item_id.as_deref())?;
                    }
                    // The person's own call through the door: one frame, since `execute: true` means the
                    // daemon runs it and appends the row, so there is no result to hand back. The permission to
                    // go is still `OperatorCallAllowed`.
                    Action::HeadRun { name, arguments } => {
                        let n = app.next_head_run();
                        let call_id = format!("{}-{n}", app.head_id());
                        let _ = self
                            .client
                            .operator_call(app.seq, &call_id, &name, &arguments, true)?;
                    }
                    // The person's own shell line: no `call_id` and no result frame, since the daemon owns the
                    // run and the rows. The echo is retired by the `User` row the daemon appends.
                    Action::OperatorShell { line } => {
                        self.client.operator_shell(app.seq, &line)?;
                    }
                    // **Taken above, before the link-down and read-only arms** — a URL is not
                    // the daemon's and the write that carries it never happens. Listed here
                    // because this match is exhaustive over `Action`.
                    Action::OpenUrl(_) => {}
                    // `!term <command>`, opened with the terminal's own rectangle: this is the one action that
                    // carries a size and it comes from `tick`'s argument rather than from `App`, since the driver
                    // read it. Sending 80x24 and resizing would make every program lay out twice.
                    Action::TermOpen { line } => {
                        self.client.term_open(&line, size.0, size.1)?;
                    }
                    // **The keys, verbatim.** No `client_request_id` and nothing to wait for:
                    // a keystroke has no answer, and the pane's next `TermOutput` is the
                    // program's reply to whatever it did with it.
                    Action::TermInput { pane, bytes } => {
                        self.client.term_input(pane, &bytes)?;
                    }
                    Action::TermResize { pane, cols, rows } => {
                        self.client.term_resize(pane, cols, rows)?;
                    }
                    Action::TermClose { pane } => {
                        self.client.term_close(pane)?;
                    }
                    // The read behind a head's own line about a program it is not drawing: asked on attach and
                    // before a `!term close` the head cannot answer, and answered as `TermStatus` on the reader.
                    Action::TermStatus => {
                        self.client.term_status()?;
                    }
                    // The model's half of the `!` completion, asked when the history has no match or its cycle
                    // is exhausted. The `client_request_id` is the head's and is passed through, since the answer
                    // comes back on the pump and must be matched to the prefix and position it asked about.
                    Action::SuggestShell {
                        prefix,
                        client_request_id,
                    } => {
                        self.client
                            .suggest_shell(app.seq, &client_request_id, &prefix)?;
                    }
                    Action::Secret { req_id, secret } => {
                        self.client.secret(&req_id, secret)?;
                    }
                    // The person's answer to a command that asked them something: off the queue and on this
                    // socket, since the worker is blocked inside the very command that is asking. Nothing to wait
                    // for, since the run's own output is the reply.
                    Action::PromptAnswer { req_id, line } => {
                        self.client.prompt_answer(&req_id, &line)?;
                    }
                    // The manual way in: `!send`, one line to whatever this session is running. No card and no
                    // request id, which is what makes it the floor under the card.
                    Action::SendLine { line } => {
                        self.client.send_line(&line)?;
                    }
                    // Leaving on purpose: a detach that fails is the socket that was
                    // already gone, which the loop is about to notice anyway. Not sent
                    // while a stop is in flight — see `App::wants_detach`.
                    Action::Quit => {
                        if app.wants_detach() {
                            let _ = self.client.detach();
                        }
                    }
                    // Ask, then stay until you know: the daemon announces the stop to every other head before
                    // it goes, so the request has to reach it while this head is still attached, and whether the
                    // frame reached the socket was a race against this head's own detach.
                    Action::StopDaemon => {
                        let who = self.client.identity().to_string();
                        // The pid comes off the head and not off this link: `main` seeds `App::daemon_pid` from
                        // `SO_PEERCRED`, and the head is what `/status` reads. A second copy here is how the two
                        // come to disagree about which process the person is sent to `ps` for.
                        let pid = app.daemon_pid();
                        // **Which process that pid is, read now** while it is certainly the
                        // daemon: the force path re-reads it before every signal and sends
                        // nothing to a pid that has since been handed to something else.
                        self.stop_started = pid.and_then(process_started);
                        let sent = self.client.stop(app.seq, &who);
                        // A failed write is not a failed stop — the socket may already be
                        // gone because the daemon is going — so it is recorded as *not
                        // sent* and the wait runs anyway. What it changes is the sentence.
                        app.stop_began(&who, sent.is_ok(), pid, now_ms());
                        // **And this head does not detach.** The connection is what the
                        // wait is reading: `detach()` closes the reader, and the daemon's
                        // socket closing is one of the facts being watched for.
                    }
                }
                Ok(())
            })();
            if let Err(e) = sent {
                // The write failed with the socket still open as far as this head knew:
                // the daemon is gone. Said, and the loop carries on to draw the line.
                app.link_down(&e.to_string());
            }
        }
        // A child this head is the parent of is reaped every tick, not only while a stop is in flight: an
        // unreaped zombie holds a pid, a task slot and a `/proc` entry, and this process is the only one that can
        // clear it. One `waitpid(WNOHANG)` per tick, on a pid the head already holds.
        if let Some(p) = app.daemon_pid()
            && parentage(p) == Parentage::Ours
        {
            let _ = reap_own_child(p);
        }
        // **Last, every tick, and after the drawing.** R30. Nothing here sends anything:
        // it reads the four things that answer the question the operator asked.
        self.watch_stop(app);
    }

    /// Has the daemon this head asked to stop actually gone? Asked once a tick and a watcher rather
    /// than a wait, so the loop keeps drawing while it runs; a `recv_timeout` loop here would be the
    /// freeze. The ack, the close, the process and the deadline are four different answers.
    pub fn watch_stop(&mut self, app: &mut App) {
        let now = now_ms();
        // Read the state out before touching it: `App::stopping` borrows the app and every write below is also
        // on the app, and a preview in one borrow and a write in the next keeps this one function. The force
        // path's answer comes first, since a force that saw the process go is still owed its line.
        if let Some(rx) = &self.force {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.force = None;
                    app.force_stop_finished(outcome);
                    app.mark_redraw();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                // The thread died without answering — a panic in it. Said rather than
                // waited on for ever.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.force = None;
                    app.force_stop_finished(ForceOutcome {
                        gone: false,
                        said: "the force path ended without an answer".into(),
                    });
                }
            }
        }
        let Some((closed, gone, pid, deadline, since, forced)) = app.stopping().map(|s| {
            (
                s.closed,
                s.gone,
                s.pid,
                s.deadline_ms,
                s.since_ms,
                s.forced.is_some(),
            )
        }) else {
            return;
        };
        if gone {
            return;
        }
        let mut next_closed = closed;
        let mut next_gone = gone;
        // The socket file. `exists` on a unix socket is a `stat` on the directory entry,
        // which is exactly the fact `shutdown`'s `remove_file` publishes.
        if !closed && !self.socket_path.exists() {
            next_closed = true;
        }
        // The process, and only while the pid is known: `Some(true)` ends the wait early and an unanswerable
        // one leaves the deadline to decide, which is the direction to be wrong in. Which test applies depends
        // on the relationship, since a child is a zombie the moment it exits.
        if let Some(p) = pid
            && process_gone(p) == Some(true)
        {
            next_gone = true;
        }
        if next_closed != closed || next_gone != gone {
            if let Some(s) = app.stopping_mut() {
                s.closed = next_closed;
                s.gone = next_gone;
            }
            app.mark_redraw();
        }
        // The ordinary stop did not finish: escalate, once. Only when the deadline passed and the process is
        // still there, and the pid is the one the head holds, checked again before each signal. A force path
        // that failed is not escalated again; it is said in the farewell with the verb to run.
        if !next_gone && !forced && now >= deadline {
            app.force_stop_escalated(
                ForceWhy::DidNotStop {
                    waited_ms: deadline.saturating_sub(since),
                },
                now,
            );
            self.start_force(pid);
        }
        // **The deadline crossing is a frame too.** The waiting line stops being drawn the
        // moment the question is answered, and without this the head would sit on the last
        // frame it drew for up to a tick with a sentence saying it was still waiting.
        if next_gone || now >= deadline {
            app.mark_redraw();
        }
    }
}

impl Drop for Link {
    /// No reader thread outlives the connection it was reading: replacing or dropping the receiver is what ends
    /// it, and a head that exited with a thread blocked in `read` on a socket nobody closes is a process that
    /// will not die.
    fn drop(&mut self) {
        if self.reader.is_some() {
            self.close();
        }
    }
}

/// The platform's opener for a link the person clicked: `open` on macOS, `xdg-open` on Linux. Waited for
/// rather than spawned and forgotten, since a child nobody reaps is a pid this head keeps. A box with neither
/// program is owed the sentence rather than a link that looks broken.
fn open_url(url: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("`{program}` refused `{url}` ({s})")),
        Err(e) => Err(format!(
            "no `{program}` on this box, so `{url}` was not opened: {e}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ppid` and the state char come off the LAST `)`: `/proc/<pid>/stat` puts the executable's name in
    /// parentheses as field 2, unbounded and free to contain spaces and brackets, so splitting from the left
    /// reads the wrong field for exactly the processes somebody would name that way.
    #[test]
    fn the_stat_line_is_parsed_from_the_last_bracket() {
        // A plain one: pid (comm) state ppid …
        assert_eq!(stat_state("42 (harnessd) S 7 42 42 0"), Some('S'));
        assert_eq!(stat_ppid("42 (harnessd) S 7 42 42 0"), Some(7));
        // A comm with a space, a `)`, and more parentheses inside it.
        let nasty = "9067 (my ) weird (program)) Z 1 9067 0";
        assert_eq!(stat_state(nasty), Some('Z'));
        assert_eq!(stat_ppid(nasty), Some(1));
        // Truncated or nonsense lines answer nothing rather than guessing.
        assert_eq!(stat_state("nonsense with no bracket"), None);
        assert_eq!(stat_ppid("42 (x) S"), None);
        assert_eq!(stat_ppid("42 (x) S not-a-number 1"), None);
    }

    /// **`starttime` is field 22, read from the last `)`** — the number the force path checks
    /// a pid against before it signals, so a pid handed on to another process is not signalled.
    #[test]
    fn the_start_time_is_field_22_and_names_one_process() {
        let line = "42 (my ) odd) S 1 42 42 0 -1 4194560 10 0 0 0 1 2 0 0 20 0 3 0 777123 0 0";
        assert_eq!(stat_starttime(line), Some(777123));
        assert_eq!(stat_starttime("42 (x) S 1"), None);
        // This process: one answer, read twice.
        let me = std::process::id() as i32;
        assert!(process_started(me).is_some());
        assert_eq!(process_started(me), process_started(me));
        assert_eq!(
            process_started(i32::MAX),
            None,
            "a pid that is not there has none"
        );
    }

    /// A zombie is gone, reproduced against a real one: the daemon had stopped and `/proc` still listed it,
    /// because it was this process's unreaped child. So this spawns a real child, lets it exit without waiting,
    /// and asks the question, which is the only test that reproduces the mechanism rather than describing it.
    #[test]
    fn an_unreaped_child_that_has_exited_is_gone() {
        use std::process::Command;
        let mut child = Command::new("true") // on PATH: `/bin/true` on Linux, `/usr/bin/true` on macOS
            .spawn()
            .expect("`true` starts");
        let pid = child.id() as i32;
        // It is ours, and the kernel says so — which is what decides which test runs.
        assert_eq!(
            parentage(pid),
            Parentage::Ours,
            "a process this test spawned is not seen as this process's child"
        );
        // Let it exit without waiting for it. A short sleep is the only synchronisation
        // available: there is no event for "it has exited but not been reaped", which is
        // exactly the state under test.
        std::thread::sleep(std::time::Duration::from_millis(200));
        // **It is a zombie, and `/proc` says so** — the observation the old test read as
        // `alive`. Asserted directly, so this test fails loudly rather than silently turning
        // into a test of something else if the timing below ever changes.
        #[cfg(not(target_os = "macos"))]
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            assert_eq!(
                stat_state(&stat),
                Some('Z'),
                "the child exited but is not a zombie — this test is not testing what it says: {stat:?}"
            );
        }
        // macOS will not describe a zombie, so the same fact is read as "the kernel still
        // has the pid (`kill 0` succeeds) and libproc says `ESRCH`".
        #[cfg(target_os = "macos")]
        assert!(
            unsafe { libc::kill(pid, 0) } == 0 && bsdinfo(pid) == Err(libc::ESRCH),
            "the child exited but is not a zombie — this test is not testing what it says"
        );
        // **And the answer is GONE**, which the old `fs::metadata` could not give: the
        // directory is there for a zombie.
        assert_eq!(
            process_gone(pid),
            Some(true),
            "an exited child was reported as still running, which is the defect"
        );
        // And the reap really happened: a second ask finds no `/proc` entry at all, because
        // this process has now collected it.
        assert_eq!(process_gone(pid), Some(true));
        let _ = child.wait();
    }

    /// **A live child is still there** — the negative, without which the test above would pass
    /// on a function that answered `gone` to everything.
    #[test]
    fn a_running_child_is_not_gone() {
        use std::process::Command;
        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .expect("`/bin/sleep` starts");
        let pid = child.id() as i32;
        assert_eq!(parentage(pid), Parentage::Ours);
        assert_eq!(
            process_gone(pid),
            Some(false),
            "a running child read as gone"
        );
        // **And asking did not kill it** — `WNOHANG` returns without waiting, and a version
        // that blocked here would hang the turn for five seconds while looking healthy.
        assert_eq!(process_gone(pid), Some(false));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            process_gone(pid),
            Some(true),
            "a killed child is gone once reaped"
        );
    }

    /// **A process this head is not the parent of is answered by `/proc`, and a pid that never
    /// existed is gone.** The second is the wrapper's own case, and it must not have been
    /// broken by routeing the child case through `waitpid`.
    #[test]
    fn a_pid_that_is_not_this_processs_child_is_answered_from_proc() {
        // **PID 1 is nobody's child**, so this takes the `/proc` path — and it is running, so
        // the answer is *not gone*.
        assert_eq!(parentage(1), Parentage::NotOurs);
        assert_eq!(process_gone(1), Some(false), "init read as gone");
        // A pid that cannot exist: `i32::MAX` is above every `pid_max` on Linux, so this asks about a process
        // that was never there, which is what `letibot --stop` saw a moment after the head exited. On macOS the
        // pid goes to `waitpid` first, so only the answer is shared.
        #[cfg(not(target_os = "macos"))]
        assert_eq!(parentage(i32::MAX), Parentage::NotOurs);
        assert_eq!(process_gone(i32::MAX), Some(true), "an absent pid is gone");
    }
}
