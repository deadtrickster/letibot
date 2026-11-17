//! A head from ANOTHER revision of this crate, attaching to a daemon of a newer one.
//!     cargo run -q --example wire_head -p letibot-sessionlog -- <socket> <session>
//! Prints what its own types made of the daemon's Hello, as one JSON line, and detaches. Exit 1
//! when the daemon's Hello did not decode into a snapshot this build can name.

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(sock), Some(session)) = (args.next(), args.next()) else {
        eprintln!("usage: wire_head <socket> <session>");
        std::process::exit(2);
    };
    let (mut head, hello, reader) =
        match HeadClient::attach(&sock, &session, 0, "tui", "wire-head", Caps::default()) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("wire_head: the attach did not round trip: {e}");
                std::process::exit(1);
            }
        };
    let ServerFrame::Hello {
        protocol_version,
        snapshot,
        ..
    } = &hello
    else {
        eprintln!("wire_head: the answer to an attach was not a Hello: {hello:?}");
        std::process::exit(1);
    };
    // A snapshot this build cannot name is the skew this job exists to find: an older head reads
    // a newer daemon's answer, and `None` here means the answer did not arrive as one.
    let Some(snapshot) = snapshot.as_deref() else {
        eprintln!("wire_head: the Hello carried no snapshot");
        std::process::exit(1);
    };
    println!(
        "{{\"mine\":{},\"daemon\":{},\"session\":{},\"seq\":{},\"rows\":{}}}",
        letibot_sessionlog::protocol::PROTOCOL_VERSION,
        protocol_version,
        serde_json::to_string(&snapshot.session_id).unwrap_or_else(|_| "null".into()),
        snapshot.seq,
        snapshot.items.len()
    );
    if let Err(e) = head.detach() {
        eprintln!("wire_head: the detach did not go out: {e}");
        std::process::exit(1);
    }
    let _ = reader;
}
