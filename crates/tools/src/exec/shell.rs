//! **One long-lived shell per session, with a terminal, whose state is the shell's.**
//!
//! # The requirement, in the operator's words
//!
//! *"can we provide some sort of viewport? say if i run `mc` the conversation window replaced
//! by mc but prompt area stays"*, and then the sentence that decides what this module is:
//! *"we want to fully integrate with the underlying shell the head runs one. i want to migrate
//! my shells to this environment."*
//!
//! So the viewport is not a per-command pty that happens to be drawn in a pane. It is the
//! first slice of a **shell session the daemon owns for this conversation**: one pty, opened
//! lazily, an interactive shell on the far end of it, and a `!` line that is a line *typed at
//! that shell* rather than a `/bin/sh -c` started for it.
//!
//! # The three things this buys, in the order they matter
//!
//! 1. **State persists.** `! cd /x` and then `! ls` lists `/x`. So do `export`, shell
//!    functions, aliases, history and background jobs — because they are the shell's state
//!    and the shell is still running. Today every `!` line is a fresh `/bin/sh -c` (or
//!    `bash -ic`) with nothing carried over but the workspace, and `cd` in one line is
//!    discarded before the next one is read.
//! 2. **A program that owns a screen can be run.** `mc`, `top`, `nano` are refused by name on
//!    the `!` line — [`super::terminal`] says why: the pty is a *capture*, its output is
//!    folded into one transcript row and its input is `/dev/null`, so a screen program draws
//!    cursor-addressing escapes into a row and waits for a keystroke that cannot arrive. A
//!    session with a terminal and a pane to draw it in is the fix that refusal names.
//! 3. **The shell becomes the authority for the facts it owns.** See below — this is the part
//!    that is easy to get subtly wrong, and the wrong version looks right on the screen.
//!
//! # Where the truth lives, and it is not the daemon
//!
//! **Once this session exists, the shell is the authority for the working directory and for
//! the environment.** The daemon has a `cwd` too — the workspace it was started for — and that
//! value is a *startup* fact: it is where the session began, not where it is. After the first
//! `cd` the two are different, and anything that reports the daemon's copy is reporting a
//! directory the operator left.
//!
//! So there is exactly one reader for each of the two, and this module is it:
//!
//! | the fact | who answers | how |
//! |---|---|---|
//! | the working directory | **the shell** | `$PWD` in the trailer — see [`Turn::cwd`] |
//! | the environment | **the shell** | `$VAR` is expanded by the shell, so a line that reads one reads the session's |
//! | the workspace root | the daemon | it is the confine root and the `--cwd` a session was opened with; the shell does not own it |
//! | the exit status | **the shell** | `$?` in the trailer — see [`Turn::status`] |
//!
//! **What that means for a caller, said as a rule rather than as advice: a caller may not
//! report a cwd it remembered.** `ShellSession::cwd` is the last answer the *shell* gave, and
//! a caller that has never run a line has no answer at all — it has a starting directory,
//! which is a different thing and is [`ShellConfig::cwd`]. The head's header, a tool that
//! prints where we are, and the `!` line's own context all read the one field, and none of
//! them keeps a second copy. This is the same defect as `SubagentState::is_finished` — two
//! readers of one fact — with the extra sharpness that here the two readers disagree the
//! moment anybody types `cd`.
//!
//! # The framing problem, which is the hard part
//!
//! A pty is a byte stream with no message boundaries. To read *one command's output* back you
//! have to know where the output ends, and there are two ways to think about that:
//!
//! * **Guess from the prompt.** Watch for the shell's prompt to come back. This is what every
//!   expect-style tool does, and it is wrong for a reason that has nothing to do with
//!   cleverness: **a prompt is not a protocol.** It is a string the operator's rc chooses and
//!   can change at any moment (`PS1`, `PROMPT_COMMAND`, a git-status prompt, a prompt with a
//!   right-hand timestamp); it can appear inside a command's own output (`cat` a shell script
//!   and you have read a prompt); and an interactive shell prints one *before* it reads each
//!   line, so the number of prompts is not the number of commands.
//! * **Write a sentinel.** Append a marked trailer after the line and read until the trailer
//!   arrives. The marker is ours, it is written by the shell as a side effect of a line *we*
//!   composed, and it carries the two facts we need.
//!
//! This module does the second. The line written for `run("ls")` is two lines:
//!
//! ```text
//!   ls
//!   __lt_end_<nonce>
//! ```
//!
//! and `__lt_end_<nonce>` is a shell function defined once at session start:
//!
//! ```text
//!   __lt_end_<nonce>() {
//!       printf '\033]1337;letibot;%s;%s;%s\007' "$LETIBOT_MARK" "$?" "$PWD"
//!   }
//! ```
//!
//! The output of the call is **one OSC sequence** — `ESC ] 1337 ; letibot ; <nonce> ; <status>
//! ; <cwd> BEL` — and four properties fall out of that choice rather than being arranged:
//!
//! 1. **It cannot be forged by the command.** The nonce is in the function's own *name* and in
//!    `$LETIBOT_MARK`, and neither is in the echoed text the shell sends back before the
//!    command runs. A command that prints `$LETIBOT_MARK` prints the nonce and not the
//!    `ESC ] 1337 ; letibot ;` introducer, so the scan for a trailer cannot be spoofed by
//!    `printf`.
//! 2. **`$?` is the shell's own answer**, taken at the top of the function body where it is
//!    still the status of the line we sent — the shell reporting on itself, which is the
//!    authority rule above applied to the exit status.
//! 3. **`$PWD` rides in the same trailer**, so the cwd is refreshed by the same mechanism as
//!    the status: one trailer, one read, and no second question that could be answered at a
//!    different moment. A `pwd` sent as a separate line would be a *second* command, and its
//!    answer could belong to a directory the first command had already left.
//! 4. **The marker is invisible to a screen.** It is an OSC sequence, and
//!    [`letibot_vt::Screen`] — the pane's renderer — consumes OSC sequences whole and
//!    drops them. So one stream has two readers with no negotiation between them: the parser
//!    stops at the trailer, and the pane draws the same bytes as a terminal would, with the
//!    trailer invisible because a terminal would not draw it either.
//!
//! ## What the framing cannot do, said now rather than discovered
//!
//! * **It frames a *command*, not a *session*.** A full-screen program — `mc`, `top`, `vim` —
//!    never returns to the shell, so its trailer never arrives and [`ShellSession::run`] waits
//!    out its deadline. The pane's live view of such a program needs the streaming path
//!    (bytes forwarded as they arrive, the trailer only when the line *ends*), which is a
//!    named TODO below and is why the refusal in [`super::terminal`] is still in place.
//! * **A timeout desynchronises the session.** The trailer for the line that timed out is
//!    still coming, so the *next* `run` would read it and attribute it to the wrong line.
//!    [`ShellSession::run`] therefore refuses after a timeout rather than answering wrongly,
//!    and [`ShellSession::resync`] is the way back. A wrong answer here would look exactly
//!    like a right one.
//! * **The echo of the line is part of the output**, and that is deliberate: a terminal shows
//!    what was typed at it, and the pane is a terminal. Only the echo of the *trailer's* line
//!    is removed, because that line is this module's machinery and not something anybody
//!    typed — see [`strip_echo`].
//!
//! # The daemon owns the process, and the vocabulary is tonight's
//!
//! A shell that outlives the thing that started it is a leak, and this tree already has the
//! answer rather than needing a second one: [`super::scope`]. The session is opened **inside a
//! [`ScopeKind::Session`] cgroup**, through [`super::scope::join_script`] — the wrapper writes
//! its own pid into `cgroup.procs` and *then* `exec`s, so there is no window in which the shell
//! or any of its children could be forked outside the scope — and the session ends by
//! `ScopeTree::end`, which kills the tree and records what it killed ([`Reaping`]: presence
//! first, then absence, which is `docs/closed-loop.md` §3's ordering).
//!
//! **That is why a background job started in this shell dies with the conversation** and not
//! with the turn: the cgroup is the session's, so `sleep 300 &` is a member of it, and there
//! is no `pgrep` anywhere in the story. **Nothing here invents a lifetime**: no pid file, no
//! `pkill`, no signal to a remembered number. The one fallback — a session opened with no
//! scope at all, which is what a unit test does — is named in [`ShellSession::close`] and says
//! what it is.
//!
//! **What this module does not own is the registry.** *One shell per session, started lazily*
//! is a fact about the daemon's map from a session id to its live things, and that map does not
//! exist yet — see the TODOs. This module is the mechanism: open, run, resize, close. A caller
//! that opens two of them has two shells, which is correct and is not the policy.
//!
//! # The controlling terminal, which the row path declined and this path wants
//!
//! [`super::pty`] records the decision that an operator's `!` run gets a pty that is **not** its
//! controlling terminal: `setsid` plus `TIOCSCTTY` was rejected there because it makes
//! `/dev/tty` *openable*, and a program reached **indirectly** — git's editor, `gpg`'s
//! pinentry — would then wait for a keystroke that cannot arrive, which is the hang the sibling
//! `bang-term` branch closed.
//!
//! **That hazard does not exist here, and the reason is the whole difference between the two
//! paths.** In a pane, keystrokes arrive: the head forwards the operator's keys down this pty,
//! and a program that opens `/dev/tty` finds a person at the other end. So the child is given a
//! **new session** ([`libc::setsid`]) and the pty as its **controlling terminal**
//! ([`libc::TIOCSCTTY`]), which is also what makes job control work — `Ctrl-Z`, `fg`, `bg`, and
//! the shell's own `[1]+ Stopped` messages — and what silences bash's two lines about job
//! control that a capture's terminal produces on every row today.
//!
//! # The two rules this feature must not break
//!
//! **1. The model may propose a line but may never run one.** A `run` here executes a line. The
//! line that reaches it is one a person typed: the `!` line at the composer, or a line the head
//! forwards from the pane's keyboard. The model's proposals travel a different path entirely —
//! `ClientFrame::SuggestShell` returns *candidates*, only Tab fills the composer, Enter is the
//! operator's, and the frame that submits is `ClientFrame::OperatorShell` with a line a person
//! pressed Enter on. So this module's rule is that **its caller must already hold a submitted
//! line**; it has no API that takes a model's turn, no timer, and no way to be reached from the
//! suggestion channel. The version-31 frame that carries a line here is `ShellLine`, and its
//! own docs say the same thing at the wire.
//!
//! **2. A secret stays a person's card.** The secret card owns the keyboard ahead of everything
//! else in the head, so nothing can be typed while it is up; but the sharper half is here,
//! because a shell is a place a secret could *land*: this module never takes a secret, never
//! puts one in a line, and — the part that matters — **the trailer carries `$PWD` and `$?` and
//! nothing else**. No environment dump, no `env` at session start, no `set -x`. A session whose
//! state the head displays by *asking the shell for a value* would be a session that prints
//! whatever that value is into a transcript, and a `sudo` password exported into the
//! environment is exactly such a value. The cwd and the status are the two facts a head may
//! read back, and they are the two that cannot be a credential.
//!
//! # The protocol addition it needs: `PROTOCOL_VERSION` 30 → 31
//!
//! A session the daemon owns and a head draws is a conversation the wire has to carry, and
//! the wire has no frames for it. The ladder in `crates/sessionlog/src/protocol.rs` ends at
//! *30: the model proposes `!` completions*; this is 31, and it is four frames rather than
//! one bump each, the way 23 and 29 each carried a whole feature at one version.
//!
//! | direction | frame | what it is |
//! |---|---|---|
//! | down | `ClientFrame::ShellLine { line }` | **a submitted line.** The operator's `!` line, or a line the pane's own keyboard composed — and never a model's proposal, see the first rule above |
//! | up | `ServerFrame::ShellTurn { bytes, status, cwd }` | the answer: this module's [`Turn`], verbatim |
//! | down | `ClientFrame::ShellResize { cols, rows }` | the pane's rectangle, because **the head is the half that knows it** and the daemon has no screen |
//! | up | `ServerFrame::ShellEnded { reason }` | the session is over — the shell exited, or the daemon closed it. Not an answer to a line, and the reason it is its own frame |
//!
//! **The pair is `ShellLine`/`ShellTurn`**, and it is the same argument
//! `ClientFrame::OperatorCall`/`OperatorResult` makes one version earlier: a line has to be
//! admitted *before* it runs and its result has to be published *after*, one frame can carry
//! one of those, so one frame buys either a line nobody recorded or a conversation that does
//! not contain what it produced. The other two are not padding: a resize is not a line (it
//! has no exit status and produces no output) and an ending is not an answer (it arrives
//! minutes after the last line, or with none in flight).
//!
//! **Why a version bump and not a defaulted field.** `ShellLine` is a new `ClientFrame`, so a
//! version-30 daemon fails to parse it — the version-4 argument, and the same ATTACH-time
//! refusal, which is a clean `Bye` rather than a mid-session deserialization failure that
//! hangs the connection in silence. `ShellTurn` and `ShellEnded` are new `ServerFrame`s and
//! the version-25 argument applies in the other direction: a head with no arm for one would
//! fail to decode it mid-session. One bump covers all four, and
//! `every_frame_is_accounted_for_at_this_version` is the test that will not compile until the
//! arms are written.
//!
//! **Why not `ClientFrame::OperatorShell`, which already carries a typed line.** Because that
//! frame's daemon half is a *different act*: it runs the line through the `bash` tool path —
//! a fresh process, a confine plan, a transcript row, byte caps and a spill file — and its
//! answer is a row the model reads. This one writes a line at a session that is already
//! running, and its answer is a screen's worth of bytes for a person to look at. Widening
//! `OperatorShell` to mean both would make one frame's meaning depend on whether a session
//! happened to be open, which is the kind of conditional this protocol does not have anywhere
//! else.
//!
//! **The frames are named here and are NOT added to `protocol.rs` by this branch.** A frame
//! the daemon has no arm for is a head sending into a void, and the version bump is a
//! *refusal* — bumping it without the daemon's half would make two correct installs refuse
//! each other for a feature neither of them has. The bump belongs with the registry (the
//! first TODO below), and this section is what that branch will be written against.
//!
//! # What is deliberately NOT built here
//!
//! - **TODO: the head's pane, and the frame branch that would draw it.** The renderer exists
//!   and is tested — `letibot_vt::Screen` consumes this module's bytes and
//!   `letibot_ui::ansi::pane_rows(screen, cols, room, palette)` returns **exactly `room` rows**,
//!   which is the whole of the row budget: the pane takes the conversation's rectangle and gives
//!   it back, so the composer, the status row and the header keep the rows they had and nothing
//!   above the pane moves when it opens. **The head's half exists now, for the SCREEN case** —
//!   `!term`, `ClientFrame::TermOpen`/`TermInput`/`TermResize`/`TermClose` and
//!   `ServerFrame::TermOutput`/`TermEnded`, with [`super::term`] on the daemon's side and the
//!   pane in `letibot-tui`. What is still not built is the same pane fed by a *line*: this
//!   module's `Turn` is one frame per line and needs no screen to draw, so the branch that
//!   would draw one waits on the streaming TODO above and on nothing else.
//! - **TODO: the daemon's registry.** `harnessd` holds a session's live things; this is not one
//!   of them yet. The decision it waits on is *when a session's shell is started* — lazily on
//!   the first `!` line, or eagerly when the session opens — and what `Hello` tells a head that
//!   attaches to a session whose shell is already running (the cwd, and whether the shell is
//!   alive, both of which a second head would otherwise have to guess).
//! - **TODO: the input path.** Keystrokes down, and one unambiguous way out. **A sibling has
//!   built both for the screen case and this one still has neither** — see [`super::term`]: the
//!   pane's keys travel as bytes on `ClientFrame::TermInput`, and its way out is `ctrl-\`,
//!   intercepted by the head before a byte is forwarded. What is *not* reusable as it stands is
//!   the byte stream itself: this module's traffic is a line, and a keystroke typed at a shell
//!   the daemon keeps is a line's worth of bytes only once Enter arrives. The decision it waits
//!   on is whether a line at a pane's shell is `ShellLine`'s (one frame, one turn) or the
//!   pane's (bytes in, bytes out) — and the two are different enough that this module has not
//!   answered it by borrowing.
//! - **TODO: an answer to a program that asks the terminal a question.** `CSI 6n` (report cursor
//!   position), `CSI 5n` and `CSI c` (device attributes) are *received and dropped* —
//!   `letibot_vt::Screen` consumes them whole and has no output path by design — so a program
//!   that waits for a report **waits**. **This is the one gap in the pane that can look like a
//!   hang rather than like a missing feature**, and the fix is here rather than there: the pane
//!   is the half that owns the write path, and it is the only half that can answer.
//!
//!   It is two halves and both are named, because either one alone does nothing. The screen has
//!   to **report** that it saw the question — `letibot_vt::parser` already builds the `Csi` and
//!   `Screen::feed` already reads its final byte, so what is missing is a way *out* of the walk
//!   (a queued question the caller drains after each `feed`, rather than a callback: the screen
//!   is fed from a read loop that must not block on a write). And this module has to write the
//!   **reply** down the pty — `ESC [ row ; col R` for `6n`, and a device-attributes answer for
//!   `c` — which is the same `write` path `run` already uses for a line, and needs no new frame
//!   on the wire.
//!
//!   **Which programs ask is a fact about the program and not about this crate.** `mc` does not,
//!   which is why the pane's first slice is not blocked on this; the ones that do are the ones
//!   that will look like a hang, and a report that is never answered is the worst way for a
//!   pane to fail — no error, no exit status, just a program that has stopped.
//! - **TODO: streaming.** `run` returns when the trailer arrives, so a screen program is a
//!   deadline rather than a view. **The screen case is built** ([`super::term`] streams both
//!   ways and the pane draws it); what is not is streaming for a *line*, which is the case this
//!   module owns. The decision it waits on is whether the daemon forwards bytes as they arrive
//!   (a `ServerFrame::ShellBytes` per read, with the pane live) and how the trailer is then
//!   delivered — as its own frame, or as a field on the last one.
//! - **TODO: `SIGWINCH`, and who sends it.** `resize` sets the pty's size and the kernel raises
//!   `SIGWINCH` for the foreground process group by itself; what is not built is the *path*
//!   from the head's terminal size to this call, and whether the daemon or the head is the one
//!   that knows the pane's rectangle. (It is the head: the rectangle is the frame's, and the
//!   daemon has no screen. So the frame carries it — a field on the version-31 input frame.)
//! - **TODO: scrollback.** The pane draws the screen; a program that scrolls *off* it is gone.
//!   The decision it waits on is whether the scrollback is the VT screen's (a bounded ring of
//!   scrolled-off rows, which is a rendering question) or the daemon's (a byte log, which is a
//!   storage question) — and it is not the transcript's, because a screen's repaints are not
//!   conversation.
//! - **TODO: the shell is assumed to be bash.** `console::shell()` is `/bin/bash -ic` and the
//!   trailer's function syntax is bash's and zsh's. `$SHELL` is where a console says otherwise,
//!   and reading it needs the decision [`super::console`] already filed (flags differ per
//!   shell: `-ic` is not fish's, and fish has no `name() { … }`).
//! - **TODO: a line that is not a line.** A multi-line paste is several commands to the shell
//!   and one turn to this module, so `$?` is the last one's. A heredoc read from the pty waits
//!   for its terminator and will hit the deadline. Both are honest today and both want the
//!   same answer as streaming.
//!
//! # Provenance
//!
//! The sentinel technique is not from either project this tree's `NOTICE` credits and no
//! expect-like tool was read for it; the shape — a marked trailer carrying `$?`, and reading
//! until it — is the standard one, and the reasons above are why this tree takes it rather
//! than watching for a prompt. The lifetime mechanism is [`super::scope`]'s and is reused
//! rather than copied.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Pty;
use super::scope::{Cgroup2, Reaping, ScopeId, ScopeTree, join_script};

