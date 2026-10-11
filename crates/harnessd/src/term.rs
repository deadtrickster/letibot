//! **The daemon's half of `!term`** — who owns a pane's pty, and how its bytes reach the head.
//!
//! # What this module is
//!
//! [`letibot_sessionlog::TerminalDriver`] is the seam: a trait the *server* can call on the
//! connection's thread, implemented here because this is the crate with the daemon in it.
//! [`Terminals`] is the implementation, and it is small on purpose — the mechanism is
//! [`letibot_tools::exec::term`], which owns the pty, the `setsid`/`TIOCSCTTY`, the reader
//! thread and the cgroup. What is here is the three things that are *the daemon's*:
//!
//! | | |
//! |---|---|
//! | **the panes** | several per session, each with a number the session never reuses, so a second `TermOpen` starts a second program beside the first rather than replacing it or being refused (R60 item 2) |
//! | **the boundary** | the session's own, read off its process host when a pane opens ([`letibot_sessionlog::PaneBoundary`]) — the view a `bash` call gets (R60 item 3) |
//! | **the workspace** | the session's own, read from the registry — a pane runs where the conversation runs, not where the daemon does |
//! | **the screen** | [`Pane::log`] — what the program has drawn, kept here and replayed to a head that asks for it (a bare `!term`) |
//! | **the sink** | [`Sink`], which turns the pty's reader thread's bytes into [`ServerFrame::TermOutput`] on the session's heads |
//!
//! # Who holds the screen, and why it is the daemon
//!
//! A head draws the pane, but **the daemon holds it**, and the operator's own defect is what
//! settles the question: *"i typed `!term mc` … it flashed and was gone … a second `!term`
//! then said 'term pane exists'"*, and the code admitted the same thing in its own words
//! (*"a head that switches back does not find its pane again, it finds the transcript"*).
//! A head is one view of a session and there may be several; a program on a pty is the
//! session's and there is exactly one. So the screen belongs to the half that outlives any
//! one head — and putting it here buys three things a head cannot:
//!
//! * **any head that asks can be given it back** — `attach`, the answer to a bare `!term`;
//! * **it survives a session switch**, because nothing about it was ever in a head;
//! * **and it can be handed to the model as text later** (the `capture-pane` idea): the bytes
//!   are here, and `letibot_vt::Screen` is what turns them into rows — a decision that needs
//!   no wire change because the crate that would do it is already below both halves.
//!
//! **What is held is the byte log, and the screen is what those bytes are.** [`Pane::log`] is
//! the last [`SCREEN_LOG`] bytes the program wrote; a replay is one [`ServerFrame::TermOutput`]
//! and the head's own `letibot_vt::Screen` reconstructs the screen from it — the same crate,
//! the same parser, the same cells, so the replay is not a rendering of the screen but the
//! screen's own input. **No new frame is needed to carry a screen**, and that is not a
//! saving: a frame carrying *cells* would have to invent a serialisation for a screen that
//! the wire already has one for, and a head would have to learn a second way to be told what
//! a program drew.
//!
//! **What a cap costs, said rather than discovered.** The log is bounded, so an attach to a
//! pane that has been drawing for hours replays its *tail* — and the tail is trimmed to the
//! next `ESC` so a cut mid-sequence cannot be painted as text. A program that drew its frame
//! before the tail began and has not repainted since would come back incomplete; a program
//! that repaints (which is what a screen program is) comes back whole, and the resize to the
//! attaching head's rectangle gives it one more reason to redraw.
//!
//! **The nudge is not the mechanism, and it was measured.** Resizing the pty to the attaching
//! head's rectangle raises `SIGWINCH` for a program that redraws on a resize — and
//! `TIOCSWINSZ` **with the same size raises nothing at all**: the kernel compares the new
//! `winsize` with the current one and returns before it signals (measured on this box,
//! `crates/tools/src/exec/term.rs`'s `a_same_size_resize_is_not_a_nudge`). So an attach from
//! the *same* head, at the same rectangle, would be nudged into nothing. **The replay is what
//! the attach is proved by**; the resize is what makes a head that switched sessions get a
//! program laid out for its own screen.
//!
//! # *Is a program running* is [`TermSession::live`], and not whether anybody closed it
//!
//! `open` used to refuse while a pane was live (it now runs a second one — see *Several
//! panes*), and the question it asked, and that `status` and the sweep still ask, is
//! [`TermSession::live`] — not `closed`. That distinction is the **ghost** the operator hit:
//! `!term mc` printed one line and exited, nothing had *closed* the pane, so `closed` was
//! false, so the daemon kept the slot — and the next `!term` in that session was refused
//! with *"a pane is already open in this session"* about a pane that had been gone for a
//! minute, with nothing on the screen to leave and no way to clear it. A program that
//! exits on its own ends the pane, and the pane now says so: `TermSession::ended` is set by
//! the reader thread before it reports, and a dead pane's slot is dropped on the next
//! `open` (and by [`Terminals::close`] and [`Terminals`]' own `Drop`).
//!
//! # Why the sink pushes through the hub and not a channel of its own
//!
//! A pane's bytes arrive on a thread this module owns, and they have to reach a socket the
//! server owns. The obvious shape — a `Sender<ServerFrame>` per pane, handed to the
//! connection — is wrong for a reason the tree has already paid for once: **one socket needs
//! one ordering**, and a second writer on it would interleave a repaint into the middle of an
//! event batch. So the sink calls [`Hub::push_frame`], which is the one door for a frame that
//! is not the record, and the seat's own pump writes it in the same order as everything else.
//!
//! **It never blocks.** `push_frame` takes the hub's lock, appends and wakes; a head that is
//! not reading has its oldest pane frame dropped rather than the pty's reader thread parked —
//! see `Hub`'s `MAX_SIDE`. That is the difference between a stale repaint and a program that
//! has stopped drawing, and only one of those is survivable.
//!
//! # The lifetime is a scope, and it is this module that opens it
//!
//! A pane's program must not outlive the pane, and the answer is the tree's own: the command
//! joins a [`ScopeKind::Session`] cgroup through `join_script` before it `exec`s, and the pane
//! ends by [`ScopeTree::end`]. **The scope is this module's and not the session's**, and that
//! is deliberate: the session's cgroup is the harness's and holds the turn's processes, so a
//! pane that ended the session's scope would kill the turn that opened it. One scope per pane,
//! named for the session it belongs to, ended when the operator closes it and again by
//! [`Terminals`]' own `Drop` — so a daemon that stops takes its panes with it.
//!
//! # Leaving is not ending, and this module is where the difference lives
//!
//! **`ctrl-\` detaches.** The head hides the rectangle, returns the conversation and **sends
//! nothing at all** — so this module is not told, the program keeps running on the pty, the
//! screen stays in [`Pane::log`], and the slot stays occupied. That is the whole point of the
//! attach work one version earlier: a pane that ends when you look away is a pane that cannot
//! hold anything you care about. The operator's words for the correction are the 34 note's:
//! *"but i dont want it to exit"*.
//!
//! **`!term close` ends it**, and that is the only act that reaches [`Terminals::close`]: the
//! head asks the operator to confirm it first (its own card, its own key — see
//! `PROTOCOL_VERSION`'s 34 section for why the two questions must not be confusable) and then
//! sends the `TermClose` that was already on the wire. A program that exits on its own is the
//! third ending, and it is nobody's act: the reader thread reports it and the slot is freed on
//! the next `open`.
//!
//! **And a head that is not drawing the pane can still ask what is running in it** —
//! [`Terminals::status`], the answer to `ClientFrame::TermStatus`. A detach is not an event, so
//! there is no row for it; the head draws the fact while it is true, which is what
//! [`TermSession::live`] decides and what this module answers with.
//!
//! **A box with no cgroups degrades and says so**: [`letibot_tools::host_tree`] failing gives
//! [`NoScopes`], `open` fails, the pane runs with no scope, and the ending falls back to
//! `SIGHUP` to the pane's process group — which reaches the program and not what it
//! daemonised away. That is [`letibot_tools::exec::term::TermSession::close`]'s own documented
//! fallback and not a second mechanism invented here.
//!
//! # What is deliberately not here
//!
//! - **TODO: the pane's bytes are not recorded anywhere.** No transcript row, no corpus entry.
//!   The operator's `!` line becomes two rows because it is a *command with a result*; a pane is
//!   the conversation's rectangle given to a program, and its repaints are not conversation. The
//!   log [`Pane::log`] keeps is **not a record** — it is the screen, capped at [`SCREEN_LOG`],
//!   never written to the store and gone when the daemon stops.
//! - **The scrollback is the head's screen's, and this log is what refills it** (R60 item 1,
//!   decided): what scrolled off is a fact only the thing that applies a line feed knows, so the
//!   ring is `letibot_vt::Screen`'s. An attach replays this log into a fresh screen, which
//!   rebuilds as much history as [`SCREEN_LOG`] holds — the cap is the scrollback's depth after
//!   an attach, and that is the cost of the cap, said here.
//!
//! # Several panes, one drawn at a time (R60 item 2)
//!
//! *"One pane per session, and no second one … Two panes on one screen is a layout question
//! this head has no answer for."* The refusal was protecting something real — a program killed
//! by the next `!term` loses its work — and the protection does not need a refusal: a second
//! `TermOpen` starts a **second** program and the first keeps running. Each pane has a number
//! ([`Session::next`], never reused in a session while this daemon lives, so a stale number
//! names nothing rather than the wrong program), every frame carries it, and a head draws one
//! pane at a time: `!term 2` changes which, `!term` alone comes back to the most recent, and
//! `!term close 2` ends one. The layout question is answered by not asking it, the way a
//! terminal multiplexer's windows answer it. The cap is [`MAX_PANES`], said as a refusal.
//!
//! # The boundary is the session's (R60 item 3)
//!
//! A pane runs inside the namespace view its session's `bash` calls get: the session's harness
//! installs a [`letibot_sessionlog::PaneBoundary`] at open — its process host's confinement,
//! read when the pane opens, so a grant made since is in it — and `open` puts that argv in front
//! of the pane's shell (`letibot_tools::exec::term`'s header has what keeps working inside:
//! `/dev/tty`, the cgroup, the workspace). An unconfined session's boundary is the empty argv,
//! which is its commands' own. **A session with no process host installs none**, and its pane
//! runs on the host as panes always did — that session runs no commands at all, so there is no
//! boundary of its commands to share; the operator's pane there is the operator's own program
//! on their own box, which is the argument `exec::term`'s `PANE_HOME` makes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{PaneBrief, ServerFrame};
use letibot_sessionlog::{Registry, TerminalDriver};
use letibot_tools::exec::{
    NoScopes, ScopeId, ScopeKind, ScopeTree, TermConfig, TermError, TermSession, TermSink,
};

