//! **The review queue's own SESSION** — one, hardcoded, started by the daemon, whose todo board is
//! the queue.
//!
//! # The design, in the operator's words
//!
//! *"reviews are subagents managed by the main gatekeeper session which is hardcoded, started
//! either inside default model or configured model. it is a normal auto-driving session that treats
//! review queue as todo list - so nags and all. gatekeepers are subagents. so - i can connect to
//! that session, it managed via its prompt and so on."*
//!
//! And the analogy that draws the boundary, which is the cleanest statement of it there is:
//! *"Parents with children sending their homework to the teachers. School administration doesnt
//! teach or checks homework. it just makes sure pencils are here and rooms are warm."*
//!
//! So: the daemon is the **administration**. It fills the queue, keeps the worktrees and runs the
//! gates and the landing — and it does **not** review. The **teacher is a session**: the one this
//! module starts, whose board carries the queue as work. The parents are the working sessions and
//! the children are their subagents, and the reviews are the teacher's own children — which is the
//! NEXT slice and is deliberately absent here.
//!
//! **The design note this comes out of is `the-review-queue-and-the-merge-queue.md`** (the box-wide
//! notes, beside the operator's other ones), and one of its corrections is load-bearing here:
//! *"well gatekeeper is not a child so we are not doing up"* — the tutor is a session in its own
//! right, a **peer** of the main session, with no tree edge to walk. So this one is a ROOT with no
//! `parent_session_id`, and its rows carry the queue's own authorship and never `Parent <id>` (that
//! string is a claim about a session that wrote a child's board, and the daemon is not that session).
//! The note's own list of what the tutor still owes — the lateral `send`, the review/merge split —
//! is the work after this.
//!
//! # What this slice is, and what it deliberately is not
//!
//! **In:** the session exists (started once, resumable, listed, attachable), and its board carries
//! one row per entry waiting for a verdict, refreshed from the store on the daemon's existing review
//! clock. That is the whole of it.
//!
//! **Not in:** no review is dispatched, no gatekeeper child is started, no verdict is written, and
//! the queue's review row is still served the old way (`Harness::serve_reviews`). This slice makes
//! the SESSION real; the next one moves *who the review runs under*.
//!
//! # Where the rows live, and where the STATES live — the boundary this must not cross
//!
//! **The rows are on the session's board**, on the fourth half (`TodoBy::Queue`, written by
//! `TodoBoard::set_queue`), and they are a **view**: the half is REPLACED from the store on every
//! pass, so a row is on the board exactly while its entry is waiting for a verdict and is gone the
//! moment its verdict arrives — wherever that verdict was written.
//!
//! **The states are in the store and nowhere else.** `MergeEntry::state` is the queue's own word for
//! where an entry is, `ReviewRecord::decision` is the reviewer's, and this module writes neither.
//! The board is what a SESSION can be nagged about; it is not a second place a state can be read
//! from, and nothing that COUNTS the queue — the status line, the merge-queue pane, a head attached
//! to another daemon — counts it. That is the operator's boundary, in their words: *"there is a
//! mechanical review queue with well defined states. it is just not me moving between them but
//! gatekeeper subagent"* — the STATES do not move, only who moves an entry between them.
//!
//! # Why the row is on the board at all, rather than a pane
//!
//! Because the operator asked for a session *"that treats review queue as todo list - so nags and
//! all"*. Everything a session already has then applies to the queue with no new mechanism: the
//! idle plan-check (`Sessions::rearm_todo_nag` → `Harness::nag_notice`) names the next row, the pane
//! draws it, the store keeps it, and a head that attaches sees it. Nothing here arms anything: the
//! arming is the ordinary one, taken by `Sessions::fill_gatekeeper_board` after the board moves.
//!
//! # What the model is told about the rows
//!
//! Three places, and none of them is the system prompt — the session's prompt is the daemon's
//! ordinary one, because the operator's *"it managed via its prompt and so on"* is their own text
//! and not a paragraph this code writes:
//!
//! * **The nag** (`[todo check]`, `unfinished_plan_for`): the row's words, then `— the merge queue's`,
//!   then the advice, which says where the row comes off and that no field of `todo_write` moves it.
//!   That is the one place a model hears about a row BEFORE acting on it, which is why the author
//!   and the advice are both there.
//! * **`todo_write`'s reply** (`render`): the whole board, each of these rows marked `— the merge
//!   queue's`, so the model can tell what it wrote from what it was given.
//! * **The pane**, which draws these rows like any other: the one author it marks specially is the
//!   operator's (`crates/tui/src/ui/panes/todos.rs`), so a queue row draws as ordinary open work.
//!   That is a gap and a small one — the row says `review \`agent/…\` (entry …)` and the queue is
//!   legible from its own words.
//!
//! What none of them names is a verb to run, deliberately: no field of `todo_write` reaches this
//! half, and the review the row asks for is the next slice.
//!
//! # Which model it runs on
//!
//! The daemon's own — the operator's *"started either inside default model or configured model"*
//! read as *the default is the daemon's model, and the configured one is whatever this session was
//! switched to*: it is opened through the ordinary door, so `restore_provider_choice` hands it back
//! the model it was last switched to, which is the rule `--continue` already keeps for a
//! conversation. **A `[gatekeeper] model` of its own is not wired**, and deliberately not borrowed
//! from `Config::oracle_model`: that key is the GUARD's model — the local endpoint behind the smart
//! `!` and the adjudicator — and one key with two meanings is how a session comes to run on a model
//! nobody chose.
//!
//! # The one honest gap, named rather than discovered
//!
//! `cli.rs`'s `is_conversation` is the ONE predicate `--list-sessions` and `--continue` both read,
//! and the slice requires that `letibot --sessions` SHOW this session — so it must pass that
//! predicate. Which means `--continue` (and the launcher, which reads the same listing) can land the
//! operator in the review queue's session once it has rows, which is the shape of the defect
//! `GATEKEEPER_TITLE_PREFIX` was added for one word over (*"restart pg-noop and was brought to one
//! of the gatekeeper sessions"*). The fix is a decision and not a line: the two questions have to be
//! split, or this session marked non-conversational and the listing given a way to show it anyway.
//! It is left as it is because the slice's requirement is that the listing shows it, and because
//! choosing between those two is the operator's call.

