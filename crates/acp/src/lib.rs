//! The Agent Client Protocol adapter: an ACP agent on stdio that holds a letibot session.
//!
//! ```text
//! Zed / Neovim ──stdio JSON-RPC (ACP)── letibot-acp ──unix socket── harnessd
//! ```
//!
//! The daemon's vocabulary was shaped so that "the adapter is a mapping when somebody writes it"
//! (`crates/sessionlog/src/lib.rs`), and this is that mapping and nothing else. A session is held
//! through the same head protocol the TUI and the bridge discharge ([`session`]), a ranged
//! resource in a prompt becomes the editor context a rano head would have sent ([`context`]), and
//! a call's changed lines become an ACP `diff` ([`marks`]).
//!
//! What is deliberately NOT here: a model, a store, a daemon's config, and the launcher's choice
//! of mode or placement. ACP says the agent is a process the client launches; this one is a head,
//! and the session it holds is the daemon's.

pub mod acp;
pub mod context;
pub mod marks;
pub mod session;
pub mod wire;

pub use session::{Adapter, Event};

use std::io::{Read, Write};

/// Run the adapter over a transport, as `main` does with the process's own stdio. Returns the exit
/// status: `0` for a client that closed the pipe, which is a disconnect and not a failure.
pub fn serve<R: Read + Send + 'static, W: Write>(input: R, output: W) -> i32 {
    let mut adapter = Adapter::new(output);
    adapter.run(input);
    0
}