/// **What the operator is told when they end the pane deliberately.**
///
/// The sentence comes from here rather than from the signal the program died of, because the
/// operator's act is the fact worth reporting: `nano` killed by `SIGKILL` reads as a crash, and
/// *"you closed the terminal"* reads as what happened. `TermSession::close` takes it for exactly
/// that reason — see its own doc.
///
/// **`closed` and not `left`, and the word is this version's correction.** `ctrl-\` used to end
/// the pane and this sentence used to say *"you left the terminal"*; leaving is now a **detach**
/// that ends nothing, so the only act that reaches this constant is the deliberate one — the
/// head's `!term close`, after it has asked the operator to confirm it (see
/// `PROTOCOL_VERSION`'s 34 section). A row saying *left* about an ending would now read as the
/// opposite of what happened.
const CLOSED: &str = "you closed the terminal";

/// **How much of what a program drew the daemon keeps.**
///
/// The cap is what makes a log a screen rather than a leak: a program redrawing as fast as it
/// likes would otherwise grow this without bound, and the only reader is an attach, which
/// wants the screen as it is *now* — which is the tail. 256 KiB is chosen against the same
/// measurement the pane's own frames were (`top` repaints about 2 KB, a 200×50 full repaint
/// about 4 KB): it is **several hundred repaints**, so the tail of any pane that is drawing
/// contains the current screen many times over, and a pane that has been idle for hours still
/// has everything it drew in the last hour of it.
///
/// See the module header for what the cap costs a program that has not repainted since before
/// the tail began, and for the trim that keeps a cut from being painted as text.
const SCREEN_LOG: usize = 256 * 1024;

