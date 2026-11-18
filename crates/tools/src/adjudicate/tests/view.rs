//! May a path the boundary hid become a question, and is it asked once? A grant on a directory
//! covers what is under it, a grant made for one subagent answers for its siblings and its
//! parent, and a card already up for a path is not raised a second time.

use super::*;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::runtime::ViewGrant;
use crate::view::{ViewAccess, ViewGrants};

/// An adjudicator that answers every card with one option and keeps what it was shown: a test
/// counts questions as well as reading them, because "no card" is the claim most of these make.
#[derive(Clone)]
struct Cards {
    answer: &'static str,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    /// The ladder each card offered: a test reads what a person could have picked, not only that
    /// a card went up.
    ladders: Arc<Mutex<Vec<Vec<DecisionOption>>>>,
}

impl Cards {
    fn new(answer: &'static str) -> Cards {
        Cards {
            answer,
            seen: Arc::new(Mutex::new(Vec::new())),
            ladders: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The option ids of the first card, in the order it drew them.
    fn first_ladder(&self) -> Vec<String> {
        self.ladders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first()
            .map(|l| l.iter().map(|o| o.id.clone()).collect())
            .unwrap_or_default()
    }

    fn count(&self) -> usize {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn summaries(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(s, _)| s.clone())
            .collect()
    }
}

impl Adjudicator for Cards {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((req.summary.clone(), req.target.clone()));
        self.ladders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req.options.clone());
        AdjudicationDecision::selected(req, self.answer, "human:test", "because")
    }

    fn describe(&self) -> String {
        "asked at the head: a test".into()
    }
}

/// One session's gate over a tree's grant record. `asker` is the subagent's handle, which a
/// child's gate carries and the operator's own does not.
fn gate(adjudicator: Cards, view: &Arc<ViewGrants>, asker: Option<&str>) -> AdjudicatedGate {
    AdjudicatedGate::new(Box::new(adjudicator))
        .with_identity("s-test", "agent")
        .with_view_grants(Arc::clone(view))
        .with_asker(asker.map(str::to_string))
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn a_file_under_a_granted_directory_is_read_and_written_without_a_card() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_rw");
    let mut g = gate(cards.clone(), &view, None);
    assert_eq!(
        g.grant_view(
            Path::new("/build/letibot-r114/review/"),
            "bash",
            ViewAccess::Read
        ),
        ViewGrant::Granted { writable: true },
        "the directory itself is the question the operator answered"
    );
    // The same subagent, a moment later, naming the file it was granted the directory for.
    assert_eq!(
        g.grant_view(
            Path::new("/build/letibot-r114/review/chunk02.jsonl"),
            "bash",
            ViewAccess::Read
        ),
        ViewGrant::Covered { writable: true },
        "a grant on a directory covers every path under it"
    );
    assert_eq!(
        g.grant_view(
            Path::new("/build/letibot-r114/review/chunk02.jsonl"),
            "bash",
            ViewAccess::Write
        ),
        ViewGrant::Covered { writable: true },
        "and a read-write grant covers the writes below it too"
    );
    assert_eq!(
        cards.count(),
        1,
        "one path, and every path under it, is one card"
    );
}

#[test]
fn a_read_only_grant_covers_a_read_below_it_and_a_write_still_asks() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_ro");
    let mut g = gate(cards.clone(), &view, None);
    assert_eq!(
        g.grant_view(
            Path::new("/home/dead/sdb-fuzz-skills/"),
            "bash",
            ViewAccess::Read
        ),
        ViewGrant::Granted { writable: false }
    );
    assert_eq!(
        g.grant_view(
            Path::new("/home/dead/sdb-fuzz-skills/briefs/full-pgsession.md"),
            "bash",
            ViewAccess::Read
        ),
        ViewGrant::Covered { writable: false }
    );
    assert_eq!(
        cards.count(),
        1,
        "a read below a read grant is not a question"
    );
    assert_eq!(
        g.grant_view(
            Path::new("/home/dead/sdb-fuzz-skills/briefs/full-pgsession.md"),
            "bash",
            ViewAccess::Write
        ),
        ViewGrant::Granted { writable: false },
        "a write is the wider ask, and a read-only grant is not an answer to it"
    );
    assert_eq!(cards.count(), 2, "the write was asked about");
}

