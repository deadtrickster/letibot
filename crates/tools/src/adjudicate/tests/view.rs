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
}

impl Cards {
    fn new(answer: &'static str) -> Cards {
        Cards {
            answer,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
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
    let second = std::thread::spawn(move || {
        let mut g = gate(Cards::new("grant_rw"), &sibling_view, Some("sub-b"));
        g.grant_view(&sibling_path, "bash", ViewAccess::Read)
    });

    let mine = first.grant_view(&path, "bash", ViewAccess::Read);
    let theirs = second.join().expect("the sibling thread");
    assert_eq!(mine, ViewGrant::Granted { writable: true });
    assert_eq!(
        theirs,
        ViewGrant::Covered { writable: true },
        "the sibling waited on that one card and was released by its answer"
    );
    assert_eq!(
        cards.count(),
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