/// **How many live panes one session may hold.** A cap so a stuck key or a loop of `!term`
/// lines cannot fill the box with ptys; eight is more programs than a person switches between
/// by number. The refusal names the panes, so the way out is in the sentence.
const MAX_PANES: usize = 8;

/// **One session's panes**, by number, and the bookkeeping that numbers them.
#[derive(Default)]
struct Session {
    panes: std::collections::BTreeMap<u32, Pane>,
    /// The number the next pane gets. Starts at 1 and only grows.
    next: u32,
    /// The pane last opened or attached to — what a bare `!term` comes back to.
    last: Option<u32>,
}

impl Session {
    /// **Drop every pane whose program has ended**, ending its scope — see [`Pane`] for why an
    /// ended pane is kept until here rather than dropped the instant its program exits.
    fn sweep(&mut self) {
        self.panes.retain(|_, p| p.session.live());
    }

    fn live(&self) -> Vec<(u32, &Pane)> {
        self.panes
            .iter()
            .filter(|(_, p)| p.session.live())
            .map(|(n, p)| (*n, p))
            .collect()
    }
}

/// **Every session's panes, and the pty behind each.** See the module header.
pub struct Terminals {
    /// **A `Weak` and not an `Arc`, because the registry holds this driver.** The cycle is
    /// real and would be a leak: `Registry::set_terminal` stores an `Arc<dyn TerminalDriver>`
    /// on the registry, so a driver holding an `Arc<Registry>` keeps every session's log,
    /// view and hub alive for the life of the process. Upgraded once per `open`, which is an
    /// operator's act and not a hot path.
    registry: Weak<Registry>,
    /// The cgroup tree the panes' scopes are opened in, or [`NoScopes`] on a box without
    /// cgroup v2 — see the module header for what degrades and what does not.
    tree: Arc<dyn ScopeTree>,
    panes: Mutex<HashMap<String, Session>>,
}

