//! Serves the wire corpus at a socket, for a head built from ANOTHER revision of this crate.
//!     cargo run -q --example wire_serve -p letibot-sessionlog -- <socket> <for-ms>
//! `scripts/wire-skew.sh` runs the previous release's own client against it.

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(sock), Some(for_ms)) = (args.next(), args.next().and_then(|n| n.parse::<u64>().ok()))
    else {
        eprintln!("usage: wire_serve <socket> <for-ms>");
        std::process::exit(2);
    };
    if let Err(e) =
        letibot_sessionlog::testing::wire_corpus::serve_for_an_older_head(sock.as_ref(), for_ms)
    {
        eprintln!("wire_serve: {e}");
        std::process::exit(1);
    }
}