/// **The OSC introducer every trailer starts with.**
///
/// `OSC 1337` is the range terminals reserve for private use, and the name inside it is this
/// tree's. Nothing about it is a terminal feature: the bytes are chosen because they are a
/// sequence a screen consumes whole and a parser can find, and because a program cannot
/// produce one by accident.
const MARK_OPEN: &str = "\x1b]1337;letibot;";

/// What ends a trailer — BEL, the short spelling of an OSC's terminator.
const MARK_CLOSE: u8 = 0x07;

/// How long a session waits for the shell to come up and answer its first trailer.
pub const READY: Duration = Duration::from_secs(10);

/// How long a line gets before the session is called desynchronised. The caller may override
/// it per line; this is what a `!` line uses, and it is the same order as the 300 s budget a
/// tool call gets because a build is a command a person typed.
pub const WAIT: Duration = Duration::from_secs(300);

/// How many bytes of shell output one turn may accumulate before it is called a runaway.
///
/// Not a cap on what a command may print — that is the transcript's business and a `!` line
/// has its own byte caps — but a bound on what this module will hold in memory while it scans
/// for a trailer that may never come. A turn that hits it is a desynchronised session, which
/// is the same ending as a deadline and is answered the same way.
const MAX_TURN: usize = 32 * 1024 * 1024;