/// A pane: the pty session, what it is running, and **what it has drawn**.
///
/// Its scope is the session's own ([`TermSession`] holds both ends of it) so that closing the
/// pane and dropping the pane are the same act.
///
/// **The command and the log are the daemon's answer to a bare `!term`** — see the module
/// header for why the screen is the daemon's at all, and [`Terminals::attach`] for what it does
/// with the two.
///
/// **A pane whose program has ended is kept here until the next `open`**, deliberately: its
/// scope is still the cgroup that has to be ended (a program that forked something away
/// leaves that something in it — see [`TermSession::close`]), and a slot that is dropped the
/// instant a program exits would be a second path ending scopes. What must not happen is a
/// *refusal*, and that is [`TermSession::live`]'s job.
struct Pane {
    session: TermSession,
    /// The line the operator typed after the verb, as the daemon was handed it — what a head
    /// that attaches is told is running. The verb is not part of it: see
    /// [`ServerFrame::TermAttached`].
    command: String,
    /// **The last [`SCREEN_LOG`] bytes the program wrote**, shared with the reader thread's
    /// sink. See the module header.
    log: Arc<Mutex<Vec<u8>>>,
}

impl Pane {
    /// **What the program has drawn**, as one block of bytes for a head that asked.
    ///
    /// Cloned rather than lent: the caller pushes it through the hub, which takes its own lock,
    /// and a borrow held across that would be a pane's log lock held while another head's
    /// queue is walked. One clone per attach, which is an operator's act and not a hot path.
    fn screen(&self) -> Vec<u8> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Terminals {
    /// **A driver for this daemon.** `registry` is weak — see the field.
    pub fn new(registry: Weak<Registry>) -> Terminals {
        let tree: Arc<dyn ScopeTree> = match letibot_tools::host_tree() {
            Ok(c) => Arc::from(c),
            Err(e) => Arc::new(NoScopes::new(format!(
                "a pane has no cgroup to live in, so it will be ended by a signal to its own \
                 process group and not by the tree: {e}"
            ))),
        };
        Terminals {
            registry,
            tree,
            panes: Mutex::new(HashMap::new()),
        }
    }

    fn panes(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.panes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **Where this session runs.** The registry's own wiring, and the daemon's directory as
    /// the fallback — which is what a session the registry has never heard of gets, and is a
    /// starting directory rather than a report about one.
    fn workspace(&self, session_id: &str) -> PathBuf {
        let said = self
            .registry
            .upgrade()
            .map(|r| r.wiring(session_id).workspace)
            .unwrap_or_default();
        if said.trim().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(said)
        }
    }
}

/// **Where a pane's bytes go.** See the module header for why this is the hub and not a
/// channel: one socket, one ordering.
///
/// **And it is where the daemon's copy of the screen is kept**, which is the second half of
/// the same job: the bytes go up to the session's heads *and* into the pane's log, from the one
/// place they already pass through. A log written by a second reader — a thread of its own,
/// a tee on the pty — would be a second place the same bytes could be lost, delayed or
/// reordered, and the replay would then disagree with what the heads saw.
struct Sink {
    hub: Arc<Hub>,
    log: Arc<Mutex<Vec<u8>>>,
    /// The pane's number, on every frame — see the module header's *Several panes*.
    pane: u32,
}

impl TermSink for Sink {
    fn output(&self, bytes: &[u8]) {
        // **Kept first, and never allowed to block**: this runs on the pty's reader thread,
        // and the lock is held only for an append and a trim.
        if let Ok(mut log) = self.log.lock() {
            log.extend_from_slice(bytes);
            trim(&mut log);
        }
        self.hub.push_frame(ServerFrame::TermOutput {
            pane: self.pane,
            bytes: bytes.to_vec(),
        });
    }

