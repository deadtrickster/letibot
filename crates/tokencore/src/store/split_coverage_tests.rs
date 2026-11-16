//! Every public `Store` method that touches a project table, run against a split set. A read or
//! write that takes the shared connection fails here with "no such table". The scan below lists
//! those methods from the source, so a new one fails the test until it is called here too.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use super::split::PROJECT_TABLES;
use super::*;
use crate::ledger::{TokenLedger, hash_tokens};

/// The store's source, test modules cut off. `live_view` is a single-file test tool.
const SOURCES: &[(&str, &str)] = &[
    ("store.rs", include_str!("../store.rs")),
    ("citations.rs", include_str!("citations.rs")),
    ("disclosures.rs", include_str!("disclosures.rs")),
    ("floor.rs", include_str!("floor.rs")),
    ("grants.rs", include_str!("grants.rs")),
    ("leases.rs", include_str!("leases.rs")),
    ("marks.rs", include_str!("marks.rs")),
    ("modes.rs", include_str!("modes.rs")),
    ("owners.rs", include_str!("owners.rs")),
    ("procs.rs", include_str!("procs.rs")),
    ("prompts.rs", include_str!("prompts.rs")),
    ("queues.rs", include_str!("queues.rs")),
    ("recall.rs", include_str!("recall.rs")),
    ("seat.rs", include_str!("seat.rs")),
    ("split.rs", include_str!("split.rs")),
    ("subagents.rs", include_str!("subagents.rs")),
    ("told.rs", include_str!("told.rs")),
    ("transfer.rs", include_str!("transfer.rs")),
    ("writers.rs", include_str!("writers.rs")),
];

/// Named by the scan and not row reads of a session: opening, migrating, or naming tables.
const NOT_ROW_ACCESS: &[&str] = &[
    "open",
    "open_in_memory",
    "open_private",
    "open_set",
    "open_set_existing",
    "open_set_reader",
    "place_of",
    "tables_in",
    "holds",
    "parent_of",
    "name_default",
    "life",
    "digest_agrees",
];

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Does this text name a project table in SQL or as a quoted table name?
fn names_a_project_table(text: &str) -> bool {
    PROJECT_TABLES.iter().any(|t| {
        let quoted = format!("\"{t}\"");
        if text.contains(&quoted) {
            return true;
        }
        ["FROM ", "INTO ", "UPDATE ", "JOIN "].iter().any(|k| {
            let needle = format!("{k}{t}");
            text.match_indices(&needle)
                .any(|(at, _)| !text[at + needle.len()..].chars().next().is_some_and(ident))
        })
    })
}

/// `(name, is plain pub, body)` of one `fn`.
type FnItem = (String, bool, String);

/// Every `fn` in `src`, and its `const` texts.
fn items(src: &str) -> (Vec<FnItem>, HashMap<String, String>) {
    let src = match src.find("\n#[cfg(test)]\nmod tests") {
        Some(cut) => &src[..cut],
        None => src,
    };
    let mut consts = HashMap::new();
    for (at, _) in src.match_indices("const ") {
        let rest = &src[at + 6..];
        let name: String = rest.chars().take_while(|c| ident(*c)).collect();
        if name.is_empty() || !rest[name.len()..].starts_with(':') {
            continue;
        }
        let end = rest.find(";\n").unwrap_or(rest.len());
        consts.insert(name, norm(&rest[..end]));
    }
    let mut heads: Vec<(usize, String, bool)> = Vec::new();
    for (at, _) in src.match_indices("fn ") {
        let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
        let lead = src[line_start..at].trim();
        if !matches!(lead, "" | "pub" | "pub(crate)" | "pub(super)") {
            continue;
        }
        let name: String = src[at + 3..].chars().take_while(|c| ident(*c)).collect();
        if !name.is_empty() {
            heads.push((at, name, lead == "pub"));
        }
    }
    let mut fns = Vec::new();
    for (i, (at, name, public)) in heads.iter().enumerate() {
        let end = heads.get(i + 1).map_or(src.len(), |h| h.0);
        fns.push((name.clone(), *public, norm(&src[*at..end])));
    }
    (fns, consts)
}

