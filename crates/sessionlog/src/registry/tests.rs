use super::*;
use crate::hub::{CommandKind, Delivery};
use crate::protocol::Caps;
use crate::testing;

fn reg() -> Arc<Registry> {
    Registry::new()
}

fn stored(id: &str, parent: Option<&str>) -> StoredBrief {
    StoredBrief {
        session_id: id.into(),
        title: id.into(),
        items: 1,
        last_activity_ms: 1,
        wiring: SessionWiring::default(),
        parent_session_id: parent.map(Into::into),
        context_tokens: None,
        context_cached: None,
        stored_end: None,
        nags_paused: None,
        waits_on_operator: 0,
        slug: String::new(),
        role: String::new(),
        origin: SessionOrigin::default(),
    }
}

/// A source that keys its answers and counts how often the whole store is walked.
struct Keyed {
    rows: Vec<StoredBrief>,
    lists: std::sync::atomic::AtomicUsize,
}

impl SessionSource for Keyed {
    fn list(&self) -> Vec<StoredBrief> {
        self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.rows.clone()
    }
    fn lookup(&self, id: &str) -> Option<StoredBrief> {
        self.rows.iter().find(|r| r.session_id == id).cloned()
    }
    fn children_of(&self, parent: &str) -> Vec<StoredBrief> {
        self.rows
            .iter()
            .filter(|r| r.parent_session_id.as_deref() == Some(parent))
            .cloned()
            .collect()
    }
}

/// The per-round kin read and a one-session read ask the source by key and never list it.
#[test]
fn kin_and_brief_read_the_source_by_key_and_never_list_it() {
    let r = reg();
    let mut rows: Vec<StoredBrief> = (0..500)
        .map(|i| stored(&format!("s-other-{i}"), None))
        .collect();
    rows.push(stored("s-me", Some("s-mum")));
    rows.push(stored("s-mum", None));
    rows.push(stored("s-kid-1", Some("s-me")));
    rows.push(stored("s-kid-2", Some("s-me")));
    let src = Arc::new(Keyed {
        rows,
        lists: Default::default(),
    });
    r.set_source(src.clone());
    // One kid is live: it must come back once, as live.
    r.create_under("s-kid-1", "", SessionWiring::default(), Some("s-me".into()))
        .unwrap();
    let mut got: Vec<(String, bool)> = r
        .kin("s-me", Some("s-mum"))
        .into_iter()
        .map(|b| (b.session_id, b.live))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("s-kid-1".to_string(), true),
            ("s-kid-2".to_string(), false),
            ("s-mum".to_string(), false),
        ]
    );
    assert_eq!(
        r.brief("s-me").map(|b| b.session_id).as_deref(),
        Some("s-me")
    );
    assert!(r.brief("s-nobody").is_none());
    assert_eq!(
        src.lists.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the whole store was listed"
    );
}

/// A moved `model` row, said to the heads already attached. The rows answer a request, so a
/// switch made from another head (or by the daemon itself) left this one drawing the old name
/// until its next turn.
#[test]
fn a_moved_model_row_is_said_on_the_session_hub() {
    let r = reg();
    r.create("s-1", "", SessionWiring::default()).unwrap();
    let hub = r.get("s-1").expect("the session is held");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    let row = |v: &str| crate::protocol::SettingRow {
        key: crate::protocol::MODEL_KEY.into(),
        value: v.into(),
        source: String::new(),
        editable: String::new(),
        choices: Vec::new(),
        tools: Vec::new(),
    };
    // A probe of the head's own, so a read has something to answer even when nothing was said.
    // A settings publish rides the side channel ahead of it, and the events are the subject.
    let said = |hub: &Hub| -> Vec<(String, String)> {
        hub.publish(crate::SessionEvent::Warning {
            code: "prefix_current".into(),
            detail: String::new(),
            compaction: None,
        });
        let batch = loop {
            match hub.next_batch(&head.head_id, 64) {
                Delivery::Events(batch) => break batch,
                Delivery::Frames(_) => {}
                other => panic!("a batch, not a {other:?}"),
            }
        };
        batch
            .events()
            .iter()
            .filter_map(|e| match &e.event {
                crate::SessionEvent::Warning { code, detail, .. } => {
                    Some((code.clone(), detail.clone()))
                }
                _ => None,
            })
            .collect()
    };

    // The first publish is what a head reads when it asks for the rows: not news.
    r.set_settings("s-1", vec![row("local (qwen-3.8-27b)")]);
    assert_eq!(
        said(&hub),
        vec![("prefix_current".to_string(), String::new())]
    );

    // The switch itself. The world spelling is the row's, and the head strips the alias.
    r.set_settings("s-1", vec![row("deepseek/deepseek-flash")]);
    assert!(
        said(&hub).contains(&("model_set".into(), "deepseek/deepseek-flash".into())),
        "the move reached nobody"
    );

    // A republish of the same rows - every `/config` - is not a move and says nothing.
    r.set_settings("s-1", vec![row("deepseek/deepseek-flash")]);
    assert_eq!(
        said(&hub),
        vec![("prefix_current".to_string(), String::new())]
    );

    // With no head on the session there is nobody to tell, and the switch is not made history.
    hub.detach(&head.head_id);
    r.set_settings("s-1", vec![row("grok/grok-4")]);
    let late = hub.attach("tui", "later", Caps::default(), 0);
    let snapshot = late.snapshot.expect("a fresh head gets a snapshot");
    let moved: Vec<&str> = snapshot
        .warnings
        .iter()
        .filter(|w| w.code == crate::protocol::MODEL_SET)
        .map(|w| w.detail.as_str())
        .collect();
    assert_eq!(
        moved,
        vec!["deepseek/deepseek-flash"],
        "only the move a head was there for is a warning at all"
    );
}