use std::path::Path;

use letibot_tokencore::store::{MergeEntry, Store, TodoBy, TodoItem, TodoStatus};

use crate::mergequeue::{ReviewGate, is_ready, repo_of_worktree, review_gate, served_by};

/// **The session id, hardcoded, and the reason it is not minted like every other one.**
///
/// Session ids are `s-<now_ns>` — unique per creation and meaningless. This one has to be the
/// OPPOSITE: the same string in every daemon, on every start, forever, because it is the key three
/// things are decided by:
///
/// * **"Started once"** is a fact about this string. A daemon that comes up finds the session in the
///   store by it and RESUMES it ([`Registry::resumable`]'s own door, the same one
///   `mergequeue::GatekeeperDoor` uses to resume a host that has closed), so a second daemon — or
///   the same daemon restarted — does not make a twin.
/// * **Attaching is by name.** *"so - i can connect to that session"*: the operator types this id,
///   and it is the same id tomorrow.
/// * **The board is the queue for THAT session**, and a per-run id would strand a board and its
///   store rows behind every restart.
///
/// [`Registry::resumable`]: letibot_sessionlog::registry::Registry::resumable
pub const SESSION: &str = "s-gatekeeper";

/// What the session is called in a picker, and on its own store row. A title is set once and only
/// changed on purpose (`SessionBrief::title`), so this is what a person reads in `--sessions`.
pub const TITLE: &str = "the review queue";

/// **The session's SEAT, hardcoded, and it is the default one on purpose.**
///
/// The session has to spawn the review children (`task`, `task_result`, `task_message`,
/// `task_start`) and it has to open with **nobody attached** — the daemon starts it at startup,
/// before any head exists. Those two facts pick the seat between them: `orchestrator` is the only
/// seat that carries the whole task surface and **no gated tool at all**, so `Harness::open` has no
/// adjudicator to reach for and does not refuse.
///
/// A seat with `bash` (`gatekeeper` itself, `coder`, `runner`) would be refused at open with *"the
/// adjudicator attached is `none attached right now`"* — which is the right refusal, and the reason
/// the operator's *"gatekeepers are subagents"* is load-bearing: the CHILD that runs a review seats
/// `gatekeeper` and its asks go up to its tree's root, which is this session's head.
///
/// **It is not `self.base.seat`.** The daemon's `--role` is the seat of the OPERATOR's conversation;
/// a daemon started `--role coder` would otherwise seat the review queue as a coder, which cannot
/// open unattended.
pub const SEAT: crate::config::Seat = crate::config::Seat::Orchestrator;

