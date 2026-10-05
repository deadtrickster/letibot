//! **Stop one session's turn, by name** — the interrupt the head's `Esc-Esc` sends, aimed at a
//! session you are not sitting in.
//!
//! WHY THIS EXISTS. `job_kill` on a subagent handle does not stop it, and that is a MEASUREMENT
//! rather than a reading (2026-10-05): the tool reported *"interrupted the turn"*, the daemon
//! answered `Rejected { reason: "not attached", expected_seq: 0, actual_seq: 60236 }`, and the
//! session went on running and writing rows — 102 to 108 while the kill was in flight. The cause
//! is `Hub::submit`'s own rule, which is right: it refuses a head that is not seated in the
//! session the command is for, and the parent's seat is seated in the PARENT.
//!
//! The TUI already knows the answer; `head.rs`'s `interrupt_all` says it in as many words:
//!
//! > One seat, moved across the sessions that need the interrupt: `Switch` exists so a
//! > connection can change sessions, and an interrupt is scoped to the seat's session, so the
//! > moving is how one client covers many.
//!
//! This is that for ONE named session — which is what the parent needs, because the daemon-wide
//! form would take its own turn with it.
//!
//!     cargo run -q --example interrupt -p letibot-sessionlog -- <socket> <session> [reason]
//!
//! `expected_seq = 0` is *"no expectation"* at the hub — never stale — which is what a client
//! that is not following the stream gets to say, and the same value `interrupt_all` passes.
//!
//! Detaching is not an abort: a head going away is ordinary (`serve_conn` says so), so this
//! leaves the session stopped rather than closed.

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

fn main() {
    let mut args = std::env::args().skip(1);
    let (socket, session) = match (args.next(), args.next()) {
        (Some(socket), Some(session)) => (socket, session),
        _ => {
            eprintln!("usage: interrupt <socket> <session> [reason]");
            std::process::exit(2);
        }
    };
    let reason = args
        .next()
        .unwrap_or_else(|| "the parent asked for this turn to stop".to_string());

    let (mut client, hello, mut reader) = match HeadClient::attach(
        std::path::Path::new(&socket),
        &session,
        0,
        "remote",
        "interrupt",
        Caps::default(),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not attach to {session} on {socket}: {e}");
            std::process::exit(1);
        }
    };
    client.seated_by(&hello);
    // **The seat has to BE in the session it interrupts, and `Switch` MINTS A NEW HEAD ID.**
    // Attaching is not enough — that is the whole reason the parent's `job_kill` was refused —
    // and neither is switching, because the daemon answers a `Switch` with a *second* `Hello`
    // (see `app.rs`: *"`Switch` is answered with a second `Hello`"*) and `Hub::submit` attributes
    // a command to the head id it was last given. An interrupt sent under the FIRST `Hello`'s id
    // is therefore refused with `not attached` — MEASURED, and it is the same refusal from this
    // side of the wire.
    if let Err(e) = client.switch(&session, 0) {
        eprintln!("could not switch the seat to {session}: {e}");
        std::process::exit(1);
    }
    for _ in 0..8 {
        match reader.read::<ServerFrame>() {
            Ok(f @ ServerFrame::Hello { .. }) => {
                client.seated_by(&f);
                break;
            }
            Ok(_other) => continue,
            Err(e) => {
                eprintln!("the connection ended before the seat moved: {e}");
                std::process::exit(1);
            }
        }
    }
    let request = match client.interrupt(0, &reason) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("could not send the interrupt: {e}");
            std::process::exit(1);
        }
    };
    // The answer to a submit is one of two frames; anything that arrives in between (the second
    // `Hello` a `Switch` produces, for one) is read past rather than mistaken for it.
    for _ in 0..8 {
        match reader.read::<ServerFrame>() {
            Ok(ServerFrame::Accepted {
                client_request_id, ..
            }) => {
                println!("interrupted {session} as {client_request_id} ({request})");
                return;
            }
            Ok(ServerFrame::Rejected { reason, .. }) => {
                eprintln!("refused by {session}: {reason}");
                std::process::exit(1);
            }
            Ok(_other) => continue,
            Err(e) => {
                eprintln!("the connection ended before an answer: {e}");
                std::process::exit(1);
            }
        }
    }
    eprintln!("no answer to the interrupt after eight frames");
    std::process::exit(1);
}