/// An operator run in flight when the registry closes is named, even though the prompts and
/// the host's ender are two different tables.
#[test]
fn an_operator_run_in_flight_is_named_when_the_registry_closes() {
    /// The operator's own run, as the `!send` door knows it: one run in flight, and nothing else
    /// this fixture needs.
    struct ARun(String, String);
    impl PromptDriver for ARun {
        fn send(&self, _s: &str, _r: Option<&str>, _line: &str) -> Result<Option<String>, String> {
            Err("nothing is running in this fixture".into())
        }
        fn in_flight(&self) -> Option<(String, String)> {
            Some((self.0.clone(), self.1.clone()))
        }
    }

    let r = reg();
    r.create("s-1", "", SessionWiring::default()).unwrap();
    let hub = r.get("s-1").expect("the session is held");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    r.set_prompt("s-1", Arc::new(ARun("j1".into(), "/bin/cat".into())));
    // The ender says nothing at all: the run is not in the table the host knows.
    r.watch_runs("s-1", Arc::new(|| {}));

    r.close();

    let codes: Vec<String> = std::iter::from_fn(|| match hub.next_batch(&head.head_id, 64) {
        Delivery::Events(batch) => Some(
            batch
                .events()
                .iter()
                .filter_map(|env| match &env.event {
                    crate::event::SessionEvent::Warning { code, detail, .. } => {
                        Some(format!("{code}: {detail}"))
                    }
                    _ => None,
                })
                .collect::<Vec<String>>(),
        ),
        Delivery::Resync { .. } | Delivery::Frames(_) => Some(Vec::new()),
        Delivery::Closed => None,
    })
    .flatten()
    .collect();
    assert_eq!(codes.len(), 1, "one row for one run: {codes:?}");
    assert!(
        codes[0].starts_with("daemon_stopping_runs: "),
        "the code is the register's: {codes:?}"
    );
    assert!(
        codes[0].contains("/bin/cat") && codes[0].contains("j1"),
        "it names the command the person typed and the handle: {codes:?}"
    );
    assert!(
        codes[0].contains("stop") && codes[0].contains("ended"),
        "what happened and why: {codes:?}"
    );
}

/// A stop names the run it is about to end, before the kill and before any hub closes, even
/// when a second caller asks for the stop that is already running.
#[test]
fn a_run_the_stop_ends_is_named_before_the_hubs_close() {
    let r = reg();
    r.create("s-1", "", SessionWiring::default()).unwrap();
    let hub = r.get("s-1").expect("the session is held");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    // The pure half says what is running; the ender takes a beat over it and says nothing.
    r.name_runs("s-1", Arc::new(|| vec!["`/bin/cat` (`j1`)".to_string()]));
    r.watch_runs(
        "s-1",
        Arc::new(|| std::thread::sleep(std::time::Duration::from_millis(200))),
    );

    // A head's writer thread, from the schema's own side: a loop that stops at the first
    // `Closed`, which is what the server does with a dead socket.
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let reader = {
        let (hub, head_id, seen) = (Arc::clone(&hub), head.head_id.clone(), Arc::clone(&seen));
        std::thread::spawn(move || {
            loop {
                match hub.next_batch(&head_id, 64) {
                    Delivery::Events(batch) => {
                        for env in batch.events() {
                            if let crate::event::SessionEvent::Warning { code, .. } = &env.event {
                                seen.lock().unwrap().push(code.clone());
                            }
                        }
                    }
                    Delivery::Resync { .. } | Delivery::Frames(_) => continue,
                    Delivery::Closed => return,
                }
            }
        })
    };

    // The winner, and a second caller that arrives while it is inside the ender.
    let loser = {
        let r = Arc::clone(&r);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            r.close();
        })
    };
    r.close();
    loser.join().expect("the second caller returns");
    reader.join().expect("the reader stops at the close");

    let codes = seen.lock().unwrap().clone();
    assert_eq!(
        codes,
        ["daemon_stopping_runs"],
        "the run the stop ended must be named, and named before the hub closes: {codes:?}"
    );
}