#[test]
fn a_granted_directory_does_not_cover_a_name_that_only_starts_the_same_way() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_ro");
    let mut g = gate(cards.clone(), &view, None);
    assert_eq!(
        g.grant_view(Path::new("/build"), "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false }
    );
    assert_eq!(
        g.grant_view(Path::new("/build/letibot-r114/x"), "bash", ViewAccess::Read),
        ViewGrant::Covered { writable: false }
    );
    assert_eq!(
        g.grant_view(Path::new("/build-other/y"), "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false },
        "`/build-other` is a different directory, whatever its name starts with"
    );
    assert_eq!(cards.count(), 2);
}

#[test]
fn a_sibling_subagent_does_not_ask_for_what_its_sibling_was_granted() {
    let view = Arc::new(ViewGrants::new());
    let first = Cards::new("grant_rw");
    let mut sub_a = gate(first.clone(), &view, Some("sub-1794122961532460811"));
    let sibling = Cards::new("grant_rw");
    let mut sub_b = gate(sibling.clone(), &view, Some("sub-1794122966502117730"));
    let parent_cards = Cards::new("grant_rw");
    let mut parent = gate(parent_cards.clone(), &view, None);

    assert_eq!(
        sub_a.grant_view(
            Path::new("/build/letibot-r114/review/"),
            "bash",
            ViewAccess::Read
        ),
        ViewGrant::Granted { writable: true }
    );
    assert_eq!(
        sub_b.grant_view(
            Path::new("/build/letibot-r114/review/"),
            "bash",
            ViewAccess::Read
        ),
        ViewGrant::Covered { writable: true },
        "a sibling's grant is the tree's"
    );
    assert_eq!(
        parent.grant_view(
            Path::new("/build/letibot-r114/review/"),
            "bash",
            ViewAccess::Read
        ),
        ViewGrant::Covered { writable: true },
        "and the session that spawned both of them"
    );
    assert_eq!(first.count(), 1);
    assert_eq!(
        sibling.count(),
        0,
        "the sibling asked once, and got no card"
    );
    assert_eq!(parent_cards.count(), 0);
}

#[test]
fn a_denial_is_the_call_s_own_and_the_next_ask_is_a_card_again() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("deny");
    let mut g = gate(cards.clone(), &view, None);
    assert!(
        matches!(
            g.grant_view(Path::new("/build/x"), "bash", ViewAccess::Read),
            ViewGrant::Refused(_)
        ),
        "a denial is a refusal, and it says so"
    );
    assert!(matches!(
        g.grant_view(Path::new("/build/x"), "bash", ViewAccess::Read),
        ViewGrant::Refused(_)
    ));
    assert_eq!(
        cards.count(),
        2,
        "a deny stays per call: it does not become a standing denial"
    );
    assert_eq!(
        view.entries(),
        Vec::new(),
        "and nothing was recorded as granted"
    );
}

/// The one-card rule, driven rather than raced: this adjudicator holds the card it is asked
/// about until a sibling has taken its place behind it, which is the moment the second ask
/// would have put a second card up.
struct WaitsForASibling {
    view: Arc<ViewGrants>,
    path: PathBuf,
    cards: Cards,
}

impl Adjudicator for WaitsForASibling {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        let counted = self.cards.decide(req);
        // Bounded on purpose: a sibling that never arrives fails the count below, and this
        // test must not hang waiting for it.
        for _ in 0..600 {
            if self.view.waiting_on(&self.path) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        counted
    }

    fn describe(&self) -> String {
        "asked at the head: a test".into()
    }
}

#[test]
fn two_asks_for_one_path_at_once_are_one_card_and_its_answer_releases_both() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_rw");
    let path = p("/build/letibot-r114/review/chunk02.jsonl");
    let mut first = AdjudicatedGate::new(Box::new(WaitsForASibling {
        view: Arc::clone(&view),
        path: path.clone(),
        cards: cards.clone(),
    }))
    .with_identity("s-a", "agent")
    .with_view_grants(Arc::clone(&view))
    .with_asker(Some("sub-a".into()));

    let sibling_view = Arc::clone(&view);
    let sibling_path = path.clone();
    let sibling_cards = Cards::new("grant_rw");
    let theirs_cards = sibling_cards.clone();
    let second = std::thread::spawn(move || {
        let mut g = gate(sibling_cards, &sibling_view, Some("sub-b"));
        g.grant_view(&sibling_path, "bash", ViewAccess::Read)
    });

    let mine = first.grant_view(&path, "bash", ViewAccess::Read);
    let theirs = second.join().expect("the sibling thread");
    // Which thread raises the card is the scheduler's choice; one is granted, the other released.
    let mut got = [mine, theirs];
    got.sort_by_key(|g| matches!(g, ViewGrant::Covered { .. }));
    assert_eq!(
        got,
        [
            ViewGrant::Granted { writable: true },
            ViewGrant::Covered { writable: true }
        ],
        "one asker's card was answered and the other waited on it and was released"
    );
    assert_eq!(
        cards.count() + theirs_cards.count(),
        1,
        "two concurrent asks for one path are one card"
    );
}

