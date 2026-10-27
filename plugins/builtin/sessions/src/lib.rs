//! **The session list, as a built-in plugin** — the first of the layers `docs/plugins-plan.md`
//! §7 step 10 compiles into the binary.
//!
//! The head that draws this pane has a session picker of its own, and the point of this module is
//! that it should not: which sessions are listed, how they are grouped, what a row says, what
//! `WAITING ON YOU` means and which keys act on a row are all *policy* — a project that wants its
//! reviewers' children folded away, or its one-shots hidden, should be able to say so in one small
//! layer that calls below and edits what this one answers.
//!
//! **This is the mechanism's first cut.** The layer exists, loads, admits its manifest and answers
//! a view; the rows arrive next, with the feed contract that hands this module the daemon's
//! session list. Answering an empty pane rather than nothing is deliberate: *nothing* would be
//! *unchanged* on the wire, which is the layer passing, and a built-in at the bottom of the stack
//! has nobody below it to pass to.

use letibot_guest_abi as abi;

/// What this pane answers while it has no rows: a title and an empty body, which is a view the
/// host validates and draws.
const VIEW: &str = r#"{"title":"sessions in this daemon"}"#;

/// The pane hook — `pane_<name>`, the name a head opens by.
#[unsafe(no_mangle)]
pub extern "C" fn pane_sessions(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, |_input| Some(VIEW.to_string()))
}
