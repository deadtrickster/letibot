//! The review and merge queues' moves as channel notifications: an enqueue, a verdict, a
//! landing, a red gate and a red target, so an orchestrator needs no `letibot queue` poll of its
//! own. One move is one notification, keyed by the transition's own `seq`.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

use letibot_tokencore::store::{HealthState, MergeState, QueueTransition, ReviewState, Store};
use serde_json::{Value, json};

/// How often the store is read for a move nobody has been told about. The queue's own watcher
/// reads `queue_transition` every 2 s, so this follows its grain rather than polling harder.
pub const POLL: Duration = Duration::from_secs(2);

/// Most moves one pass says. A backlog - a bridge restarted with the store unreachable for a
/// while - drains over the next passes instead of firing a burst at one turn.
pub const PER_PASS: usize = 8;

/// Longest reason line a notification carries; the queue's pane has the whole sentence.
const MAX_REASON: usize = 600;

/// One folder's queue news: the store, the repository whose rows are that folder's, and how far
/// it has already been said. The place is kept in memory for this run and written to a file so a
/// bridge restarted on the same folder resumes instead of repeating itself.
#[derive(Debug)]
pub struct Mover {
    store: PathBuf,
    repository: String,
    cursor: PathBuf,
    /// The last move said, as this process knows it - the authority while it runs, because a
    /// file that cannot be written must not make this run repeat itself or fall silent.
    place: Cell<Option<i64>>,
    /// Whether the unkeepable place has been said once, so it is not said every pass.
    told: Cell<bool>,
}

impl Mover {
    /// A follower of `repository`'s rows in `store`, keeping its place in `cursor`.
    pub fn new(
        store: impl Into<PathBuf>,
        repository: impl Into<String>,
        cursor: impl Into<PathBuf>,
    ) -> Mover {
        Mover {
            store: store.into(),
            repository: repository.into(),
            cursor: cursor.into(),
            place: Cell::new(None),
            told: Cell::new(false),
        }
    }

    /// Say what has moved since the last pass: at most [`PER_PASS`] notifications, oldest first.
    /// `Err` is a store this pass could not read, which the caller retries - the moves stay
    /// recorded and a follower that gave up would never say anything again.
    pub fn step(&self) -> Result<Vec<Value>, String> {
        // A folder with no store has no queue: the daemon that serves one writes it.
        if !crate::store::exists(&self.store) {
            return Ok(Vec::new());
        }
        let store = crate::store::open_read(&self.store, "queue_transition", None)
            .map_err(|e| format!("reading the queues: {e}"))?;
        let after = self.place(&store)?;
        let moves = store
            .transitions_after(after, PER_PASS)
            .map_err(|e| format!("reading the queues: {e}"))?;
        let Some(last) = moves.last().map(|m| m.seq) else {
            return Ok(Vec::new());
        };
        let mut notes = Vec::new();
        for t in &moves {
            // Another repository's rows pass the cursor without being said: this bridge speaks
            // for its own folder, and a move somebody else's queue made is not its news.
            if t.repository != self.repository {
                continue;
            }
            let landed = landed_sha(&store, t);
            if let Some(note) = event(t, landed.as_deref()) {
                notes.push(note);
            }
        }
        self.remember(last);
        Ok(notes)
    }

    /// Where this follower has got to. A folder followed for the first time starts at the queue's
    /// head: a client joining now wants the moves from now, not every row the box ever landed.
    fn place(&self, store: &Store) -> Result<i64, String> {
        if let Some(seq) = self.place.get() {
            return Ok(seq);
        }
        let seq = match read_cursor(&self.cursor) {
            Some(seq) => seq,
            None => store
                .last_transition_seq()
                .map_err(|e| format!("reading the queues: {e}"))?,
        };
        self.remember(seq);
        Ok(seq)
    }

    /// Move the place on, and try to make it outlive this process. A cursor file that cannot be
    /// written costs a restart its memory and nothing else, so it is said once - never a reason
    /// to re-read the store's head every pass and repeat the same moves.
    fn remember(&self, seq: i64) {
        self.place.set(Some(seq));
        if !write_cursor(&self.cursor, seq) && !self.told.replace(true) {
            eprintln!(
                "  queue push: {} cannot be written, so a restart starts from the queue's head \
                 rather than from here",
                self.cursor.display()
            );
        }
    }
}