#[test]
fn the_card_says_which_subagent_asked() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_ro");
    let mut g = gate(cards.clone(), &view, Some("sub-1794122961532460811"));
    let path = "/build/letibot-r114/review/chunk02.jsonl";
    let _ = g.grant_view(Path::new(path), "bash", ViewAccess::Read);
    let said = cards.summaries();
    assert_eq!(said.len(), 1);
    assert!(
        said[0].contains("sub-1794122961532460811"),
        "the card names the child that asked: {}",
        said[0]
    );
    assert!(
        said[0].contains(path),
        "and the path it is about: {}",
        said[0]
    );
    let target = cards
        .seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .first()
        .map(|(_, t)| t.clone())
        .unwrap_or_default();
    assert_eq!(target, path);
}

#[test]
fn a_write_the_view_refuses_says_which_of_the_two_it_was() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_rw");
    let mut g = gate(cards.clone(), &view, None);
    let _ = g.grant_view(Path::new("/build/x"), "bash", ViewAccess::Write);
    let seen = cards.seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].0.contains("read-only") && seen[0].0.contains("/build/x"),
        "a write ask is not a read ask with a different path: {}",
        seen[0].0
    );
}

/// A write refusal is a wider ask than the same path's read, which the session already holds: the
/// card must not offer the rung that changes nothing, and the first option must grant the write.
#[test]
fn a_write_refusal_offers_the_write_and_never_a_read_only_rung() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_rw");
    let mut g = gate(cards.clone(), &view, None);
    assert_eq!(
        g.grant_view(
            Path::new("/home/dead/.gitconfig"),
            "bash",
            ViewAccess::Write
        ),
        ViewGrant::Granted { writable: true }
    );
    let ladder = cards.first_ladder();
    assert!(
        !ladder.iter().any(|id| id == "grant_ro"),
        "a read-only rung on a write refusal is a button that changes nothing: {ladder:?}"
    );
    assert_eq!(
        ladder.first().map(String::as_str),
        Some("grant_rw"),
        "Enter on an untouched write card has to grant the write: {ladder:?}"
    );
    assert_eq!(ladder.last().map(String::as_str), Some("deny"));
}