/// **Why a session did not answer.** Small and local on purpose: this is not an
/// [`ExecError`], because nothing here is a tool call and a caller should not have to read a
/// variant that talks about commands and gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// The shell is not there — it exited, or the pty's far end closed.
    Gone(String),
    /// The trailer did not arrive inside the deadline. **The session is desynchronised**: the
    /// trailer for this line is still coming, and the next `run` would read it and call it
    /// the next line's answer.
    Timeout(Duration),
    /// A previous timeout, or a runaway turn, left a trailer in flight.
    Desynchronised(String),
    /// The turn grew past [`MAX_TURN`] with no trailer in it.
    Runaway(usize),
    /// The session could not be started, or the scope could not be joined.
    Start(String),
    /// The caller asked for something a session cannot do.
    Config(String),
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellError::Gone(w) => write!(f, "the shell session is gone: {w}"),
            ShellError::Timeout(d) => write!(
                f,
                "the shell did not reach its trailer within {d:?}; the session is \
                 desynchronised and must be resynced or replaced"
            ),
            ShellError::Desynchronised(w) => write!(f, "the shell session is desynchronised: {w}"),
            ShellError::Runaway(n) => write!(
                f,
                "the shell wrote {n} bytes with no trailer in them; the session is \
                 desynchronised"
            ),
            ShellError::Start(w) => write!(f, "the shell session could not be started: {w}"),
            ShellError::Config(w) => write!(f, "{w}"),
        }
    }
}

