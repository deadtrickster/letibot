//! A view grant outlives the daemon, a queue's covers every reviewer and nobody else, and a
//! revoke takes one away.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_tools::adjudicate::{AdjudicationDecision, AdjudicationRequest, Adjudicator};
use letibot_tools::grant_ledger::{Kept, Scope};
use letibot_tools::runtime::ViewGrant;
use letibot_tools::view::{ViewAccess, ViewGrants};
use letibot_tools::{AdjudicatedGate, Gate as _};

use super::*;

const GATEKEEPER: &str = "s-gatekeeper-c74d1fd17483";
const MAIN: &str = "s-1794057745451974498";

/// Answers every card with `answer`, as if on a relayed card when `relayed`, and counts them.
#[derive(Clone)]
struct Cards {
    answer: &'static str,
    relayed: bool,
    seen: Arc<Mutex<usize>>,
}

impl Cards {
    fn new(answer: &'static str, relayed: bool) -> Cards {
        Cards {
            answer,
            relayed,
            seen: Arc::new(Mutex::new(0)),
        }
    }

    fn count(&self) -> usize {
        *self.seen.lock().unwrap()
    }
}

impl Adjudicator for Cards {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        *self.seen.lock().unwrap() += 1;
        AdjudicationDecision::selected(req, self.answer, "human:test", "because")
    }

    fn describe(&self) -> String {
        "asked at the head: a test".into()
    }

    fn last_relayed(&self) -> bool {
        self.relayed
    }
}

struct Fixture {
    dir: PathBuf,
    at: StoreAt,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("letibot-kept-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("build/letibot-r114")).unwrap();
        let at = StoreAt::File(dir.join("store.db"));
        Fixture { dir, at }
    }

    /// The path a grant is about: under the fixture, so it exists and keys the same every time.
    fn build(&self) -> PathBuf {
        self.dir.join("build")
    }

    /// A session's gate as a daemon opens it: its own view record, grants read from the store.
    fn gate(&self, id: &str, cards: &Cards) -> (AdjudicatedGate, Arc<ViewGrants>) {
        let view = Arc::new(ViewGrants::new());
        let queue = letibot_tools::grant_ledger::queue_of(id)
            .filter(|_| id != MAIN)
            .map(str::to_string);
        let g = AdjudicatedGate::new(Box::new(cards.clone()))
            .with_identity(id, "agent")
            .with_view_grants(view.clone())
            .with_grant_ledger(Arc::new(StoreLedger::new(self.at.clone())), queue);
        (g, view)
    }

    fn store(&self) -> letibot_tokencore::store::Store {
        self.at.open().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn config(dir: &Path, id: &str) -> Config {
    let mut cfg = Config::for_this_box(dir);
    cfg.session_id = id.to_string();
    cfg
}

#[test]
fn a_view_grant_survives_a_daemon_restart() {
    let f = Fixture::new("restart");
    let cards = Cards::new("grant_ro", false);
    let (mut g, _) = f.gate(MAIN, &cards);
    assert_eq!(
        g.grant_view(&f.build(), "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false }
    );
    drop(g);

    // A new daemon: a new view record and a new gate, the same store.
    let after = Cards::new("deny", false);
    let (mut g, view) = f.gate(MAIN, &after);
    assert_eq!(
        g.grant_view(&f.build().join("letibot-r114"), "bash", ViewAccess::Read),
        ViewGrant::Covered { writable: false }
    );
    assert_eq!(after.count(), 0, "no card after the restart");
    assert_eq!(
        view.covers(&f.build(), ViewAccess::Write),
        None,
        "a read grant stays a read grant"
    );
    // And the open binds it before the first command: the view the backend is built with.
    let cfg = config(&f.dir, MAIN);
    let kept = kept_views(&f.store(), &cfg);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].0, letibot_tools::view::key(&f.build()));
    assert!(!kept[0].1);
}

#[test]
fn a_queue_grant_admits_the_next_reviewer_and_survives_a_restart() {
    let f = Fixture::new("queue");
    let cards = Cards::new("grant_ro", true);
    let (mut first, _) = f.gate(&format!("{GATEKEEPER}-sub-1"), &cards);
    assert_eq!(
        first.grant_view(&f.build(), "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false }
    );
    assert_eq!(cards.count(), 1);

    // The next review: a new sub-session with a record of its own.
    let (mut next, _) = f.gate(&format!("{GATEKEEPER}-sub-2"), &cards);
    assert_eq!(
        next.grant_view(&f.build(), "bash", ViewAccess::Read),
        ViewGrant::Covered { writable: false }
    );
    assert_eq!(cards.count(), 1, "no card for the next reviewer");
    drop((first, next));

    // A restart.
    let (mut after, _) = f.gate(&format!("{GATEKEEPER}-sub-3"), &cards);
    assert_eq!(
        after.grant_view(&f.build(), "bash", ViewAccess::Read),
        ViewGrant::Covered { writable: false }
    );
    assert_eq!(cards.count(), 1, "no card after a restart either");
    let stored = load(&f.store(), &Scope::Queue(GATEKEEPER.into())).unwrap();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert!(
        load(&f.store(), &Scope::Session(format!("{GATEKEEPER}-sub-1")))
            .unwrap()
            .is_empty(),
        "kept at the queue's scope, not the asker's"
    );
}

#[test]
fn a_queue_grant_does_not_widen_the_main_session() {
    let f = Fixture::new("main");
    let cards = Cards::new("grant_rw", true);
    let (mut reviewer, _) = f.gate(&format!("{GATEKEEPER}-sub-1"), &cards);
    assert_eq!(
        reviewer.grant_view(&f.build(), "bash", ViewAccess::Write),
        ViewGrant::Granted { writable: true }
    );

    let main_cards = Cards::new("deny", false);
    let (mut main, view) = f.gate(MAIN, &main_cards);
    assert_eq!(view.covers(&f.build(), ViewAccess::Read), None);
    assert!(matches!(
        main.grant_view(&f.build(), "bash", ViewAccess::Read),
        ViewGrant::Refused(_)
    ));
    assert_eq!(main_cards.count(), 1, "the main session is asked");
    assert!(kept_views(&f.store(), &config(&f.dir, MAIN)).is_empty());
}

#[test]
fn revoking_a_grant_removes_it_from_the_store_and_a_running_session() {
    let f = Fixture::new("revoke");
    let cards = Cards::new("grant_ro", false);
    let (mut g, view) = f.gate(MAIN, &cards);
    g.grant_view(&f.build(), "bash", ViewAccess::Read);
    let store = f.store();
    let rows = load(&store, &Scope::Session(MAIN.into())).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].kept, Kept::View { .. }));

    let listed = permissions(&store, MAIN, None, None, None);
    assert!(
        listed
            .lines
            .iter()
            .any(|l| l.contains(&rows[0].id.to_string())),
        "{:?}",
        listed.lines
    );
    let revoked = permissions(&store, MAIN, None, Some(rows[0].id), None);
    assert!(revoked.ok, "{:?}", revoked.lines);
    assert!(
        load(&store, &Scope::Session(MAIN.into()))
            .unwrap()
            .is_empty()
    );

    // The running session unbinds it at its next call, and asks again.
    assert_eq!(
        g.view_revocations(),
        vec![letibot_tools::view::key(&f.build())]
    );
    assert_eq!(
        g.grant_view(&f.build().join("letibot-r114"), "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false }
    );
    assert_eq!(cards.count(), 2, "asked again after the revoke");
    let _ = view;

    // Another session's grant cannot be revoked from here.
    let other = permissions(&store, "s-other", None, Some(rows[0].id), None);
    assert!(!other.ok);
}

