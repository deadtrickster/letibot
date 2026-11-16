//! Writes the wire corpus as this build speaks it to a head at one protocol.
//!     cargo run -q --example wire_capture -p letibot-sessionlog -- <dir> <head-protocol>
//! `tests/wire_golden/v<N>/` is this run at `N = PROTOCOL_VERSION`.

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(head)) = (args.next(), args.next().and_then(|h| h.parse::<u32>().ok()))
    else {
        eprintln!("usage: wire_capture <dir> <head-protocol>");
        std::process::exit(2);
    };
    if let Err(e) = letibot_sessionlog::testing::wire_corpus::write(dir.as_ref(), head) {
        eprintln!("wire_capture: {e}");
        std::process::exit(1);
    }
}
