//! The turn monitor judges an action once. A finding about an action the session has already been
//! told about is not filed again at a later turn end - and the keys are in the store, so the turn a
//! restart wakes on finds it told too. The sequence is the rano one: a branch deleted long ago, a
//! notice about it, and empty turns after it.

use super::*;
use crate::shadow::{self, Job, record, runner};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The model's answer to every check: nothing, so the finding under test is the rule's own and the
/// verdict adds none of its own.
struct Silent;

impl shadow::judge::Judge for Silent {
    fn label(&self) -> String {
        "stub/judge".into()
    }
    fn judge(&self, _: &str, _: &str) -> Result<String, String> {
        Ok(String::new())
    }
}

/// The shadow session's own record, which these tests read nothing from.
struct Quiet;

impl record::Recorder for Quiet {
    fn row(&self, _: &str, _: &str) {}
}

/// A session over `dir`'s store, with the turn monitor on, and the channel a turn_end verdict
/// rings on.
fn a_session(dir: &Path, id: &str) -> (Harness, mpsc::Receiver<()>) {
    let mut cfg = Config::for_this_box(dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.join("sessions.db"));
    cfg.session_id = id.into();
    cfg.vocab_gguf = None;
    cfg.shadow = shadow::config::ShadowConfig::from_lines(
        letibot_provider::keys::ShadowLines {
            enabled: vec!["turn_end".into()],
            ..Default::default()
        },
        "local".into(),
    )
    .expect("turn_end is a job");
    at_always_ask(&mut cfg);
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    *parts.mode_store.write().unwrap() = crate::modes::ModeStore::default();
    let mut h = Harness::open(&parts, cfg.clone(), Hub::new(id)).expect("opens");
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let events: shadow::Events = Default::default();
    let ev = events.clone();
    let on = move |job: Job| -> runner::OnOutcome {
        let (ev, tx) = (ev.clone(), tx.lock().unwrap().clone());
        Box::new(move |position, outcome| {
            if job == Job::TurnEnd && *outcome != runner::Outcome::Superseded {
                ev.lock().unwrap().push_back(shadow::Event {
                    job,
                    position,
                    outcome: outcome.clone(),
                });
                let _ = tx.send(());
            }
        })
    };
    let rows: Arc<dyn record::Recorder> = Arc::new(Quiet);
    let s = runner::Shadow::start(
        &cfg.shadow,
        "deepseek/fake",
        None,
        rows.clone(),
        &|_, _| Arc::new(Silent),
        &runner::Timing::of,
        &on,
    )
    .expect("turn_end is on");
    h.shadow = shadow::Slot::On {
        shadow: Arc::new(s),
        events,
        recorder: rows,
        session_id: shadow::session_id(&cfg.session_id),
    };
    (h, rx)
}

/// The monitor's rows: the notice the head draws and the model reads in its context.
fn monitor_rows(h: &Harness) -> Vec<String> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Notice {
                kind: letibot_transcript::NoticeKind::Monitor,
                text,
                ..
            } if text.starts_with("[monitor]") => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// One row, appended through the store the way the daemon's own turn did: the transcript a
/// reopened harness reads is the store's, so the fixture's rows have to be there.
fn stage(dir: &Path, id: &str, items: &[TranscriptItem]) {
    let store = Store::open(&dir.join("sessions.db")).expect("the store");
    let transcript = store
        .current_transcript_id(id)
        .expect("the store is readable")
        .expect("the open wrote this session's transcript");
    let prefix_id = store
        .transcript_prefix_id(&transcript)
        .expect("readable")
        .expect("a transcript has a prefix");
    let (prefix_tokens, mut h) = store
        .get_stable_prefix(&prefix_id)
        .expect("the prefix is readable");
    let tokens: Vec<u32> = vec![7, 8, 9];
    for (seq, item) in items.iter().enumerate() {
        h = letibot_tokencore::ledger::chain(&h, &tokens);
        let row = letibot_tokencore::ledger::LedgerRow {
            item_id: format!("{transcript}.{seq}"),
            tok_offset: (prefix_tokens.len() + seq * tokens.len()) as u32,
            tok_len: tokens.len() as u32,
            h_k: h,
        };
        store
            .append_item(&transcript, seq as u32, item, &row, &tokens)
            .expect("the row is appended");
    }
}

