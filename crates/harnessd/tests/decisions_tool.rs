//! **`decisions` against a real corpus.**
//!
//! The unit tests in `letibot-tools` prove the reporting against a fake source.
//! This proves the SQL: that `by=oracle` reaches `oracle:glm` through a prefix
//! match, that `asked` is not the same question as "who decided", and that the
//! miss report's "values present" come from the SCOPE rather than from the empty
//! result set — which is the bug a naive implementation has, and the one that
//! turns a helpful miss into "no rows, and nothing present either".

use std::sync::Arc;

use letibot_harnessd::decision_source::StoreDecisions;
use letibot_tokencore::store::{NewAdjudication, SessionRecord, Store};
use letibot_tools::builtins::decisions::{DecisionQuery, DecisionSource};

fn adj(request_id: &str, session_id: &str) -> NewAdjudication {
    NewAdjudication {
        request_id: request_id.into(),
        session_id: session_id.into(),
        turn_id: "t1".into(),
        action: "read a file".into(),
        baseline: "read".into(),
        tier: "may_approve".into(),
        trail_json: "[]".into(),
        shown: Some("brief — a file was read".into()),
        // R11: both halves of the exchange are on the row now, and this fixture carries
        // them so a reader that renders one cannot quietly drop the other.
        reply: Some("It follows from what was asked.\nALLOW 0".into()),
        // R11: the flag the counts read. This fixture's oracle spoke.
        consulted: Some(true),
        // R12: this row's answer was a verdict and not an unsure, so there is no reading.
        oracle_reading: None,
        tool: "bash".into(),
        arguments_json: "{}".into(),
        mode: "automode-edits".into(),
        options_json: "[]".into(),
        agent: "leticode".into(),
        model_verdict: None,
        verdict: None,
        verdict_by: None,
        verdict_basis: None,
        p_allow: None,
        oracle_ms: None,
        oracle_model: None,
        brief_sha: None,
        effect: "admit".into(),
        asked: false,
        shape: None,
        shape_class: None,
    }
}