/// **The rows this daemon's queue is waiting for verdicts on** — one per entry, in the order the
/// queue would serve them.
///
/// # Which entries, and why it is the queue's own answer rather than a second opinion
///
/// Two of the queue's own functions decide it, and nothing here re-derives either:
///
/// * [`is_ready`] — nothing the entry waits on is unlanded. An entry that is not ready is not
///   waiting for a REVIEW, it is waiting for another entry, and asking a session to review it would
///   be asking about a branch that is going to be rebased.
/// * [`review_gate`] — `Awaiting` is *nobody has judged this*: no review row at all, or one whose
///   `decision` is NULL. `Accepted` is a verdict that has landed (nothing is waiting), and
///   `Refused` is a verdict that parked the entry (nothing is being waited FOR) — neither is a row
///   on this board.
///
/// **And only this daemon's entries**, by [`served_by`] — the same rule the queue's own pass reads.
/// A board carrying another daemon's entries would be a session nagged about work it must not touch.
///
/// # The order
///
/// The queue's serving order — priority, then age, then id — so the first row on the board is the
/// next entry the queue would take. `next_ready`'s own comparator, spelled here because the board
/// has to be STABLE across passes: `set_queue` compares the rows to decide whether anything moved,
/// and a list that reshuffled itself would be an event on every pass.
pub fn queue_rows(store: &Store, serving: &Path) -> Vec<TodoItem> {
    // **This daemon's queue first, then the Waiting ones among them** — `MergeQueueDaemon::step`'s
    // own order of the same two questions, so the two cannot come to different answers about which
    // entries are waiting for a verdict. `is_ready` reads the SERVED set and not the whole table,
    // deliberately: that is the set the queue itself resolves a need against.
    let entries: Vec<MergeEntry> = store
        .merge_entries()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| mine(store, serving, e))
        .collect();
    let reviews = store.reviews().unwrap_or_default();
    let mut waiting: Vec<MergeEntry> = entries
        .iter()
        .filter(|e| {
            e.state == letibot_tokencore::store::MergeState::Waiting
                && is_ready(e, &entries)
                && matches!(
                    review_gate(e, reviews.iter().find(|r| r.entry_id == e.id)),
                    ReviewGate::Awaiting
                )
        })
        .cloned()
        .collect();
    waiting.sort_by_key(|e| (e.priority.rank(), e.created_ms, e.id.clone()));
    waiting.iter().map(row).collect()
}

/// **Whether this entry is this daemon's to work** — [`served_by`], with the entry's repository read
/// only when the fallback arm needs it.
///
/// `served_by` takes the repository eagerly and that is right for the queue's own daemon, which
/// caches it per entry. This caller runs on the WORKER thread — the one that runs turns — once per
/// pass, and `repo_of_worktree` forks a `git`; so the row is read first and the subprocess is spent
/// only for the entry whose root session the store has forgotten, which is the one case the
/// repository decides. The row is then read a second time inside `served_by`, which costs one
/// indexed `SELECT` and no process.
fn mine(store: &Store, serving: &Path, entry: &MergeEntry) -> bool {
    let root = crate::mergequeue::root_session(store, &entry.session_id);
    if store.session(&root).ok().flatten().is_some() {
        return served_by(store, Some(serving), entry, serving);
    }
    served_by(store, Some(serving), entry, &repo_for(entry, serving))
}

/// **The repository an entry's worktree belongs to**, or the workspace itself — the fallback
/// [`served_by`] consults when the store has no row for the session that queued the entry.
///
/// Read here rather than cached, unlike `MergeQueueDaemon::repo_for`: the queue is one row per
/// branch, and the caller ([`mine`]) reaches this only for the entry whose root session the store
/// has forgotten. A caller that read it eagerly would spend a `git rev-parse` per entry per pass.
fn repo_for(entry: &MergeEntry, serving: &Path) -> std::path::PathBuf {
    entry
        .worktree
        .as_deref()
        .and_then(|w| repo_of_worktree(Path::new(w)))
        .unwrap_or_else(|| serving.to_path_buf())
}

