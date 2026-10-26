//! **`--continue` opens a conversation — and a reviewer is not one.**
//!
//! # The state this measures
//!
//! The launcher defines `--continue` as *reopen the newest session in THIS workspace*, and the
//! merge queue spawns its reviewers as **sessions** — subagents of the session that enqueued the
//! entry. Ten of them for one workspace's entries is enough to make the newest row in it a
//! reviewer, which is what the operator hit:
//!
//! > *"interesting, restart pg-noop and was brought to one of the gatekeeper sessions. I guess on
//! > exit you have to print main session id to stdout too. and make sure --continue brings me to
//! > the main convo"*
//!
//! The resolution is a pure rule and its unit tests are beside it (`cli.rs`); what is asserted
//! HERE is the interface a person and a script meet: the **one line on stdout**, which must be
//! the conversation's id and nothing else, and the sentence on stderr for a workspace that holds
//! nothing but children.
//!
//! # Why the real binary
//!
//! `--latest-session` is read by `~/bin/letibot` as `$(harnessd … --latest-session …)`, so the
//! printed line IS the contract. A unit test on the resolution cannot see the printing, and the
//! printing is the half a person meets.

use std::path::{Path, PathBuf};
use std::process::Command;

use letibot_tokencore::ledger::LedgerRow;
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::{TranscriptItem, UserPart};

/// The workspace every fixture session is opened in.
const WS: &str = "/ws/one-project";

/// A scratch directory of this test's own, removed when it goes.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock")
            .as_nanos();
        let p = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-continue-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).expect("scratch");
        Scratch(p)
    }

    fn store(&self) -> PathBuf {
        let path = self.0.join("sessions.db");
        Store::open(&path).expect("opening the store");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session row with the four fields the resolution reads: where it was opened, what it is
/// called, the seat it was recorded in, and the session that spawned it.
fn session_row(
    store: &Store,
    id: &str,
    title: Option<&str>,
    role: Option<&str>,
    parent: Option<&str>,
) {
    store
        .put_session(&SessionRecord {
            id: id.into(),
            title: title.map(str::to_string),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: WS.into(),
            owner: "dead".into(),
            approvers: vec![],
            role: role.map(str::to_string),
            parent_session_id: parent.map(str::to_string),
        })
        .expect("the session row");
}

/// **Give a session rows, and make sure the store's clock has passed `after_ms` FIRST.**
///
/// Returns the `last_activity_ms` the store then reports, which is what the resolution orders by.
///
/// The wait comes *before* the rows, and that is the whole of the correctness here. `created_at` is
/// stamped once, when a row is appended, so a fixture that waits afterwards is waiting for a number
/// that cannot change: two sessions written inside the same millisecond tie, and a tie is broken by
/// the order the session rows were inserted — which is the order a fixture writes them in. A test
/// that means *the reviewer is NEWER than the conversation* would then pass with the rule removed.
/// (It did more than that the first time this was written: the wait was after the append, ten
/// reviewers landed inside one millisecond of their host, the stamps came out equal and the loop
/// spun for ever.)
fn rows_for(store: &Store, id: &str, rows: u32, after_ms: i64) -> i64 {
    wait_past(after_ms);
    let prefix = store
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: "d".into(),
            system: format!("system of {id}"),
            tools_json: vec![],
            tokens: vec![1, 2, 3],
            h_init: [0u8; 32],
            vocab_source: "test".into(),
        })
        .expect("the prefix row");
    let transcript = format!("{id}#t0");
    store
        .put_transcript(&transcript, id, &prefix)
        .expect("the transcript row");
    for seq in 0..rows {
        let tokens: Vec<u32> = vec![7, 8, 9];
        store
            .append_item(
                &transcript,
                seq,
                &TranscriptItem::User {
                    speaker: Default::default(),
                    parts: vec![UserPart::Text {
                        text: format!("row {seq} of {id}"),
                    }],
                },
                &LedgerRow {
                    item_id: format!("{transcript}.{seq}"),
                    // The prefix above is three tokens long, so the first row starts after
                    // them — the store refuses a row that starts anywhere else, because a
                    // resume rebuilds the chain from the prefix's length.
                    tok_offset: 3 + seq * 3,
                    tok_len: 3,
                    h_k: [0u8; 32],
                },
                &tokens,
            )
            .expect("a row");
    }
    // Read back what the store says rather than what this function believes: the stamp is the
    // store's, and it is the number the resolution orders by.
    store
        .session(id)
        .expect("reading")
        .expect("the row just written")
        .last_activity_ms
}