/// Follow this folder's queue for as long as the bridge lives, sending each new move on `tx`.
/// `socket` is the folder's daemon, and `None` is a caller that has one of its own (a test):
/// while that daemon is down nothing is moving the queue, so the store is not opened at all.
pub fn watch(store: &Path, folder: &Path, socket: Option<&Path>, tx: Sender<Value>) {
    let repository = repository_of(folder);
    // Named by the FOLDER, not the repository: a bridge in the main checkout and one in a
    // worktree of it follow one queue, and a place they shared would let either one pass a move
    // the other never says. A restart of that folder's bridge finds its own place.
    let cursor = crate::daemon::rundir().join(format!("{}.queue", crate::daemon::key(folder)));
    let mover = Mover::new(store, repository, cursor);
    let socket = socket.map(Path::to_path_buf);
    let _ = std::thread::Builder::new()
        .name("leticode-push-queue".into())
        .spawn(move || run(mover, socket, tx));
}

fn run(mover: Mover, socket: Option<PathBuf>, tx: Sender<Value>) {
    let mut said: Option<String> = None;
    loop {
        let up = socket.as_deref().is_none_or(crate::daemon::is_up);
        match up.then(|| mover.step()) {
            Some(Ok(notes)) => {
                said = None;
                for note in notes {
                    if tx.send(note).is_err() {
                        return;
                    }
                }
            }
            // Said once per distinct failure, not once per pass: a store held by a writer all
            // evening is one fact about the box, not hundreds.
            Some(Err(why)) if said.as_deref() != Some(why.as_str()) => {
                eprintln!("  queue push: {why}");
                said = Some(why);
            }
            // No move, the same failure again, or a daemon that is not up: nothing to say.
            _ => {}
        }
        std::thread::sleep(POLL);
    }
}

/// The notification for one move, `None` for a move that is not news - a row picked up, a
/// rebase, a withdrawal, a target that went green. The merge row an approval births is the
/// verdict's second half and not a second move, so a verdict is one event and not two.
pub fn event(t: &QueueTransition, landed: Option<&str>) -> Option<Value> {
    let (kind, mut said, fact): (&str, String, Option<(&str, String)>) = match (
        t.queue.as_str(),
        t.from_state.as_deref(),
        t.to_state.as_str(),
    ) {
        ("review", None, s) if s == ReviewState::Waiting.as_str() => {
            ("enqueued", "went into the review queue".into(), None)
        }
        ("review", _, s) if s == ReviewState::Approved.as_str() => (
            "verdict",
            "was APPROVED".into(),
            Some(("verdict", "approved".into())),
        ),
        ("review", _, s) if s == ReviewState::Rejected.as_str() => (
            "verdict",
            "was REFUSED".into(),
            Some(("verdict", "refused".into())),
        ),
        ("review", _, s) if s == ReviewState::CannotDecide.as_str() => (
            "verdict",
            "could not be decided".into(),
            Some(("verdict", "cannot_decide".into())),
        ),
        ("merge", _, s)
            if s == MergeState::Landed.as_str() || s == MergeState::LandedLocal.as_str() =>
        {
            let at = match landed {
                Some(sha) => format!(" at {sha}"),
                None => String::new(),
            };
            (
                "landed",
                format!("LANDED{at}"),
                landed.map(|sha| ("sha", sha.to_string())),
            )
        }
        ("merge", _, s) if s == MergeState::Failed.as_str() => (
            "gate_failed",
            "FAILED its gate".into(),
            failing_test(&t.why).map(|name| ("test", name)),
        ),
        ("target", _, s) if s == HealthState::Red.as_str() => ("main_red", "is RED".into(), None),
        _ => return None,
    };
    if kind == "gate_failed"
        && let Some((_, name)) = &fact
    {
        said = format!("FAILED its gate: first failing test `{name}`");
    }
    let mut meta = serde_json::Map::new();
    meta.insert("source".into(), json!("leticode"));
    meta.insert("event".into(), json!(kind));
    meta.insert("queue".into(), json!(t.queue));
    meta.insert("entry".into(), json!(t.entry_id));
    meta.insert("repository".into(), json!(t.repository));
    meta.insert("state".into(), json!(t.to_state));
    // The transition's own id: the queue's record of this move, and what a follower dedupes by.
    meta.insert("transition".into(), json!(t.seq.to_string()));
    if let Some((key, value)) = fact {
        meta.insert(key.into(), json!(value));
    }
    let head = format!(
        "leticode queue `{}` ({}) `{}` {said}",
        t.repository, t.queue, t.entry_id
    );
    let reason = cut(t.why.trim(), MAX_REASON);
    let content = if reason.is_empty() {
        head
    } else {
        format!("{head}\n{reason}")
    };
    Some(json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": { "content": content, "meta": meta },
    }))
}

