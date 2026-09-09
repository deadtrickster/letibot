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

use std::sync::mpsc::{Receiver, TryRecvError};

use letibot_sessionlog::client::{ClientError, HeadClient};
use letibot_sessionlog::protocol::{Ack, ServerFrame};

use crate::app::{Action, App, Disposition, Key};

/// Where the terminal's caret belongs: `(row, column)`, zero-based, or nowhere.
pub type Caret = Option<(usize, usize)>;

/// What puts a frame on a screen. A closure, so the same loop serves a terminal
/// and a test that renders to a `Vec<String>`.
pub type Draw<'a> = dyn FnMut(&[String], Caret) + 'a;

/// One pass: drain, draw, ack.
///
/// `draw` is a closure so the same loop serves a terminal and a test that renders
/// to a `Vec<String>`.
pub fn tick(
    app: &mut App,
    rx: &Receiver<ServerFrame>,
    client: &mut HeadClient,
    size: (usize, usize),
    keys: &[Key],
    draw: &mut Draw<'_>,
) -> Result<(), ClientError> {
    app.clock(now_ms());

    // 1. Drain. Nothing is sent in this phase.
    let mut rendered = 0u64;
    let mut filtered = 0u64;
    let mut last_seq = 0u64;
    loop {
        match rx.try_recv() {
            Ok(frame) => {
                if let ServerFrame::Event(env) = &frame {
                    // The read mark, taken from what was *read*. There is no
                    // "last rendered seq" variable here, on purpose.
                    last_seq = env.seq;
                }
                match app.apply(frame) {
                    Disposition::Rendered => rendered += 1,
                    Disposition::Filtered => filtered += 1,
                    Disposition::Control => {}
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }

    // A `Hello` in the drain above may have seated this connection as a different
    // head — that is what a session switch is — and a client still acking under the
    // old id would ack into a session it has left, which the hub ignores in silence.
    if let Some(id) = app.take_seated() {
        client.seated(&id);
    }

    // Actions a *frame* produced, not a key: the switch that follows a session
    // being created. Ahead of the key actions, because they are the answer to
    // something the operator already asked for.
    let mut actions = app.take_actions();
    for k in keys {
        // Cloned rather than copied: `Key::Paste` carries the paste, because the
        // point of bracketed paste is that a 3 KB stack trace is one key.
        if let Some(a) = app.key(k.clone()) {
            actions.push(a);
        }
    }

    // 2. Draw. Every frame is built; whether any of it reaches the terminal is
    //    `Terminal::draw`'s business, and for an unchanged frame the answer is no
    //    bytes at all.
    let screen = app.screen(size.0, size.1);
    draw(&screen, app.cursor());

    // 3. Ack — after the frame is out.
    if last_seq > 0 {
        client.ack(Ack {
            seq: last_seq,
            rendered,
            filtered,
        })?;
    }

    for a in actions {
        match a {
            // `expected_seq` is what this head was looking at when the operator
            // acted. The daemon decides what that means per command; the head's
            // job is to report it honestly.
            Action::Prompt(text) => {
                client.prompt(app.seq, &text)?;
            }
            Action::Interrupt(reason) => {
                client.interrupt(app.seq, &reason)?;
            }
            Action::Answer { req_id, option_id } => {
                client.answer(&req_id, &option_id)?;
            }
            Action::Resync => client.request_resync()?,
            Action::ListSessions => client.list_sessions()?,
            Action::NewSession(title) => {
                client.new_session(&title)?;
            }
            // The answer is a second `Hello`, which arrives on the pump and goes
            // through `App::apply` exactly like the first one. Nothing is torn down
            // here: the switch happens inside the daemon, on this same socket, so
            // there is no window in which this head is attached to nothing.
            Action::Switch(id) => client.switch(&id, 0)?,
            Action::Quit => {
                let _ = client.detach();
            }
        }
    }
    Ok(())
}

/// Wall clock in milliseconds. The head's own, not the daemon's: it is used only
/// to notice that the daemon has gone quiet, and a clock taken from the party that
/// has stopped talking cannot notice that.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
