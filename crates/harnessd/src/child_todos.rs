//! `todo_write`'s `target`: the daemon-side half of a PARENT writing a CHILD's board.
//!
//! # Why a module and not a method on `Harness`
//!
//! The parent's tool and the child's board are in the SAME daemon but never in the same
//! [`crate::harness::Harness`]: a child spawned by `task` is run by the thread that spawned it
//! (`serve_child` parks it there), and the daemon holds no `Harness` for it — `Sessions::open`
//! has the roots, the registry has the hubs, and the boards were, until now, reachable only
//! through the harness that owned them. So a session-id → board map, registered by every
//! `Harness` at open (root or child, resumed or fresh), is the one place a parent's write can
//! find its child — the same shape as `tree_slots` and `tree_head` one feature over: a live
//! `Arc` that rides [`crate::harness::Parts`] because it belongs to the daemon and not to any
//! one session's config.
//!
//! # What the resolver refuses, and why by name
//!
//! The operator's ask is narrow — *"yes - i want parent agents to be able to create todos for
//! subagents. throught tree author - (Parent <session-id-of-parent>)"* — and the whole of its
//! safety is one predicate: **the target must be a session whose `parent_session_id` is the
//! caller.** A session that could write a sibling's board, or its own parent's, is a session
//! that can overwrite another agent's plan, so every other shape is refused naming what was
//! asked for and what this session actually holds — the register the rest of this tree's
//! refusals keep. The caller's id is BOUND at construction (the resolver is built per session
//! in `Harness::open_with_registry`), so the author stamp `Parent <caller>` is the daemon's
//! fact and never the call's claim.
//!
//! # The flush, and why it is here rather than at the child's next turn
//!
//! [`crate::harness::Harness::flush_todos`] is version-gated and runs at the CHILD's turn
//! boundaries. A parked child runs no turn — `serve_child` is explicitly *"not on a clock"* —
//! so rows left to that path would sit in memory only, and a daemon restart would take them:
//! exactly the omission `Harness::set_operator_todos` records for the operator's rows
//! (*"on a session where no turn ever runs again, the operator's rows would sit on the board
//! unpublished and unwritten"*). So the resolver persists and publishes at the moment of the
//! write, through its own connection to the session store — the store's declared shape (WAL,
//! two connections: the harness's and the corpus's) rather than a workaround — and the child's
//! own `flush_todos` re-publishing the same list at its next boundary is a harmless duplicate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::registry::{Registry, short_id};
use letibot_tokencore::store::{Store, TodoBy, TodoStatus};

use letibot_tools::builtins::todo::{ChildTodos, TodoBoard};

/// Every live board this daemon holds, by session id — and the store connection a parent's
/// write flushes through. Held in [`crate::harness::Parts`], registered by every `Harness` at
/// open; never unregistered, because a finished child's board is still the truth its store row
/// holds and a late write to it is still the parent's to make.
#[derive(Default)]
pub struct ChildBoards {
    boards: Mutex<HashMap<String, Arc<TodoBoard>>>,
    /// The session store's path, for the resolver's own connection. `None` is a storeless
    /// daemon: the write still lands on the board, publishes to the child's hub, and is honestly
    /// not durable — the same shape a storeless session's own todos have.
    store_path: Option<PathBuf>,
    /// **A second connection to the session store, opened once on first write** — the corpus's
    /// own declared shape (`Store` is `Send` and not `Sync`, so the `Mutex` is what makes this
    /// shareable, and the WAL is what makes two connections the design rather than a race).
    store: Mutex<Option<Store>>,
}

impl ChildBoards {
    /// The daemon-wide object, from the config the daemon started under.
    pub fn shared(store_path: Option<PathBuf>) -> Arc<ChildBoards> {
        Arc::new(ChildBoards {
            boards: Mutex::new(HashMap::new()),
            store_path,
            store: Mutex::new(None),
        })
    }