/// The sha a landing put on the target, read from the row: the move's own sentence carries it
/// too, but a reader that needs the commit must not have to parse prose.
fn landed_sha(store: &Store, t: &QueueTransition) -> Option<String> {
    if t.queue != "merge" {
        return None;
    }
    if t.to_state != MergeState::Landed.as_str() && t.to_state != MergeState::LandedLocal.as_str() {
        return None;
    }
    store
        .merge_row(&t.entry_id)
        .ok()
        .flatten()
        .and_then(|m| m.landed_sha)
        .filter(|s| !s.is_empty())
}

/// The failing test a gate's reason names. The gate writes the excerpt's own words first, and a
/// step whose output fit its cap has no excerpt - so libtest's header is read too.
fn failing_test(why: &str) -> Option<String> {
    let marker = "first failing test `";
    if let Some(name) = why
        .split_once(marker)
        .and_then(|(_, rest)| rest.split('`').next())
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        return Some(name.to_string());
    }
    why.lines().find_map(|line| {
        line.trim()
            .strip_prefix("---- ")
            .and_then(|rest| rest.strip_suffix(" stdout ----"))
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
    })
}

/// `s` cut to at most `max` bytes on a char boundary, saying where the rest is.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} ... (cut; the queue's pane has the rest)", &s[..end])
}

