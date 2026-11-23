//! A grant the operator gives LATE: the card was put to them, nobody answered inside its
//! window, and the call was refused with `not_run`. `/gate grant ID` is that person answering it
//! afterwards - for the one call that timed out, once, and never as a standing answer.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// How long a late grant waits to be used. The work it is for is being retried now or not at
/// all, and a person's answer about one command does not stand for a command a day later.
pub const LATE_GRANT_MS: u64 = 30 * 60 * 1000;

/// One call, allowed once: what the operator granted, and when it stops standing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LateGrant {
    pub tool: String,
    /// The call's own key - see [`key`] - so a second call with other arguments is a new question.
    pub key: String,
    pub until_ms: u64,
    /// Where the grant came from, in the operator's own words: the row they ruled on.
    pub why: String,
}

fn table() -> &'static Mutex<BTreeMap<String, Vec<LateGrant>>> {
    static TABLE: OnceLock<Mutex<BTreeMap<String, Vec<LateGrant>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Milliseconds since the epoch. This crate has no clock seam of its own, so the two callers
/// that need one pass their own in and this is only the default.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What one call is known by: its tool and the digest of its arguments - the same pair the
/// corpus row records as `arguments_digest`, so the row a person ruled on and the call that
/// comes back are keyed by one derivation rather than two.
pub fn key(tool: &str, arguments: &serde_json::Value) -> String {
    format!(
        "{tool} {}",
        crate::events::payload_digest(&arguments.to_string())
    )
}

/// Keep a grant for one call in one session, valid until it is used or [`LATE_GRANT_MS`] has
/// passed.
pub fn grant(
    session: &str,
    tool: &str,
    arguments: &serde_json::Value,
    why: impl Into<String>,
    now_ms: u64,
) -> LateGrant {
    let kept = LateGrant {
        tool: tool.to_string(),
        key: key(tool, arguments),
        until_ms: now_ms + LATE_GRANT_MS,
        why: why.into(),
    };
    let mut g = table().lock().unwrap_or_else(|e| e.into_inner());
    let held = g.entry(session.to_string()).or_default();
    held.retain(|h| h.until_ms > now_ms && h.key != kept.key);
    held.push(kept.clone());
    kept
}

/// The grant standing for this call, if there is one - and it is spent, because a late grant is
/// one call's. `None` when nothing was granted, or the window closed before the call came back.
pub fn take(
    session: &str,
    tool: &str,
    arguments: &serde_json::Value,
    now_ms: u64,
) -> Option<LateGrant> {
    let want = key(tool, arguments);
    let mut g = table().lock().unwrap_or_else(|e| e.into_inner());
    let held = g.get_mut(session)?;
    let at = held
        .iter()
        .position(|h| h.key == want && h.until_ms > now_ms)?;
    let taken = held.remove(at);
    if held.is_empty() {
        g.remove(session);
    }
    Some(taken)
}

/// Drop everything this session was granted late - a session that closed carries none of it.
pub fn forget(session: &str) {
    table()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session);
}

// ---------------------------------------------------------------------------
// What the session is then TOLD
// ---------------------------------------------------------------------------

/// How many sentences one session may be owed at once. An operator granting three calls is
/// three notices; more than this is a loop, and the newest are the ones worth keeping.
const MOST_OWED: usize = 8;

fn owed() -> &'static Mutex<BTreeMap<String, Vec<String>>> {
    static OWED: OnceLock<Mutex<BTreeMap<String, Vec<String>>>> = OnceLock::new();
    OWED.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Leave a sentence where this session reads it at its next boundary. A queue rather than a call
/// into the session's state: the operator's verb and the session need not share a thread, and the
/// row being ruled on may be another session's - or one with no thread at all.
pub fn queue_notice(session: &str, text: String) {
    let mut g = owed().lock().unwrap_or_else(|e| e.into_inner());
    let held = g.entry(session.to_string()).or_default();
    if held.len() >= MOST_OWED {
        held.remove(0);
    }
    held.push(text);
}

/// Everything this session is owed, oldest first, taken. Empty for a session that is owed
/// nothing, which is every session between the operator's verbs.
pub fn take_notices(session: &str) -> Vec<String> {
    owed()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session)
        .unwrap_or_default()
}

/// The oldest sentence this session is owed, taken: for a turn already running, which reads one
/// at a boundary rather than joining a batch nobody typed.
pub fn take_one_notice(session: &str) -> Option<String> {
    let mut g = owed().lock().unwrap_or_else(|e| e.into_inner());
    let held = g.get_mut(session)?;
    let one = held.remove(0);
    if held.is_empty() {
        g.remove(session);
    }
    Some(one)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_late_grant_is_one_call_and_is_spent_by_it() {
        let s = "s-late-1";
        let args = json!({"command": "cat > /var/tmp/x <<'EOF'\nhi\nEOF"});
        let kept = grant(s, "bash", &args, "adj-1", 1_000);
        assert_eq!(kept.until_ms, 1_000 + LATE_GRANT_MS);
        assert_eq!(
            take(s, "bash", &args, 2_000).map(|g| g.why),
            Some("adj-1".to_string())
        );
        assert_eq!(
            take(s, "bash", &args, 2_000),
            None,
            "a late grant is one call's"
        );
    }

    #[test]
    fn a_different_call_in_the_same_session_is_not_covered() {
        let s = "s-late-2";
        grant(s, "bash", &json!({"command": "ls"}), "adj-2", 1_000);
        // Another tool, and the same tool with other arguments: two new questions.
        assert_eq!(take(s, "write", &json!({"command": "ls"}), 2_000), None);
        assert_eq!(take(s, "bash", &json!({"command": "ls -l"}), 2_000), None);
        assert!(
            take(s, "bash", &json!({"command": "ls"}), 2_000).is_some(),
            "and the call it was made for is still there"
        );
    }

    #[test]
    fn a_grant_that_waited_past_its_window_is_gone() {
        let s = "s-late-3";
        let args = json!({"command": "id"});
        grant(s, "bash", &args, "adj-3", 1_000);
        assert_eq!(take(s, "bash", &args, 1_000 + LATE_GRANT_MS + 1), None);
    }

    #[test]
    fn a_grant_is_one_session_s() {
        let args = json!({"command": "id"});
        grant("s-late-4", "bash", &args, "adj-4", 1_000);
        assert_eq!(take("s-late-5", "bash", &args, 2_000), None);
    }

    #[test]
    fn a_sentence_left_for_a_session_is_its_and_is_handed_over_once() {
        queue_notice("s-late-6", "the operator granted `bash`".into());
        queue_notice("s-late-6", "and said so again".into());
        assert_eq!(
            take_one_notice("s-late-6").as_deref(),
            Some("the operator granted `bash`"),
            "a running turn reads them one at a boundary"
        );
        assert_eq!(
            take_notices("s-late-6"),
            vec!["and said so again".to_string()],
            "and the rest is still owed"
        );
        assert!(take_notices("s-late-6").is_empty(), "taken, not read twice");
        assert!(take_notices("s-late-7").is_empty(), "one session's");
    }
}