/// One card can grant a whole folder: the operator answered four cards for four paths inside one
/// worktree, each granting only the path it named. The card offers the tree, and one answer on it
/// covers every sibling afterwards, with no second card.
#[test]
fn one_answer_on_the_folder_covers_every_path_below_it() {
    let dir = crate::backend::tempdir::TempDir::new();
    let tree = dir.path().join("leakidx-wt");
    std::fs::create_dir_all(tree.join("b")).expect("a sibling directory");
    std::fs::write(tree.join(".git"), "gitdir: /elsewhere/.git/worktrees/w\n")
        .expect("a worktree's git file");
    std::fs::write(tree.join("b/c"), "x\n").expect("a sibling file");
    let tree = tree.canonicalize().expect("a real directory");
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_ro_folder");
    let mut g = gate(cards.clone(), &view, None);

    // `a` does not exist: the card offers the worktree's root, and the operator takes it.
    let asked = tree.join("a");
    assert_eq!(
        g.grant_view(&asked, "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false }
    );
    let ladder = cards.first_ladder();
    assert!(
        ladder.iter().any(|id| id == "grant_ro_folder"),
        "the card offers the tree: {ladder:?}"
    );
    // The record is the folder, so every sibling is covered and no second card goes up.
    assert_eq!(view.covers(&tree, ViewAccess::Read), Some(false));
    assert_eq!(
        g.grant_view(&tree.join("b/c"), "read", ViewAccess::Read),
        ViewGrant::Covered { writable: false }
    );
    assert_eq!(cards.count(), 1, "one answer on the tree, one card");
    // And the write below it is still the wider ask: a read-only answer did not cover it.
    let _ = g.grant_view(&tree.join("b/c"), "bash", ViewAccess::Write);
    assert_eq!(
        cards.count(),
        2,
        "a read-only answer on the tree does not cover a write below it"
    );
}

/// The read refusal keeps its read rung, first: that is the card an operator answers most often and
/// the narrower grant is the one Enter should take.
#[test]
fn a_read_refusal_still_offers_the_read_rung_first() {
    let view = Arc::new(ViewGrants::new());
    let cards = Cards::new("grant_ro");
    let mut g = gate(cards.clone(), &view, None);
    assert_eq!(
        g.grant_view(
            Path::new("/home/dead/skills/full.md"),
            "read",
            ViewAccess::Read
        ),
        ViewGrant::Granted { writable: false }
    );
    let ladder = cards.first_ladder();
    assert_eq!(
        &ladder[..2],
        ["grant_ro", "grant_rw"],
        "a read refusal keeps both grants, narrowest first: {ladder:?}"
    );
    assert_eq!(ladder.last().map(String::as_str), Some("deny"));
}

/// Keeps the tier and whether a trail came with each view ask, and refuses it.
#[derive(Clone, Default)]
struct Spy {
    seen: Arc<Mutex<Vec<(Tier, bool)>>>,
}

impl Adjudicator for Spy {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((req.tier.clone(), req.trail.was_collected()));
        AdjudicationDecision::selected(req, "deny", "human:test", "no")
    }

    fn describe(&self) -> String {
        "asked at the head: a spy".into()
    }
}

fn operator_trail() -> crate::authorise::AuthorisationTrail {
    crate::authorise::AuthorisationTrail::from_messages(
        vec![crate::authorise::Utterance::operator(
            "read the review notes under /build",
            0,
        )],
        1,
    )
}

/// A secret store is the operator's every time: its view ask is always-ask, never a literal
/// may-approve a model could widen, and every view ask carries the asker's trail.
#[test]
fn view_ask_for_credential_path_is_always_ask() {
    let spy = Spy::default();
    let mut g = AdjudicatedGate::new(Box::new(spy.clone()))
        .with_identity("s-test", "agent")
        .with_trail_source(|_| operator_trail());
    for path in ["/home/u/.aws/credentials", "/build/letibot-r1"] {
        g.grant_view(Path::new(path), "bash", ViewAccess::Read);
    }
    let seen = spy.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(
        matches!(
            seen[0].0,
            Tier::AlwaysAsk {
                rule: "credential_use",
                ..
            }
        ),
        "{:?}",
        seen[0].0
    );
    assert_eq!(seen[1].0, Tier::MayApprove);
    assert!(seen.iter().all(|(_, trail)| *trail), "{seen:?}");
}

/// An oracle answers `allow_once`, the call vocabulary. On a view ask that is the read-only rung,
/// never a refusal and never read-write.
#[test]
fn an_oracle_allow_on_a_view_request_is_granted_read_only() {
    use crate::authorise::{ModelAdjudicator, OracleAnswer, OracleScope, ScriptedOracle, Widening};
    let oracle = ScriptedOracle::new("allows", |b: &mut crate::authorise::ModelBrief| {
        let w = b
            .adjudicable()
            .expect("a may-approve view ask has a witness");
        OracleAnswer::Authorised(Widening::new(w, b.request_id.clone(), vec![0], "asked"))
    })
    .with_scope(OracleScope::earned(
        [
            crate::intent::Intent::ReadFile,
            crate::intent::Intent::WriteFile,
        ]
        .into_iter()
        .collect(),
        EffectScope::HostOther,
        "test: reads and writes",
    ));
    let adj = ModelAdjudicator::new(Box::new(oracle), |_| unreachable!("the reading travels"));
    let mut g = AdjudicatedGate::new(Box::new(adj))
        .with_identity("s-test", "agent")
        .with_trail_source(|_| operator_trail());
    for (path, access) in [
        ("/build/letibot-r1", ViewAccess::Read),
        ("/build/letibot-r2", ViewAccess::Write),
    ] {
        assert_eq!(
            g.grant_view(Path::new(path), "bash", access),
            ViewGrant::Granted { writable: false },
            "{access:?}"
        );
    }
}

