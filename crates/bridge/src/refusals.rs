//! The bridge's refusals on stderr, since nobody draws a bridge: once per layer, hook and reason.
//! One queue per process, not per [`Rules`](crate::push::Rules), because pushes, `tools/list` and
//! `tool_` calls all ask the same thing.

use std::sync::OnceLock;

#[cfg(test)]
use std::sync::Mutex;

use letibot_plugins::{Refusal, Refusals};

/// What this process has already written to stderr, in order. A test cannot read a child's stderr,
/// and "said once" is the claim being made, so the copy is kept where a test can see it.
#[cfg(test)]
static SAID: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn queue() -> &'static Refusals {
    static QUEUE: OnceLock<Refusals> = OnceLock::new();
    QUEUE.get_or_init(Refusals::new)
}

/// Queue a refusal and write whatever is now owed: the first copy is the news, and the second is
/// the same news about a layer that has not changed since - so it is dropped, not repeated.
pub fn refuse(of: Refusal, said: impl Into<String>) {
    if queue().push(of, said) {
        flush();
    }
}

/// The same for news with no failure record behind it, such as a layer that would not load at all:
/// the sentence is its own identity.
pub fn refuse_said(said: impl Into<String>) {
    if queue().push_said(said) {
        flush();
    }
}

/// Everything owed and not said yet, oldest first, written where a person or their client's log
/// will find it. A loop over a queue rather than one write per refusal: two refusals found in one
/// call are two lines, and the queue stays empty for the next caller.
fn flush() {
    for said in queue().drain() {
        #[cfg(test)]
        SAID.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(said.clone());
        eprintln!("letibot-mcp: {said}");
    }
}

/// Every refusal this process has said, in order - what a test reads instead of a stderr it has no
/// handle on.
#[cfg(test)]
pub fn said() -> Vec<String> {
    SAID.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
