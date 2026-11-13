//! `letibot-acp`: an ACP agent on stdio, holding a letibot session.
//!
//! A client launches this process and speaks JSON-RPC on its standard streams
//! ([Transports](https://agentclientprotocol.com/protocol/v1/transports)); everything else - which
//! daemon a folder has, which session, the model, the gate - stays where it is, on the other side
//! of the socket. Nothing is printed to stdout that is not a message: logs go to stderr.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("letibot-acp {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help" | "-h") => {
            println!(
                "letibot-acp {} - an Agent Client Protocol agent on stdio.\n\n\
                 A client launches it and speaks JSON-RPC on stdin/stdout. It holds one letibot \
                 session per ACP session, on the daemon of the `cwd` in `session/new`; start that \
                 daemon with `letibot` in the folder first.\n\n\
                 Options:\n  -h, --help     this text\n  -V, --version  the version",
                env!("CARGO_PKG_VERSION")
            );
            return;
        }
        Some(other) => {
            eprintln!("letibot-acp: `{other}` is not an option; it takes none (see --help)");
            std::process::exit(2);
        }
        None => {}
    }
    let status = letibot_acp::serve(std::io::stdin(), std::io::stdout().lock());
    let _ = std::io::stderr().flush();
    std::process::exit(status);
}