    fn ended(&self, reason: &str) {
        self.hub.push_frame(ServerFrame::TermEnded {
            pane: Some(self.pane),
            reason: reason.to_string(),
        });
    }
}

/// **Keep the tail of what a program drew, and start it where a parser can read it.**
///
/// A cut lands wherever the program happened to be — inside an escape sequence, most of the
/// time — and the head that receives the tail parses it from its ground state, so `[2J` would
/// be painted as three characters at the cursor. Dropping to the next `ESC` costs at most the
/// escape-free run before it, which a repainting program replaces on its next frame. **A tail
/// with no `ESC` in it at all is left alone**: that is a program writing text (`!term cat
/// file`), and text is what it wrote.
fn trim(log: &mut Vec<u8>) {
    if log.len() <= SCREEN_LOG {
        return;
    }
    let over = log.len() - SCREEN_LOG;
    log.drain(..over);
    if let Some(at) = log.iter().position(|b| *b == 0x1b) {
        log.drain(..at);
    }
}

impl TerminalDriver for Terminals {
    fn open(
        &self,
        session_id: &str,
        hub: &Arc<Hub>,
        command: &str,
        cols: usize,
        rows: usize,
    ) -> Result<u32, String> {
        let mut all = self.panes();
        let session = all.entry(session_id.to_string()).or_default();
        // **Panes whose programs have ended leave nothing to keep**, and this is where their
        // slots are freed: dropping one ends its scope (which the cgroup has usually already
        // emptied). `live` and not `closed` — the module header's *the ghost*.
        session.sweep();
        // **A live pane is no longer a refusal** (R60 item 2): the new program runs beside it.
        // What is refused is a session that already has [`MAX_PANES`], by name, with the
        // numbers that would free one.
        if session.panes.len() >= MAX_PANES {
            let held: Vec<String> = session
                .live()
                .iter()
                .map(|(n, p)| format!("{n} `{}`", p.command))
                .collect();
            return Err(format!(
                "this session already has {MAX_PANES} panes running ({}), so `{command}` was \
                 not started. `!term close N` ends one.",
                held.join(", ")
            ));
        }

        let cwd = self.workspace(session_id);
        let env = letibot_tools::exec::term::env();
        // **The session's boundary, before anything is opened** (R60 item 3). A boundary that
        // refuses — a confined session whose view is missing, a workspace outside its own view
        // — refuses the pane with its own sentence, and nothing has been started that would
        // then have to be ended. A session that installed none runs its pane on the host: see
        // the module header for which sessions those are and why.
        let boundary = self
            .registry
            .upgrade()
            .and_then(|r| r.pane_boundary(session_id));
        let confine = match &boundary {
            None => Vec::new(),
            Some(wrap) => {
                wrap(&cwd, &letibot_tools::exec::term::confined_env(&env)).map_err(|why| {
                    format!(
                        "`{command}` was not started: this session's boundary refused it — {why}"
                    )
                })?
            }
        };

        // **The scope, before the process.** `join_script` writes the pid into `cgroup.procs`
        // and *then* `exec`s, so there is no window in which the program or its children could
        // be forked outside it. A box with no cgroups fails here and the pane runs unscoped —
        // which `TermSession::close` names as its own fallback rather than pretending to own
        // what it cannot reach.
        session.next = session.next.max(1);
        let pane = session.next;
        let scope: Option<ScopeId> = self
            .tree
            .open(
                ScopeKind::Session,
                &format!("term-{session_id}-{pane}"),
                None,
            )
            .ok();

        let cfg = TermConfig {
            command: command.to_string(),
            // The pane's environment, and **not** the capture's: `console::env_from` forces
            // `PAGER=cat` because a capture has nobody at the keyboard, and a pane has
            // somebody. See `letibot_tools::exec::term::env_from`.
            env,
            cwd,
            cols,
            rows,
            scope,
            tree: Some(self.tree.clone()),
            confine,
        };
        // **The log is made before the program is**, because the reader thread starts writing
        // into it the moment the pty has a byte — and because the pane that is inserted below
        // and the sink the thread holds must be the *same* `Arc`.
        let log: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(Sink {
            hub: hub.clone(),
            log: Arc::clone(&log),
            pane,
        });
        // **The number, before the first byte.** Pushed on the same hub the bytes go through,
        // so no head can see a `TermOutput` for this pane before it knows the pane exists.
        hub.push_frame(ServerFrame::TermAttached {
            pane,
            command: command.to_string(),
            opened: true,
        });
        let started = TermSession::start(&cfg, sink).map_err(|e: TermError| e.to_string());
        let started = match started {
            Ok(s) => s,
            Err(why) => {
                // The head was told the number; it is told the ending too, under the same one.
                hub.push_frame(ServerFrame::TermEnded {
                    pane: Some(pane),
                    reason: why.clone(),
                });
                return Err(why);
            }
        };
        session.next = pane + 1;
        session.last = Some(pane);
        session.panes.insert(
            pane,
            Pane {
                session: started,
                command: command.to_string(),
                log,
            },
        );
        Ok(pane)
    }

