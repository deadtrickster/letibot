//! **Say something to a session you are not sitting in** — the two frames the head sends when
//! the operator presses `o` on a subagent in the pane and then types.
//!
//! WHY THIS EXISTS. A subagent **is a session in this daemon**, and that is the whole of the
//! mechanism: the subagents pane's `o` switches the head into the child's session
//! (`app.rs`: *"Switching is still here, one key over: Enter reads, `o` opens the subagent's
//! session for good"*), after which the composer's next line is a `Prompt` **for that
//! session**. The operator has both halves and an agent has neither — what stood here was
//! `job_kill`, which is refused with `not attached` — and they said so on 2026-10-05:
//!
//! > we should be able to talk to subagents without flowy
//! > so, you should be able to talk to them too, as well as seeing what they are doing
//!
//! So this is that same pair of frames from a command line: ATTACH as a `remote` head of the
//! child's session, then `Prompt`.
//!
//! **Mid-turn is the normal case and not an error.** [`ClientFrame::Prompt`]'s own doc: *"If a
//! turn is running this is queued as a follow-up user item, not rejected (§13.2), and the
//! queuing is announced."* A message to a subagent that is working therefore lands as its next
//! instruction, which is what makes this useful rather than a way to race it.
//!
//! **The seq is learned, not guessed.** `Hub::submit` refuses a `Prompt` whose `expected_seq`
//! is not the session's head seq, and hands the right value back as `Rejected { actual_seq }` —
//! which is what that field is for. So this sends, reads, and resends once with what it was
//! told, and reports honestly if it is refused twice.
//!
//!     cargo run -q --example say -p letibot-sessionlog -- <socket> <session> "text"
//!
//! It appends **one user row** and sends no `Ack`: this is a message, not a head. It reads
//! frames only until the daemon answers the submit, because everything after that is the
//! child's conversation and not this caller's business.

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

    // **`since_seq = 0`, so the daemon sends the snapshot.** The seat is the price of being
    // able to submit anything at all (`Hub::submit` refuses a head it does not know with
    // `not attached`), and the snapshot is what that seat costs. Nothing is acked and nothing
    // is drawn — a head that never acks is a head whose read mark stands still, which the
    // daemon already handles (see the demoted-head path).
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

    // **Send, read, resend once with the seq we were told.** Two attempts and no more: a
    // third would be a caller guessing at a protocol it was just handed the answer to.
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
                    // **A refusal is a different thing from a correction.** `not attached` is
                    // the protocol saying this caller has no business here; retrying it with
                    // a different number would be noise on the way to the same answer.
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