/// The rule, in the one place it is decided: a session a session started has that session as
/// its parent, whichever of the three ways it arrived.
#[test]
fn a_parent_comes_from_whichever_source_knows_and_is_never_invented() {
    let r = reg();
    // (1) The caller's word, for a session with neither an entry nor a row yet.
    assert_eq!(
        r.parent_for("s-new-sub-1", Some("s-new".into()), None),
        Some("s-new".to_string())
    );
    // (2) The registry's entry - the door that created it.
    r.create_under(
        "s-entry-sub-1",
        "",
        SessionWiring::default(),
        Some("s-entry".into()),
    )
    .unwrap();
    assert_eq!(
        r.parent_for("s-entry-sub-1", None, None),
        Some("s-entry".to_string())
    );
    // (3) The session's own row, for an id this registry does not hold at all.
    assert_eq!(
        r.parent_for("s-rowed-sub-1", None, Some("s-rowed")),
        Some("s-rowed".to_string())
    );
    // The corollary: a session a person started - a head's `NewSession`, a launch, a
    // `--continue` - has no parent, whatever is current at the time.
    r.create("s-mine-sub-3", "", SessionWiring::default())
        .unwrap();
    assert_eq!(r.parent_for("s-mine-sub-3", None, None), None);
    // And the caller's word wins when a session is re-registered - a parked child reopened by
    // its parent is the case: the entry may be gone, the row may say something else.
    assert_eq!(
        r.parent_for("s-rowed-sub-1", Some("s-reopened".into()), Some("s-rowed")),
        Some("s-reopened".to_string())
    );
}

/// A subagent's hub is built first and registered only once its owner has opened it - so nothing
/// can switch into it early, and the worker is never told to open a session somebody else is
/// opening.
#[test]
fn a_new_hub_is_not_listed_until_adopted_and_adopting_does_not_ring_the_worker() {
    let r = reg();
    r.create("s-parent", "", SessionWiring::default()).unwrap();
    // The parent's own creation rang the worker; drain that.
    assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-parent"));

    let hub = r.new_hub("s-parent-sub-1");
    assert!(
        r.resolve("s-parent-sub-1").is_none(),
        "listed before it was open"
    );
    assert_eq!(r.list().len(), 1);

    r.adopt(
        hub.clone(),
        "find the bug",
        SessionWiring::default(),
        Some("s-parent".into()),
    )
    .unwrap();
    let brief = r.brief("s-parent-sub-1").expect("adopted");
    assert_eq!(brief.parent_session_id.as_deref(), Some("s-parent"));
    assert!(Arc::ptr_eq(&r.resolve("s-parent-sub-1").unwrap(), &hub));
    let again = r.adopt(hub.clone(), "", SessionWiring::default(), None);
    assert_eq!(again, Err(CreateError::Exists("s-parent-sub-1".into())));
    // The bell blocks with nothing pending, so "did not ring" is proven by
    // closing it: a drained, closed bell answers None at once, and a ring
    // for the child would come out first.
    r.bell().close();
    assert!(
        r.next_work().is_none(),
        "adopting rang the worker to open it again"
    );
}

/// A spawned session is alive BEFORE it is open, and only while it is: the child's handle
/// exists and is returned before its own thread adopts the hub.
#[test]
fn a_spawn_is_alive_before_its_session_exists_and_not_after_the_spawn_ends() {
    let r = reg();
    r.create("s-parent", "", SessionWiring::default()).unwrap();
    r.announce_spawn("s-parent-sub-1");
    assert!(
        r.get("s-parent-sub-1").is_none(),
        "no hub yet: nobody may attach to it"
    );
    assert!(r.opening("s-parent-sub-1"), "but it has not gone either");

    // The open spends the announcement, and the entry is what answers from then on.
    let hub = r.new_hub("s-parent-sub-1");
    r.adopt(
        hub.clone(),
        "find the bug",
        SessionWiring::default(),
        Some("s-parent".into()),
    )
    .unwrap();
    assert!(!r.opening("s-parent-sub-1"), "an open one is not opening");
    assert!(Arc::ptr_eq(&r.get("s-parent-sub-1").unwrap(), &hub));

    // A spawn that ends without a session is not alive - withdrawing twice is not an error,
    // because `adopt` has usually done it already (the child thread's guard runs after).
    r.announce_spawn("s-parent-sub-2");
    r.spawn_settled("s-parent-sub-2");
    r.spawn_settled("s-parent-sub-2");
    assert!(!r.opening("s-parent-sub-2") && r.get("s-parent-sub-2").is_none());
    // And an ordinary session was never announced as anything.
    assert!(!r.opening("s-parent"));
    // Removing a session does not leave its announcement behind either.
    r.announce_spawn("s-parent-sub-3");
    r.remove("s-parent-sub-1").expect("the child is held");
    assert!(!r.opening("s-parent-sub-1"));
    assert!(
        r.opening("s-parent-sub-3"),
        "a live announcement survives another's removal"
    );
}

