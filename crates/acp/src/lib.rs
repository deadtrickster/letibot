//! The Agent Client Protocol adapter: an ACP agent on stdio that holds a letibot session.
//!
//! ```text
//! Zed / Neovim ──stdio JSON-RPC (ACP)── letibot-acp ──unix socket── harnessd
//! ```
//!
//! The mapping the daemon's vocabulary was shaped for, and nothing else: the head protocol
//! ([`session`]), ranged resources as editor context ([`context`]), and changed lines as ACP
//! `diff`s ([`marks`]). No model, store, daemon config or launcher choice: this is a head, and the
//! session is the daemon's.

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