impl std::error::Error for ShellError {}

/// **How to start a session.** Everything a caller must decide, in one place, with the
/// defaults a `!` line wants.
///
/// `Clone` and not `Debug`: the scope tree is a trait object that carries no `Debug`, and a
/// session's own [`ShellSession`] prints what a reader needs anyway.
#[derive(Clone)]
pub struct ShellConfig {
    /// The shell and its flags. [`super::console::shell`] is the operator's — `/bin/bash -ic`
    /// — and the tests pass `--norc` so an assertion is about this module and not about
    /// whatever the operator's rc prints.
    pub shell: Vec<String>,
    /// Pairs put in front of the shell's environment. The daemon's own environment is the
    /// console's (`super::console`'s argument), so this is what a *session* adds — `TERM`
    /// above all, because a shell on a pty without one is a shell that cannot colourise.
    pub env: Vec<(String, String)>,
    /// Where the session starts. **A startup fact, not a report** — see the module header:
    /// once the shell is running, [`ShellSession::cwd`] is the answer and this is history.
    pub cwd: PathBuf,
    /// The pty's size, in columns and rows. The head's pane rectangle at the moment of
    /// opening; [`ShellSession::resize`] moves it after that.
    pub cols: usize,
    pub rows: usize,
    /// **The scope the session lives in**, and the tree that will end it. `None` is a session
    /// nobody owns — a test's, and named as such by [`ShellSession::close`].
    pub scope: Option<ScopeId>,
    pub tree: Option<Arc<dyn ScopeTree>>,
    /// How long the first trailer gets.
    pub ready: Duration,
}

impl Default for ShellConfig {
    fn default() -> ShellConfig {
        ShellConfig {
            shell: super::console::shell(),
            env: Vec::new(),
            cwd: PathBuf::from("."),
            cols: 80,
            rows: 24,
            scope: None,
            tree: None,
            ready: READY,
        }
    }
}

/// **One line's answer**, and the two facts the shell is the authority for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// Everything the shell wrote between the previous trailer and this one, with the echo of
    /// the trailer's own line removed. **This is the pane's feed** — raw bytes, escapes and
    /// all, because a screen is what consumes them.
    pub bytes: Vec<u8>,
    /// `$?`, read at the top of the trailer's function body, where it is still the status of
    /// the line that was sent.
    pub status: i32,
    /// `$PWD`, the shell's own answer. See the module header: **a caller may not report a cwd
    /// it remembered.**
    pub cwd: String,
}

/// A nonce, unique per session on this box. The pid makes two sessions on one host differ; the
/// clock makes two sessions in one process differ; the counter makes two sessions in one
/// nanosecond differ, which is what a test does.
fn nonce() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{:x}{:x}{:x}",
        std::process::id(),
        t,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// **The name of the function that writes the trailer**, which carries the nonce.
///
/// In the *name* and not only in the environment, because the shell echoes the line we send
/// before the command runs: an echo carrying `$LETIBOT_MARK` unexpanded is harmless, and a
/// name that is unique per session is what makes [`strip_echo`] exact rather than a guess.
fn end_fn(mark: &str) -> String {
    format!("__lt_end_{mark}")
}

/// The line written once, at session start, that defines the trailer's function.
///
/// `$?` is read at the top of the body, where it is still the status of the line before the
/// call — a function call does not disturb it. `\033` and `\007` are octal because that is the
/// spelling every shell's `printf` accepts.
pub fn trailer_definition(fn_name: &str) -> String {
    format!(
        "{fn_name}() {{ printf '\\033]1337;letibot;%s;%s;%s\\007' \
         \"$LETIBOT_MARK\" \"$?\" \"$PWD\"; }}"
    )
}

/// **What to run, and with what.** Pure, so the scope wrapper can be asserted without a
/// process starting — the same discipline [`super::scope`]'s own tests keep.
///
/// With no scope this is the shell and its flags. With a scope it is `/bin/sh -c <join_script>
/// …` in front of them, so that **the pid we hold is a member of the cgroup** and the shell
/// `exec`s into that same pid: a session that could not join its scope is not run, which is
/// [`join_script`]'s own `exit 125` and its own sentence.
pub fn argv(cfg: &ShellConfig) -> Result<(String, Vec<String>), ShellError> {
    let mut it = cfg.shell.iter().cloned();
    let prog = it
        .next()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ShellError::Config("the shell is empty".to_string()))?;
    let rest: Vec<String> = it.collect();
    match &cfg.scope {
        None => Ok((prog, rest)),
        Some(scope) => {
            let mut args = vec![
                "-c".to_string(),
                join_script().to_string(),
                // `$0` for the wrapper, which is only ever a name in a message.
                "letibot-shell".to_string(),
                Cgroup2::procs_path(scope).display().to_string(),
                // `$2` is the join token the host uses as *evidence* it can observe. This
                // module does not need it — it has the trailer, which is an answer rather
                // than a token — so the wrapper's `: > "$2"` is pointed at the null device.
                // Named rather than silently passed, because a reader who knows `host.rs`
                // will look for a real file here.
                "/dev/null".to_string(),
            ];
            args.push(prog);
            args.extend(rest);
            Ok(("/bin/sh".to_string(), args))
        }
    }
}

/// **Find a trailer in `buf` and take the turn out of it.**
///
/// `Ok(None)` means *not yet*: either the marker is not in the buffer, or it is there and its
/// terminator has not arrived — both are the same instruction to the caller, which is to read
/// more. Pure, so the scan is testable against hand-written bytes: the split-marker case, the
/// echoed line, and a command whose output looks like a prompt.
pub fn take_turn(buf: &mut Vec<u8>, mark: &str) -> Result<Option<Turn>, ShellError> {
    let open = format!("{MARK_OPEN}{mark};");
    let open = open.as_bytes();
    let Some(at) = find(buf, open) else {
        return Ok(None);
    };
    let body = at + open.len();
    let Some(rel) = buf[body..].iter().position(|b| *b == MARK_CLOSE) else {
        // The introducer is here and the terminator is not: one more read, and it will be.
        return Ok(None);
    };
    let raw = String::from_utf8_lossy(&buf[body..body + rel]).into_owned();
    // `<status>;<cwd>`, and the split is on the FIRST `;` because a path may contain one and
    // a status may not.
    let (status, cwd) = raw
        .split_once(';')
        .ok_or_else(|| ShellError::Start(format!("a trailer with no `;` in it: {raw:?}")))?;
    let status: i32 = status.trim().parse().map_err(|_| {
        ShellError::Start(format!(
            "a trailer whose status is not a number: {status:?}"
        ))
    })?;
    let mut bytes = buf[..at].to_vec();
    strip_echo(&mut bytes, &end_fn(mark));
    buf.drain(..body + rel + 1);
    Ok(Some(Turn {
        bytes,
        status,
        cwd: cwd.to_string(),
    }))
}

