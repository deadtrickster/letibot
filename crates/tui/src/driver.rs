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
    draw: &mut dyn FnMut(&[String]),
) -> Result<(), ClientError> {
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

    let mut actions = Vec::new();
    for k in keys {
        if let Some(a) = app.key(*k) {
            actions.push(a);
        }
    }

    // 2. Draw.
    let screen = app.screen(size.0, size.1);
    draw(&screen);

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
            Action::Quit => {
                let _ = client.detach();
            }
        }
    }
    Ok(())
}