    /// **Give this head a pane the session already has** — a bare `!term`, or `!term N`.
    ///
    /// Three things, in this order, and each of them is a decision:
    ///
    /// 1. **The attaching head's rectangle.** `TermSession::resize` is `TIOCSWINSZ`, so a
    ///    program that lays out for the screen it is drawn in gets the *new* one — a head that
    ///    switched sessions has a different rectangle from the one the pane opened at. It is
    ///    **not** the mechanism the attach is proved by: `TIOCSWINSZ` with an unchanged size
    ///    raises no `SIGWINCH` at all (the kernel compares before it signals — see
    ///    `letibot_tools::exec::term::tests::a_same_size_resize_is_not_a_nudge`), so for the
    ///    same head at the same size this is a no-op.
    /// 2. **What is running**, as [`ServerFrame::TermAttached`] — *before* the bytes, because a
    ///    head that drew the screen first and learned what it was afterwards would flash a
    ///    rectangle it could not name.
    /// 3. **The screen**: the whole log as **one** [`ServerFrame::TermOutput`]. One frame and
    ///    not several, and that is not tidiness — `Hub::push_frame` drops the oldest pane frame
    ///    for a head that is behind, so a replay split across frames could be delivered in part
    ///    and a *partial* screen is a corrupt screen where no screen is an honest empty one.
    ///
    /// **Which pane**: the one named, or — for a bare `!term` — the one last opened or attached
    /// to, and failing that the newest live one.
    ///
    /// **A session with no pane is a sentence, not a silence.** The head opened the rectangle
    /// the moment the operator pressed enter (it cannot wait for the daemon without showing the
    /// transcript for as long as the round trip takes), so *there is nothing to attach to* has
    /// to arrive as the `TermEnded` that closes it — the same frame a refusal uses, for the same
    /// reason: the head's act is identical either way.
    fn attach(
        &self,
        session_id: &str,
        pane: Option<u32>,
        hub: &Arc<Hub>,
        cols: usize,
        rows: usize,
    ) -> Result<u32, String> {
        let mut all = self.panes();
        let Some(session) = all.get_mut(session_id).filter(|s| !s.live().is_empty()) else {
            return Err(
                "this session has no pane to attach to — `!term COMMAND` starts one. Nothing \
                 was attached."
                    .to_string(),
            );
        };
        let live: Vec<u32> = session.live().iter().map(|(n, _)| *n).collect();
        let n = match pane {
            Some(n) if live.contains(&n) => n,
            Some(n) => {
                return Err(format!(
                    "there is no pane {n} running in this session — the live ones are {}. \
                     Nothing was attached.",
                    live.iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            None => session
                .last
                .filter(|n| live.contains(n))
                .unwrap_or(*live.last().expect("checked non-empty")),
        };
        session.last = Some(n);
        let p = &session.panes[&n];
        p.session.resize(cols, rows);
        hub.push_frame(ServerFrame::TermAttached {
            pane: n,
            command: p.command.clone(),
            opened: false,
        });
        let screen = p.screen();
        if !screen.is_empty() {
            hub.push_frame(ServerFrame::TermOutput {
                pane: n,
                bytes: screen,
            });
        }
        Ok(n)
    }

    /// **What this session's panes are running** — the read behind a head's own line about
    /// programs it is not drawing.
    ///
    /// **`live` and not merely *present*, exactly as `open`'s sweep is.** A pane whose program
    /// has exited is kept until the next `open` (its scope is still the cgroup that has to be
    /// ended — see [`Pane`]), and reporting it as *running* would put a head's *a pane is
    /// running `!term mc`* line on the screen for a program that has been gone for a minute.
    ///
    /// The command is the daemon's own string, the one it was handed at `open` — the same one
    /// [`TerminalDriver::attach`] puts on [`ServerFrame::TermAttached`], so the two answers
    /// cannot disagree.
    fn status(&self, session_id: &str) -> Vec<PaneBrief> {
        self.panes()
            .get(session_id)
            .map(|s| {
                s.live()
                    .into_iter()
                    .map(|(pane, p)| PaneBrief {
                        pane,
                        command: p.command.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn input(&self, session_id: &str, pane: u32, bytes: &[u8]) -> Result<(), String> {
        match self
            .panes()
            .get(session_id)
            .and_then(|s| s.panes.get(&pane))
        {
            Some(p) => p.session.input(bytes).map_err(|e| e.to_string()),
            // No pane: a keystroke nobody is listening for. Quiet, like `close` — a key
            // that arrived a moment after the pane ended is not an error anybody can act on.
            None => Ok(()),
        }
    }

    fn resize(&self, session_id: &str, pane: u32, cols: usize, rows: usize) -> Result<(), String> {
        if let Some(p) = self
            .panes()
            .get(session_id)
            .and_then(|s| s.panes.get(&pane))
        {
            p.session.resize(cols, rows);
        }
        Ok(())
    }

    fn close(&self, session_id: &str, pane: u32) -> Result<(), String> {
        let taken = self
            .panes()
            .get_mut(session_id)
            .and_then(|s| s.panes.remove(&pane));
        if let Some(mut p) = taken {
            // **The operator's sentence, set before the kill.** The pty's reader thread
            // composes the `TermEnded` the head will show, and it reads this — so the pane
            // that was closed says *"you closed the terminal"* rather than the signal number
            // the program died of. See `TermSession::close`.
            //
            // **This is the deliberate act, and the only one that reaches here.** `ctrl-\`
            // detaches and sends nothing at all; the frame that lands on this method is sent
            // by a head whose `!term close` the operator confirmed.
            p.session.close(CLOSED);
        }
        Ok(())
    }
}

impl Drop for Terminals {
    /// **A daemon that stops takes its panes with it.** Every live pane is closed, which is
    /// `ScopeTree::end` on its cgroup — the same act a confirmed `!term close` performs, and for
    /// the same reason: a screen program left running with no pane to draw in is a process
    /// nobody can see and nobody can end.
    fn drop(&mut self) {
        let mut all = self.panes();
        for (_, session) in all.drain() {
            for (_, mut p) in session.panes {
                p.session.close("the daemon stopped");
            }
        }
    }
}