/// A session served by its own thread is never served by the worker. Without the owner on the
/// hub, a head prompting a subagent would race that subagent's own thread.
#[test]
fn the_worker_never_takes_a_command_off_a_hub_owned_by_its_own_thread() {
    let r = reg();
    let hub = r.new_hub("s-parent-sub-1");
    hub.owned_by_its_own_thread();
    r.adopt(
        hub.clone(),
        "a child",
        SessionWiring::default(),
        Some("s-parent".into()),
    )
    .unwrap();
    let h = hub.attach("tui", "dead", Caps::default(), 0);
    hub.submit(
        &h.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "what did you find?".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    let soon = Instant::now() + std::time::Duration::from_millis(50);
    assert!(
        matches!(r.next_work_until(Some(soon)), WorkOrIdle::Idle),
        "the worker took a prompt off a subagent's queue — the second writer"
    );
    match hub.take_own_work_until(Some(Instant::now())) {
        crate::hub::OwnWork::Command(c) => {
            assert!(matches!(c.kind, CommandKind::Prompt { .. }))
        }
        other => panic!("the child's own reader must still find its prompt: {other:?}"),
    }
}

/// A removed session is gone from every table, its hub is closed, and the sessions after it are
/// still found by id - the index is rewritten for every position the removal shifted, and `list`
/// keeps arrival order.
#[test]
fn a_removed_session_leaves_every_table_and_the_rest_are_still_found() {
    let r = reg();
    for id in ["s-a", "s-b", "s-c", "s-d"] {
        r.create(id, id, SessionWiring::default()).unwrap();
    }
    let b = r.get("s-b").unwrap();
    let head = b.attach("tui", "dead", Caps::default(), 0);
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = ended.clone();
    r.watch_runs(
        "s-b",
        Arc::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    r.set_notes_source("s-b", Arc::new(Vec::new));
    r.set_default("s-b");

    let gone = r.remove("s-b").expect("it was held");
    assert!(Arc::ptr_eq(&gone, &b));
    assert!(
        b.is_closed(),
        "a removed hub must not keep a head waiting on it"
    );
    assert!(matches!(b.next_batch(&head.head_id, 10), Delivery::Closed));
    assert!(r.get("s-b").is_none() && !r.holds("s-b"));
    assert!(r.remove("s-b").is_none(), "a second close is not an error");
    for id in ["s-a", "s-c", "s-d"] {
        assert_eq!(
            r.get(id).map(|h| h.session_id()),
            Some(id.to_string()),
            "the index points {id} at somebody else's entry after the removal"
        );
        assert_eq!(r.title_of(id).as_deref(), Some(id));
    }
    let order: Vec<String> = r.list().into_iter().map(|b| b.session_id).collect();
    assert_eq!(order, ["s-a", "s-c", "s-d"], "arrival order survives");
    assert_eq!(
        r.default_id(),
        "s-d",
        "the default moved to a session that exists"
    );
    // The run ender went with it: a stop ends nothing for a session that is gone.
    r.close();
    assert!(
        !ended.load(std::sync::atomic::Ordering::SeqCst),
        "the removed session's run ender was still called at stop"
    );
}

/// `parent_of` answers from the entry, and does not need a source at all - which is the point:
/// the other lookup is `list()`, a read of every stored session.
#[test]
fn the_parent_is_read_from_the_entry_not_the_store() {
    let r = reg();
    r.create("s-root", "", SessionWiring::default()).unwrap();
    let child = r.new_hub("s-root-sub");
    r.adopt(child, "", SessionWiring::default(), Some("s-root".into()))
        .unwrap();
    assert_eq!(r.parent_of("s-root-sub").as_deref(), Some("s-root"));
    assert_eq!(r.parent_of("s-root"), None);
    assert!(r.holds("s-root") && !r.holds("s-nobody"));
}

#[test]
fn two_sessions_do_not_share_a_log_a_view_or_a_queue() {
    // The property that makes the ledger and the memfd region safe one layer
    // down: nothing in this file can hand two sessions the same `Hub`, and a
    // `Harness` is opened per `Hub`.
    let r = reg();
    let a = r.create("s-a", "", SessionWiring::default()).unwrap();
    let b = r.create("s-b", "", SessionWiring::default()).unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    a.publish(testing::turn_started("t1"));
    a.publish(testing::delta("t1", "only in a"));
    assert_eq!(b.head_seq(), 0, "b saw a's events");
    assert_eq!(b.snapshot().turn, None);
    assert_eq!(a.snapshot().turn.unwrap().text, "only in a");
}

/// A stop interrupts the turns it holds, and starts none: the state this measures is the one
/// that costs a daemon its folder's files.
#[test]
fn a_stop_interrupts_the_turns_it_holds_and_starts_none() {
    let r = reg();
    let running = r.create("s-running", "", SessionWiring::default()).unwrap();
    let idle = r.create("s-idle", "", SessionWiring::default()).unwrap();
    running.publish(testing::turn_started("t1"));
    assert!(running.status().running, "the premise: a turn is in flight");
    assert!(!idle.status().running, "the premise: and this one is idle");

    r.close();

    match running.try_steering_command() {
        Some(cmd) => assert_eq!(
            cmd.kind,
            CommandKind::Interrupt {
                reason: STOPPING_TURN_REASON.to_string()
            },
            "the stop hands the turn the interrupt an Esc-Esc sends, and says why"
        ),
        None => panic!(
            "the stop left the turn in flight: nothing for its steering poll, so the \
                 daemon waits for the round it was asked to end"
        ),
    }
    assert!(
        idle.try_command().is_none(),
        "a stop queued a command for a session with nothing running"
    );
    // And it is an interrupt rather than a turn: nothing was appended to the conversation.
    assert_eq!(idle.snapshot().turn, None, "a stop must not start a turn");
}

#[test]
fn creating_the_same_id_twice_is_refused_rather_than_returning_the_first() {
    let r = reg();
    r.create("s-a", "", SessionWiring::default()).unwrap();
    let again = r.create("s-a", "", SessionWiring::default());
    assert!(
        matches!(&again, Err(CreateError::Exists(id)) if id == "s-a"),
        "a second create must not hand back the first session"
    );
}

/// A broadcast reaches every session's log, and only the log: a head attached to any of them
/// can open the card it announces.
#[test]
fn a_broadcast_reaches_every_sessions_log_and_rings_no_bell() {
    let r = reg();
    for id in ["s-a", "s-b", "s-c"] {
        r.create(id, "", SessionWiring::default()).unwrap();
    }
    let event = crate::event::SessionEvent::QueueMoved {
        queue: "merge".into(),
        id: "m-1".into(),
        to: "landed".into(),
        why: "landed at deadbeef".into(),
    };
    assert_eq!(r.broadcast(event.clone()), 3, "every session's log");

    // Every one of them really holds it, read back through the hub's own retained log.
    for id in ["s-a", "s-b", "s-c"] {
        let hub = r.get(id).expect("the session");
        let retained = hub.retained();
        assert!(
            retained.iter().any(|e| matches!(
                &e.event,
                crate::event::SessionEvent::QueueMoved { id, .. } if id == "m-1"
            )),
            "{id} did not record the move: {retained:?}"
        );
    }

    // And no wake was rung. A broadcast that started a turn in every session would be a queue
    // move that costs a generation per session, which is the cost this door exists to bound.
    // The bell is drained and found empty.
    r.bell().close();
    assert!(
        r.bell().next().is_none(),
        "a broadcast must not wake anybody"
    );
}

#[test]
fn an_unknown_session_is_none_rather_than_the_default_one() {
    // A typo that seats you in somebody else's session is worse than a typo that fails.
    let r = reg();
    r.create("s-a", "", SessionWiring::default()).unwrap();
    assert!(r.resolve("s-typo").is_none());
    assert!(r.resolve("").is_some(), "an empty id is still the default");
}

#[test]
fn one_worker_is_woken_by_whichever_session_was_prompted() {
    let r = reg();
    let a = r.create("s-a", "", SessionWiring::default()).unwrap();
    let b = r.create("s-b", "", SessionWiring::default()).unwrap();
    let ha = a.attach("tui", "alice", Caps::default(), 0);
    let hb = b.attach("tui", "bob", Caps::default(), 0);

    b.submit(
        &hb.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "for b".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    a.submit(
        &ha.head_id,
        "c2",
        0,
        CommandKind::Prompt {
            text: "for a".into(),
            attachments: Vec::new(),
            context: None,
        },
    );

    // Arrival order across sessions: the same promise two heads on one session already get.
    let (id1, c1) = r.next_command().unwrap();
    assert_eq!(id1, "s-b");
    assert_eq!(
        c1.kind,
        CommandKind::Prompt {
            text: "for b".into(),
            attachments: Vec::new(),
            context: None,
        }
    );
    let (id2, c2) = r.next_command().unwrap();
    assert_eq!(id2, "s-a");
    assert_eq!(
        c2.kind,
        CommandKind::Prompt {
            text: "for a".into(),
            attachments: Vec::new(),
            context: None,
        }
    );
}

/// A take-back dequeues the whole queued run in one go: the head put both messages back in the
/// composer in one press, so the queue drains as a unit and never a message at a time. What it
/// could not have taken is anything that arrives behind the withdraw - the next test.
#[test]
fn a_take_back_drops_the_issuing_heads_queued_prompts_only() {
    let r = reg();
    let a = r.create("s-a", "", SessionWiring::default()).unwrap();
    let ha = a.attach("tui", "alice", Caps::default(), 0);
    let hb = a.attach("tui", "bob", Caps::default(), 0);

    a.submit(
        &ha.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "alice one".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    a.submit(
        &hb.head_id,
        "c2",
        0,
        CommandKind::Prompt {
            text: "bob".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    a.submit(
        &ha.head_id,
        "c3",
        0,
        CommandKind::Prompt {
            text: "alice two".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    a.submit(&ha.head_id, "c4", 0, CommandKind::WithdrawPrompts);

    assert!(
        a.try_withdraw_command(),
        "the take-back was found and acted on"
    );
    // Alice's prompts are gone - both of them - and bob's is not alice's to take back.
    let left: Vec<_> = (0..2).filter_map(|_| a.try_steering_command()).collect();
    assert_eq!(left.len(), 1);
    assert_eq!(
        left[0].kind,
        CommandKind::Prompt {
            text: "bob".into(),
            attachments: Vec::new(),
            context: None,
        }
    );
    // And the take-back itself is consumed: a second one finds nothing.
    assert!(!a.try_withdraw_command());
}

/// A prompt typed after the take-back is not one of the prompts it names: the take-back
/// recalls what is queued, and its point is what was there when it was asked.
#[test]
fn a_take_back_does_not_drop_a_prompt_sent_after_it() {
    let r = reg();
    let a = r.create("s-a", "", SessionWiring::default()).unwrap();
    let ha = a.attach("tui", "alice", Caps::default(), 0);

    // the message the operator wants back
    a.submit(
        &ha.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "lol, it is a bag".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    // Up - the recall itself
    a.submit(&ha.head_id, "c2", 0, CommandKind::WithdrawPrompts);
    // and the corrected copy, typed after the recall and sent after it
    a.submit(
        &ha.head_id,
        "c3",
        0,
        CommandKind::Prompt {
            text: "lol, it is a bug".into(),
            attachments: Vec::new(),
            context: None,
        },
    );

    assert!(a.try_withdraw_command(), "the take-back was acted on");
    let left: Vec<_> = (0..3).filter_map(|_| a.try_steering_command()).collect();
    assert_eq!(
        left.len(),
        1,
        "the recalled prompt is gone and the corrected copy is not: {left:?}"
    );
    assert_eq!(
        left[0].kind,
        CommandKind::Prompt {
            text: "lol, it is a bug".into(),
            attachments: Vec::new(),
            context: None,
        },
        "the prompt typed AFTER the take-back survives it"
    );
}

#[test]
fn a_ring_whose_command_was_already_drained_does_not_return_an_empty_wake() {
    // What happens for real: a turn is running, `HubSteering` drains the queue
    // at a step boundary, and the ring is still outstanding.
    let r = reg();
    let a = r.create("s-a", "", SessionWiring::default()).unwrap();
    assert!(
        matches!(r.next_work(), Some(Work::Open(_))),
        "create rings an open"
    );
    let h = a.attach("tui", "alice", Caps::default(), 0);
    a.submit(
        &h.head_id,
        "c1",
        0,
        CommandKind::Interrupt {
            reason: "stop".into(),
        },
    );
    assert!(a.try_command().is_some(), "the steering source took it");
    r.close();
    // Closed and drained: `None`, and not a `(session, command)` invented to
    // satisfy the outstanding ring.
    assert!(r.next_command().is_none());
}

#[test]
fn closing_wakes_a_worker_that_is_blocked_on_the_bell() {
    let r = reg();
    r.create("s-a", "", SessionWiring::default()).unwrap();
    let r2 = r.clone();
    let t = std::thread::spawn(move || r2.next_command());
    std::thread::sleep(std::time::Duration::from_millis(30));
    r.close();
    assert!(t.join().unwrap().is_none());
}

#[test]
fn a_list_says_which_session_is_busy_and_which_is_idle() {
    let r = reg();
    let a = r
        .create("s-a", "the cache question", SessionWiring::default())
        .unwrap();
    r.create("s-b", "", SessionWiring::default()).unwrap();
    a.publish(testing::turn_started("t1"));
    let rows = r.list();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].title, "the cache question");
    assert!(rows[0].status.running);
    assert!(!rows[1].status.running);
    // And it stops being busy when the turn ends, rather than staying busy because only
    // `TurnFinished` was ever taught to clear it.
    a.publish(crate::SessionEvent::TurnFailed {
        turn_id: "t1".into(),
        error: "the model called tools 12 times without answering".into(),
        partial_kept: false,
    });
    assert!(!r.list()[0].status.running);
}

/// A wake with no command behind it reaches the worker: `Bell::ring` is not it, because
/// `next_work` skips a session whose command queue is empty.
#[test]
fn a_wake_with_no_command_behind_it_still_reaches_the_worker() {
    let r = reg();
    r.create("s-a", "", SessionWiring::default()).unwrap();
    // `create` rings an open, and an open is drained first (a session whose resume has not run
    // must not serve a command). Take it so the assertion below is about the wake.
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    r.bell().ring_wake("s-a");
    match r.next_work() {
        Some(Work::Woken(id)) => assert_eq!(id, "s-a"),
        _ => panic!("a wake must be work"),
    }
}

/// A head that pressed enter is waiting; a monitor is not. Wakes are drained last, and this is
/// asserted rather than left to the queue order in `next_any`, because that order is a policy
/// and a reader of the three `pop_front`s cannot tell a policy from an accident.
#[test]
fn a_wake_is_served_after_every_queued_command() {
    let r = reg();
    let a = r.create("s-a", "", SessionWiring::default()).unwrap();
    assert!(
        matches!(r.next_work(), Some(Work::Open(_))),
        "create rings an open"
    );
    let h = a.attach("tui", "dead", Caps::default(), 0);
    r.bell().ring_wake("s-a");
    a.submit(
        &h.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "hello".into(),
            attachments: Vec::new(),
            context: None,
        },
    );

    match r.next_work() {
        Some(Work::Command(id, cmd)) => {
            assert_eq!(id, "s-a");
            assert!(matches!(cmd.kind, CommandKind::Prompt { .. }));
        }
        _ => panic!("the operator's prompt goes first even though the wake rang first"),
    }
    assert!(matches!(r.next_work(), Some(Work::Woken(_))));
}

/// Wakes for one session coalesce while they wait - one `Harness::wake` drains everything the
/// session owns, so a hundred rings before the worker gets there are one pass, not a hundred
/// passes over the store. Another session's wake keeps its own place.
#[test]
fn a_storm_of_wakes_for_one_session_is_one_wake() {
    let r = reg();
    r.create("s-a", "", SessionWiring::default()).unwrap();
    r.create("s-b", "", SessionWiring::default()).unwrap();
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    for _ in 0..100 {
        r.bell().ring_wake("s-a");
    }
    r.bell().ring_wake("s-b");
    r.bell().ring_wake("s-a");
    let soon = || Some(Instant::now() + std::time::Duration::from_millis(30));
    assert!(matches!(r.next_work_until(soon()), WorkOrIdle::Work(Work::Woken(id)) if id == "s-a"));
    assert!(matches!(r.next_work_until(soon()), WorkOrIdle::Work(Work::Woken(id)) if id == "s-b"));
    assert!(
        matches!(r.next_work_until(soon()), WorkOrIdle::Idle),
        "the storm was served more than once"
    );
}

/// The idle clock is not starved by wakes: a due deadline waits behind at most
/// [`WAKES_AHEAD_OF_A_DUE_CLOCK`] wakes, is answered `Idle`, and the wake after it is served on
/// the next call. A command still goes first - somebody is waiting on it.
#[test]
fn a_due_clock_waits_behind_a_bounded_run_of_wakes_but_not_of_commands() {
    let r = reg();
    let n = WAKES_AHEAD_OF_A_DUE_CLOCK + 2;
    for i in 0..n {
        r.create(format!("s-{i}"), "", SessionWiring::default())
            .unwrap();
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
    }
    for i in 0..n {
        r.bell().ring_wake(&format!("s-{i}"));
    }
    let past = Some(Instant::now());
    let mut answers = Vec::new();
    for _ in 0..n + 1 {
        answers.push(match r.next_work_until(past) {
            WorkOrIdle::Work(Work::Woken(_)) => "wake",
            WorkOrIdle::Idle => "idle",
            _ => "other",
        });
    }
    let mut expected = vec!["wake"; WAKES_AHEAD_OF_A_DUE_CLOCK];
    expected.push("idle");
    expected.extend(["wake", "wake"]);
    assert_eq!(
        answers, expected,
        "a due clock waited behind every wake — under a storm it would wait for ever"
    );

    let a = r.get("s-0").unwrap();
    let h = a.attach("tui", "dead", Caps::default(), 0);
    for i in 0..n {
        r.bell().ring_wake(&format!("s-{i}"));
    }
    for _ in 0..WAKES_AHEAD_OF_A_DUE_CLOCK {
        let _ = r.next_work_until(past);
    }
    a.submit(
        &h.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "hello".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    assert!(
        matches!(r.next_work_until(past), WorkOrIdle::Work(Work::Command(..))),
        "the operator's prompt waited behind a timer"
    );
}

/// A wake for a session this registry does not hold is dropped, the same way a command for one
/// is. The session is gone; there is nothing to wake.
#[test]
fn a_wake_for_an_unknown_session_does_not_stall_the_worker() {
    let r = reg();
    r.create("s-a", "", SessionWiring::default()).unwrap();
    // `create` rings an open, and an open is drained first (a session whose resume has not run
    // must not serve a command). Take it so the assertion below is about the wake.
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    r.bell().ring_wake("s-gone");
    r.bell().ring_wake("s-a");
    match r.next_work() {
        Some(Work::Woken(id)) => assert_eq!(id, "s-a"),
        _ => panic!("the unknown one must be skipped, not returned"),
    }
}

#[test]
fn the_default_session_follows_whoever_was_last_used() {
    let r = reg();
    let a = r.create("s-a", "", SessionWiring::default()).unwrap();
    let b = r.create("s-b", "", SessionWiring::default()).unwrap();
    assert_eq!(r.default_id(), "s-a");
    let hb = b.attach("tui", "bob", Caps::default(), 0);
    b.submit(
        &hb.head_id,
        "c1",
        0,
        CommandKind::Prompt {
            text: "x".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    r.next_command().unwrap();
    assert_eq!(r.default_id(), "s-b");
    let _ = a;
}

/// The notes source is read on the ASK, not remembered, and that is the one thing that
/// separates it from the job table's mailbox: a note written a second ago must be offered.
#[test]
fn the_notes_source_is_read_on_the_ask_and_a_session_without_one_has_none() {
    let r = reg();
    r.create("s1", "", SessionWiring::default()).unwrap();
    // A session whose harness has not opened has no source, and that is no rows - the same
    // answer an empty corpus gives, which the pane says out loud.
    assert!(r.notes("s1").is_empty());
    assert!(r.notes("nobody").is_empty());

    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = reads.clone();
    r.set_notes_source(
        "s1",
        Arc::new(move || {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (0..=n)
                .map(|i| crate::protocol::NoteEntry {
                    path: format!("/tmp/note-{i}.md"),
                    abstract_line: Some(format!("note {i}")),
                    abstract_written: false,
                    form: crate::protocol::NoteForm::Verbatim,
                })
                .collect()
        }),
    );
    assert_eq!(r.notes("s1").len(), 1);
    assert_eq!(
        r.notes("s1").len(),
        2,
        "the source is called again, not replayed"
    );
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// A wake is not one queue entry. A stopped or failed turn takes back the wake a settlement
/// rang, because the settlement is still queued for the next turn; a monitor's firing has no
/// queue behind it, so a drop that takes that back is a firing the session never reads.
#[test]
fn a_monitors_wake_survives_the_drop_a_stopped_turn_makes() {
    let r = reg();
    // A session the registry holds: a wake naming one it does not hold is refused by the
    // worker's own door, which is a different absence from the one this test is about.
    r.create("s-1", "", SessionWiring::default()).unwrap();
    assert!(matches!(r.next_work(), Some(Work::Open(_))));
    let bell = Arc::clone(r.bell());
    bell.ring_wake("s-1");
    bell.ring_wake("s-1");
    bell.drop_wakes("s-1");
    assert!(
        matches!(
            r.next_work_until(Some(Instant::now() + std::time::Duration::from_millis(200))),
            WorkOrIdle::Work(Work::Woken(id)) if id == "s-1"
        ),
        "nothing rung this session has to hear is left: the drop took a fact no queue holds"
    );
}