/// The public methods whose bodies, or the helpers they call, touch a project table.
fn scanned() -> BTreeSet<String> {
    let mut fns = Vec::new();
    let mut consts = HashMap::new();
    for (_, src) in SOURCES {
        let (f, c) = items(src);
        fns.extend(f);
        consts.extend(c);
    }
    let tables_consts: Vec<&String> = consts
        .iter()
        .filter(|(_, t)| names_a_project_table(t))
        .map(|(n, _)| n)
        .collect();
    let mut touch: BTreeSet<String> = fns
        .iter()
        .filter(|(_, _, body)| {
            names_a_project_table(body)
                || tables_consts.iter().any(|c| {
                    body.match_indices(c.as_str()).any(|(at, _)| {
                        !body[..at].chars().last().is_some_and(ident)
                            && !body[at + c.len()..].chars().next().is_some_and(ident)
                    })
                })
        })
        .map(|(n, _, _)| n.clone())
        .collect();
    loop {
        let more: Vec<String> = fns
            .iter()
            .filter(|(n, _, body)| {
                !touch.contains(n)
                    && touch.iter().any(|h| {
                        // `tx.method(` and `conn.method(` are how a write reached through a
                        // transaction or a held connection called the next one; only `self.` was
                        // followed, so a read one hop away was invisible to this scan.
                        ["self.", "Self::", "store.", "tx.", "conn."]
                            .iter()
                            .any(|p| body.contains(&format!("{p}{h}(")))
                    })
            })
            .map(|(n, _, _)| n.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        touch.extend(more);
    }
    fns.iter()
        .filter(|(n, public, _)| *public && touch.contains(n))
        .map(|(n, _, _)| n.clone())
        .collect()
}

/// The names [`every_project_method_runs_on_a_split_set`] calls. Kept beside the calls.
const EXERCISED: &[&str] = &[
    "put_stable_prefix",
    "get_stable_prefix",
    "stable_prefix_record",
    "stable_prefix_meta",
    "stable_prefix_ids_of_len",
    "put_transcript",
    "transcript_prefix_id",
    "set_transcript_prefix",
    "current_transcript_id",
    "append_item",
    "append_items",
    "append_items_with_context",
    "item_count",
    "load_transcript",
    "row_json_at",
    "rows_json_from",
    "put_fork",
    "put_fork_unsealed",
    "seal_transcript",
    "fork_session",
    "children_of",
    "list_sessions",
    "list_sessions_archived",
    "recent_models",
    "delete_empty_session",
    "put_todos",
    "todos",
    "sessions_waiting_on_operator",
    "put_job",
    "jobs",
    "job_updated_ms",
    "deliver_job",
    "undelivered_jobs",
    "max_job_number",
    "put_job_context",
    "job_context",
    "put_monitor",
    "monitors_of",
    "end_monitor",
    "put_subagent_run",
    "subagent_run",
    "subagent_runs",
    "subagent_runs_of",
    "first_subagent_run_ms",
    "settle_subagent_run",
    "settle_subagent_run_again",
    "deliver_subagent_run",
    "reopen_subagent_run",
    "rest_subagent_run",
    "interrupt_subagent_run",
    "children_without_runs",
    "record_adjudication",
    "max_adjudication_seq",
    "record_operator_ruling",
    "diagnostic",
    "corpus",
    "corpus_counts",
    "approved_shapes",
    "shapeless_human_admits",
    "backfill_shape",
    "put_citations",
    "citations",
    "set_session_marks",
    "session_marks",
    "set_session_mode",
    "session_mode",
    "put_process",
    "process",
    "processes_of",
    "open_processes",
    "check_process",
    "set_process_log_read",
    "replace_queued_prompts",
    "queued_prompts",
    "append_disclosure",
    "disclosures",
    "put_recall_units",
    "recall_units",
    "recall_pending",
    "put_recall_vectors",
    "recall_vectors",
    "put_grant",
    "grants",
    "grants_by",
    "revoke_grant",
    "clear_grants",
    "expire_grants",
    "forget_session_grants",
    "grant_generation",
    "breaker",
    "put_breaker",
    "import_pending",
    "forget_import",
    "recall_transcripts",
    "recall_rows",
    "item_created_at",
    "item_times",
    "export_session",
    "import_session",
];

#[test]
fn every_public_method_on_a_project_table_is_exercised_on_a_split_set() {
    let found = scanned();
    assert!(
        found.contains("job_context") && found.contains("subagent_run"),
        "the scan finds nothing: {found:?}"
    );
    let missing: Vec<&String> = found
        .iter()
        .filter(|n| !EXERCISED.contains(&n.as_str()) && !NOT_ROW_ACCESS.contains(&n.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "public Store methods that touch a project table and are not run on a split set in \
         `every_project_method_runs_on_a_split_set`: {missing:?}"
    );
}

/// Every module the store's own declaration names is scanned. A list is spelled out because
/// `include_str!` wants a literal path, so this is what keeps it from falling behind: a new
/// module's reads are invisible to the scan above until somebody adds its file here.
#[test]
fn every_module_the_store_has_is_scanned() {
    /// Declared by the store and holding no SQL of its own: the busy handler, the single-file test
    /// tool, and the test modules.
    const NOT_SCANNED: &[&str] = &["busy.rs", "live_view.rs", "tests.rs"];

    let src = include_str!("../store.rs");
    let mut unscanned: Vec<String> = Vec::new();
    for (at, _) in src.match_indices("mod ") {
        let rest = &src[at + 4..];
        let name: String = rest.chars().take_while(|c| ident(*c)).collect();
        if name.is_empty() || !rest[name.len()..].trim_start().starts_with(';') {
            continue;
        }
        let file = format!("{name}.rs");
        let test_module = name.ends_with("_tests") || name == "tests";
        if SOURCES.iter().any(|(listed, _)| *listed == file) || test_module {
            continue;
        }
        if !NOT_SCANNED.contains(&file.as_str()) {
            unscanned.push(file);
        }
    }
    assert!(
        unscanned.is_empty(),
        "modules of the store whose files the scan does not read: {unscanned:?}"
    );
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("store-split-cover-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn session(id: &str, parent: Option<&str>) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        title: Some("t".into()),
        model_id: "qwen".into(),
        dialect_sha: "aa".repeat(32),
        workspace_root: "/w".into(),
        owner: "op".into(),
        role: None,
        approvers: Vec::new(),
        parent_session_id: parent.map(str::to_string),
    }
}

fn run(child: &str, parent: &str) -> SubagentRun {
    SubagentRun {
        child_id: child.into(),
        parent_id: parent.into(),
        state: "running".into(),
        prompt: "p".into(),
        spec_json: "{}".into(),
        instance: "i-1".into(),
        resumes: 0,
        outcome: None,
        later_answer: None,
        delivered: false,
        started_ms: 1,
        updated_ms: 1,
    }
}

#[test]
fn every_project_method_runs_on_a_split_set() {
    let dir = temp("all");
    let s = Store::open_set(&dir, "42ce9f1aae08").unwrap();
    let tokens = vec![1u32, 2, 3, 4];
    let prefix = StablePrefixRecord {
        dialect_sha: "aa".repeat(32),
        system: "sys".into(),
        tools_json: vec![],
        h_init: hash_tokens(&tokens),
        tokens: tokens.clone(),
        vocab_source: "/m.gguf".into(),
        effort: None,
    };
    let pid = s.put_stable_prefix(&prefix).unwrap();
    s.get_stable_prefix(&pid).unwrap();
    s.stable_prefix_record(&pid).unwrap();
    assert!(s.stable_prefix_meta(&pid).unwrap().is_some());
    assert_eq!(s.stable_prefix_ids_of_len(4).unwrap(), vec![pid.clone()]);

    s.put_session(&session("s1", None)).unwrap();
    s.put_transcript("t1", "s1", &pid).unwrap();
    assert_eq!(s.transcript_prefix_id("t1").unwrap(), Some(pid.clone()));
    s.set_transcript_prefix("t1", &pid).unwrap();
    assert_eq!(
        s.current_transcript_id("s1").unwrap().as_deref(),
        Some("t1")
    );

    let mut ledger = TokenLedger::new("t1", &tokens).unwrap();
    let say = |t: &str| TranscriptItem::Assistant {
        text: t.into(),
        tool_calls: vec![],
        truncated: false,
    };
    let (a, b, c) = (say("a"), say("b"), say("c"));
    let r0 = ledger.append("it-0", &[10]).unwrap().clone();
    s.append_item("t1", 0, &a, &r0, &[10]).unwrap();
    let r1 = ledger.append("it-1", &[11]).unwrap().clone();
    s.append_items("t1", 1, [(&b, &r1, &[11u32][..])]).unwrap();
    let r2 = ledger.append("it-2", &[12]).unwrap().clone();
    s.append_items_with_context("t1", 2, [(&c, &r2, &[12u32][..])], None)
        .unwrap();
    assert_eq!(s.item_count("t1").unwrap(), 3);
    s.load_transcript("t1").unwrap();
    assert!(s.row_json_at("t1", 0).unwrap().is_some());
    assert_eq!(s.rows_json_from("t1", 0, 3).unwrap().len(), 3);
    assert!(s.item_created_at("t1", 0).unwrap().is_some());
    assert_eq!(s.item_times("t1").unwrap().len(), 3);
    assert_eq!(s.recall_transcripts("s1", 5).unwrap().len(), 1);
    s.recall_rows("t1", 5).unwrap();

    s.put_fork("t2", "s1", &pid, "t1", 1).unwrap();
    s.put_fork_unsealed("t3", "s1", &pid, "t1", 1).unwrap();
    s.seal_transcript("t3").unwrap();
    s.put_session(&session("s2", Some("s1"))).unwrap();
    s.put_transcript("t4", "s2", &pid).unwrap();
    s.fork_session("s2", "s3", 0).unwrap();
    // A fork of a child keeps the child's parent.
    assert_eq!(s.children_of("s1").unwrap().len(), 2);
    assert!(!s.list_sessions().unwrap().is_empty());
    s.list_sessions_archived(Archived::No).unwrap();
    s.recent_models(5).unwrap();
    s.put_session(&session("s-empty", None)).unwrap();
    s.delete_empty_session("s-empty").unwrap();

    s.put_todos(
        "s1",
        &[TodoItem {
            content: "x".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }],
    )
    .unwrap();
    assert_eq!(s.todos("s1").unwrap().len(), 1);
    s.sessions_waiting_on_operator().unwrap();

    let job = JobRecord {
        handle: "j7".into(),
        command: "make".into(),
        how: String::new(),
        state: "exited 0".into(),
        produced: 0,
        elapsed_ms: 0,
        redirect: None,
    };
    s.put_job("s1", &job).unwrap();
    assert_eq!(s.jobs("s1").unwrap().len(), 1);
    assert!(s.job_updated_ms("s1", "j7").unwrap().is_some());
    assert_eq!(s.undelivered_jobs("s1").unwrap().len(), 1);
    s.deliver_job("s1", "j7").unwrap();
    assert!(s.undelivered_jobs("s1").unwrap().is_empty());
    assert_eq!(s.max_job_number().unwrap(), 7);
    s.put_job_context(
        "s1",
        &JobContext {
            handle: "j7".into(),
            slug: None,
            cwd: "/w".into(),
            instance: "d-1-2-0".into(),
            started_ms: 1,
        },
    )
    .unwrap();
    assert!(s.job_context("s1", "j7").unwrap().is_some());

    s.put_monitor("s1", "m", "{}", "i-1", 1000).unwrap();
    assert_eq!(s.monitors_of("s1").unwrap().len(), 1);
    s.end_monitor("s1", "m").unwrap();

    s.put_subagent_run(&run("s2", "s1")).unwrap();
    assert!(s.subagent_run("s2").unwrap().is_some());
    assert_eq!(s.subagent_runs().unwrap().len(), 1);
    assert_eq!(s.subagent_runs_of("s1").unwrap().len(), 1);
    assert!(s.first_subagent_run_ms().unwrap().is_some());
    s.settle_subagent_run("s2", "done", Some("ok")).unwrap();
    s.settle_subagent_run_again("s2", "again").unwrap();
    s.deliver_subagent_run("s2").unwrap();
    assert!(s.reopen_subagent_run("s2").unwrap().is_some());
    s.rest_subagent_run("s2", "done").unwrap();
    s.interrupt_subagent_run("s2").unwrap();
    s.children_without_runs("s1").unwrap();

    assert!(
        s.record_adjudication(&NewAdjudication {
            request_id: "r1".into(),
            session_id: "s1".into(),
            asked: true,
            ..Default::default()
        })
        .unwrap()
    );
    s.max_adjudication_seq("s1").unwrap();
    s.record_operator_ruling("r1", "allow", "fine").unwrap();
    s.diagnostic("r1", "reply").unwrap();
    s.corpus(false, 10).unwrap();
    s.corpus_counts().unwrap();
    s.approved_shapes("/w").unwrap();
    s.shapeless_human_admits().unwrap();
    s.backfill_shape("r1", "shape").unwrap();

    s.put_citations("s1", "h", "[]", 1).unwrap();
    assert!(s.citations("s1", "h").unwrap().is_some());
    s.set_session_marks("s1", "{}").unwrap();
    assert!(s.session_marks("s1").unwrap().is_some());
    s.set_session_mode("s1", "allow-all", None).unwrap();
    assert!(s.session_mode("s1").unwrap().is_some());

    s.put_process(&ProcessRecord::starting("s1", "j7", None, 42))
        .unwrap();
    assert!(s.process("s1", "j7").unwrap().is_some());
    assert_eq!(s.processes_of("s1").unwrap().len(), 1);
    s.open_processes().unwrap();
    s.check_process("s1", "j7", &Check::new(ProcState::Running, 1, 1, "probe"))
        .unwrap();
    s.set_process_log_read("s1", "j7", 3).unwrap();

    s.replace_queued_prompts(
        "s1",
        &[QueuedPrompt {
            enqueued_ms: 1,
            head_id: "h".into(),
            identity: "op".into(),
            client_request_id: "c".into(),
            text: "hi".into(),
            attachments_json: "[]".into(),
            context_json: String::new(),
            from_who: String::new(),
            delivery: String::new(),
            skill: String::new(),
        }],
    )
    .unwrap();
    assert_eq!(s.queued_prompts("s1").unwrap().len(), 1);

    s.append_disclosure(
        "s1",
        &Disclosure {
            ts_ms: 1,
            code: "operator_shell_failed".into(),
            detail: "lost".into(),
        },
    )
    .unwrap();
    assert_eq!(s.disclosures("s1", 8).unwrap().len(), 1);

    s.put_recall_units(&[RecallUnit {
        unit: "u1".into(),
        session: "s1".into(),
        title: "t".into(),
        at_ms: 1,
        text: "words".into(),
    }])
    .unwrap();
    assert_eq!(s.recall_units(10).unwrap().len(), 1);
    assert_eq!(s.recall_pending("m", 10).unwrap().len(), 1);
    s.put_recall_vectors("m", &[("u1".into(), vec![1.0])], 2)
        .unwrap();
    assert_eq!(s.recall_vectors("m").unwrap().len(), 1);

    // The grants a restart re-applies, and the breaker beside them: both are the project file's.
    s.put_grant(&NewGrant {
        session_id: "s1",
        scope: "session",
        kind: "allow",
        key: "cargo test",
        body: "{}",
        why: "asked once",
        by: "human:operator",
    })
    .unwrap();
    assert_eq!(s.grants("s1", "session").unwrap().len(), 1);
    assert_eq!(s.grants_by("human").unwrap().len(), 1);
    assert!(!s.grant_generation().unwrap().is_empty());
    let id = s.grants("s1", "session").unwrap()[0].id;
    assert!(s.revoke_grant(id).unwrap());
    s.put_grant(&NewGrant {
        session_id: "s1",
        scope: "session",
        kind: "view",
        key: "/w",
        body: "{}",
        why: "asked once",
        by: "human:operator",
    })
    .unwrap();
    s.clear_grants("s1", "session", "view").unwrap();
    s.expire_grants("s1", "session", 9_999_999_999).unwrap();
    s.forget_session_grants("s1").unwrap();
    assert!(s.breaker("s1", "permission").unwrap().is_none());
    s.put_breaker(
        "s1",
        "permission",
        &BreakerRow {
            disagreements: 2,
            tripped_ms: Some(5),
            why: "two".into(),
        },
    )
    .unwrap();
    assert_eq!(
        s.breaker("s1", "permission")
            .unwrap()
            .map(|b| b.disagreements),
        Some(2)
    );

    // The half-import marker: written by an import, read by a retry, cleared by the operator.
    assert_eq!(s.import_pending("s1").unwrap(), None);
    assert!(!s.forget_import("s1").unwrap(), "nothing was pending");

    // Out of this set and into another, which takes the rows into its own two files.
    let transfer = s.export_session("s1").unwrap();
    assert!(
        transfer
            .tables
            .iter()
            .any(|t| t.table == "transcript_item" && t.rows.len() == 3),
        "the export read the project file"
    );
    let other_dir = temp("into");
    let other = Store::open_set(&other_dir, "42ce9f1aae08").unwrap();
    let report = other
        .import_session(
            &transfer,
            &ImportInto {
                workspace: "/there".into(),
            },
        )
        .unwrap();
    assert_eq!((report.transcripts, report.items), (3, 3));
    assert_eq!(other.item_count("t1").unwrap(), 3);
    assert!(other.session("s1").unwrap().is_some());
    // And back over a copy marked as living elsewhere: the catch-up path.
    other.set_lives_on("s1", Some("away")).unwrap();
    let again = other
        .import_session(
            &transfer,
            &ImportInto {
                workspace: "/there".into(),
            },
        )
        .unwrap();
    assert!(again.caught_up);
    drop((s, other));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other_dir);
}
