//! Who is waiting on a gate's verdict, and the one row each is woken with when it answers.
//!
//! The wait is a `queue_told` row, and the verdict TAKES it: so a verdict is one row per waiting
//! session however often a pass re-reads the health, and a session that asks again (`restart`)
//! is woken again by the next one.

use letibot_tokencore::store::{MergeState, ReviewRow, ReviewState, Store, StoreError};

use super::gate::now_ms;

/// What every wait's key starts with, so nothing else's `queue_told` row is ever taken for one.
pub const PREFIX: &str = "wait:";

/// How long the one line a notice carries may be - the pane and `queue status` have the rest.
pub const WHY_MAX: usize = 240;

/// What a session is waiting on: a target's gate, or one row's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waiting {
    Target(String),
    /// A row by its REVIEW id. That id is the row the session put up and the one it can look up,
    /// and it survives the hand-off to the merge queue - so one wait covers both its gates.
    Row(String),
}

impl Waiting {
    /// The body of the key: `t:main`, `r:r-abc`.
    pub fn body(&self) -> String {
        match self {
            Waiting::Target(t) => format!("t:{t}"),
            Waiting::Row(id) => format!("r:{id}"),
        }
    }

    /// How a sentence names it: `target \`main\``, `row \`r-abc\``.
    fn named(&self) -> String {
        match self {
            Waiting::Target(t) => format!("target `{t}`"),
            Waiting::Row(id) => format!("row `{id}`"),
        }
    }
}

fn key(repository: &str, what: &Waiting, session: &str) -> String {
    format!("{PREFIX}{repository}:{}:{session}", what.body())
}

/// Write down that `session` waits on `what` of `repository`. Idempotent: a session that asks
/// twice is one waiter, and a session that names nobody (an operator, a bridge) is no waiter.
pub fn watch(
    store: &Store,
    repository: &str,
    what: &Waiting,
    session: &str,
) -> Result<(), StoreError> {
    if session.is_empty() {
        return Ok(());
    }
    store.tell_once(&key(repository, what, session), now_ms())?;
    Ok(())
}

/// The sessions waiting on `what`, TAKEN as they are answered.
pub fn take(store: &Store, repository: &str, what: &Waiting) -> Result<Vec<String>, StoreError> {
    let prefix = format!("{PREFIX}{repository}:{}:", what.body());
    let mut out: Vec<String> = Vec::new();
    for (k, _) in store.told_with_prefix(&prefix)? {
        let session = k.get(prefix.len()..).unwrap_or_default().to_string();
        store.untell_prefix(&k)?;
        if !session.is_empty() && !out.contains(&session) {
            out.push(session);
        }
    }
    Ok(out)
}

/// The one row a waiter is woken with.
pub fn notice(what: &Waiting, sha: &str, verdict: &str, why: &str) -> String {
    let at = match sha.trim().is_empty() {
        true => String::new(),
        false => format!(" at {}", super::capacity::short(sha)),
    };
    format!(
        "[queue] the gate of {}{at} is {verdict}: {}; queue status for the evidence",
        what.named(),
        one_line(why, WHY_MAX)
    )
}

/// The first line of `s`, cut to `n` characters: a notice is one row in a transcript, and the
/// queue's evidence can be paragraphs of gate output. A full stop goes, because the sentence is
/// continued by the clause after it.
fn one_line(s: &str, n: usize) -> String {
    let first = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .trim_end_matches('.');
    match first.chars().count() > n {
        true => format!("{}…", first.chars().take(n).collect::<String>().trim_end()),
        false => first.to_string(),
    }
}

/// The session a verb's actor names, when the actor IS a session: `agent:s-1`, `operator:s-1`.
/// A `bridge:<handle>` is a Claude Code seat, and the bridge's own push already tells it every
/// move of the queue it follows.
pub fn session_of(who: &str) -> Option<&str> {
    let (kind, name) = who.split_once(':')?;
    matches!(kind, "agent" | "operator")
        .then_some(name)
        .filter(|n| !n.is_empty())
}

/// Whether a landing's move is a verdict its waiter is owed. `queued` and `gating` are the queue
/// moving, not a gate answering, and a person's `withdrawn` is theirs.
pub fn merge_verdict(to: MergeState) -> bool {
    matches!(
        to,
        MergeState::Landed
            | MergeState::LandedLocal
            | MergeState::Failed
            | MergeState::TimedOut
            | MergeState::Conflict
            | MergeState::Blocked
            | MergeState::Moved
    )
}