/// The last move said, `None` when this folder's queue has never been followed.
fn read_cursor(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Remember the last move said, through a temp name and a rename so a bridge killed mid-write
/// leaves the old number rather than half of a new one. `false` is a place this process could
/// not keep, which its caller says once and carries on without.
fn write_cursor(path: &Path, seq: i64) -> bool {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, seq.to_string()).is_err() {
        return false;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// The repository a folder is a session of, as the queues key it: the canonical main checkout,
/// which a worktree answers with its common git dir's owner. The rule is harnessd's
/// (`mergequeue::git::repo_of_worktree`), copied because this crate does not link the daemon.
pub fn repository_of(folder: &Path) -> String {
    let common = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(common) = common else {
        return folder.display().to_string();
    };
    let common = PathBuf::from(common);
    let repo = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf).unwrap_or(common)
    } else {
        common
    };
    std::fs::canonicalize(&repo)
        .unwrap_or(repo)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{
        Approval, MergePriority, Move, NewReview, ReviewKind, TargetHealth,
    };

    /// A directory of its own per test, removed when it ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let p = std::env::temp_dir()
                .join(format!("letibot-push-queue-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn db(&self) -> PathBuf {
            self.0.join("sessions.db")
        }
        fn cursor(&self) -> PathBuf {
            self.0.join("cursor")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const REPO: &str = "/w/letibot";

    fn review(id: &str, repo: &str) -> NewReview {
        NewReview {
            id: id.into(),
            repository: repo.into(),
            target: "main".into(),
            kind: ReviewKind::Branch,
            session_id: "s-child".into(),
            branch: format!("agent/{id}"),
            worktree: None,
            base_sha: "base".into(),
            head_sha: "head1".into(),
            brief: "do the thing".into(),
            priority: MergePriority::Subagent,
            fixes: None,
            evidence: "the child finished".into(),
        }
    }

    /// The verdict that approves `id` and gives birth to the merge row `merge`.
    fn approve(s: &Store, id: &str, merge: &str, why: &str, at: u64) {
        s.move_review(
            id,
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new("me", "claimed").set("owner", "me"),
            at,
        )
        .unwrap();
        let ok = s
            .approve_review(
                id,
                &Approval {
                    expect: ReviewState::Reviewing,
                    expect_owner: Some("me".into()),
                    reviewed_sha: "head1".into(),
                    by: "reviewer:rev".into(),
                    reasons: vec![],
                    files: vec![],
                    commands: vec![],
                    why: why.into(),
                    merge_id: merge.into(),
                },
                at + 1,
            )
            .unwrap();
        assert!(ok, "`{id}` was not approved");
    }

    /// Every pass until the store is quiet, oldest first. A pass that reads exactly [`PER_PASS`]
    /// moves of which none is news also says nothing - so one quiet pass is not the end.
    fn drain(m: &Mover) -> Vec<Value> {
        let mut all = Vec::new();
        let mut quiet = 0;
        for _ in 0..100 {
            let notes = m.step().expect("a step");
            if notes.is_empty() {
                quiet += 1;
                if quiet == 2 {
                    return all;
                }
            } else {
                quiet = 0;
                all.extend(notes);
            }
        }
        all
    }

    fn meta<'a>(note: &'a Value, key: &str) -> &'a str {
        note["params"]["meta"][key].as_str().unwrap_or("")
    }

    fn content(note: &Value) -> &str {
        note["params"]["content"].as_str().unwrap_or("")
    }

    /// A birth, a verdict, a landing, a red gate and a red target are each one channel event,
    /// carrying their own fact: the reason line, the sha, the failing test. A row picked up, a
    /// rebase and the merge row an approval births are the transitions that produce nothing.
    #[test]
    fn each_move_kind_is_one_event_with_the_fact_it_carries() {
        let d = TempDir::new("kinds");
        let s = Store::open(&d.db()).unwrap();
        let m = Mover::new(d.db(), REPO, d.cursor());
        // Followed before anything moved: a follower says what happens from here.
        assert!(drain(&m).is_empty(), "nothing has moved yet");

        s.enqueue_review(&review("r-1", REPO), "me", 1).unwrap();
        approve(
            &s,
            "r-1",
            "m-1",
            "the branch is good, and it is one commit",
            2,
        );
        s.move_merge(
            "m-1",
            MergeState::Queued,
            MergeState::Gating,
            &Move::new("me", "rebased onto tip; running the gate").set("owner", "me"),
            5,
        )
        .unwrap();
        let landed = s
            .move_merge(
                "m-1",
                MergeState::Gating,
                MergeState::Landed,
                &Move::new("me", "landed on `main` at abc1234")
                    .set("landed_sha", "abc1234")
                    .set("gate_steps_json", "[]"),
                6,
            )
            .unwrap();
        assert!(landed, "m-1 did not land");

        s.enqueue_review(&review("r-2", REPO), "me", 7).unwrap();
        approve(&s, "r-2", "m-2", "looks fine", 8);
        s.move_merge(
            "m-2",
            MergeState::Queued,
            MergeState::Gating,
            &Move::new("me", "running the gate").set("owner", "me"),
            10,
        )
        .unwrap();
        let failed = s
            .move_merge(
                "m-2",
                MergeState::Gating,
                MergeState::Failed,
                &Move::new(
                    "me",
                    "`sh cargo test` failed:\n[gate excerpt: failing target `-p x --lib` \
                     — first failing test `worker::a_test`]\n    thread panicked",
                )
                .set("gate_steps_json", "[]"),
                11,
            )
            .unwrap();
        assert!(failed, "m-2 was not failed");
        // The two arms a passing test would otherwise not reach: a reviewer that could not
        // decide, and a landing still local because the push out failed.
        s.enqueue_review(&review("r-3", REPO), "me", 15).unwrap();
        s.move_review(
            "r-3",
            ReviewState::Waiting,
            ReviewState::CannotDecide,
            &Move::new("reviewer:rev", "the brief names two branches"),
            16,
        )
        .unwrap();
        s.enqueue_review(&review("r-4", REPO), "me", 17).unwrap();
        approve(&s, "r-4", "m-4", "good", 18);
        s.move_merge(
            "m-4",
            MergeState::Queued,
            MergeState::LandedLocal,
            &Move::new(
                "me",
                "landed on `main` at dead99; the push to origin failed",
            )
            .set("landed_sha", "dead99"),
            20,
        )
        .unwrap();
        s.set_target_health(
            &TargetHealth {
                repository: REPO.into(),
                target: "main".into(),
                sha: "beef1234".into(),
                state: HealthState::Red,
                checked_ms: 12,
                evidence: "the gate of the target failed".into(),
                gate_steps: Vec::new(),
            },
            "me",
        )
        .unwrap();

        let notes = drain(&m);
        let kinds: Vec<&str> = notes.iter().map(|n| meta(n, "event")).collect();
        assert_eq!(
            kinds,
            [
                "enqueued",
                "verdict",
                "landed",
                "enqueued",
                "verdict",
                "gate_failed",
                "enqueued",
                "verdict",
                "enqueued",
                "verdict",
                "landed",
                "main_red"
            ],
            "one event per move that is news, and none for the rest: {notes:#?}"
        );
        for n in &notes {
            assert_eq!(meta(n, "source"), "leticode");
            assert_eq!(meta(n, "repository"), REPO);
            assert!(
                !meta(n, "transition").is_empty(),
                "a move is named by its transition: {n}"
            );
            assert_eq!(n["method"], "notifications/claude/channel");
        }
        assert_eq!(meta(&notes[0], "entry"), "r-1");
        assert_eq!(meta(&notes[0], "queue"), "review");
        assert!(
            content(&notes[1]).contains("APPROVED") && content(&notes[1]).contains("one commit"),
            "a verdict carries its reason line: {}",
            content(&notes[1])
        );
        assert_eq!(meta(&notes[1], "verdict"), "approved");
        assert_eq!(meta(&notes[2], "sha"), "abc1234");
        assert!(
            content(&notes[2]).contains("abc1234"),
            "a landing carries its sha: {}",
            content(&notes[2])
        );
        assert_eq!(meta(&notes[5], "test"), "worker::a_test");
        assert!(
            content(&notes[5]).contains("worker::a_test"),
            "a red gate carries the failing test: {}",
            content(&notes[5])
        );
        assert_eq!(meta(&notes[7], "verdict"), "cannot_decide");
        assert_eq!(meta(&notes[10], "sha"), "dead99");
        assert_eq!(meta(&notes[11], "entry"), "main");
        assert_eq!(meta(&notes[11], "queue"), "target");
    }

    /// A refused verdict is the same event as an approval and the opposite news, and the reason
    /// line is the reviewer's own words.
    #[test]
    fn a_refusal_is_a_verdict_with_the_reviewers_reason() {
        let d = TempDir::new("refused");
        let s = Store::open(&d.db()).unwrap();
        let m = Mover::new(d.db(), REPO, d.cursor());
        assert!(drain(&m).is_empty());
        s.enqueue_review(&review("r-1", REPO), "me", 1).unwrap();
        s.move_review(
            "r-1",
            ReviewState::Waiting,
            ReviewState::Rejected,
            &Move::new("reviewer:rev", "the tests it adds never run"),
            2,
        )
        .unwrap();
        let notes = drain(&m);
        assert_eq!(notes.len(), 2, "the birth and the verdict: {notes:#?}");
        assert_eq!(meta(&notes[1], "event"), "verdict");
        assert_eq!(meta(&notes[1], "verdict"), "refused");
        assert!(
            content(&notes[1]).contains("REFUSED") && content(&notes[1]).contains("never run"),
            "{}",
            content(&notes[1])
        );
    }

    /// The move a restarted bridge must not say twice: the cursor is where its place lives, and
    /// a second bridge on the same folder - the same file - starts where the first one stopped.
    #[test]
    fn a_restarted_bridge_does_not_say_a_move_it_already_said() {
        let d = TempDir::new("restart");
        let s = Store::open(&d.db()).unwrap();
        let first = Mover::new(d.db(), REPO, d.cursor());
        // Following before the first move, so it is news rather than history.
        assert!(drain(&first).is_empty());
        s.enqueue_review(&review("r-1", REPO), "me", 1).unwrap();
        let said = drain(&first);
        assert_eq!(said.len(), 1, "the enqueue: {said:#?}");

        let restarted = Mover::new(d.db(), REPO, d.cursor());
        assert!(
            drain(&restarted).is_empty(),
            "a restart says nothing about a move already said"
        );

        // And it is still following: the next move is news.
        s.enqueue_review(&review("r-2", REPO), "me", 2).unwrap();
        let after = drain(&restarted);
        assert_eq!(after.len(), 1, "{after:#?}");
        assert_eq!(meta(&after[0], "entry"), "r-2");
    }

    /// Another folder's queue is not this bridge's news, and passing its move must not lose the
    /// moves that come after it.
    #[test]
    fn another_folders_queue_is_not_said() {
        let d = TempDir::new("elsewhere");
        let s = Store::open(&d.db()).unwrap();
        let m = Mover::new(d.db(), REPO, d.cursor());
        assert!(drain(&m).is_empty());
        s.enqueue_review(&review("r-other", "/w/other"), "me", 1)
            .unwrap();
        assert!(
            drain(&m).is_empty(),
            "a move of another repository is not said"
        );
        s.enqueue_review(&review("r-mine", REPO), "me", 2).unwrap();
        let notes = drain(&m);
        assert_eq!(notes.len(), 1, "{notes:#?}");
        assert_eq!(meta(&notes[0], "entry"), "r-mine");
    }

    /// A bridge started on a folder with a history says nothing about it: a client that joined
    /// now wants the moves from now, not every row the box ever landed.
    #[test]
    fn a_history_the_bridge_never_followed_is_not_said() {
        let d = TempDir::new("history");
        let s = Store::open(&d.db()).unwrap();
        s.enqueue_review(&review("r-old", REPO), "me", 1).unwrap();
        let m = Mover::new(d.db(), REPO, d.cursor());
        assert!(drain(&m).is_empty(), "an old enqueue is history");
        s.enqueue_review(&review("r-new", REPO), "me", 2).unwrap();
        assert_eq!(drain(&m).len(), 1);
    }

    /// The failing test is the gate's own words when it wrote an excerpt, and libtest's header
    /// when the output fit its cap and there is no excerpt at all.
    #[test]
    fn the_failing_test_is_read_from_the_excerpt_or_from_libtest() {
        assert_eq!(
            failing_test(
                "`sh cargo test` failed:\n[gate excerpt: failing target `-p x --lib` \
                 — first failing test `worker::a_test`]"
            ),
            Some("worker::a_test".into())
        );
        assert_eq!(
            failing_test("failures:\n\n---- retry::tests::it_retried stdout ----\nthread")
                .as_deref(),
            Some("retry::tests::it_retried")
        );
        assert_eq!(failing_test("`cargo build` failed:\nerror[E0001]"), None);
    }

    /// The key a move is matched by: a checkout is itself, and a worktree of it is the checkout,
    /// which is the repository the queue records. The rule is the daemon's, so it is asserted
    /// against a real repository rather than against a spelling of it.
    #[test]
    fn a_worktree_is_keyed_by_its_main_checkout() {
        let git_here = Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        let Some(_) = letibot_tokencore::apparatus::present(
            "git (a folder is keyed to the queues by its checkout)",
            git_here,
        ) else {
            return;
        };
        let d = TempDir::new("worktree");
        let main = d.0.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&main)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let wt = d.0.join("wt-topic");
        git(&["worktree", "add", wt.to_str().unwrap()]);
        let main_key = std::fs::canonicalize(&main).unwrap().display().to_string();
        assert_eq!(repository_of(&main), main_key);
        assert_eq!(repository_of(&wt), main_key, "a worktree is its checkout");
        let not_a_repo = d.0.join("plain");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        assert_eq!(
            repository_of(&not_a_repo),
            not_a_repo.display().to_string(),
            "a folder git cannot place is its own key"
        );
    }
}