/// Answers every view ask as `by` would, with the read-only rung.
struct AnsweredBy(&'static str);

impl Adjudicator for AnsweredBy {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        AdjudicationDecision::selected(req, "grant_ro", self.0, "because")
    }

    fn describe(&self) -> String {
        "asked at the head: a test".into()
    }

    fn last_advice(&self) -> Option<ModelAdvice> {
        Some(ModelAdvice {
            consulted: true,
            would: "admit",
            by: "shadow:deepseek".into(),
            basis: "the operator asked".into(),
            cites: Vec::new(),
            latency_ms: 1,
            unsure: None,
        })
    }
}

/// A view ask is a corpus row like any call. A row the shadow settled is not an operator's answer,
/// so it is neither asked nor labelled: scored, it would agree with itself.
#[test]
fn view_asks_are_recorded_and_shadow_rows_excluded_from_agreement() {
    let sink = Arc::new(crate::authorise::RecordingCorpusSink::default());
    for by in ["shadow:deepseek", "human:op"] {
        let mut g = AdjudicatedGate::new(Box::new(AnsweredBy(by)))
            .with_identity("s-test", "agent")
            .with_corpus_sink(sink.clone());
        assert_eq!(
            g.grant_view(Path::new("/build/letibot-r1"), "bash", ViewAccess::Read),
            ViewGrant::Granted { writable: false }
        );
    }
    let rows = sink.rows.lock().unwrap().clone();
    assert_eq!(rows.len(), 2, "both view asks are rows: {rows:?}");
    for r in &rows {
        assert_eq!(r.tool, "bash");
        assert_eq!(r.arguments["path"], "/build/letibot-r1");
        assert_eq!(r.effect, "admit");
        assert_eq!(r.tier, "may_approve");
    }
    let (shadow, human) = (&rows[0], &rows[1]);
    assert_eq!(shadow.verdict_by.as_deref(), Some("shadow:deepseek"));
    assert!(!shadow.asked, "a shadow's answer is not the operator's");
    assert!(shadow.operator.is_none(), "{:?}", shadow.operator);
    assert!(human.asked);
    assert!(
        human.operator.is_some(),
        "the person's answer labels the advice"
    );
}

/// A ledger in memory whose reads can be made to fail.
#[derive(Default)]
struct Flaky {
    rows: Mutex<Vec<crate::grant_ledger::Stored>>,
    down: std::sync::atomic::AtomicBool,
}

impl crate::grant_ledger::GrantLedger for Flaky {
    fn keep(
        &self,
        scope: &crate::grant_ledger::Scope,
        kept: &crate::grant_ledger::Kept,
        why: &str,
        by: &str,
    ) -> Result<(), String> {
        let mut rows = self.rows.lock().unwrap();
        let id = rows.len() as i64 + 1;
        rows.push(crate::grant_ledger::Stored {
            id,
            scope: scope.clone(),
            kept: kept.clone(),
            why: why.into(),
            by: by.into(),
        });
        Ok(())
    }

    fn load(
        &self,
        scope: &crate::grant_ledger::Scope,
    ) -> Result<Vec<crate::grant_ledger::Stored>, String> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the store is gone".into());
        }
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| &r.scope == scope)
            .cloned()
            .collect())
    }

    fn clear(&self, _: &crate::grant_ledger::Scope, _: &str) -> Result<usize, String> {
        Ok(0)
    }

    fn generation(&self) -> Result<String, String> {
        match self.down.load(std::sync::atomic::Ordering::SeqCst) {
            true => Err("the store is gone".into()),
            false => Ok(self.rows.lock().unwrap().len().to_string()),
        }
    }
}

/// A ledger that cannot be read may be hiding a revoke, so the view grants read from it are
/// unbound until it can be read again.
#[test]
fn an_unreadable_ledger_unbinds_the_view_grants_it_held() {
    let view = Arc::new(ViewGrants::new());
    let ledger = Arc::new(Flaky::default());
    let cards = Cards::new("grant_ro");
    let mut g = gate(cards.clone(), &view, None).with_grant_ledger(ledger.clone(), None);
    let path = Path::new("/build/letibot-r1");
    assert_eq!(
        g.grant_view(path, "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false }
    );
    ledger.down.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(g.view_revocations(), vec![crate::view::key(path)]);
    assert_eq!(view.covers(path, ViewAccess::Read), None);
}