/// **Take the echo of the trailer's own line out of a turn.**
///
/// The shell echoes what is typed at it, and what is typed at it here is our two lines. The
/// first — the operator's — **stays**: a terminal shows the command that ran, and the pane is
/// a terminal. The second is machinery nobody typed, and it would otherwise appear as a line
/// reading `__lt_end_<nonce>` above every command's output. The name carries the session's
/// nonce, so this removes exactly one string and cannot remove a command's own output unless
/// that command printed the nonce inside a name it could not know.
pub fn strip_echo(bytes: &mut Vec<u8>, fn_name: &str) {
    let name = fn_name.as_bytes();
    let mut i = 0usize;
    while i + name.len() <= bytes.len() {
        if &bytes[i..i + name.len()] == name {
            let mut end = i + name.len();
            // The echo's newline, in either spelling: readline emits `\r\n` and a plain line
            // discipline emits `\n`.
            if end < bytes.len() && bytes[end] == b'\r' {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b'\n' {
                end += 1;
            }
            bytes.drain(i..end);
            continue;
        }
        i += 1;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|i| &hay[*i..*i + needle.len()] == needle)
}

/// **One shell, one pty, one cgroup.** See the module header for the whole argument.
pub struct ShellSession {
    /// The master end. Reads are the shell's output; writes are its input.
    master: std::fs::File,
    /// The shell's pid — which is the pid the scope wrapper `exec`ed into, so it is a member
    /// of the session's cgroup.
    pid: i32,
    mark: String,
    fn_name: String,
    /// Bytes read and not yet consumed by a trailer scan. **Carried between turns**, so a
    /// marker split across two reads is one marker and the prompt that follows a turn belongs
    /// to the next one.
    pending: Vec<u8>,
    /// The last cwd the SHELL reported. `None` until a trailer has been read, because a
    /// starting directory is not an answer — see the module header.
    cwd: Option<String>,
    scope: Option<ScopeId>,
    tree: Option<Arc<dyn ScopeTree>>,
    /// What the reaper thread saw, once it has seen it.
    exit: Arc<Mutex<Option<i32>>>,
    /// Why this session must not be used, if it must not.
    blocked: Option<ShellError>,
}

impl std::fmt::Debug for ShellSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellSession")
            .field("pid", &self.pid)
            .field("mark", &self.mark)
            .field("cwd", &self.cwd)
            .field("scope", &self.scope)
            .field("blocked", &self.blocked)
            .finish()
    }
}

