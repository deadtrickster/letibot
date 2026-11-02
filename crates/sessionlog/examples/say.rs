//! Say something to a session you are not sitting in: attach as a `remote` head of the child's
//! session, then `Prompt`. A mid-turn prompt is queued as a follow-up rather than rejected,
//! which is what makes this useful instead of a race; the seq is learned from the refusal's
//! `actual_seq` and resent once, never guessed.
//!
//!     cargo run -q --example say -p letibot-sessionlog -- <socket> <session> "text"

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

fn main() {
    let mut args = std::env::args().skip(1);
    let (socket, session, text) = match (args.next(), args.next(), args.next()) {
        (Some(socket), Some(session), Some(text)) => (socket, session, text),
        _ => {
            eprintln!("usage: say <socket> <session> <text>");
            std::process::exit(2);
        }
    };

    // `since_seq = 0` so the daemon sends the snapshot: the seat is the price of being able to
    // submit anything at all, and the snapshot is what that seat costs. Nothing is acked or
    // drawn, and a head that never acks is one the daemon already handles.
    let (mut client, hello, mut reader) = match HeadClient::attach(
        std::path::Path::new(&socket),
        &session,
        0,
        "remote",
        "say",
        Caps::default(),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not attach to {session} on {socket}: {e}");
            std::process::exit(1);
        }
    };
    // The name comes from the `Hello`, and a `Prompt` sent before this would be refused as
    // unattributed.
    client.seated_by(&hello);
    let ServerFrame::Hello { session_id, .. } = &hello else {
        eprintln!("the first frame was not a Hello: {hello:?}");
        std::process::exit(1);
    };

    // Send, read, resend once with the seq we were told. Two attempts and no more: a third
    // would be a caller guessing at a protocol it was just handed the answer to.
    let mut seq = 0u64;
    for attempt in 1..=2 {
        let request = match client.prompt(seq, &text) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("could not send: {e}");
                std::process::exit(1);
            }
        };
        // Bounded: the answer to a submit is one of two frames, and anything else that
        // arrives in between (a batch, an announcement) is read past rather than mistaken
        // for the answer.
        for _ in 0..8 {
            match reader.read::<ServerFrame>() {
                Ok(ServerFrame::Accepted {
                    client_request_id, ..
                }) => {
                    println!(
                        "said it to {session_id} as {client_request_id} ({} bytes{})",
                        text.len(),
                        if seq == 0 {
                            ""
                        } else {
                            ", after a seq correction"
                        }
                    );
                    return;
                }
                Ok(ServerFrame::Rejected {
                    reason, actual_seq, ..
                }) => {
                    // A refusal is a different thing from a correction: `not attached` says
                    // this caller has no business here, and retrying it with a different
                    // number would be noise on the way to the same answer.
                    if reason == "not attached" {
                        eprintln!("refused by {session_id}: {reason} — {request}");
                        std::process::exit(1);
                    }
                    if attempt == 2 {
                        eprintln!(
                            "refused by {session_id}: {reason} (expected_seq {seq}, it holds \
                             {actual_seq}) — {request}"
                        );
                        std::process::exit(1);
                    }
                    seq = actual_seq;
                    break;
                }
                Ok(_other) => continue,
                Err(e) => {
                    eprintln!("the connection ended before an answer: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
    eprintln!("no answer to the submit after two attempts");
    std::process::exit(1);
}