/// A tripped breaker outlives the daemon with the grants it guards, and only `/shadow arm`
/// clears it.
#[test]
fn breaker_survives_reopen_while_grants_persist() {
    use crate::shadow::permission::{Answering, Relay};
    use letibot_tools::grant_ledger::GrantLedger as _;
    let f = Fixture::new("breaker");
    let reviewer = format!("{GATEKEEPER}-sub-1");
    let ledger = StoreLedger::new(f.at.clone());
    for name in ["a", "b", "c"] {
        std::fs::create_dir_all(f.build().join(name)).unwrap();
        ledger
            .keep(
                &Scope::Session(reviewer.clone()),
                &Kept::View {
                    path: letibot_tools::view::key(&f.build().join(name)),
                    writable: false,
                },
                "the shadow allowed it",
                "shadow:deepseek",
            )
            .unwrap();
    }
    let answering = Answering::open(true, Some((f.at.clone(), MAIN.to_string())));
    assert!(answering.is_on());
    let set = PendingAsks::new();
    set.set_annotator(Some(Arc::new(Relay {
        cell: Default::default(),
        home: "/home/op".into(),
        answering: answering.clone(),
    })));
    let store = f.store();
    for _ in 0..2 {
        let r = shadow(&store, Some(&set), Some(None));
        assert!(r.ok, "{:?}", r.lines);
    }
    assert!(!answering.is_on(), "two revokes trip it");

    // A restart: nothing in memory, only the store.
    drop((set, answering));
    let after = Answering::open(true, Some((f.at.clone(), MAIN.to_string())));
    assert!(!after.is_on(), "a restart does not re-arm it");
    assert!(after.tripped());
    assert_eq!(after.disagreements(), 2);
    let listed = shadow(&store, None, None);
    assert_eq!(listed.lines.len(), 1, "{:?}", listed.lines);
    assert!(
        listed.lines[0].contains("(by shadow:deepseek)"),
        "{:?}",
        listed.lines
    );

    // The operator's re-arm, kept too.
    after.arm().unwrap();
    assert!(after.is_on());
    let again = Answering::open(true, Some((f.at.clone(), MAIN.to_string())));
    assert!(again.is_on());
    assert_eq!(again.disagreements(), 0);
    assert!(
        Answering::open(false, Some((f.at.clone(), MAIN.to_string())))
            .arm()
            .is_err(),
        "nothing to arm while answer_reads is off"
    );
}

/// A queue grant covers later reviewers for its TTL, then asks again.
#[test]
fn a_queue_grant_expires_after_its_ttl() {
    let f = Fixture::new("ttl");
    let cards = Cards::new("grant_ro", true);
    let (mut first, _) = f.gate(&format!("{GATEKEEPER}-sub-1"), &cards);
    first.grant_view(&f.build(), "bash", ViewAccess::Read);
    assert_eq!(cards.count(), 1);
    std::thread::sleep(std::time::Duration::from_millis(5));

    let id = format!("{GATEKEEPER}-sub-2");
    let mut next = AdjudicatedGate::new(Box::new(cards.clone()))
        .with_identity(&id, "agent")
        .with_view_grants(Arc::new(ViewGrants::new()))
        .with_grant_ledger(
            Arc::new(StoreLedger::new(f.at.clone()).with_queue_ttl(std::time::Duration::ZERO)),
            letibot_tools::grant_ledger::queue_of(&id).map(str::to_string),
        );
    assert_eq!(
        next.grant_view(&f.build(), "bash", ViewAccess::Read),
        ViewGrant::Granted { writable: false },
        "an expired queue grant is asked again"
    );
    assert_eq!(cards.count(), 2);
}