/// Wait until the store's own clock has passed `ms` — for a fixture whose time is stamped at write
/// time and cannot be moved afterwards.
fn wait_past(ms: i64) {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    };
    while now() <= ms {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The store query the launcher runs, as the launcher runs it: exit code, stdout, stderr.
fn latest(store: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .arg("--store")
        .arg(store)
        .arg("--latest-session")
        .arg("--scope")
        .arg(WS)
        .output()
        .expect("running harnessd");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `letibot --sessions`: the listing the person reads, which the resolution must agree with.
fn listing(store: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .arg("--store")
        .arg(store)
        .arg("--list-sessions")
        .arg("--scope")
        .arg(WS)
        .output()
        .expect("running harnessd");
    assert_eq!(out.status.code(), Some(0), "the listing answers");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **The rows a listing puts in one group** — the ids under that group's header, in the order they
/// were drawn.
///
/// The listing is grouped since 2026-10-26 (`cli.rs`'s `Query::List`): a header line per group,
/// `mine (1) — …`, then its rows, each an id at the left margin with its workspace under it. Read
/// the header's TITLE and the ids between it and the next header.
fn group_of_rows(listed: &str, group: &str) -> Vec<String> {
    const GROUPS: [&str; 5] = ["mine", "subagents", "review queue", "packages", "one-shots"];
    let header = |l: &str| {
        GROUPS
            .iter()
            .find(|g| l.trim_start().starts_with(&format!("{g} (")))
            .copied()
    };
    let mut out = Vec::new();
    let mut current: Option<&str> = None;
    for line in listed.lines() {
        if let Some(g) = header(line) {
            current = Some(g);
            continue;
        }
        if current != Some(group) {
            continue;
        }
        // A session's line starts with its id at the left margin; the workspace line under it
        // starts with padding, and the header lines are caught above.
        let id = line.split_whitespace().next().unwrap_or("");
        if line.starts_with("s-") {
            out.push(id.to_string());
        }
    }
    out
}

/// **The line the launcher reads is the conversation's id**, with ten reviewers in the store
/// ahead of it — the operator's report, end to end.
#[test]
fn the_line_on_stdout_is_the_conversation_and_not_the_reviewer() {
    let scratch = Scratch::new("reviewer");
    let path = scratch.store();
    let store = Store::open(&path).expect("opening");

    session_row(&store, "s-mine", Some("the conversation"), None, None);
    let mine = rows_for(&store, "s-mine", 3, 0);

    // The queue's reviewers: children of that conversation, seated `gatekeeper`, their titles
    // cut from the review brief — and every one of them NEWER than anything the person said.
    let mut newer = mine;
    for i in 0..10 {
        let id = format!("s-gk-{i}");
        session_row(
            &store,
            &id,
            Some(&format!(
                "{} a subagent has finished work on a branch",
                letibot_sessionlog::GATEKEEPER_TITLE_PREFIX
            )),
            Some("gatekeeper"),
            Some("s-mine"),
        );
        newer = rows_for(&store, &id, 40, newer);
    }
    assert!(newer > mine, "the reviewers have to be the newer rows");

    let (code, stdout, stderr) = latest(&path);
    assert_eq!(code, 0, "a conversation was found: {stderr}");
    assert_eq!(
        stdout, "s-mine\n",
        "stdout is the id and nothing else — it is read as `$(harnessd --latest-session)`"
    );
    assert!(
        stderr.contains("not conversations"),
        "and the ten rows it walked past are said rather than silently dropped: {stderr}"
    );

    // **And the listing agrees with the resolution, which is what it did not do before.** The
    // operator's own measurement is the shape asserted here: `--sessions` offers the conversation
    // and no reviewer, so the row `--continue` picks is one of them. Two queries, one store, one
    // answer.
    //
    // **Grouped since 2026-10-26**, and the group a reviewer lands in is the point: these ten are
    // parented straight on the conversation, seated `gatekeeper`, titled from the review brief —
    // the layout before the queue had a session of its own. They are the REVIEW QUEUE's work, not
    // the operator's subagents, and a listing that offered them under `subagents` would be the
    // same defect with a header on it.
    let listed = listing(&path);
    assert_eq!(
        group_of_rows(&listed, "mine"),
        ["s-mine"],
        "mine offers exactly the conversation `--continue` picked: {listed}"
    );
    assert!(
        group_of_rows(&listed, "subagents").is_empty(),
        "a reviewer is not one of the operator's subagents: {listed}"
    );
    assert_eq!(
        group_of_rows(&listed, "review queue").len(),
        10,
        "and all ten reviewers are the queue's: {listed}"
    );
    assert!(
        !group_of_rows(&listed, "mine")
            .iter()
            .chain(group_of_rows(&listed, "subagents").iter())
            .any(|id| id.starts_with("s-gk")),
        "no reviewer is offered as a conversation anywhere: {listed}"
    );
}

/// **A workspace holding nothing but children says so, and exits 1** — which the launcher turns
/// into its own remedy. The sentence is the point: `no stored session` over a store that is not
/// empty sends a person looking for a fault that is not there.
#[test]
fn a_workspace_holding_nothing_but_children_says_so_and_exits_one() {
    let scratch = Scratch::new("children");
    let path = scratch.store();
    let store = Store::open(&path).expect("opening");

    // A sub-session whose parent row is not in this store — the same shape as one whose parent
    // was deleted, and the shape a `--scope` narrowing to a child's own workspace produces.
    session_row(
        &store,
        "s-orphan",
        Some("a child of a session that is gone"),
        Some("coder"),
        Some("s-parent-nobody-has"),
    );
    rows_for(&store, "s-orphan", 4, 0);

    let (code, stdout, stderr) = latest(&path);
    assert_eq!(code, 1, "there is nothing here to continue");
    assert!(stdout.is_empty(), "and no id to print: {stdout:?}");
    assert!(
        stderr.contains("sub-session") && stderr.contains("--session ID"),
        "the sentence names what is there and what to do about it: {stderr}"
    );
}