/// A corpus with the three kinds of decision that actually occur: one an oracle
/// measured and admitted, one a person was asked and refused, and one a boundary
/// rule settled with nobody asked at all.
fn corpus(dir: &std::path::Path) -> (Store, String) {
    let store = Store::open(&dir.join("sessions.db")).expect("opening");
    let session_id = "s-gate".to_string();
    store
        .put_session(&SessionRecord {
            id: session_id.clone(),
            title: Some("the gate session".into()),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("session");

    let mut oracle = adj("adj-s-gate-0225", &session_id);
    oracle.verdict = Some("allow".into());
    oracle.verdict_by = Some("oracle:glm".into());
    oracle.verdict_basis = Some("the operator asked for the man page sweep".into());
    oracle.oracle_ms = Some(2_100);
    oracle.oracle_model = Some("glm-5.3-flash".into());
    oracle.p_allow = Some(0.94);
    oracle.shown = Some("=== brief ===\ncommand: ar t libfoo.a".into());
    assert!(
        store.record_adjudication(&oracle).expect("oracle row"),
        "oracle row was not written"
    );

    let mut human = adj("adj-s-gate-0226", &session_id);
    // **A person decided this one, so no model was asked.** `adj()`'s default is `true`
    // because the ORACLE row is the one that fixture is about; carried over to these two it
    // made `measured` count every row in the session, which is the assertion below's whole
    // subject (`counts_are_per_session_and_any_widens_to_everything` — *"one oracle
    // answered"*). Found while adding R12's column to this file.
    human.consulted = Some(false);
    human.tool = "write".into();
    human.verdict = Some("deny".into());
    human.verdict_by = Some("human:dead".into());
    human.verdict_basis = Some("dead chose `deny` at the head".into());
    human.asked = true;
    human.effect = "refuse".into();
    assert!(
        store.record_adjudication(&human).expect("human row"),
        "human row was not written"
    );

    let mut boundary = adj("adj-s-gate-0227", &session_id);
    boundary.consulted = Some(false);
    boundary.tool = "bash".into();
    boundary.verdict = Some("deny".into());
    boundary.verdict_by = Some("boundary:flow".into());
    boundary.verdict_basis = Some("network egress to an unseen host".into());
    boundary.tier = "blocked".into();
    boundary.effect = "refuse".into();
    assert!(
        store.record_adjudication(&boundary).expect("boundary row"),
        "boundary row was not written"
    );

    (store, session_id)
}

fn src(dir: &std::path::Path, session_id: &str) -> Arc<dyn DecisionSource> {
    Arc::new(
        StoreDecisions::open(&dir.join("sessions.db"), session_id.to_string()).expect("opening"),
    )
}

fn q() -> DecisionQuery {
    DecisionQuery {
        limit: 50,
        ..Default::default()
    }
}

/// The call this replaced: one request id, every field the sqlite3 query selected
/// by name, and the brief it did not think to ask for.
#[test]
fn one_request_comes_back_with_the_oracles_own_reason_and_its_brief() {
    let d = tempdir();
    let (_s, session_id) = corpus(d.path());
    let hits = src(d.path(), &session_id)
        .find(&DecisionQuery {
            request: Some("adj-s-gate-0225".into()),
            ..q()
        })
        .expect("finding");
    assert_eq!(hits.rows.len(), 1);
    let r = &hits.rows[0];
    assert_eq!(r.verdict, "allow");
    assert_eq!(r.verdict_by, "oracle:glm");
    assert_eq!(r.verdict_basis, "the operator asked for the man page sweep");
    assert_eq!(r.oracle_ms, Some(2_100));
    assert_eq!(r.p_allow, Some(0.94));
    assert!(r.shown.as_deref().unwrap().contains("ar t libfoo.a"));
}

/// `by` is a prefix, because the stored value joins the decider to its identity
/// with a colon and the question is almost always about the decider.
#[test]
fn by_matches_the_decider_not_the_whole_identity() {
    let d = tempdir();
    let (_s, session_id) = corpus(d.path());
    let src = src(d.path(), &session_id);

    for (want, id) in [
        ("oracle", "adj-s-gate-0225"),
        ("boundary", "adj-s-gate-0227"),
        // `operator` is the word the rest of the system uses; the corpus stores
        // `human`. Both reach the same row.
        ("operator", "adj-s-gate-0226"),
        ("human", "adj-s-gate-0226"),
    ] {
        let hits = src
            .find(&DecisionQuery {
                by: Some(want.into()),
                ..q()
            })
            .expect("finding");
        assert_eq!(hits.matched, 1, "`by={want}`");
        assert_eq!(hits.rows[0].request_id, id, "`by={want}`");
    }
}

/// `asked` and `by` are different questions. The boundary row was refused with
/// nobody asked; the human row was refused with somebody asked. A tool that
/// conflated them would report two.
#[test]
fn asked_is_not_the_same_question_as_who_decided() {
    let d = tempdir();
    let (_s, session_id) = corpus(d.path());
    let src = src(d.path(), &session_id);

    let denied = src
        .find(&DecisionQuery {
            verdict: Some("deny".into()),
            ..q()
        })
        .expect("finding");
    assert_eq!(denied.matched, 2, "the human one and the boundary one");

    let denied_and_asked = src
        .find(&DecisionQuery {
            verdict: Some("deny".into()),
            asked: Some(true),
            ..q()
        })
        .expect("finding");
    assert_eq!(denied_and_asked.matched, 1);
    assert_eq!(denied_and_asked.rows[0].request_id, "adj-s-gate-0226");
}

/// **The bug this test exists for.** The miss report's "values present" must come
/// from the scope, not from the result set — otherwise a query that matched
/// nothing reports nothing present, and the model is back at sqlite.
#[test]
fn a_miss_still_reports_the_values_that_are_present_in_scope() {
    let d = tempdir();
    let (_s, session_id) = corpus(d.path());
    let hits = src(d.path(), &session_id)
        .find(&DecisionQuery {
            // A verdict spelled the way the wire enum spells it, which is the
            // common miss.
            verdict: Some("Selected".into()),
            ..q()
        })
        .expect("finding");
    assert_eq!(hits.matched, 0, "nothing matched");
    assert_eq!(hits.scanned, 3, "but the scope was read");
    assert_eq!(hits.verdicts_present, vec!["allow", "deny"]);
    assert!(hits.tools_present.contains(&"bash".to_string()));
    assert!(
        hits.deciders_present.contains(&"oracle:glm".to_string()),
        "{:?}",
        hits.deciders_present
    );
}

/// The four counts, over one session and over the whole corpus.
#[test]
fn counts_are_per_session_and_any_widens_to_everything() {
    let d = tempdir();
    let (store, session_id) = corpus(d.path());
    // A second session with one decision, so `any` is bigger than `session`.
    store
        .put_session(&SessionRecord {
            id: "s-other".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("session");
    let mut other = adj("adj-s-other-0001", "s-other");
    other.verdict = Some("allow".into());
    other.verdict_by = Some("human:dead".into());
    other.asked = true;
    assert!(
        store.record_adjudication(&other).expect("other row"),
        "other row was not written"
    );

    let src = src(d.path(), &session_id);
    let mine = src.counts(None).expect("counting");
    assert_eq!(mine.total, 3);
    assert_eq!(mine.decided_by_operator, 1, "one person was actually asked");
    assert_eq!(mine.measured, 1, "one oracle answered");

    let all = src.counts(Some("any")).expect("counting");
    assert_eq!(all.total, 4);
    assert_eq!(all.decided_by_operator, 2);
}

/// A session that never gated anything reports zero without erroring — a
/// read-only seat never reaches the gate, and that is not a broken corpus.
#[test]
fn a_session_with_no_decisions_is_empty_not_an_error() {
    let d = tempdir();
    let (store, _) = corpus(d.path());
    store
        .put_session(&SessionRecord {
            id: "s-quiet".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("session");
    let hits = src(d.path(), "s-quiet").find(&q()).expect("finding");
    assert_eq!(hits.scanned, 0);
    assert_eq!(hits.matched, 0);
    assert!(hits.verdicts_present.is_empty(), "nothing in scope to list");
}

/// `last` caps the MATCH set, not the scan — `last=1` with a filter is the newest
/// one matching decision, not "whichever of the newest one matched".
#[test]
fn last_caps_the_matches() {
    let d = tempdir();
    let (_s, session_id) = corpus(d.path());
    let hits = src(d.path(), &session_id)
        .find(&DecisionQuery {
            verdict: Some("deny".into()),
            last: Some(1),
            ..q()
        })
        .expect("finding");
    assert_eq!(hits.matched, 1);
    assert_eq!(hits.rows.len(), 1);
}

struct Dir(std::path::PathBuf);
impl Dir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> Dir {
    // **A counter as well as the clock**: macOS's clock resolves microseconds, so two tests
    // in this process starting in the same one got the same directory, seeded the corpus
    // twice, and read `matched: 2` — measured 2026-10-07 under a full workspace run.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "letibot-decisions-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).expect("scratch dir");
    Dir(p)
}