/// The turn that deleted a branch and answered, as the rows the monitor reads.
fn the_branch_deletion() -> Vec<TranscriptItem> {
    vec![
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![letibot_transcript::UserPart::Text {
                text: "clean up the stale branches".into(),
            }],
        },
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call-branch".into(),
                name: "bash".into(),
                arguments: r#"{"command":"git branch -D stale"}"#.into(),
            }],
            truncated: false,
        },
        TranscriptItem::ToolResult {
            call_id: "call-branch".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "Deleted branch stale".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
            ran_ms: None,
            decision: None,
        },
        TranscriptItem::Assistant {
            text: "Deleted the stale branch.".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    ]
}

/// A session that has NOT got the store's lock already: the fixture opens one to write its rows
/// and then hands the file to the harness.
fn a_staged_session(dir: &Path, id: &str) -> (Harness, mpsc::Receiver<()>) {
    std::fs::create_dir_all(dir).unwrap();
    {
        let (h, rx) = a_session(dir, id);
        drop(h);
        // The open made the transcript and the session row; the turn is what a daemon wrote before
        // it stopped.
        stage(dir, id, &the_branch_deletion());
        drop(rx);
    }
    a_session(dir, id)
}

/// The rano sequence: one action told once, at the turn it happened, and never again at the empty
/// turn ends after it or at the turn a restart wakes.
#[test]
fn a_monitor_finding_is_filed_once_across_turn_ends_and_a_restart() {
    let id = "s-monitor-once";
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "monitor-once-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (mut h, rang) = a_staged_session(&dir, id);
    let found = "[monitor] beyond the ask: deleted a branch: git branch -D stale";
    h.submit_turn_end("Deleted the stale branch.");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict rings the wake");
    assert!(
        h.wake().expect("the wake runs").is_none(),
        "a finding is no turn"
    );
    assert_eq!(monitor_rows(&h), vec![found.to_string()]);

    // The turn after it did nothing, and the monitor re-reads every row since the operator's last
    // prompt - the same branch, the same finding - and says nothing.
    h.submit_turn_end("");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the second verdict rings the wake");
    assert!(h.wake().expect("the wake runs").is_none());
    assert_eq!(
        monitor_rows(&h),
        vec![found.to_string()],
        "the same finding was filed twice at one daemon"
    );

    // A restart: the store carries what was told, so the turn it wakes on finds the action met.
    drop(h);
    let (mut h, rang) = a_session(&dir, id);
    assert_eq!(monitor_rows(&h), vec![found.to_string()]);
    h.submit_turn_end("");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict after the restart rings the wake");
    assert!(h.wake().expect("the wake runs").is_none());
    assert_eq!(
        monitor_rows(&h),
        vec![found.to_string()],
        "a restart re-armed a finding the session had already read"
    );
}

/// A second action is a second finding: the record is keyed by the action, not by the monitor.
#[test]
fn a_different_action_is_told_after_the_first_one() {
    let id = "s-monitor-twice";
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "monitor-twice-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (mut h, rang) = a_staged_session(&dir, id);
    h.submit_turn_end("Deleted the stale branch.");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict rings the wake");
    assert!(h.wake().expect("the wake runs").is_none());
    assert_eq!(monitor_rows(&h).len(), 1);

    // A second branch deleted in a later turn of the same conversation.
    h.session.items.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: "call-tags".into(),
            name: "bash".into(),
            arguments: r#"{"command":"git branch -D tags-old"}"#.into(),
        }],
        truncated: false,
    });
    h.session.items.push(TranscriptItem::ToolResult {
        call_id: "call-tags".into(),
        name: "bash".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: "Deleted branch tags-old".into(),
        edits: Vec::new(),
        origin: None,
        media: None,
        ran_ms: None,
        decision: None,
    });
    h.submit_turn_end("");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the third verdict rings the wake");
    assert!(h.wake().expect("the wake runs").is_none());
    let rows = monitor_rows(&h);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[1].contains("tags-old"), "{rows:?}");
}