/// Who a person's withdrawal of a review row is told, once: the live sessions that waited on it,
/// and the root session that put it up - or, when that root is gone, the project's live roots.
/// The mover is never told what they just did.
pub fn withdrawn_recipients(
    waiters: &[String],
    owner_root: &str,
    mover: Option<&str>,
    project_roots: &[String],
    live: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !s.is_empty() && Some(s) != mover && live(s) && !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    };
    for w in waiters {
        add(w);
    }
    if !owner_root.is_empty() && live(owner_root) {
        add(owner_root);
    } else {
        for r in project_roots {
            add(r);
        }
    }
    out
}

/// The one row a withdrawn review row's owner is told.
pub fn withdrawn_notice(r: &ReviewRow, actor: &str) -> String {
    format!(
        "[queue] review row `{}` (`{}`) was withdrawn by the {actor}: nothing will review or land \
         it. The branch and its worktree stay",
        r.id, r.branch
    )
}

/// Whether a review row's move is a verdict: the reviewer's refusal or block, and a row the
/// target's own movement made moot (`superseded by <tip>`, which is nobody's refusal).
pub fn review_verdict(to: ReviewState, why: &str) -> bool {
    match to {
        ReviewState::Rejected | ReviewState::CannotDecide | ReviewState::Blocked => true,
        ReviewState::Withdrawn => why.starts_with("superseded"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (Store, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!(
            "letibot-waits-{tag}-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&p);
        (Store::open(&p).unwrap(), p)
    }

    #[test]
    fn a_wait_is_written_once_taken_once_and_taken_again_only_when_asked_again() {
        let (s, p) = store("once");
        let row = Waiting::Row("r-a".into());
        watch(&s, "/r", &row, "s-1").unwrap();
        watch(&s, "/r", &row, "s-1").unwrap();
        watch(&s, "/r", &row, "s-2").unwrap();
        watch(&s, "/r", &Waiting::Target("main".into()), "s-1").unwrap();
        assert_eq!(
            take(&s, "/r", &row).unwrap(),
            vec!["s-1".to_string(), "s-2".to_string()],
            "one row per waiter, both of them"
        );
        assert!(take(&s, "/r", &row).unwrap().is_empty(), "taken is taken");
        assert_eq!(
            take(&s, "/r", &Waiting::Target("main".into())).unwrap(),
            vec!["s-1".to_string()],
            "another thing waited on is another row"
        );
        watch(&s, "/r", &row, "s-1").unwrap();
        assert_eq!(
            take(&s, "/r", &row).unwrap(),
            vec!["s-1".to_string()],
            "a session that asks again waits again"
        );
        assert!(
            take(&s, "/other", &row).unwrap().is_empty(),
            "another repository's waiter is not this one's"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_waiter_with_no_session_is_nobody_and_a_notice_is_one_line() {
        let (s, p) = store("nobody");
        let row = Waiting::Row("r-a".into());
        watch(&s, "/r", &row, "").unwrap();
        assert!(take(&s, "/r", &row).unwrap().is_empty(), "a nameless ask");

        assert_eq!(
            notice(
                &row,
                "0123456789abcdef",
                "failed",
                "the gate's step failed.\n\ncargo test"
            ),
            "[queue] the gate of row `r-a` at 0123456 is failed: the gate's step failed; \
             queue status for the evidence"
        );
        assert_eq!(
            notice(&Waiting::Target("main".into()), "", "green", "it passed"),
            "[queue] the gate of target `main` is green: it passed; \
             queue status for the evidence",
            "a sha nobody judged is not printed as an empty one"
        );
        let long = notice(&row, "abc1234", "red", &"x".repeat(400));
        assert!(
            long.contains(&format!("{}…", "x".repeat(WHY_MAX))),
            "{long}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn only_a_session_is_a_waiter_and_only_a_gate_answering_is_a_verdict() {
        assert_eq!(session_of("agent:s-1"), Some("s-1"));
        assert_eq!(session_of("operator:s-2"), Some("s-2"));
        assert_eq!(
            session_of("bridge:cc@letibot"),
            None,
            "the bridge is told by its own push"
        );
        assert_eq!(session_of("test"), None);
        assert_eq!(session_of("agent:"), None);

        assert!(merge_verdict(MergeState::Landed));
        assert!(merge_verdict(MergeState::Conflict));
        assert!(
            !merge_verdict(MergeState::Queued),
            "the queue moving is not a verdict"
        );
        assert!(!merge_verdict(MergeState::Gating));

        assert!(review_verdict(
            ReviewState::Rejected,
            "the reviewer said no"
        ));
        assert!(
            review_verdict(
                ReviewState::Withdrawn,
                "superseded by abc: a target review is about one tip"
            ),
            "the target moving past the tip is a verdict; a person's `rm` is not"
        );
        assert!(!review_verdict(
            ReviewState::Withdrawn,
            "withdrawn by operator:s-1: not this one"
        ));
        assert!(!review_verdict(ReviewState::Approved, "accepted"));
    }
}