/// **One entry, as a row on the session's board.**
///
/// The text is the row's NAME, and the board has no id: `todo_write` addresses a row by quoting its
/// words and the nag hands them over character for character, so this has to be STABLE across passes
/// (the whole half is compared) and it has to say what the row is about. Three facts, in the order
/// they read:
///
/// * **the branch** — what the verdict is about, and what the worktree holds;
/// * **the entry id** — what `/queue approve|restart|rm` take, and what the review is keyed by;
/// * **the first line of the brief** — what the review is *against*, which is the one thing a
///   reviewer cannot get from the branch alone. Capped, because a row is a name and not a document:
///   the whole brief is on the entry, and the session that reviews it is handed the brief itself.
///
/// The state is `Pending` and the author is `TodoBy::Queue`; there are no edges and no condition,
/// because the entry's own `needs` are already resolved by `is_ready` and a condition is the
/// operator's own act (`TodoItem::when`).
fn row(entry: &MergeEntry) -> TodoItem {
    TodoItem {
        content: format!(
            "review `{}` (entry {}): {}",
            entry.branch,
            entry.id,
            first_line(&entry.brief),
        ),
        status: TodoStatus::Pending,
        by: TodoBy::Queue,
        when: None,
        needs: Vec::new(),
    }
}

/// How much of the brief's first line a row carries. Long enough for a sentence that says what the
/// branch was asked to do, short enough that the row is still a name — see [`row`].
const BRIEF_CHARS: usize = 120;