impl ShellSession {
    /// **Start the session.** One pty, one shell on the far end of it, one cgroup if the
    /// caller named one, and a first trailer read before this returns — so a caller never
    /// holds a session that has not answered anything.
    pub fn open(cfg: &ShellConfig) -> Result<ShellSession, ShellError> {
        let (prog, args) = argv(cfg)?;
        let pty = Pty::open().map_err(|e| ShellError::Start(format!("no pty: {e}")))?;
        let mark = nonce();
        let fn_name = end_fn(&mark);

        let mut cmd = std::process::Command::new(&prog);
        cmd.args(&args);
        cmd.current_dir(&cfg.cwd);
        // The mark the trailer's function reads. In the environment rather than in the
        // definition, so the line the shell echoes carries the *name* and not the value.
        cmd.env("LETIBOT_MARK", &mark);
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        // **A terminal on all three**, which is the difference between this and the row path:
        // the shell reads the lines a person types at it.
        let (a, b, c) = (
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
        );
        cmd.stdin(a).stdout(b).stderr(c);

        // **`setsid` and `TIOCSCTTY`, and here rather than in `pty.rs` on purpose.** The row
        // path declines both (see the module header); a pane wants both. Failure is not
        // fatal: a shell without a controlling terminal still runs, it just has no job
        // control and says so in two lines of its own.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    // Already a group leader, which a forked child is not — so this is a
                    // failure we do not understand, and the shell still runs.
                }
                let _ = libc::ioctl(0, libc::TIOCSCTTY, 0);
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| ShellError::Start(format!("{prog}: {e}")))?;
        let pid = child.id() as i32;
        // **The parent's own slave handles go now.** A slave this process still holds open is
        // a master read that never reports the child's exit — `pty.rs`'s own test measures
        // thirty seconds of that.
        let master = pty.into_master();
        drop(cmd);

        // **The size before the first byte**, so the shell's `$COLUMNS` and any program that
        // asks are right from the start rather than right from the first resize.
        set_size(&master, cfg.cols, cfg.rows);

        // A reaper, so a shell that exits is a fact rather than a zombie.
        let exit = Arc::new(Mutex::new(None));
        {
            let slot = Arc::clone(&exit);
            let mut child = child;
            std::thread::spawn(move || {
                let code = child.wait().ok().map(|s| s.code().unwrap_or(-1));
                if let Ok(mut slot) = slot.lock() {
                    *slot = code;
                }
            });
        }

        let mut session = ShellSession {
            master,
            pid,
            mark,
            fn_name,
            pending: Vec::new(),
            cwd: None,
            scope: cfg.scope.clone(),
            tree: cfg.tree.clone(),
            exit,
            blocked: None,
        };
        // **The session is not open until it has answered.** Defining the function and
        // calling it is also the readiness probe: a shell that is not up yet has the bytes
        // sitting in the pty's buffer and reads them when it is, and a shell that never comes
        // up is a `Start`-shaped failure here rather than a mystery on the first `run`. The
        // two lines go together because the shell reads them in order, and a definition it has
        // not read yet is not a function.
        let setup = format!(
            "{}\n{}\n",
            trailer_definition(&session.fn_name),
            session.fn_name
        );
        session.write(setup.as_bytes())?;
        match session.read_turn(cfg.ready) {
            Ok(t) => {
                session.cwd = Some(t.cwd);
                Ok(session)
            }
            Err(e) => {
                session.close();
                Err(e)
            }
        }
    }

    /// The nonce this session's trailer carries. Unique on this box, and in the function's
    /// name so an echo of it can be removed exactly.
    pub fn mark(&self) -> &str {
        &self.mark
    }

    /// The shell's pid. **A member of the session's cgroup**, because the wrapper joined
    /// before `exec`.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// **The last cwd the shell reported**, or `None` before it has reported one.
    ///
    /// `None` is not "unknown, assume the workspace": it is *this session has not answered
    /// yet*, and a caller with a `None` has a [`ShellConfig::cwd`] and a startup fact rather
    /// than a directory. See the module header.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Whether the shell has exited. `None` while the reaper is still waiting.
    pub fn exited(&self) -> Option<i32> {
        self.exit.lock().ok().and_then(|e| *e)
    }

    /// Whether this session may be used. `Err` says why not, in the words a caller can show.
    pub fn usable(&self) -> Result<(), ShellError> {
        match &self.blocked {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// **Run one line, and answer with what the shell wrote and what the shell said.**
    ///
    /// `line` is written verbatim — this module does not quote, escape or rewrite it, because
    /// the shell is the thing that reads it and a second grammar in front of the shell is a
    /// second answer to what a line means. A trailing newline is not required and is not
    /// doubled.
    ///
    /// The line a caller passes **must be one a person submitted**: see the module header's
    /// first rule. Nothing here takes a model's turn, and there is no timer.
    pub fn run(&mut self, line: &str, wait: Duration) -> Result<Turn, ShellError> {
        self.usable()?;
        if let Some(code) = self.exited() {
            let e = ShellError::Gone(format!("the shell exited with {code}"));
            self.blocked = Some(e.clone());
            return Err(e);
        }
        let mut write = String::with_capacity(line.len() + self.fn_name.len() + 2);
        write.push_str(line.trim_end_matches('\n'));
        write.push('\n');
        write.push_str(&self.fn_name);
        write.push('\n');
        self.write(write.as_bytes())?;
        let turn = self.read_turn(wait)?;
        self.cwd = Some(turn.cwd.clone());
        Ok(turn)
    }

    /// **Read the trailer that is still in flight**, after a timeout.
    ///
    /// A timeout does not lose the trailer: the line is still running and its trailer is still
    /// coming. So the session is not broken, it is *behind*, and this is the way to catch up —
    /// the turn it returns is the one that timed out, and its output is worth having. `false`
    /// means it is still not here.
    pub fn resync(&mut self, wait: Duration) -> bool {
        match self.read_turn(wait) {
            Ok(t) => {
                self.cwd = Some(t.cwd);
                self.blocked = None;
                true
            }
            Err(_) => false,
        }
    }

    /// **Move the pty's size.** The kernel raises `SIGWINCH` for the pty's foreground process
    /// group, so a program that redraws on a resize does — what is not built is the path from
    /// the head's pane rectangle to this call (a TODO in the module header).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        set_size(&self.master, cols, rows);
    }

    /// **End the session.**
    ///
    /// With a scope, this is `ScopeTree::end`: the cgroup is killed, **everything under it
    /// dies with it**, and the record carries what was observed before and what survived
    /// after. Without one — a unit test, or a box where cgroups are unavailable — the fallback
    /// is `SIGHUP` to the shell itself, which a job-controlling shell passes on to its jobs.
    /// **The fallback is not the mechanism** and says so: it reaches the shell's jobs only
    /// because a shell with a terminal does that, and it reaches nothing a job daemonised away
    /// from the session.
    pub fn close(&mut self) -> Option<Reaping> {
        match (&self.tree, &self.scope) {
            (Some(tree), Some(scope)) => {
                let reaping = tree.end(scope);
                // The shell is dead by now; the master read would block for ever, so the
                // descriptor goes.
                self.blocked = Some(ShellError::Gone("the session was closed".to_string()));
                Some(reaping)
            }
            _ => {
                unsafe {
                    libc::kill(self.pid, libc::SIGHUP);
                }
                self.blocked = Some(ShellError::Gone("the session was closed".to_string()));
                None
            }
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), ShellError> {
        self.master
            .write_all(bytes)
            .map_err(|e| ShellError::Gone(format!("the pty would not take the line: {e}")))
    }

    /// **Read until a trailer, or until the deadline.**
    ///
    /// The whole of the framing is here: scan what has arrived, and if there is no trailer,
    /// wait for more bytes with a bounded `poll` and append them. Nothing about a prompt, and
    /// nothing about how many reads it takes.
    fn read_turn(&mut self, wait: Duration) -> Result<Turn, ShellError> {
        let deadline = Instant::now() + wait;
        let mut buf = [0u8; 8192];
        loop {
            if let Some(turn) = take_turn(&mut self.pending, &self.mark)? {
                return Ok(turn);
            }
            if self.pending.len() > MAX_TURN {
                let n = self.pending.len();
                // **Blocked as *behind*, not as *this call failed*.** The two are different
                // facts and only one of them is about the caller's line: what the next call
                // needs to know is that a trailer is in flight.
                self.blocked = Some(ShellError::Desynchronised(format!(
                    "a turn grew to {n} bytes with no trailer in it"
                )));
                return Err(ShellError::Runaway(n));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.blocked = Some(ShellError::Desynchronised(format!(
                    "a line timed out after {wait:?}; its trailer is still in flight"
                )));
                return Err(ShellError::Timeout(wait));
            }
            match wait_readable(&self.master, left) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(e) => {
                    let e = ShellError::Gone(e.to_string());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
            }
            match self.master.read(&mut buf) {
                Ok(0) => {
                    let e = ShellError::Gone(self.gone_why());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                // **`EIO` is this platform's end-of-pty**, not a failure: it is what a read
                // on the master returns once the last slave is closed, and `pty.rs`'s own
                // drain treats it the same way.
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    let e = ShellError::Gone(self.gone_why());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let e = ShellError::Gone(e.to_string());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
            }
        }
    }

    fn gone_why(&self) -> String {
        match self.exited() {
            Some(code) => format!("the shell exited with {code}"),
            None => "the pty's far end closed".to_string(),
        }
    }
}

impl Drop for ShellSession {
    /// **A session nobody closed is still a session that must not leak.** See
    /// [`ShellSession::close`] for what the two paths do.
    fn drop(&mut self) {
        if self.blocked.is_none() {
            self.close();
        }
    }
}

/// `TIOCSWINSZ` on the master. Failure is ignored: a pty that will not take a size is a pty
/// whose programs see the default, which is a cosmetic defect and not a reason to lose a
/// session.
///
/// **`pub(crate)` because [`super::term`] is the second caller and there is one answer to
/// *how a pty is told its size*.** A pane and a shell session both have a rectangle that
/// belongs to the head, and two `ioctl` wrappers would be two places for the clamp to be
/// wrong.
pub(crate) fn set_size(master: &std::fs::File, cols: usize, rows: usize) {
    use std::os::fd::AsRawFd;
    let ws = libc::winsize {
        ws_row: rows.clamp(1, u16::MAX as usize) as u16,
        ws_col: cols.clamp(1, u16::MAX as usize) as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
    }
}

/// Is there anything to read, inside `d`? `Ok(false)` is a timeout and not an error — the
/// caller loops, because the deadline is the caller's and not the poll's.
fn wait_readable(f: &std::fs::File, d: Duration) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut p = libc::pollfd {
        fd: f.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = d.as_millis().min(i32::MAX as u128) as i32;
    let rc = unsafe { libc::poll(&mut p, 1, ms.max(1)) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(rc > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use super::super::scope::ScopeKind;

    /// **The shell the tests drive.** `--norc --noprofile` on purpose: an assertion here is
    /// about this module's framing, and the operator's rc is a file that prints things. The
    /// production default is [`super::super::console::shell`], which is their shell *with*
    /// their rc — the whole point of the console path — and it is the same argv shape.
    fn shell() -> Vec<String> {
        if Path::new("/bin/bash").exists() {
            vec![
                "/bin/bash".to_string(),
                "--norc".to_string(),
                "--noprofile".to_string(),
                "-i".to_string(),
            ]
        } else {
            vec!["/bin/sh".to_string(), "-i".to_string()]
        }
    }

    fn config(cwd: &Path) -> ShellConfig {
        ShellConfig {
            shell: shell(),
            cwd: cwd.to_path_buf(),
            ..ShellConfig::default()
        }
    }

    /// A scratch directory, per test and per process — the tree's own pattern.
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-shell-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("a scratch directory");
        d
    }

    fn text(t: &Turn) -> String {
        String::from_utf8_lossy(&t.bytes).into_owned()
    }

    /// Long enough that a loaded box does not fail the test, short enough that a hang does
    /// not hold the suite: every wait ends when the trailer arrives.
    const LONG: Duration = Duration::from_secs(30);

    /// **A session answers a line, and the status is the shell's own answer.**
    ///
    /// The two halves are the whole of the framing: the bytes between two trailers, and `$?`
    /// read where it is still the status of the line that was sent. The `42` is the half that
    /// matters — a `0` here would pass on a module that always answered zero.
    #[test]
    fn a_session_answers_a_line_and_reports_the_shells_status() {
        let dir = tmp("status");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s.run("printf 'hi\\n'", LONG).expect("a turn");
        assert!(text(&t).contains("hi"), "{:?}", text(&t));
        assert_eq!(t.status, 0);
        let t = s.run("(exit 42)", LONG).expect("a turn");
        assert_eq!(
            t.status, 42,
            "the status is the shell's, and it is not always zero"
        );
        // And the session is still usable after a failing line, which is what makes the next
        // test meaningful rather than lucky.
        let t = s.run("printf 'after'", LONG).expect("a turn");
        assert!(text(&t).contains("after"));
        assert_eq!(t.status, 0);
    }

    /// **The slice: one session, two lines, and the state carried between them.**
    ///
    /// This is the requirement in its smallest honest form — *"a session that survives one
    /// command and answers a second, with state carried"*. `cd` is the case the operator
    /// named (`! cd /x` then `! ls` lists `/x`), and it is the one that a fresh
    /// `sh -c` per line cannot do however many flags it is given.
    #[test]
    fn the_working_directory_survives_between_two_lines() {
        let dir = tmp("cwd");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).expect("a subdirectory");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        assert_eq!(
            s.cwd(),
            Some(dir.to_str().unwrap()),
            "the first trailer is the shell's own answer about where it started"
        );

        s.run(&format!("cd {}", sub.display()), LONG)
            .expect("a turn");
        let t = s.run("pwd", LONG).expect("a turn");
        assert!(
            text(&t).contains(sub.to_str().unwrap()),
            "`pwd` after a `cd` on the previous line: {:?}",
            text(&t)
        );

        // **And the head's notion follows the shell, because it IS the shell's answer.** The
        // assertion that matters is the negative: the session is not reporting the directory
        // it was opened in.
        assert_eq!(t.cwd, sub.to_str().unwrap());
        assert_eq!(s.cwd(), Some(sub.to_str().unwrap()));
        assert_ne!(
            s.cwd(),
            Some(dir.to_str().unwrap()),
            "the session is still reporting its startup directory"
        );
    }

    /// **`export`, a function and an alias survive too** — the other three things the operator
    /// named, and each is a different mechanism: an environment variable, a shell function,
    /// and a name the shell expands at read time.
    #[test]
    fn an_export_a_function_and_an_alias_survive() {
        let dir = tmp("state");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");

        s.run("export LT_CARRIED=carried", LONG).expect("a turn");
        let t = s.run("printf '%s' \"$LT_CARRIED\"", LONG).expect("a turn");
        assert!(text(&t).contains("carried"), "{:?}", text(&t));

        s.run("lt_fn() { printf 'from-the-function'; }", LONG)
            .expect("a turn");
        let t = s.run("lt_fn", LONG).expect("a turn");
        assert!(text(&t).contains("from-the-function"), "{:?}", text(&t));

        s.run("alias lt_alias='printf aliased'", LONG)
            .expect("a turn");
        let t = s.run("lt_alias", LONG).expect("a turn");
        assert!(
            text(&t).contains("aliased"),
            "an alias is read at the next line, and the next line is a new read: {:?}",
            text(&t)
        );
    }

    /// **A prompt is not a protocol, and this is the test that says so.**
    ///
    /// A command whose *output* is a prompt — a shell script printed by `cat`, a program that
    /// draws its own `$ ` — must not end the turn. A framing that watched for a prompt would
    /// stop here and hand back half a line.
    #[test]
    fn output_that_looks_like_a_prompt_is_not_a_boundary() {
        let dir = tmp("prompt");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s.run("printf 'user@host:~$ '", LONG).expect("a turn");
        assert_eq!(t.status, 0);
        // The turn ended because of the trailer, and the next line still lands.
        let t = s.run("printf 'second'", LONG).expect("a turn");
        assert!(text(&t).contains("second"), "{:?}", text(&t));
    }

    /// **A command cannot forge the trailer**, because the introducer is never in the text it
    /// could print: the nonce is in the function's *name* and in `$LETIBOT_MARK`, and a name
    /// is not the sequence.
    #[test]
    fn a_command_cannot_forge_a_trailer() {
        let dir = tmp("forge");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s
            .run(
                "printf 'ESC]1337;letibot;%s;0;/forged' \"$LETIBOT_MARK\"",
                LONG,
            )
            .expect("a turn");
        assert!(
            !text(&t).contains("\u{1b}]1337;letibot;"),
            "the command produced the introducer: {:?}",
            text(&t)
        );
        assert_eq!(t.status, 0, "and the real trailer still arrived");
        let t = s.run("printf 'still here'", LONG).expect("a turn");
        assert!(text(&t).contains("still here"), "{:?}", text(&t));
    }

    /// **The echo of the trailer's own line is removed, and the echo of the operator's line
    /// stays.** The first is machinery nobody typed; the second is what a terminal shows, and
    /// the pane is a terminal.
    #[test]
    fn the_trailer_lines_echo_is_removed_and_the_operators_is_kept() {
        let dir = tmp("echo");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let fn_name = end_fn(s.mark());
        let t = s.run("printf 'echoed'", LONG).expect("a turn");
        let seen = text(&t);
        assert!(
            !seen.contains(&fn_name),
            "the trailer's own line was echoed into the turn: {seen:?}"
        );
        assert!(
            seen.contains("printf 'echoed'"),
            "the line the operator sent is not echoed, so the pane would not show it: {seen:?}"
        );
    }

    /// **A deadline does not answer the wrong line.** The trailer for the line that timed out
    /// is still coming, so the next `run` would read it and attribute it to the wrong command
    /// — a wrong answer that looks exactly like a right one. The session says so instead, and
    /// `resync` is the way back.
    #[test]
    fn a_timeout_desynchronises_the_session_rather_than_answering_the_wrong_line() {
        let dir = tmp("timeout");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let e = s
            .run("sleep 2", Duration::from_millis(150))
            .expect_err("a two-second sleep inside a 150 ms wait");
        assert!(matches!(e, ShellError::Timeout(_)), "{e}");
        let e = s
            .run("printf 'never'", Duration::from_millis(150))
            .expect_err("a desynchronised session must refuse");
        assert!(matches!(e, ShellError::Desynchronised(_)), "{e}");

        // **And it is not dead.** The trailer that was in flight arrives, and catching up
        // puts the session back in step — the turn it returns is the one that timed out.
        assert!(
            s.resync(LONG),
            "the trailer that was in flight never arrived"
        );
        let t = s.run("printf 'back'", LONG).expect("a turn");
        assert!(text(&t).contains("back"), "{:?}", text(&t));
    }

    /// **The shell has a terminal, and a controlling one.** The first assertion is the pty;
    /// the second is the half [`super::super::pty`] declined for the row path and this path
    /// wants — a shell with a controlling terminal does not print the two lines about job
    /// control that land on every `!` row today.
    #[test]
    fn the_shell_has_a_terminal_and_a_controlling_one() {
        let dir = tmp("tty");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s
            .run(
                "test -t 0 && echo stdin-is-a-tty || echo stdin-is-not-a-tty",
                LONG,
            )
            .expect("a turn");
        assert!(
            text(&t).contains("stdin-is-a-tty"),
            "the shell is not reading a terminal: {:?}",
            text(&t)
        );
        // `/dev/tty` is openable exactly when the pty is the child's controlling terminal.
        let t = s
            .run(
                "test -r /dev/tty && echo has-controlling || echo none",
                LONG,
            )
            .expect("a turn");
        assert!(
            text(&t).contains("has-controlling"),
            "no controlling terminal, so job control and `/dev/tty` are both absent: {:?}",
            text(&t)
        );
        assert!(
            !text(&t).contains("job control"),
            "bash's own two lines about job control reached the session: {:?}",
            text(&t)
        );
    }

    /// **Two sessions do not share a nonce.** The mark is what makes a trailer unforgeable
    /// and an echo removable, and one mark for two sessions would make both of those false at
    /// once.
    #[test]
    fn two_sessions_have_two_marks() {
        let dir = tmp("marks");
        let a = ShellSession::open(&config(&dir)).expect("a shell session");
        let b = ShellSession::open(&config(&dir)).expect("a second shell session");
        assert_ne!(a.mark(), b.mark());
        assert_ne!(a.pid(), b.pid());
    }

    /// **The scope wrapper goes in front of the shell, and the shell is last.**
    ///
    /// Pure, so the ordering — which is the load-bearing part — is asserted without a cgroup
    /// existing: [`join_script`] writes `$$` into `cgroup.procs` and only then `exec`s, so the
    /// pid this module holds is a member of the scope and every child the shell forks
    /// inherits it.
    #[test]
    fn the_scope_wrapper_goes_in_front_of_the_shell() {
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name: "s1".to_string(),
            path: PathBuf::from("/sys/fs/cgroup/letibot.1/session.s1"),
        };
        let cfg = ShellConfig {
            shell: vec!["/bin/bash".to_string(), "-ic".to_string()],
            scope: Some(scope.clone()),
            ..ShellConfig::default()
        };
        let (prog, args) = argv(&cfg).expect("an argv");
        assert_eq!(prog, "/bin/sh", "the wrapper is a shell and not the shell");
        assert!(args.contains(&join_script().to_string()), "{args:?}");
        assert!(
            args.iter().any(|a| a.ends_with("session.s1/cgroup.procs")),
            "the scope's `cgroup.procs` is not in the argv: {args:?}"
        );
        assert_eq!(
            &args[args.len() - 2..],
            &["/bin/bash".to_string(), "-ic".to_string()],
            "the shell and its flags are the last words, which is what `exec \"$@\"` runs"
        );
        assert!(
            args.windows(2).any(|w| w[0] == "letibot-shell"),
            "the wrapper's `$0` is missing: {args:?}"
        );

        // Without a scope the shell is the program, and there is no wrapper at all.
        let cfg = ShellConfig {
            shell: vec!["/bin/bash".to_string(), "-ic".to_string()],
            ..ShellConfig::default()
        };
        let (prog, args) = argv(&cfg).expect("an argv");
        assert_eq!(prog, "/bin/bash");
        assert_eq!(args, vec!["-ic".to_string()]);
    }

    /// **An empty shell is a refusal and not a spawn of nothing.**
    #[test]
    fn an_empty_shell_is_refused_by_name() {
        let cfg = ShellConfig {
            shell: Vec::new(),
            ..ShellConfig::default()
        };
        assert!(matches!(argv(&cfg), Err(ShellError::Config(_))));
    }

    /// **The scan, on hand-written bytes** — the split marker, the two facts, and the cwd
    /// that contains the separator. Pure, so this is the framing's own test rather than a
    /// test of bash.
    #[test]
    fn the_scan_waits_for_a_split_marker_and_takes_both_facts() {
        let mark = "abc";
        let mut buf = b"out \x1b]1337;letibot;ab".to_vec();
        assert!(
            take_turn(&mut buf, mark).expect("a scan").is_none(),
            "half a marker is not a marker"
        );
        buf.extend_from_slice(b"c;0;/tmp\x07tail");
        let t = take_turn(&mut buf, mark).expect("a scan").expect("a turn");
        assert_eq!(t.status, 0);
        assert_eq!(t.cwd, "/tmp");
        assert_eq!(t.bytes, b"out ");
        assert_eq!(
            buf, b"tail",
            "what follows the trailer is the next turn's prefix"
        );

        // A path with the separator in it survives, because the split is on the FIRST `;`.
        let mut buf = b"\x1b]1337;letibot;m;3;/a;b\x07".to_vec();
        let t = take_turn(&mut buf, "m").expect("a scan").expect("a turn");
        assert_eq!(t.status, 3);
        assert_eq!(t.cwd, "/a;b");

        // A trailer with no terminator yet is *not yet*, not a failure.
        let mut buf = b"\x1b]1337;letibot;m;0;/w".to_vec();
        assert!(take_turn(&mut buf, "m").expect("a scan").is_none());
    }

    /// **A trailer this module cannot read is a refusal, not a guess.** The two malformed
    /// shapes are the two fields, and a guess here would be a status or a directory that no
    /// shell ever reported.
    #[test]
    fn a_malformed_trailer_is_refused_rather_than_guessed() {
        let mut buf = b"\x1b]1337;letibot;m;nope;/w\x07".to_vec();
        assert!(
            take_turn(&mut buf, "m").is_err(),
            "a status that is not a number"
        );
        let mut buf = b"\x1b]1337;letibot;m;0\x07".to_vec();
        assert!(take_turn(&mut buf, "m").is_err(), "no separator at all");
        // And a trailer for a *different* session's nonce is not this session's trailer.
        let mut buf = b"\x1b]1337;letibot;other;0;/w\x07".to_vec();
        assert!(take_turn(&mut buf, "m").expect("a scan").is_none());
    }

    /// **The echo removal takes the name and not the text around it.** A name that appeared
    /// twice would otherwise take the command's own output with it.
    #[test]
    fn the_echo_removal_is_exact() {
        let fn_name = end_fn("beef");
        let mut bytes = format!("printf 'x'\r\n{fn_name}\r\nout\n{fn_name}\nout2\n").into_bytes();
        strip_echo(&mut bytes, &fn_name);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "printf 'x'\r\nout\nout2\n",
            "the name went and the two newlines it sat on went with it, once each"
        );
    }

    /// **A session that is closed says so**, rather than blocking a read on a pty whose far
    /// end is gone.
    #[test]
    fn a_closed_session_refuses_rather_than_hanging() {
        let dir = tmp("closed");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        s.run("printf 'alive'", LONG).expect("a turn");
        s.close();
        assert!(matches!(s.usable(), Err(ShellError::Gone(_))));
        let e = s.run("printf 'no'", LONG).expect_err("a closed session");
        assert!(matches!(e, ShellError::Gone(_)), "{e}");
        // Dropping it again is not a second kill: `Drop` sees the block and does nothing.
    }

    /// **A shell that exits is a fact and not a zombie.** The reaper thread is what makes the
    /// next `run` say *the shell exited with N* rather than waiting out a deadline on a pty
    /// whose far end has closed.
    #[test]
    fn a_shell_that_exits_is_reported_and_not_waited_on() {
        let dir = tmp("exits");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        // The trailer is written by the function *before* the shell processes `exit`, so this
        // turn can succeed or fail depending on which line the shell read first — and either
        // ending is honest. What must not happen is a hang.
        let _ = s.run("exit 3", Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(10);
        while s.exited().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(s.exited(), Some(3), "the reaper never saw the shell leave");
        let e = s
            .run("printf 'no'", Duration::from_secs(5))
            .expect_err("a dead session");
        assert!(matches!(e, ShellError::Gone(_)), "{e}");
    }
}
