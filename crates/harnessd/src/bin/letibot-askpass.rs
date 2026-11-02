//! `letibot-askpass` - what `sudo` runs, inside a letibot session, when it wants a
//! password. The session's shell has `SUDO_ASKPASS` pointing here and a `sudo` shim that
//! adds `-A`, so a `sudo apt install x` the gate admitted never reaches for a terminal:
//! sudo runs this with its prompt as the one argument and reads the password from stdout.
//! What this does with it:

use std::io::Write;

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

fn main() {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "[sudo] password:".into());
    let socket = std::env::var("LETIBOT_SOCKET").unwrap_or_default();
    let session = std::env::var("LETIBOT_SESSION").unwrap_or_default();
    let command = std::env::var("LETIBOT_COMMAND").unwrap_or_default();
    if socket.is_empty() || session.is_empty() {
        eprintln!(
            "letibot-askpass: not inside a letibot session (LETIBOT_SOCKET / LETIBOT_SESSION \
             unset), so there is nobody to ask for a password"
        );
        std::process::exit(1);
    }
    let caps = Caps {
        queue: 8,
        can_decide: false,
        ..Caps::default()
    };
    let (mut client, _hello, mut reader) =
        match HeadClient::attach(&socket, &session, u64::MAX, "askpass", "sudo", caps) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("letibot-askpass: the session's daemon did not take the question: {e}");
                std::process::exit(1);
            }
        };
    if let Err(e) = client.askpass(&prompt, &command) {
        eprintln!("letibot-askpass: {e}");
        std::process::exit(1);
    }
    // The daemon may deliver a snapshot or events before the answer; only the
    // `Secret` frame is for us, and it comes on this connection or not at all.
    loop {
        match reader.read::<ServerFrame>() {
            Ok(ServerFrame::Secret {
                secret: Some(s), ..
            }) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(s.as_bytes());
                let _ = out.write_all(b"\n");
                let _ = out.flush();
                let _ = client.detach();
                std::process::exit(0);
            }
            Ok(ServerFrame::Secret { secret: None, why }) => {
                // The daemon names WHICH failure this was rather than leaving the helper
                // to guess: no head attached (a wiring fault), a person who did not act, a
                // person who refused. A daemon that sends no `why` leaves the one sentence.
                match why {
                    Some(why) => eprintln!("letibot-askpass: no password was given — {why}"),
                    None => eprintln!(
                        "letibot-askpass: no password was given — no head answered before the \
                         deadline, or the person refused"
                    ),
                }
                let _ = client.detach();
                std::process::exit(1);
            }
            Ok(ServerFrame::Bye { reason }) => {
                eprintln!("letibot-askpass: the daemon closed the connection: {reason}");
                std::process::exit(1);
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("letibot-askpass: {e}");
                std::process::exit(1);
            }
        }
    }
}