/// The brief's first line, trimmed, capped with an ellipsis when it was longer.
fn first_line(brief: &str) -> String {
    let line = brief.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(BRIEF_CHARS).collect();
    if line.chars().count() > BRIEF_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use letibot_sessionlog::registry::Registry;
    use letibot_tokencore::store::{MergePriority, MergeState, ReviewRecord};

    use crate::config::Config;
    use crate::harness::Parts;
    use crate::sessions::{Sessions, StoreSessions};

    /// A directory that removes itself, because a test that leaks a store per run eventually fills
    /// the disk and the run that finds out is not this one.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "letibot-gatekeeper-{tag}-{}-{}",
                std::process::id(),
                {
                    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                }
            ));
            std::fs::create_dir_all(&path).expect("creating the temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// The workspace every test's daemon serves, and the store the queue lives in. The workspace is
    /// returned CANONICAL, because that is what `served_by` compares against.
    fn store_with(tag: &str) -> (TempDir, Store, PathBuf) {
        let dir = TempDir::new(tag);
        let ws = dir.path.join("ws");
        std::fs::create_dir_all(&ws).expect("the workspace");
        let ws = std::fs::canonicalize(&ws).expect("the workspace canonicalizes");
        let store = Store::open(&dir.path.join("sessions.db")).expect("the store opens");
        (dir, store, ws)
    }

    /// One queue entry, `Waiting`, from a session rooted at `ws`.
    ///
    /// **The entry's `session_id` is the CHILD that did the work, and the root is found by walking
    /// up** — `HarnessTaskRunner::enqueue_entry` writes `entry_for_finished(handle, handle, …)`, so
    /// both columns are the child (`the-review-queue-and-the-merge-queue.md`, measured by the
    /// `queue-verbs` child). The fixture is built that way on purpose: a root row and a child under
    /// it, so `served_by`'s walk is exercised rather than assumed — an entry written as its own root
    /// would pass whether the walk worked or not.
    fn entry(store: &Store, ws: &Path, id: &str, brief: &str) {
        let now = (crate::config::now_ns() / 1_000_000) as u64;
        let root = format!("s-root-{id}");
        let child = format!("{root}-sub");
        for (sid, parent) in [(&root, None), (&child, Some(root.clone()))] {
            store
                .put_session(&letibot_tokencore::store::SessionRecord {
                    id: sid.clone(),
                    title: None,
                    model_id: "m".into(),
                    dialect_sha: "d".into(),
                    workspace_root: ws.display().to_string(),
                    owner: "operator".into(),
                    role: Some("coder".into()),
                    approvers: Vec::new(),
                    parent_session_id: parent,
                })
                .expect("the session row");
        }
        store
            .put_merge_entry(&MergeEntry {
                id: id.into(),
                session_id: child,
                branch: format!("agent/{id}"),
                base_sha: "base".into(),
                priority: MergePriority::Subagent,
                needs: Vec::new(),
                state: MergeState::Waiting,
                brief: brief.into(),
                evidence: String::new(),
                created_ms: now,
                updated_ms: now,
                worktree: None,
                landed_sha: None,
                gate_steps: Vec::new(),
            })
            .expect("the entry row");
    }

    /// The review row a verdict is written onto — unanswered when `decision` is `None`.
    fn review(store: &Store, entry_id: &str, decision: Option<&str>) {
        let now = (crate::config::now_ns() / 1_000_000) as u64;
        store
            .put_review(&ReviewRecord {
                entry_id: entry_id.into(),
                session_id: "s-gatekeeper".into(),
                branch: format!("agent/{entry_id}"),
                base_sha: "base".into(),
                asked_ms: now,
                answered_ms: decision.map(|_| now),
                decision: decision.map(str::to_string),
                attempts: 0,
                failed_ms: None,
                failure: String::new(),
                reasons: Vec::new(),
                files: Vec::new(),
                commands: Vec::new(),
            })
            .expect("the review row");
    }

    /// **One row per entry waiting for a verdict, and the row says which entry it is.**
    ///
    /// The queue's own three words are on it — the branch, the entry id and the first line of the
    /// brief — because the board has no id column and the words ARE the name.
    #[test]
    fn one_row_per_entry_waiting_for_a_verdict() {
        let (_dir, store, ws) = store_with("rows");
        entry(
            &store,
            &ws,
            "m-one",
            "make the reader keep the last record\nand then some",
        );
        entry(&store, &ws, "m-two", "the other branch");
        // One asked for and unanswered, one nobody has asked about: both are WAITING for a verdict.
        review(&store, "m-one", None);

        let rows = queue_rows(&store, &ws);
        assert_eq!(rows.len(), 2, "one row per entry waiting: {rows:?}");
        assert!(
            rows.iter().all(|r| r.by == TodoBy::Queue),
            "the rows are the QUEUE's — the fourth author — and not the model's plan: {rows:?}"
        );
        assert!(
            rows.iter().all(|r| r.status == TodoStatus::Pending),
            "a queue row is work not started: {rows:?}"
        );
        let first = &rows[0].content;
        assert!(
            first.contains("m-one") && first.contains("agent/m-one"),
            "the row names the entry AND the branch, which is what a verdict is about: {first}"
        );
        assert!(
            first.contains("make the reader keep the last record"),
            "and the first line of the brief, which is what the review is AGAINST: {first}"
        );
        assert!(
            !first.contains("and then some"),
            "one line only — the whole brief is on the entry, not in the row's name: {first}"
        );
        assert_eq!(
            rows[1].content,
            "review `agent/m-two` (entry m-two): the other branch"
        );
    }

    /// **A verdict reached elsewhere takes the row off** — the third thing this slice is for.
    ///
    /// Written the way another host would write it: straight into the store, with nothing in this
    /// process involved. The row is not marked done and not left behind as a record — it is GONE,
    /// because the board is a view of the queue and the entry is not in the queue's *waiting for a
    /// verdict* set any more.
    #[test]
    fn a_verdict_reached_elsewhere_takes_the_row_off() {
        let (_dir, store, ws) = store_with("verdict");
        entry(&store, &ws, "m-one", "keep the last record");
        entry(&store, &ws, "m-two", "the other branch");
        assert_eq!(queue_rows(&store, &ws).len(), 2, "both are waiting");

        review(&store, "m-one", Some("accept"));
        let rows = queue_rows(&store, &ws);
        assert_eq!(
            rows.len(),
            1,
            "the accepted entry is not waiting for anything and its row is off: {rows:?}"
        );
        assert!(
            rows[0].content.contains("m-two"),
            "and the one still waiting keeps its row: {:?}",
            rows[0]
        );

        // **And a REFUSAL is not a row either**, for the other reason: the entry is parked, so
        // nothing is being waited FOR — the verdict is in and it is the entry's state now.
        review(&store, "m-two", Some("reject"));
        assert!(
            queue_rows(&store, &ws).is_empty(),
            "a refused entry is parked, not waiting for a verdict"
        );
    }

    /// **An entry this daemon does not serve is not on its session's board.**
    ///
    /// One store holds every daemon's queue and each session is served by the daemon for its
    /// workspace — so a board carrying another workspace's entry would be a session nagged about
    /// work it must not touch, which is `served_by`'s whole reason for being a function.
    #[test]
    fn another_workspaces_entry_is_not_on_this_board() {
        let (_dir, store, ws) = store_with("served");
        let elsewhere = ws.parent().expect("a parent").join("other");
        std::fs::create_dir_all(&elsewhere).expect("the other workspace");
        entry(&store, &ws, "m-mine", "mine to review");
        entry(&store, &elsewhere, "m-theirs", "theirs to review");

        let rows = queue_rows(&store, &ws);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].content.contains("m-mine") && !rows[0].content.contains("m-theirs"),
            "the board is this daemon's queue and nothing else: {:?}",
            rows[0]
        );
    }

    /// **An entry that is not ready waits for another ENTRY, not for a reviewer.**
    ///
    /// `is_ready` is the queue's own word for it and this is the arm a second opinion would get
    /// wrong: a `Waiting` entry whose `needs` have not landed is going to be rebased, and asking a
    /// session to review it would be asking about a branch that is about to move.
    #[test]
    fn an_entry_waiting_on_another_entry_is_not_a_review_to_run() {
        let (_dir, store, ws) = store_with("needs");
        entry(&store, &ws, "m-first", "the first branch");
        entry(&store, &ws, "m-second", "the dependent branch");
        let mut second = store
            .merge_entry("m-second")
            .expect("reads")
            .expect("the row");
        second.needs = vec!["m-first".into()];
        store.put_merge_entry(&second).expect("the dependent row");

        let rows = queue_rows(&store, &ws);
        assert_eq!(
            rows.len(),
            1,
            "only the entry with nothing in front of it: {rows:?}"
        );
        assert!(rows[0].content.contains("m-first"), "{:?}", rows[0]);
    }

    /// **A daemon over a temp store**, for the three things this slice is measured by: the session
    /// is started ONCE, its board carries the queue, and a verdict elsewhere takes a row off.
    ///
    /// `None` after announcing the skip when this box has no vocabulary GGUF — `Harness::open`
    /// renders its stable prefix, which is `review_pass`'s situation answered the same way.
    struct Fixture {
        /// Held, not read: dropping it deletes the store.
        _dir: TempDir,
        db: PathBuf,
        ws: PathBuf,
        cfg: Config,
        registry: Arc<Registry>,
    }

    impl Fixture {
        fn new(tag: &str) -> Option<Self> {
            let gguf = letibot_tokencore::apparatus::present_gguf()?;
            let dir = TempDir::new(tag);
            let ws = dir.path.join("ws");
            std::fs::create_dir_all(&ws).expect("the workspace");
            let ws = std::fs::canonicalize(&ws).expect("the workspace canonicalizes");
            let db = dir.path.join("sessions.db");
            let mut cfg = Config::for_this_box(&ws);
            cfg.vocab_gguf = Some(gguf);
            // **A temp store, never the daemon's own**: the queue lives in `sessions.db`.
            cfg.store = Some(db.clone());
            cfg.session_id = "s-host".into();
            cfg.http_retries = 0;
            let registry = Registry::new();
            registry
                .create(&cfg.session_id, "", Sessions::wiring(&cfg))
                .expect("the daemon's own session registers");
            // **The store as a SOURCE**, which is the half `ensure_gatekeeper` resumes through: a
            // session that is on disk and not in this daemon is found here and brought in under its
            // own id. Production sets this at startup (`cli::run`), and without it the resume arm
            // would be dead code in the one test that means to exercise it.
            registry.set_source(StoreSessions::open(&cfg).expect("the store as a source"));
            Some(Self {
                _dir: dir,
                db,
                ws,
                cfg,
                registry,
            })
        }

        fn sessions<'p>(&self, parts: &'p Parts) -> Sessions<'p> {
            Sessions::open_first(parts, self.cfg.clone(), self.registry.clone())
                .expect("the daemon opens")
        }

        /// A store connection that did not write the rows.
        fn store(&self) -> Store {
            Store::open(&self.db).expect("the store reopens")
        }

        /// One queue entry, `Waiting`, queued by a session rooted at this daemon's workspace.
        fn entry(&self, id: &str, brief: &str) {
            entry(&self.store(), &self.ws, id, brief);
        }

        /// The verdict on an entry, written the way another host writes it: straight into the
        /// store, with nothing in this process involved.
        fn verdict(&self, entry_id: &str, decision: &str) {
            review(&self.store(), entry_id, Some(decision));
        }

        /// The rows the queue has put on the gatekeeper session's board.
        fn board(&self, sessions: &Sessions<'_>) -> Vec<TodoItem> {
            sessions
                .open
                .get(SESSION)
                .expect("the gatekeeper session is open")
                .todo_list()
                .into_iter()
                .filter(|r| r.by == TodoBy::Queue)
                .collect()
        }
    }

    /// **The session is started once, and a second pass or a second daemon does not make a twin.**
    ///
    /// The idle arm calls `ensure_gatekeeper` on every pass it reads the store — every thirty
    /// seconds — so "once" has to be a property of the door and not of the caller. It is two
    /// checks: `self.open` for this daemon, and the store (through the registry) for the box.
    #[test]
    fn the_session_is_started_once_and_not_twice() {
        let Some(f) = Fixture::new("once") else {
            return;
        };
        let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
        let mut sessions = f.sessions(&parts);
        let held = |reg: &Registry| {
            reg.list()
                .iter()
                .filter(|b| b.session_id == SESSION)
                .count()
        };

        assert!(
            sessions.ensure_gatekeeper().expect("it starts"),
            "the daemon starts it"
        );
        assert!(
            sessions.open.contains_key(SESSION),
            "and holds a harness for it"
        );
        assert_eq!(held(&f.registry), 1, "one row in the registry, not two");

        // **The second pass is the same session, not a second one.** This is the common case: the
        // idle arm reaches this line every time the review clock is due.
        assert!(
            !sessions.ensure_gatekeeper().expect("a no-op"),
            "the second pass starts nothing"
        );
        assert_eq!(
            sessions.opened(),
            2,
            "the held session and the queue's, and no third"
        );
        assert_eq!(held(&f.registry), 1, "still one");

        // **And a second DAEMON does not make a twin either** — the restart, one process along: a
        // fresh registry over the same store, which finds the session on disk and RESUMES it.
        //
        // **The name is changed in the store first, and that is what makes this an assertion rather
        // than a coincidence**: a session made fresh would carry the hardcoded `TITLE`, and one
        // RESUMED carries the name its own row holds. The count alone could not tell the two apart —
        // `put_session` is skipped for a row that is already there, so a twin would leave one row too.
        f.store()
            .set_title(SESSION, "the queue, renamed")
            .expect("the store takes a name");
        let second = Registry::new();
        second
            .create("s-host-2", "", Sessions::wiring(&f.cfg))
            .expect("the second daemon's own session registers");
        second.set_source(StoreSessions::open(&f.cfg).expect("the store as a source"));
        let mut other = Sessions::open_first(
            &parts,
            Config {
                session_id: "s-host-2".into(),
                ..f.cfg.clone()
            },
            second.clone(),
        )
        .expect("the second daemon opens");
        assert!(other.ensure_gatekeeper().expect("it brings it in"));
        assert_eq!(held(&second), 1, "under the SAME id, resumed");
        assert_eq!(
            held(&f.registry),
            1,
            "and the first daemon's row is untouched"
        );
        assert_eq!(
            second
                .list()
                .iter()
                .find(|b| b.session_id == SESSION)
                .map(|b| b.title.as_str()),
            Some("the queue, renamed"),
            "RESUMED, not re-made: it came back under the name its own store row holds"
        );

        let stored = f.store().list_sessions().expect("the store lists");
        assert_eq!(
            stored.iter().filter(|s| s.id == SESSION).count(),
            1,
            "ONE row in the store: a second daemon resumes the session, it does not mint a twin"
        );
        assert_eq!(
            stored
                .iter()
                .find(|s| s.id == SESSION)
                .map(|s| s.title.as_deref()),
            Some(Some("the queue, renamed")),
            "and it is the session a person reads in `--sessions`: {stored:?}"
        );
    }

    /// **Its board carries one row per entry waiting for a verdict, and the ordinary idle nag is
    /// what is driven by it.**
    ///
    /// The rows are the queue's (`TodoBy::Queue`), the clock is the one every session already has,
    /// and the notice names the row — which is the whole of *"it is a normal auto-driving session
    /// that treats review queue as todo list - so nags and all"*.
    #[test]
    fn its_board_carries_one_row_per_entry_waiting_for_a_verdict() {
        let Some(f) = Fixture::new("board") else {
            return;
        };
        let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
        f.entry("m-one", "make the reader keep the last record");
        f.entry("m-two", "the other branch");
        let mut sessions = f.sessions(&parts);
        sessions.ensure_gatekeeper().expect("the session starts");

        assert_eq!(
            sessions.fill_gatekeeper_board(),
            2,
            "one row per entry waiting for a verdict"
        );
        let rows = f.board(&sessions);
        assert_eq!(rows.len(), 2, "the rows are on the QUEUE's half: {rows:?}");
        assert!(
            rows.iter().all(|r| r.status == TodoStatus::Pending),
            "a queue row is work not started: {rows:?}"
        );
        let said: Vec<&str> = rows.iter().map(|r| r.content.as_str()).collect();
        assert!(
            said.iter().any(|r| r.contains("m-one")) && said.iter().any(|r| r.contains("m-two")),
            "one row each, named by the entry they are about: {said:?}"
        );

        // **AND THE ORDINARY IDLE NAG IS ARMED ON THEM.** The clock is the one this session already
        // had (`Sessions::rearm_todo_nag`, the same arm a turn end and an operator's row take), so
        // nothing here is a second mechanism — and the notice it will send says whose row it is.
        assert!(
            sessions
                .nags
                .get(SESSION)
                .and_then(|clock| clock.due_at())
                .is_some(),
            "the plan-check is armed on the rows the queue wrote"
        );
        let notice = sessions
            .open
            .get_mut(SESSION)
            .expect("the gatekeeper session is open")
            .nag_notice()
            .expect("the check has something to ask about");
        assert!(
            notice.contains("the merge queue's"),
            "the check says whose row it is holding the session to: {notice}"
        );
        assert!(
            notice.contains("m-one") || notice.contains("m-two"),
            "and it names the entry: {notice}"
        );
        assert!(
            notice.contains("comes off when"),
            "and it does not offer a verb that has no door: {notice}"
        );
    }

    /// **An entry that reaches a verdict elsewhere takes its row off** — written the way another
    /// host writes it, with nothing in this process involved.
    ///
    /// The row is not marked done and not kept as a record: the half is a VIEW of the queue, and the
    /// entry is not in the queue's *waiting for a verdict* set any more. The state it reached lives
    /// on the entry and the review in the store, which is where anything that COUNTS the queue reads
    /// it from.
    #[test]
    fn a_verdict_reached_elsewhere_takes_its_row_off() {
        let Some(f) = Fixture::new("verdict") else {
            return;
        };
        let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
        f.entry("m-one", "keep the last record");
        f.entry("m-two", "the other branch");
        let mut sessions = f.sessions(&parts);
        sessions.ensure_gatekeeper().expect("the session starts");
        assert_eq!(sessions.fill_gatekeeper_board(), 2, "both are waiting");

        f.verdict("m-one", "accept");
        assert_eq!(
            sessions.fill_gatekeeper_board(),
            1,
            "the accepted entry's row is off the board"
        );
        let rows = f.board(&sessions);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].content.contains("m-two"),
            "and the one still waiting keeps its row: {rows:?}"
        );

        // **And a queue standing still is not an event**: the same rows again change nothing, so the
        // board does not publish and the store is not written on every pass.
        let version = sessions
            .open
            .get(SESSION)
            .expect("open")
            .todo_board_version();
        assert_eq!(sessions.fill_gatekeeper_board(), 1);
        assert_eq!(
            sessions
                .open
                .get(SESSION)
                .expect("open")
                .todo_board_version(),
            version,
            "a queue that has not moved is not a write"
        );
    }
}