    /// Called by `Harness::open_with_registry` for EVERY session this daemon opens — root,
    /// child, resumed or fresh — so a parent's write can find any of its children's boards.
    pub fn register(&self, session_id: &str, board: Arc<TodoBoard>) {
        self.boards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), board);
    }

    fn board(&self, session_id: &str) -> Option<Arc<TodoBoard>> {
        self.boards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    /// The child's list, persisted — or a note naming what did not happen, printed the way
    /// `Harness::set_operator_todos` prints its own. The rows are already ON the board when
    /// this runs, so a flush failure is reported and never unwritten.
    fn flush(&self, child: &str, board: &TodoBoard) {
        let items = board.snapshot();
        if let Some(path) = &self.store_path {
            let mut cell = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let store = match cell.as_ref() {
                Some(s) => s,
                None => match Store::open(path) {
                    Ok(s) => {
                        cell.insert(s);
                        cell.as_ref().expect("just inserted")
                    }
                    Err(e) => {
                        eprintln!("  todos: could not open the store for {child}'s row: {e}");
                        return;
                    }
                },
            };
            if let Err(e) = store.put_todos(child, &items) {
                eprintln!("  todos: could not persist the parent's row for {child}: {e}");
            }
        }
    }
}

/// **The resolver bound to ONE caller** — built per session in `Harness::open_with_registry`,
/// with the caller's own id and the daemon's registry already in hand. Implements the tool's
/// [`ChildTodos`]; everything the feature's safety turns on is decided here and nowhere else.
pub struct ParentTodos {
    /// The session whose `todo_write` this resolves for. The author stamp is built from THIS
    /// and never from the call, so a model cannot claim an authorship it does not have.
    caller: String,
    registry: Arc<Registry>,
    boards: Arc<ChildBoards>,
}

impl ParentTodos {
    pub fn new(caller: String, registry: Arc<Registry>, boards: Arc<ChildBoards>) -> Self {
        ParentTodos {
            caller,
            registry,
            boards,
        }
    }

    /// **The caller's own children, full ids** — the names the refusal lists, because they are
    /// the names `task`'s reply and `task_result`'s listing already handed the parent.
    fn my_children(&self) -> Vec<String> {
        self.registry
            .list()
            .into_iter()
            .filter(|b| b.parent_session_id.as_deref() == Some(self.caller.as_str()))
            .map(|b| b.session_id)
            .collect()
    }

    /// **The refusal's tail, the same for every arm**: what this session holds. A parent with
    /// no children is told THAT, which is a different fact from a typo and deserves its own
    /// sentence.
    fn held(&self) -> String {
        let mine = self.my_children();
        if mine.is_empty() {
            format!(
                "this session (`{}`) has no children at all — `target` names a session `task` \
                 spawned, and this one has none",
                self.caller
            )
        } else {
            format!(
                "this session (`{}`) holds {} child(ren): {}",
                self.caller,
                mine.len(),
                mine.join(", ")
            )
        }
    }

    /// Resolve the target to one brief: an exact id, else the short `…tail` form
    /// [`short_id`] spells — the one the subagent pane and `task_result` show. Ambiguity is
    /// refused with the candidates named, the house rule for an ambiguous name: the guess this
    /// refuses to make is one child's board changing under a parent that quoted another.
    fn resolve(&self, target: &str) -> Result<letibot_sessionlog::registry::SessionBrief, String> {
        let all = self.registry.list();
        if let Some(exact) = all.iter().find(|b| b.session_id == target) {
            return Ok(exact.clone());
        }
        let hits: Vec<_> = all
            .iter()
            .filter(|b| short_id(&b.session_id) == target)
            .collect();
        match hits.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(format!(
                "no session this daemon holds is `{target}` (full id or short `…tail` form). {}",
                self.held()
            )),
            many => {
                let named: Vec<String> =
                    many.iter().map(|b| format!("`{}`", b.session_id)).collect();
                Err(format!(
                    "`{target}` is {} of this daemon's sessions, so which one is not something \
                     this can know. Quote the full id: {}",
                    many.len(),
                    named.join(", ")
                ))
            }
        }
    }
}

