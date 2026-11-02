//! Stop one session's turn by name - the interrupt the head's `Esc-Esc` sends, aimed at a
//! session you are not sitting in. `job_kill` cannot do it: `Hub::submit` refuses a head that is
//! not seated in the session the command is for, so this moves its own seat first.
//!
//!     cargo run -q --example interrupt -p letibot-sessionlog -- <socket> <session> [reason]

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
    // The seat has to BE in the session it interrupts, and `Switch` mints a NEW HEAD ID:
    // `Hub::submit` attributes a command to the id it was last given, so an interrupt sent
    // under the first `Hello`'s id is refused with `not attached`.
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