impl ChildTodos for ParentTodos {
    fn upsert_child(
        &self,
        target: &str,
        rows: &[(String, TodoStatus)],
    ) -> Result<Vec<letibot_tokencore::store::TodoItem>, String> {
        let brief = self.resolve(target)?;
        // **THE ONE PREDICATE.** The target's `parent_session_id` must be the CALLER — a
        // session that could reach a sibling's or its parent's board is a session that can
        // overwrite another agent's plan. Refused naming what was asked, what it actually is,
        // and what this session holds.
        if brief.parent_session_id.as_deref() != Some(self.caller.as_str()) {
            let what_it_is = match &brief.parent_session_id {
                // The caller naming itself lands here too, as a session with a parent that is
                // not itself — said plainly rather than special-cased.
                Some(other) => format!("a child of `{other}`"),
                None => "a top-level session nobody spawned".to_string(),
            };
            return Err(format!(
                "`{}` is {}, not one of this session's children — a `target` must be a session \
                 THIS session spawned with `task`. {}",
                brief.session_id,
                what_it_is,
                self.held()
            ));
        }
        let child = brief.session_id.clone();
        let Some(board) = self.boards.board(&child) else {
            // Named rather than guessed at: a registry row with no registered board is a state
            // the caller can do nothing about, and the honest answer is the state itself.
            return Err(format!(
                "`{child}` is this session's child but has no board in this daemon — it was \
                 never opened here. Nothing was written; retry once it is running (`task`), or \
                 tell the child the work in your reply instead."
            ));
        };
        let author = TodoBy::parent_of(&self.caller);
        let changed = board.upsert_parent(rows, &author);
        if changed > 0 {
            // **Persist and publish at the moment of the write** — see the module doc for why
            // waiting for the child's next turn boundary is the omission, not the patience.
            // The publish reaches every head attached to the child, which is what keeps its
            // pane current; the store write is what survives the daemon's restart.
            self.boards.flush(&child, &board);
            if let Some(hub) = self.registry.get(&child) {
                hub.publish(SessionEvent::TodosUpdated {
                    todos: board
                        .snapshot()
                        .into_iter()
                        .map(crate::harness::Harness::todo_entry)
                        .collect(),
                });
                // **AND THE CHILD'S OWN READER IS WOKEN**, because the child is a session and a
                // session's board move arms its idle plan-check — the operator's ruling:
                // *"childs being just a session should get nags"*. A parked child is blocked
                // in `Hub::take_own_work_until`, and a publish alone does not open that door:
                // the condvar wakes, the take's predicate finds no command and no wake, and
                // the reader sleeps on with a new row on its board and no clock armed. The
                // wake carries no reason — it never does — and needs none: the child's loop
                // looks (`Harness::wake`, usually finding nothing settled), then sees the
                // board's version move and re-arms from THIS write, which is the same anchor
                // `SetOperatorTodos` gives a session the daemon holds.
                //
                // The rows are nag-worthy by construction: `the_check_may_ask_about` filters
                // by status and never by author, so what the parent wrote is asked about
                // exactly as what the model wrote — and what the operator postponed is not
                // asked about here either.
                hub.wake_its_own_reader();
            }
        }
        Ok(board.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use letibot_sessionlog::Hub;
    use letibot_sessionlog::registry::SessionWiring;
    use letibot_tokencore::store::{TodoItem, TodoStatus};

    fn wiring() -> SessionWiring {
        SessionWiring {
            model: "m".into(),
            dialect: "d".into(),
            endpoint: "http://127.0.0.1:1".into(),
            workspace: "/w".into(),
        }
    }

    /// A registry holding a parent, its two children, and somebody else's child — the family
    /// the refusals have to name their way through. Boards are registered the way every
    /// `Harness` registers them.
    fn family() -> (Arc<Registry>, Arc<ChildBoards>, ParentTodos) {
        let registry = Registry::new();
        let boards = ChildBoards::shared(None);
        for (id, parent) in [
            ("s-parent-1111", None),
            ("s-child-aaaa", Some("s-parent-1111")),
            ("s-child-bbbb", Some("s-parent-1111")),
            ("s-other-child", Some("s-somebody-else")),
        ] {
            let hub = registry.new_hub(id.to_string());
            registry
                .adopt(hub, id.to_string(), wiring(), parent.map(str::to_string))
                .expect("adopt");
            boards.register(id, Arc::new(TodoBoard::new(Vec::new())));
        }
        let resolver = ParentTodos::new("s-parent-1111".into(), registry.clone(), boards.clone());
        (registry, boards, resolver)
    }

    /// **The one predicate, both ways.** A child of the caller is writable; a sibling's child
    /// and the caller itself are refused — BY NAME, saying what the target is and what this
    /// session holds.
    #[test]
    fn only_a_session_this_caller_spawned_may_be_written() {
        let (_registry, _boards, resolver) = family();

        // One of ours, by full id.
        let board = resolver
            .upsert_child(
                "s-child-aaaa",
                &[("do the thing".into(), TodoStatus::Pending)],
            )
            .expect("the caller's own child is writable");
        assert!(
            board.iter().any(|t| t.content == "do the thing"),
            "{board:?}"
        );

        // And by the short form the pane shows (`…` + the last 8 characters — `short_id`).
        let board = resolver
            .upsert_child(
                "…ild-aaaa",
                &[("by the short id".into(), TodoStatus::Pending)],
            )
            .expect("the short form resolves");
        assert!(board.iter().any(|t| t.content == "by the short id"));

        // **Somebody else's child.**
        let why = resolver
            .upsert_child("s-other-child", &[("x".into(), TodoStatus::Pending)])
            .expect_err("a sibling's child is not this session's");
        assert!(
            why.contains("`s-other-child` is a child of `s-somebody-else`"),
            "the refusal says what the target IS: {why}"
        );
        assert!(
            why.contains("s-child-aaaa") && why.contains("s-child-bbbb"),
            "and what this session holds, by name: {why}"
        );

        // **The caller itself.**
        let why = resolver
            .upsert_child("s-parent-1111", &[("x".into(), TodoStatus::Pending)])
            .expect_err("a session may not write its own board through `target`");
        assert!(
            why.contains("not one of this session's children"),
            "yourself included, in the same sentence: {why}"
        );

        // **No such session.**
        let why = resolver
            .upsert_child("s-nope", &[("x".into(), TodoStatus::Pending)])
            .expect_err("an unknown id is refused");
        assert!(
            why.contains("no session this daemon holds is `s-nope`"),
            "{why}"
        );
    }

    /// **The author is the CALLER's, stamped daemon-side** — `Parent <full session id>`, the
    /// operator's own string — and the rows go in as the parent's half, leaving the child's own
    /// rows and the operator's exactly where they were.
    #[test]
    fn the_author_is_the_callers_own_string_and_nobody_elses_rows_move() {
        let (_registry, boards, resolver) = family();
        boards.register(
            "s-child-aaaa",
            Arc::new(TodoBoard::new(vec![
                TodoItem {
                    content: "the child's own".into(),
                    status: TodoStatus::InProgress,
                    by: TodoBy::Model,
                    when: None,
                },
                TodoItem {
                    content: "the operator's".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Operator,
                    when: None,
                },
            ])),
        );

        let board = resolver
            .upsert_child(
                "s-child-aaaa",
                &[
                    ("told by the parent".into(), TodoStatus::Pending),
                    ("and this too".into(), TodoStatus::Pending),
                ],
            )
            .expect("writable");
        assert!(
            board
                .iter()
                .any(|t| t.by == TodoBy::parent_of("s-parent-1111")),
            "the author is exactly `Parent <the caller's full id>`: {board:?}"
        );
        assert_eq!(
            board.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
            vec![
                "the child's own",
                "told by the parent",
                "and this too",
                "the operator's",
            ],
            "the child's rows lead, the parent's are between, the operator's are last: {board:?}"
        );

        // **And a second send moves state, never membership** — the upsert, through the real
        // resolver this time.
        let board = resolver
            .upsert_child(
                "s-child-aaaa",
                &[("told by the parent".into(), TodoStatus::Completed)],
            )
            .expect("writable");
        let moved = board
            .iter()
            .find(|t| t.content == "told by the parent")
            .expect("still there");
        assert_eq!(moved.status, TodoStatus::Completed);
        assert_eq!(
            board.len(),
            4,
            "no row was added, and none of the other authors' rows were deleted: {board:?}"
        );
    }

    /// **A write is PUBLISHED on the child's hub and PERSISTED in the store** — the two halves
    /// of the flush that keep the child's pane current and the rows alive past a daemon restart.
    ///
    /// The publish is asserted on the hub's own seq (`head_seq`), which advances once per
    /// published event: an unchanged re-send must not move it and a changed write must, which is
    /// the version rule read through the whole resolver. The persistence is asserted by opening
    /// the store again and reading the row back — the restart's own path.
    #[test]
    fn a_write_is_published_and_persisted() {
        let dir = std::env::temp_dir().join(format!("child-todos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let _ = std::fs::remove_file(&db);
        // The daemon's own precondition: the todo table is keyed to a session row
        // (`put_session`, exactly as `Harness::open_with_registry` writes it), so the
        // parent and the child are seated in the store the way a real daemon would.
        {
            let seed = Store::open(&db).unwrap();
            for (id, parent) in [
                ("s-parent-1111", None),
                ("s-child-aaaa", Some("s-parent-1111")),
            ] {
                seed.put_session(&letibot_tokencore::store::SessionRecord {
                    id: id.into(),
                    title: None,
                    model_id: "m".into(),
                    dialect_sha: "d".into(),
                    workspace_root: "/w".into(),
                    owner: "dead".into(),
                    role: None,
                    approvers: vec![],
                    parent_session_id: parent.map(str::to_string),
                })
                .expect("seed session");
            }
        }
        let registry = Registry::new();
        let boards = ChildBoards::shared(Some(db.clone()));
        for (id, parent) in [
            ("s-parent-1111", None),
            ("s-child-aaaa", Some("s-parent-1111")),
        ] {
            let hub = registry.new_hub(id.to_string());
            registry
                .adopt(hub, id.to_string(), wiring(), parent.map(str::to_string))
                .expect("adopt");
            boards.register(id, Arc::new(TodoBoard::new(Vec::new())));
        }
        let resolver = ParentTodos::new("s-parent-1111".into(), registry.clone(), boards.clone());
        let child = registry.get("s-child-aaaa").expect("the child's hub");

        // **Published.** The hub's seq advances exactly once for the write.
        let before = child.head_seq();
        resolver
            .upsert_child(
                "s-child-aaaa",
                &[("seen on the pane".into(), TodoStatus::Pending)],
            )
            .expect("writable");
        assert_eq!(
            child.head_seq(),
            before + 1,
            "one event — the TodosUpdated — was published to the child's hub"
        );

        // **Persisted.** A second connection reads back what a restart would find.
        let stored = Store::open(&db).unwrap();
        let rows = stored.todos("s-child-aaaa").unwrap();
        assert!(
            rows.iter().any(|t| t.content == "seen on the pane"),
            "the row survived in the store, as it must past a restart: {rows:?}"
        );

        // **And an unchanged re-send is not an event** — the version rule, through the resolver.
        resolver
            .upsert_child(
                "s-child-aaaa",
                &[("seen on the pane".into(), TodoStatus::Pending)],
            )
            .expect("writable");
        assert_eq!(
            child.head_seq(),
            before + 1,
            "nothing was republished for an unchanged list"
        );
        let _ = std::fs::remove_file(&db);
    }

    /// **An ambiguous short id is refused with the candidates named** — the house rule; the
    /// guess it refuses to make is one child's board changing under a parent that quoted
    /// another.
    #[test]
    fn an_ambiguous_short_id_is_refused_with_the_candidates_named() {
        let (registry, boards, resolver) = family();
        // Two children whose SHORT ids collide: the same last 8 characters.
        for id in ["s-child-alpha-collide1", "s-child-beta-collide1"] {
            let hub = registry.new_hub(id.to_string());
            registry
                .adopt(
                    hub,
                    id.to_string(),
                    wiring(),
                    Some("s-parent-1111".to_string()),
                )
                .expect("adopt");
            boards.register(id, Arc::new(TodoBoard::new(Vec::new())));
        }
        let why = resolver
            .upsert_child("…collide1", &[("x".into(), TodoStatus::Pending)])
            .expect_err("two children share this short id");
        assert!(
            why.contains("s-child-alpha-collide1") && why.contains("s-child-beta-collide1"),
            "both candidates are named: {why}"
        );
    }

    /// **A registry row with no board is refused as the state it is** — named, not guessed at.
    #[test]
    fn a_child_without_a_board_is_refused_as_the_state_it_is() {
        let (registry, boards, resolver) = family();
        // Adopt a fresh child whose board was never registered.
        let hub = registry.new_hub("s-child-late");
        registry
            .adopt(
                hub,
                "s-child-late".to_string(),
                wiring(),
                Some("s-parent-1111".into()),
            )
            .expect("adopt");
        let why = resolver
            .upsert_child("s-child-late", &[("x".into(), TodoStatus::Pending)])
            .expect_err("no board, no write");
        assert!(
            why.contains("no board in this daemon"),
            "the refusal names the state: {why}"
        );
        assert!(boards.board("s-child-late").is_none(), "and none was made");
    }
}
