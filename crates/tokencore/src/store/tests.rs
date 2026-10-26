use super::*;
use crate::ledger::{TokenLedger, hash_tokens};
use letibot_transcript::{ReasoningField, SystemOrigin};

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn prefix() -> StablePrefixRecord {
    let tokens = vec![1u32, 2, 3, 4];
    StablePrefixRecord {
        dialect_sha: "aa".repeat(32),
        system: "you are a harness".into(),
        tools_json: vec![r#"{"name":"read"}"#.into()],
        h_init: hash_tokens(&tokens),
        tokens,
        vocab_source: "/models/x.gguf".into(),
    }
}

/// **The two light lookups answer from the `session` row alone**, and they keep the
/// three cases the merge queue's parent walk depends on apart: a child names its parent,
/// a root names none (`Some(None)`), and an id this store does not hold is `None`. A walk
/// that confused the last two would stop at a stranger's id as though it were a root.
#[test]
fn the_light_lookups_read_one_row_and_keep_root_and_unknown_apart() {
    let s = Store::open_in_memory().unwrap();
    let (_, _) = seeded(&s);
    s.put_session(&SessionRecord {
        id: "sess-child".into(),
        title: None,
        model_id: "qwen".into(),
        dialect_sha: "d".into(),
        workspace_root: "/w/child".into(),
        owner: "deadtrickster".into(),
        role: Some("coder".into()),
        approvers: vec![],
        parent_session_id: Some("sess-1".into()),
    })
    .unwrap();

    assert_eq!(
        s.session_parent("sess-child").unwrap(),
        Some(Some("sess-1".into()))
    );
    assert_eq!(
        s.session_parent("sess-1").unwrap(),
        Some(None),
        "a root names no parent"
    );
    assert_eq!(
        s.session_parent("nobody").unwrap(),
        None,
        "an unknown id is not a root"
    );

    assert_eq!(
        s.session_workspace("sess-child").unwrap().as_deref(),
        Some("/w/child")
    );
    assert_eq!(s.session_workspace("nobody").unwrap(), None);
}

/// **The keyed lookup is the listing's row, field for field.** `session` used to BE the
/// listing plus a `find`, so the two could not disagree; now they are two calls into one
/// query and this is what keeps them the same row.
#[test]
fn the_keyed_session_is_the_same_row_the_listing_returns() {
    let s = Store::open_in_memory().unwrap();
    let (_, _) = seeded(&s);
    let keyed = s.session("sess-1").unwrap().expect("held");
    let listed = s
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|x| x.id == "sess-1")
        .expect("listed");
    assert_eq!(keyed, listed);
    assert!(s.session("nobody").unwrap().is_none());
}

fn seeded(s: &Store) -> (String, String) {
    let p = prefix();
    let prefix_id = s.put_stable_prefix(&p).unwrap();
    s.put_session(&SessionRecord {
        id: "sess-1".into(),
        title: Some("t".into()),
        model_id: "qwen".into(),
        dialect_sha: p.dialect_sha.clone(),
        workspace_root: "/w".into(),
        owner: "deadtrickster".into(),
        role: None,
        approvers: vec!["deadtrickster".into()],
        parent_session_id: None,
    })
    .unwrap();
    s.put_transcript("tr-1", "sess-1", &prefix_id).unwrap();
    ("tr-1".into(), prefix_id)
}

/// **A listed session carries its current transcript's last row** — what the subagents
/// pane reads to say how a child it did not watch ended.
#[test]
fn a_listed_session_carries_its_last_row() {
    let s = store();
    let (tr, _) = seeded(&s);
    assert_eq!(
        s.list_sessions().unwrap()[0].last_item,
        None,
        "no rows, no last row"
    );
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let answer = TranscriptItem::Assistant {
        text: "the answer".into(),
        tool_calls: vec![],
        truncated: false,
    };
    let items = [
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text {
                text: "the question".into(),
            }],
        },
        answer.clone(),
    ];
    for (seq, item) in items.iter().enumerate() {
        let toks = vec![10 + seq as u32];
        let row = ledger.append(&format!("it-{seq}"), &toks).unwrap().clone();
        s.append_item(&tr, seq as u32, item, &row, &toks).unwrap();
    }
    assert_eq!(s.list_sessions().unwrap()[0].last_item, Some(answer));
}

/// **R19.2b: one row, by ordinal, without loading the transcript it is in.**
///
/// The read behind `FetchRow` for a row the daemon's **bounded view** has trimmed
/// (2,000 rows, 8 MB of bodies). Its whole point is that it is **one indexed
/// lookup** — `transcript_item`'s primary key is `(transcript_id, seq)` — so this
/// asserts what the index is for, what it answers for an ordinal that is not there,
/// and that an ordinal is relative to the session's **current** transcript.
#[test]
fn a_row_is_readable_by_ordinal_without_loading_its_transcript() {
    let s = store();
    let (tr, _) = seeded(&s);
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let items = [
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text {
                text: "first".into(),
            }],
        },
        TranscriptItem::Assistant {
            text: "second".into(),
            tool_calls: vec![],
            truncated: false,
        },
    ];
    for (seq, item) in items.iter().enumerate() {
        let toks = vec![10 + seq as u32];
        let row = ledger.append(&format!("it-{seq}"), &toks).unwrap().clone();
        s.append_item(&tr, seq as u32, item, &row, &toks).unwrap();
    }

    // **The ordinal is relative to the CURRENT transcript**, which is the one the
    // rows went into. This is the mapping the `FetchRow` glue depends on, and after
    // a fork it is the whole reason the lookup goes through the session rather than
    // a transcript id the head has never heard of.
    assert_eq!(
        s.current_transcript_id("sess-1").unwrap().as_deref(),
        Some("tr-1")
    );

    // The row by ordinal, read through the same serde form the wire carries — so
    // this asserts the shape `body_of` will be handed, not a private one.
    let json = s.row_json_at(&tr, 1).unwrap().expect("row 1 is there");
    let back: TranscriptItem = serde_json::from_str(&json).unwrap();
    assert_eq!(back, items[1]);

    // **An ordinal the transcript does not have is `None`, not an empty string** —
    // the rule `RowFetched` states for the wire: "nobody has it" and "it is empty"
    // must not look alike.
    assert_eq!(s.row_json_at(&tr, 2).unwrap(), None);
    assert_eq!(s.row_json_at(&tr, 9_999).unwrap(), None);

    // A session nothing was written for has no current transcript, so the same read
    // answers `None` rather than somebody else's row.
    assert_eq!(s.current_transcript_id("no-such-session").unwrap(), None);
}

/// **A fork's rows are the ones an ordinal names.** After a compaction the session
/// has two transcripts and ordinal 3 belongs to the new base — reading the old one
/// would answer with a row nobody asked about, which is the failure the "newest
/// transcript" rule exists to prevent. Asserted against the store so the SQL that
/// picks the leaf is what is under test, not a comment about it.
#[test]
fn an_ordinal_follows_the_fork_to_the_new_base() {
    let s = store();
    let (tr, prefix_id) = seeded(&s);
    s.put_fork("tr-2", "sess-1", &prefix_id, &tr, 1).unwrap();
    assert_eq!(
        s.current_transcript_id("sess-1").unwrap().as_deref(),
        Some("tr-2"),
        "the newest transcript is the one an ordinal is relative to"
    );
    // …and the new base has none of the old rows: ordinal 0 of a fork that carried
    // nothing is not the old transcript's row 0.
    assert_eq!(s.row_json_at("tr-2", 0).unwrap(), None);
}

#[test]
fn a_stable_prefix_is_addressed_by_its_content_and_its_dialect() {
    let s = store();
    let p = prefix();
    let a = s.put_stable_prefix(&p).unwrap();
    let b = s.put_stable_prefix(&p).unwrap();
    assert_eq!(a, b, "the same prefix must not be stored twice");

    // Same text, different renderer, therefore different tokens: it must not
    // collide, or a GLM session would inherit Qwen's ids.
    let mut other = p.clone();
    other.dialect_sha = "bb".repeat(32);
    other.tokens = vec![9, 9, 9];
    other.h_init = hash_tokens(&other.tokens);
    let c = s.put_stable_prefix(&other).unwrap();
    assert_ne!(a, c);
    assert_eq!(s.get_stable_prefix(&a).unwrap().0, vec![1, 2, 3, 4]);
    assert_eq!(s.get_stable_prefix(&c).unwrap().0, vec![9, 9, 9]);
}

#[test]
fn transcript_item_cannot_be_updated_or_deleted() {
    let s = store();
    let (tr, _) = seeded(&s);
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let item = TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
    };
    let row = ledger.append("it-0", &[10, 11]).unwrap().clone();
    s.append_item(&tr, 0, &item, &row, &[10, 11]).unwrap();

    let update = s.conn.execute(
        "UPDATE transcript_item SET item_json = '{}' WHERE transcript_id = ?1",
        params![tr],
    );
    assert!(update.is_err(), "UPDATE must be refused by the trigger");
    assert!(format!("{:?}", update.unwrap_err()).contains("append-only"));

    let delete = s.conn.execute(
        "DELETE FROM transcript_item WHERE transcript_id = ?1",
        params![tr],
    );
    assert!(delete.is_err(), "DELETE must be refused by the trigger");
    assert!(format!("{:?}", delete.unwrap_err()).contains("append-only"));

    assert_eq!(s.item_count(&tr).unwrap(), 1);
}

#[test]
fn a_row_that_does_not_continue_the_previous_one_is_refused() {
    let s = store();
    let (tr, _) = seeded(&s);
    let item = TranscriptItem::Reasoning {
        text: "t".into(),
        field: ReasoningField::Inline,
        truncated: false,
    };
    let good = LedgerRow {
        item_id: "a".into(),
        tok_offset: 4,
        tok_len: 2,
        h_k: [1; 32],
    };
    s.append_item(&tr, 0, &item, &good, &[1, 2]).unwrap();

    // A hole in the token stream.
    let gap = LedgerRow {
        item_id: "b".into(),
        tok_offset: 9,
        tok_len: 1,
        h_k: [2; 32],
    };
    assert!(s.append_item(&tr, 1, &item, &gap, &[3]).is_err());

    // A seq that skips.
    let skip = LedgerRow {
        item_id: "c".into(),
        tok_offset: 6,
        tok_len: 1,
        h_k: [3; 32],
    };
    assert!(s.append_item(&tr, 5, &item, &skip, &[3]).is_err());

    // A re-insert at an index already used: the seq trigger catches it before
    // the primary key does, and either way it is refused.
    let reuse = LedgerRow {
        item_id: "d".into(),
        tok_offset: 4,
        tok_len: 2,
        h_k: [4; 32],
    };
    assert!(s.append_item(&tr, 0, &item, &reuse, &[1, 2]).is_err());

    // A blob that does not match the row's length.
    let lying = LedgerRow {
        item_id: "e".into(),
        tok_offset: 6,
        tok_len: 7,
        h_k: [5; 32],
    };
    assert!(s.append_item(&tr, 1, &item, &lying, &[3]).is_err());

    // And the good continuation still works.
    let next = LedgerRow {
        item_id: "f".into(),
        tok_offset: 6,
        tok_len: 1,
        h_k: [6; 32],
    };
    s.append_item(&tr, 1, &item, &next, &[3]).unwrap();
    assert_eq!(s.item_count(&tr).unwrap(), 2);
}

#[test]
fn a_fork_records_where_it_forked() {
    let s = store();
    let (tr, prefix_id) = seeded(&s);
    s.put_fork("tr-2", "sess-1", &prefix_id, &tr, 3).unwrap();
    let loaded = s.load_transcript("tr-2").unwrap();
    assert_eq!(loaded.parent_transcript_id.as_deref(), Some("tr-1"));
    assert_eq!(loaded.forked_at_seq, Some(3));

    let fresh = s.load_transcript(&tr).unwrap();
    assert_eq!(fresh.parent_transcript_id, None);
    assert_eq!(fresh.forked_at_seq, None);

    // The CHECK keeps the two fields together.
    let half = s.conn.execute(
        "INSERT INTO transcript (id, session_id, parent_transcript_id, forked_at_seq,
                                     stable_prefix_id, created_at)
             VALUES ('tr-3', 'sess-1', 'tr-1', NULL, ?1, 0)",
        params![prefix_id],
    );
    assert!(
        half.is_err(),
        "a parent without a fork point must be refused"
    );
}

#[test]
fn store_round_trip_rebuilds_the_same_ledger() {
    let s = store();
    let (tr, _) = seeded(&s);
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();

    let items: Vec<TranscriptItem> = vec![
        TranscriptItem::System {
            text: "boot".into(),
            origin: SystemOrigin::Bootstrap,
        },
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text {
                text: "hello".into(),
            }],
        },
        TranscriptItem::Reasoning {
            text: "think".into(),
            field: ReasoningField::Inline,
            truncated: false,
        },
        TranscriptItem::SegmentMark {
            segment_id: "s0".into(),
            label: "l".into(),
            kind: "k".into(),
            edge: letibot_transcript::SegmentEdge::Open,
        },
        TranscriptItem::Assistant {
            text: "hi".into(),
            tool_calls: vec![],
            truncated: false,
        },
    ];
    // The SegmentMark renders to nothing; every other item to something.
    let payloads: Vec<Vec<u32>> = vec![vec![20, 21], vec![22], vec![23, 24, 25], vec![], vec![26]];

    for (seq, (item, toks)) in items.iter().zip(&payloads).enumerate() {
        let row = ledger.append(&format!("it-{seq}"), toks).unwrap().clone();
        s.append_item(&tr, seq as u32, item, &row, toks).unwrap();
    }

    let loaded = s.load_transcript(&tr).unwrap();
    assert_eq!(loaded.prefix_tokens, vec![1, 2, 3, 4]);
    assert_eq!(loaded.h_init, ledger.h_init());
    assert_eq!(loaded.items.len(), 5);
    assert_eq!(
        loaded
            .items
            .iter()
            .map(|(i, ..)| i.clone())
            .collect::<Vec<_>>(),
        items
    );

    let restored = TokenLedger::restore(
        "tr-1-restored",
        &loaded.prefix_tokens,
        loaded.h_init,
        &loaded.ledger_input(),
    )
    .unwrap();
    assert_eq!(restored.tokens(), ledger.tokens());
    assert_eq!(restored.head(), ledger.head());
    assert_eq!(restored.rows(), ledger.rows());
    assert_eq!(restored.span(), ledger.span());
    restored.verify_chain().unwrap();
}

#[test]
fn kinds_are_denormalised_correctly() {
    let s = store();
    let (tr, _) = seeded(&s);
    // The ledger's prefix is the STORE's — `seeded`'s four tokens — so the first row starts
    // where they end. A ledger over an empty prefix would be a row the store refuses.
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let item = TranscriptItem::ToolResult {
        call_id: "c".into(),
        name: "read".into(),
        outcome: letibot_transcript::ToolOutcome::Abstained {
            reason: "no cover".into(),
        },
        payload: "{}".into(),
        edit: None,
        origin: None,
        media: None,
    };
    let row = ledger.append("it", &[1]).unwrap().clone();
    s.append_item(&tr, 0, &item, &row, &[1]).unwrap();
    let kind: String = s
        .conn
        .query_row("SELECT kind FROM transcript_item", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, "tool_result");

    // Abstained must survive the round trip as itself, not as Ok.
    let back = s.load_transcript(&tr).unwrap();
    assert_eq!(back.items[0].0, item);
}

/// A v1 store — one written before a session could record its role — is carried
/// forward, and every row in it reads back as "unrecorded".
///
/// This is the test the operator's ten live sessions depend on. `migrate()` used
/// to return `Ok(())` for any version it already knew, so a column added to
/// `SCHEMA_SQL` reached a fresh file and nothing else; the failure would have been
/// a `no such column: role` on the first list, after the daemon had already
/// started.
#[test]
fn a_v1_store_is_migrated_and_its_rows_read_as_unrecorded() {
    // A file, not `open_in_memory`, because the point is a store that already
    // exists on disk at an older version. No dev-dependency for one test.
    let path = std::env::temp_dir().join(format!(
        "letibot-migrate-v1-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    // Make a REAL store, then reverse the v2 step on it. Hand-writing the v1
    // DDL was the first attempt and it was wrong: the fixture lacked
    // `transcript`, which `list_sessions` joins, so the test failed on a table
    // the migration never touches. Reversing a real store keeps every other
    // table exactly as v1 had it and cannot drift from `SCHEMA_SQL`.
    {
        let s = Store::open(&path).unwrap();
        s.put_session(&SessionRecord {
            id: "s-old".into(),
            title: Some("a title".into()),
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "ALTER TABLE session DROP COLUMN role;
                 ALTER TABLE session DROP COLUMN parent_session_id;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (1);",
        )
        .unwrap();
        // Prove the fixture really is v1: the column is gone.
        assert!(
            c.query_row("SELECT role FROM session", [], |r| r
                .get::<_, Option<String>>(0))
                .is_err(),
            "the fixture still has a role column, so it is not a v1 store"
        );
        assert!(
            c.query_row("SELECT parent_session_id FROM session", [], |r| r
                .get::<_, Option<String>>(0))
                .is_err(),
            "the fixture still has a parent_session_id column, so it is not a v1 store"
        );
    }

    // Opening it runs the migration.
    let s = Store::open(&path).unwrap();
    let got = s.session("s-old").unwrap().expect("the v1 row survived");
    assert_eq!(got.title.as_deref(), Some("a title"));
    assert_eq!(
        got.role, None,
        "a session written before v2 must read as unrecorded, not as some default"
    );

    // The version moved, and opening again is a no-op rather than a re-migration.
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    drop(s);
    let s = Store::open(&path).unwrap();
    assert!(
        s.session("s-old").unwrap().is_some(),
        "reopen is idempotent"
    );

    // And a role written after the migration comes back.
    s.put_session(&SessionRecord {
        id: "s-new".into(),
        title: None,
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/w".into(),
        owner: "dead".into(),
        role: Some("planner".into()),
        approvers: vec![],
        parent_session_id: None,
    })
    .unwrap();
    assert_eq!(
        s.session("s-new").unwrap().unwrap().role.as_deref(),
        Some("planner")
    );
}

#[test]
fn a_v2_store_is_migrated_and_gains_a_todo_table() {
    // Same fixture rule as the v1 test: a real store, one step reversed, and
    // the migration has to put back exactly what `SCHEMA_SQL` would have.
    let path = std::env::temp_dir().join(format!(
        "letibot-migrate-v2-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let s = Store::open(&path).unwrap();
        s.put_session(&SessionRecord {
            id: "s-todo".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: Some("coder".into()),
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    {
        // Reverse the v3 step (the table goes away) and the v4 step (the column
        // goes away), and the version says 2 — so the migration has to put both
        // back.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("DROP TABLE todo", []).unwrap();
        c.execute("ALTER TABLE session DROP COLUMN parent_session_id", [])
            .unwrap();
        c.execute("UPDATE schema_version SET version = 2", [])
            .unwrap();
    }
    let s = Store::open(&path).unwrap();
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    // The migrated table works, and the session's rows survived the trip.
    assert!(s.todos("s-todo").unwrap().is_empty());
    s.put_todos(
        "s-todo",
        &[TodoItem {
            content: "ship the pane".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }],
    )
    .unwrap();
    assert_eq!(s.todos("s-todo").unwrap().len(), 1);
}

/// **A job survives the daemon that ran it** — the whole point of the table, and the half
/// nothing could answer before it existed.
///
/// The measured shape: the process table is memory, so a handle the pane listed a moment ago
/// answered `no job ... here` to the next daemon, *about a job whose ending was written
/// down*. This closes the store and reopens it, which is the only thing that proves the row
/// is on disk rather than in a cache the next daemon would not have.
#[test]
fn a_job_survives_the_daemon_that_ran_it() {
    let dir = std::env::temp_dir().join(format!("letibot-jobs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");

    let started = JobRecord {
        handle: "j7".into(),
        command: "cargo test --release".into(),
        how: "asked".into(),
        state: "running".into(),
        produced: 0,
        elapsed_ms: 12_000,
        redirect: None,
    };
    {
        let s = Store::open(&path).expect("a store");
        // **The session row first**, because `job.session_id` is a foreign key and
        // `foreign_keys` is on — a job belongs to a session the same way a transcript
        // does. The daemon's own order gives this for free: the session row is written at
        // open, and a job is only ever backgrounded inside a session.
        s.put_session(&SessionRecord {
            id: "s-jobs".into(),
            title: Some("jobs".into()),
            model_id: "qwen".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .expect("the session row");
        s.put_job("s-jobs", &started).expect("the job row");
    }
    {
        let s = Store::open(&path).expect("the same store, a second daemon");
        let back = s.jobs("s-jobs").expect("the job table reads");
        assert_eq!(back.len(), 1, "the job did not come back: {back:?}");
        assert_eq!(back[0], started, "the row came back changed");

        // **And the settlement is the SAME row.** A job has one state at a time, so the
        // second write updates it — a table that appended would make `jobs()` a history of
        // states, when its caller wants one handle's fate.
        let settled = JobRecord {
            state: "exited 0".into(),
            produced: 4_096,
            ..started.clone()
        };
        s.put_job("s-jobs", &settled).expect("the settlement");
        assert_eq!(
            s.jobs("s-jobs").expect("reads"),
            vec![settled],
            "the settlement made a second row"
        );
        // **And a session that never ran a job reads empty** — "no jobs" and "the table
        // is missing" have to be different answers or the second shows up as the first.
        assert!(s.jobs("s-other").expect("reads").is_empty());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A store written before the job table gains one** — and this is the test that guards a
/// live box, which is at v14 today.
///
/// Same fixture rule as the v1 and v2 tests: build a real store, reverse the step by hand with
/// `DROP TABLE`, walk the version back, and assert the migration puts back exactly what
/// `SCHEMA_SQL` would have. **A migration that only ever ran on an empty file would reach a
/// fresh checkout and no existing store** — which is the failure those tests were written for
/// and the reason this one exists rather than trusting `SCHEMA_SQL` alone.
#[test]
fn a_v14_store_is_migrated_and_gains_a_job_table() {
    let path = std::env::temp_dir().join(format!(
        "letibot-migrate-v14-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let s = Store::open(&path).unwrap();
        s.put_session(&SessionRecord {
            id: "s-v14".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: Some("coder".into()),
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("DROP TABLE job", []).unwrap();
        c.execute("UPDATE schema_version SET version = 14", [])
            .unwrap();
        // Prove the fixture really is v14: the table is gone.
        assert!(
            c.query_row("SELECT 1 FROM job", [], |r| r.get::<_, i64>(0))
                .is_err(),
            "the fixture still has a job table, so it is not a v14 store"
        );
    }

    // Opening it runs the migration, and the table it added is a working one — not merely
    // present: a `CREATE TABLE` that disagreed with `SCHEMA_SQL` would pass an existence check
    // and fail on the first insert.
    let s = Store::open(&path).unwrap();
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    assert!(s.jobs("s-v14").unwrap().is_empty());
    s.put_job(
        "s-v14",
        &JobRecord {
            handle: "j1".into(),
            command: "cargo build".into(),
            how: "asked".into(),
            state: "running".into(),
            produced: 0,
            elapsed_ms: 10,
            redirect: None,
        },
    )
    .unwrap();
    assert_eq!(s.jobs("s-v14").unwrap().len(), 1);
}

/// **An old board reads back as a board with no edges, and a need of a kind this build does not
/// know does not take the plan with it.**
///
/// The first half is the `by` precedent: a `todos_json` written before `needs` existed loads
/// with an empty `needs`, which is what it was, and the plan is whole. The second is the reason
/// [`TodoNeed`] has an `Unknown` variant at all — and it is MEASURED, not imagined:
/// `Harness::new` restores a resumed board with `store.todos(id).unwrap_or_default()`, so a
/// need that refused to deserialize would silently EMPTY THE WHOLE PLAN on the first resume
/// after a newer daemon wrote one. What survives instead is the ANSWER: the row is there, the
/// need reads as a kind this build cannot evaluate, and the idle check blocks the row and says
/// so. The two halves are asserted on the raw JSON, because that is the only way to write a
/// list this build did not produce.
#[test]
fn a_board_without_needs_reads_back_and_an_unknown_kind_does_not_empty_the_plan() {
    let s = store();
    let _seeded = seeded(&s);
    let write_raw = |json: &str| {
        s.conn
            .execute(
                "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES (?1, ?2, 0) \
                     ON CONFLICT(session_id) DO UPDATE SET todos_json = ?2",
                params!["sess-1", json],
            )
            .unwrap();
    };
    // **A row exactly as a build before this field would have written it.**
    write_raw(r#"[{"content":"old row","status":"pending","by":"model","when":null}]"#);
    let back = s.todos("sess-1").unwrap();
    assert_eq!(back.len(), 1, "the plan is not emptied");
    assert_eq!(back[0].content, "old row");
    assert!(
        back[0].needs.is_empty(),
        "and the row has no edges, which is what it was"
    );

    // **And a need of a kind a NEWER build wrote** — a wasm predicate, which this one cannot
    // evaluate (see `TodoNeed`'s own list of what is still only shaped for). The plan comes
    // back; the edge reads as unknown; nothing is satisfied.
    write_raw(
        r#"[{"content":"waiting on a child","status":"pending","by":"model","when":null,
                "needs":[{"kind":"wasm","module":"check.wasm"}]}]"#,
    );
    let back = s.todos("sess-1").unwrap();
    assert_eq!(
        back.len(),
        1,
        "a need kind this build does not know must not take the plan with it"
    );
    assert_eq!(back[0].content, "waiting on a child");
    assert_eq!(
        back[0].needs,
        vec![TodoNeed::Unknown],
        "and the edge is kept as one nobody can evaluate, never dropped"
    );

    // **And the shape it round-trips as is the tagged one**, so a reader of the raw row sees a
    // kind rather than a bare string: `{"kind": "row", "content": …}`.
    write_raw(
        r#"[{"content":"deploy","status":"pending","by":"model","when":null,
                       "needs":[{"kind":"row","content":"run the tests"}]}]"#,
    );
    s.put_todos("sess-1", &s.todos("sess-1").unwrap()).unwrap();
    let raw: String = s
        .conn
        .query_row(
            "SELECT todos_json FROM todo WHERE session_id = ?1",
            params!["sess-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        raw.contains(r#""kind":"row","content":"run the tests""#),
        "the edge is a tagged kind on the wire between this crate and `sqlite3`: {raw}"
    );

    // **And the job edge is the same tagged shape under its own kind**, so the two facts a
    // board cannot hold — a child session and a background job — are told apart by a reader of
    // the raw row rather than guessed at from the payload.
    write_raw(
        r#"[{"content":"deploy","status":"pending","by":"model","when":null,
                       "needs":[{"kind":"job","handle":"j2070"}]}]"#,
    );
    let back = s.todos("sess-1").unwrap();
    assert_eq!(
        back[0].needs,
        vec![TodoNeed::Job {
            handle: "j2070".into()
        }],
        "a job edge round-trips as a job edge"
    );
    s.put_todos("sess-1", &back).unwrap();
    let raw: String = s
        .conn
        .query_row(
            "SELECT todos_json FROM todo WHERE session_id = ?1",
            params!["sess-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        raw.contains(r#""kind":"job","handle":"j2070""#),
        "and it is written back as the kind a later build reads: {raw}"
    );
}

#[test]
fn todos_are_replaced_whole_and_read_back_in_order() {
    let s = store();
    let _seeded = seeded(&s);

    // No list yet: empty, not an error, not a distinction from "cleared".
    assert!(s.todos("sess-1").unwrap().is_empty());

    let first = vec![
        TodoItem {
            content: "read the harness".into(),
            status: TodoStatus::Completed,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "seat the tool".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "render the pane".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
    ];
    s.put_todos("sess-1", &first).unwrap();
    assert_eq!(
        s.todos("sess-1").unwrap(),
        first,
        "order survives the store"
    );

    // The second write is the list, not a patch on it: the revision the model
    // made is the only one the store holds.
    let second = vec![TodoItem {
        content: "render the pane".into(),
        status: TodoStatus::InProgress,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }];
    s.put_todos("sess-1", &second).unwrap();
    assert_eq!(s.todos("sess-1").unwrap(), second);

    // An empty write clears; the row remains and answers empty.
    s.put_todos("sess-1", &[]).unwrap();
    assert!(s.todos("sess-1").unwrap().is_empty());
}

/// **A row's title is what precedes the detail separator** — the string
/// [`TODO_TITLE_MAX`] bounds — and a row with no separator is all title.
///
/// The split is a function rather than a rule each writer spells, because there are three
/// writers and the cap is only worth anything if all three measure the same string. The first
/// separator wins: a row's detail may itself contain the em dash, and the title is the part the
/// pane's reader identifies the row by.
#[test]
fn a_rows_title_is_what_precedes_the_detail_separator() {
    assert_eq!(todo_title("plain row"), "plain row");
    assert_eq!(
        todo_title("fix the test — it fails when the store is busy"),
        "fix the test"
    );
    assert_eq!(todo_title("  padded  "), "padded", "the board's own trim");
    assert_eq!(
        todo_title("a — b — c"),
        "a",
        "the FIRST separator, not the last"
    );
    assert_eq!(
        todo_title("one line\nand another"),
        "one line\nand another",
        "**a newline is not a separator**: the pane flattens one, so a second line is not a \
             body the row has — see `todo_title`'s doc"
    );
    assert_eq!(TODO_DETAIL_SEP, " — ", "the spelling both heads write");
    assert_eq!(
        TODO_TITLE_MAX, 135,
        "the pane's own arithmetic — see its doc"
    );
}

/// **A row written before the title cap still reads** — the cap is a rule about WRITING, and
/// nothing in the store refuses or shortens a row that is already there.
///
/// MEASURED as the shape of the failure it prevents: the operator's board has rows on it now,
/// written by a build that had no cap, and a store that dropped or refused one would be a
/// migration by accident — the row would vanish from the plan on the first resume after this
/// change, which is the one outcome the fail-loud rules in this file exist against.
#[test]
fn a_row_written_before_the_title_cap_still_reads() {
    let s = store();
    let _seeded = seeded(&s);
    let legacy = "L".repeat(TODO_TITLE_MAX + 40);
    s.conn
        .execute(
            "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES (?1, ?2, 0) \
                 ON CONFLICT(session_id) DO UPDATE SET todos_json = ?2",
            params![
                "sess-1",
                format!(
                    r#"[{{"content":"{legacy}","status":"pending","by":"model","when":null}}]"#
                )
            ],
        )
        .unwrap();
    let back = s.todos("sess-1").unwrap();
    assert_eq!(back.len(), 1, "the row is not dropped");
    assert_eq!(
        back[0].content, legacy,
        "and not one character of it is cut"
    );
    assert_eq!(
        todo_title(&back[0].content).chars().count(),
        TODO_TITLE_MAX + 40
    );

    // **And it survives a write of the whole list**, which is what a resumed session does with
    // it: the store is not a second door the cap is enforced at.
    s.put_todos("sess-1", &back).unwrap();
    assert_eq!(s.todos("sess-1").unwrap()[0].content, legacy);
}

/// **A postponed row is a row the store keeps, and the LIFT is the same round trip.**
///
/// The operator's ask is that a postponed item *"persists"* — so the state has to survive the
/// one thing persistence means here, which is a write and a read. Asserted in both directions
/// because the two acts are one word apart and a store that read the lift back as a fresh
/// pending row would be the same defect wearing a shrug: the row's own words, its author and
/// its condition have to come back with it, or lifting a row would quietly lose the handle it
/// was waiting on.
///
/// The spelling is asserted too: `postponed` is what a `sqlite3` reader sees in the row, the
/// same way `pending` and `completed` are.
#[test]
fn a_postponed_row_round_trips_through_the_store_and_so_does_lifting_it() {
    let s = store();
    let _seeded = seeded(&s);
    let set_aside = TodoItem {
        content: "push once CI lands".into(),
        status: TodoStatus::Postponed,
        by: TodoBy::Operator,
        when: Some(TodoCondition::Job {
            handle: "j121".into(),
        }),
        needs: Vec::new(),
    };
    s.put_todos("sess-1", &[set_aside.clone()]).unwrap();
    let back = s.todos("sess-1").unwrap();
    assert_eq!(
        back,
        vec![set_aside],
        "the postponed row, its author and its handle all survive the store"
    );
    // The word on the wire between this crate and `sqlite3`.
    let raw: String = s
        .conn
        .query_row(
            "SELECT todos_json FROM todo WHERE session_id = ?1",
            params!["sess-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        raw.contains(r#""status":"postponed""#),
        "a store reader spells it the way every other status is spelled: {raw}"
    );

    // **And the lift.** Back to open work, which is a different row to the evaluator and to
    // the idle check, and it must be a different row after a restart too.
    let lifted = TodoItem {
        status: TodoStatus::Pending,
        ..s.todos("sess-1").unwrap().remove(0)
    };
    s.put_todos("sess-1", &[lifted.clone()]).unwrap();
    assert_eq!(s.todos("sess-1").unwrap(), vec![lifted]);
}

/// **A cancelled row is a row the store KEEPS, and the word on the wire is `cancelled`.**
///
/// The operator's ruling is the whole of what this asserts: *"only i should be able to delete
/// todo items. as a rule everything that ever created stays in history"* and *"so done items or
/// canceled items should be kept"*. `/todo rm N` writes this status instead of taking the row
/// out, so the one thing that has to be true is that a store read hands it back — words, author,
/// condition and all — rather than the list coming back shorter.
///
/// The spelling is asserted for `postponed`'s own reason: `cancelled` is what a `sqlite3` reader
/// sees in the row, and a head reads the same word back.
#[test]
fn a_cancelled_row_stays_on_the_board_and_the_store_keeps_it() {
    let s = store();
    let _seeded = seeded(&s);
    let struck_off = TodoItem {
        content: "push leticl to github".into(),
        status: TodoStatus::Cancelled,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    };
    s.put_todos("sess-1", &[struck_off.clone()]).unwrap();
    assert_eq!(
        s.todos("sess-1").unwrap(),
        vec![struck_off],
        "**the row is still there** — `rm` is a status, not a removal"
    );
    let raw: String = s
        .conn
        .query_row(
            "SELECT todos_json FROM todo WHERE session_id = ?1",
            params!["sess-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        raw.contains(r#""status":"cancelled""#),
        "a store reader spells it the way every other status is spelled: {raw}"
    );
}

/// **A parent's row round trips with the operator's own spelling for its author.**
///
/// The ruling names the author string exactly — *"throught tree author - (Parent
/// <session-id-of-parent>)"* — and the store is where that string has to survive, because a
/// resumed child rebuilds its board from here and a child that could not tell `Parent s-…`
/// from `model` could not tell what it decided from what it was told. The raw JSON is asserted
/// for the same reason the postponed test asserts its own word: `"by":"Parent s-…"` beside
/// `"by":"model"` is the whole of the third author as a `sqlite3` reader sees it.
///
/// And the refusal is asserted on the word: an author this reader does not know is an error
/// naming the word, not a silent `model` — the same rule every other vocabulary in this tree
/// keeps.
#[test]
fn a_parents_row_round_trips_with_the_authors_own_string() {
    let s = store();
    let _seeded = seeded(&s);
    let told = TodoItem {
        content: "land the parity row".into(),
        status: TodoStatus::Pending,
        by: TodoBy::parent_of("s-1789462738453908838"),
        when: None,
        needs: Vec::new(),
    };
    s.put_todos("sess-1", &[told.clone()]).unwrap();
    assert_eq!(s.todos("sess-1").unwrap(), vec![told]);

    let raw: String = s
        .conn
        .query_row(
            "SELECT todos_json FROM todo WHERE session_id = ?1",
            params!["sess-1"],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        raw.contains(r#""by":"Parent s-1789462738453908838""#),
        "the author is the operator's string, in full, beside the other two words: {raw}"
    );

    // **An unknown author is refused by name.** Not read back as the model's — a list whose
    // `by` this reader cannot parse is a list it says so about, taking nothing with it. (The
    // `at line … column …` tail is serde_json's own and not part of the sentence.)
    let bad = r#"[{"content":"x","status":"pending","by":"the cat"}]"#;
    let err = serde_json::from_str::<Vec<TodoItem>>(bad)
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with(
            "`the cat` is not a todo author: model, operator, queue, or `Parent <session-id>`"
        ),
        "the refusal names the word it did not understand: {err}"
    );
}

/// **The queue's word round-trips too** — the fourth author, and the one that is not a session.
///
/// It is a bare name on the wire (`"queue"`), so this is the pair of custom impls agreeing; the
/// rows it stamps are written by the daemon (`TodoBoard::set_queue`) and are a view of the merge
/// queue rather than anything a session wrote.
#[test]
fn the_queues_word_round_trips_and_is_not_read_as_anybody_elses() {
    let raw = r#"[{"content":"review `agent/m-1` (entry m-1): keep the record","status":"pending","by":"queue"}]"#;
    let rows: Vec<TodoItem> = serde_json::from_str(raw).expect("the queue's word is known");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].by, TodoBy::Queue);
    assert_eq!(
        serde_json::to_string(&rows[0].by).expect("serialises"),
        r#""queue""#,
        "one word, the same on both sides"
    );
}

#[test]
fn the_last_turns_context_lives_on_the_session_row() {
    let s = store();
    let _seeded = seeded(&s);

    // Before a turn has finished the row says so: `None`, not zero. A head
    // renders `None` as no number; a stored zero would be a number nobody took.
    let got = s.session("sess-1").unwrap().unwrap();
    assert_eq!(got.context_tokens, None);
    assert_eq!(got.context_cached, None);
    assert_eq!(got.context_ledger, None);

    s.set_context("sess-1", Some(44_700), Some(40_000), Some(43_900))
        .unwrap();
    let got = s.session("sess-1").unwrap().unwrap();
    assert_eq!(got.context_tokens, Some(44_700));
    assert_eq!(got.context_cached, Some(40_000));
    // **The other half of the measurement, and the reason both moved together.**
    // A provider count with no ledger beside it cannot be turned into a ratio:
    // whoever reads it back has to supply the ledger, and supplying TODAY's is
    // the bug this column exists to stop.
    assert_eq!(got.context_ledger, Some(43_900));

    // A later turn replaces the number: it is the LAST turn's prompt, not a
    // sum and not a maximum.
    s.set_context("sess-1", Some(51_000), Some(44_700), Some(50_200))
        .unwrap();
    let got = s.session("sess-1").unwrap().unwrap();
    assert_eq!(got.context_tokens, Some(51_000));
    assert_eq!(got.context_cached, Some(44_700));
    assert_eq!(got.context_ledger, Some(50_200));

    // Clearing takes the pair with it. A row that kept the ledger while the
    // count went away would read as a measurement of nothing.
    s.set_context("sess-1", None, None, None).unwrap();
    let got = s.session("sess-1").unwrap().unwrap();
    assert_eq!(got.context_tokens, None);
    assert_eq!(got.context_ledger, None);

    // **The window, which has its own setter and its own lifetime.** A session opens
    // with one and keeps it until the model underneath it moves; nothing about a turn
    // touches it, which is exactly why it is not a fifth parameter on `set_context`.
    s.set_window("sess-1", Some(262_144)).unwrap();
    assert_eq!(
        s.session("sess-1").unwrap().unwrap().context_window,
        Some(262_144)
    );
    // **A turn's write does not disturb it** — the two facts are measured at different
    // times and folding them together would say they were measured together.
    s.set_context("sess-1", Some(9), Some(8), Some(7)).unwrap();
    assert_eq!(
        s.session("sess-1").unwrap().unwrap().context_window,
        Some(262_144)
    );
    // A session whose window nobody can name CLEARS it rather than keeping a stale
    // one: `None` is the record saying it cannot answer, not a large window.
    s.set_window("sess-1", None).unwrap();
    assert_eq!(s.session("sess-1").unwrap().unwrap().context_window, None);
    assert!(matches!(
        s.set_window("nope", Some(1)),
        Err(StoreError::NotFound(_))
    ));

    // A session nobody has written is not a row to update.
    assert!(matches!(
        s.set_context("nope", Some(1), None, None),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn a_v7_store_is_migrated_and_gains_the_context_columns() {
    // Same fixture rule as the v1/v2 tests: a real store, one step reversed,
    // and the migration has to put back exactly what `SCHEMA_SQL` would have.
    let path = std::env::temp_dir().join(format!(
        "letibot-migrate-v7-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let s = Store::open(&path).unwrap();
        s.put_session(&SessionRecord {
            id: "s-ctx".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    {
        // Reverse the v8 step: the columns go away and the version says 7.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("ALTER TABLE session DROP COLUMN context_tokens", [])
            .unwrap();
        c.execute("ALTER TABLE session DROP COLUMN context_cached", [])
            .unwrap();
        // **And v12's, because a genuine v7 store never had it either.** The
        // fixture is built by walking a CURRENT store backwards, so leaving this
        // column in place would mean the v12 arm below hit its idempotence check
        // (`has = true`) and its `ALTER` — the one line a real pre-v12 store
        // needs — was never run by any test. Same reasoning as the v6 arm's own
        // note about the fixtures, pointed the other way.
        // **v14's column is NOT dropped here, because this table will not give it up.**
        // sqlite refuses `ALTER TABLE session DROP COLUMN context_window` on it —
        // MEASURED: *"error in table session after drop column: incomplete input"*, the
        // statement-text re-splice tripping over this table's own comment blocks (the
        // same class of constraint the note below records for the other drop order). So
        // this walk stops at v13, and the v14 arm has a fixture of its own — the honest
        // shape for an ADDITIVE arm, which needs a `session` table rather than a walk.
        // **v13's first, and the order is not taste.** The store is walked backwards,
        // so both columns are here and neither arm's `ALTER` would run without this —
        // and dropping provider_choice while context_ledger still follows is the drop
        // sqlite accepts; the other order dies reconstructing the table's CREATE text
        // ("incomplete input"), which is also why the column sits before v12's in
        // SCHEMA_SQL rather than at the end.
        c.execute("ALTER TABLE session DROP COLUMN provider_choice", [])
            .unwrap();
        c.execute("ALTER TABLE session DROP COLUMN context_ledger", [])
            .unwrap();
        c.execute("UPDATE schema_version SET version = 7", [])
            .unwrap();
        assert!(
            c.query_row("SELECT context_tokens FROM session", [], |r| r
                .get::<_, Option<i64>>(0))
                .is_err(),
            "the fixture still has a context_tokens column, so it is not a v7 store"
        );
        assert!(
            c.query_row("SELECT context_ledger FROM session", [], |r| r
                .get::<_, Option<i64>>(0))
                .is_err(),
            "the fixture still has a context_ledger column, so the v12 ALTER is untested"
        );
    }

    let s = Store::open(&path).unwrap();
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    // The migrated row reads as "no turn has finished since the column
    // existed", and a write after the migration comes back.
    let got = s.session("s-ctx").unwrap().expect("the v7 row survived");
    assert_eq!(got.context_tokens, None);
    // v12's column arrived with the same migration run, and is `None` on this
    // pre-existing row: unverifiable rather than zero, so the recovery refuses
    // the pair. See the v12 arm.
    assert_eq!(got.context_ledger, None);
    // v14's column arrived with the same run, and is `None` on a pre-existing row:
    // *nobody recorded one* — never the parent's window and never a guess. See the
    // v14 arm for why it is not backfilled.
    assert_eq!(got.context_window, None);
    s.set_window("s-ctx", Some(262_144)).unwrap();
    assert_eq!(
        s.session("s-ctx").unwrap().unwrap().context_window,
        Some(262_144)
    );

    s.set_context("s-ctx", Some(12_000), Some(11_000), Some(11_800))
        .unwrap();
    let got = s.session("s-ctx").unwrap().unwrap();
    assert_eq!(got.context_tokens, Some(12_000));
    assert_eq!(got.context_cached, Some(11_000));
    assert_eq!(got.context_ledger, Some(11_800));
}

/// **A v13 store gains the window column** — v14's arm, with a fixture of its own.
///
/// Every other arm here is tested by walking a CURRENT store backwards, and this one
/// cannot be: sqlite refuses to drop `context_window` from this table (see the v7
/// fixture's note — measured, `incomplete input`). A hand-written v13 `session` is the
/// right fixture for an **additive** arm, and it is the one place in this file where
/// hand-written DDL is not drift: the arm only needs the table to exist, and what is
/// asserted is what the arm DOES — the column appears, a row written before it reads
/// `NULL` rather than a guess, and a value written through the API comes back.
///
/// The other tables are deliberately absent, so this reads through `connection()`
/// rather than `session()`: `list_sessions` joins `transcript` and `transcript_item`,
/// and building those by hand is the drift this fixture is trying to avoid.
#[test]
fn a_v13_store_gains_the_context_window_column() {
    let path = std::env::temp_dir().join(format!(
        "letibot-migrate-v13-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                     id    TEXT PRIMARY KEY,
                     title TEXT,
                     role  TEXT
                 );
                 INSERT INTO session (id, title, role) VALUES ('s-window', 'old', 'coder');
                 CREATE TABLE schema_version (version INTEGER);
                 INSERT INTO schema_version (version) VALUES (13);",
        )
        .unwrap();
        // Prove the fixture really is pre-v14, or the arm below is tested by nothing.
        assert!(
            c.query_row("SELECT context_window FROM session", [], |r| r
                .get::<_, Option<i64>>(0))
                .is_err(),
            "the fixture already has a context_window column, so it is not a v13 store"
        );
    }

    let s = Store::open(&path).unwrap();
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION, "the migration stamped the new version");

    // The column is there, and the row that predates it says *nobody recorded one* —
    // never the parent window, and never a catalogue guess. See the v14 arm.
    let got: Option<i64> = s
        .connection()
        .query_row(
            "SELECT context_window FROM session WHERE id = 's-window'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got, None);

    // And it is writeable through the API this column exists for.
    s.set_window("s-window", Some(262_144)).unwrap();
    let got: Option<i64> = s
        .connection()
        .query_row(
            "SELECT context_window FROM session WHERE id = 's-window'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got, Some(262_144));

    // Reopening is a no-op rather than a second migration.
    drop(s);
    let s = Store::open(&path).unwrap();
    assert!(
        s.set_window("s-window", Some(1)).is_ok(),
        "a store already at the current version still answers a write"
    );
}

/// **A v18 store gains the stood-down pair** — v19's arm, in the v13
/// fixture's shape (additive arms take hand-written DDL; see that test's
/// note for why this is the one place hand-written DDL is not drift).
///
/// What is asserted is what the arm DOES: the two columns appear, a row
/// written before them reads as *the guard has not fired* rather than as a
/// guess, and a pair written through the API comes back.
#[test]
fn a_v18_store_gains_the_stood_down_pair() {
    let path = std::env::temp_dir().join(format!(
        "letibot-migrate-v18-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                     id    TEXT PRIMARY KEY,
                     title TEXT,
                     role  TEXT
                 );
                 INSERT INTO session (id, title, role) VALUES ('s-stood', 'old', 'coder');
                 CREATE TABLE schema_version (version INTEGER);
                 INSERT INTO schema_version (version) VALUES (18);",
        )
        .unwrap();
        // Prove the fixture really is pre-v19, or the arm below is tested
        // by nothing.
        assert!(
            c.query_row("SELECT auto_compact_resident FROM session", [], |r| r
                .get::<_, Option<i64>>(0))
                .is_err(),
            "the fixture already has an auto_compact_resident column, so it is not a v18 store"
        );
    }

    let s = Store::open(&path).unwrap();
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION, "the migration stamped the new version");

    // Both columns are there, and the row that predates them says *the
    // guard has not fired* — which is not the same fact as `auto_compact`
    // being on, and is deliberately not backfilled.
    let (resident, after): (Option<i64>, Option<i64>) = s
        .connection()
        .query_row(
            "SELECT auto_compact_resident, auto_compact_after FROM session \
                 WHERE id = 's-stood'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((resident, after), (None, None));

    // And the pair is writeable through the API this column exists for.
    s.set_auto_compact_stood_down("s-stood", Some((1_849_499, 1_850_000)))
        .unwrap();
    let (resident, after): (Option<i64>, Option<i64>) = s
        .connection()
        .query_row(
            "SELECT auto_compact_resident, auto_compact_after FROM session \
                 WHERE id = 's-stood'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((resident, after), (Some(1_849_499), Some(1_850_000)));

    // Reopening is a no-op rather than a second migration.
    drop(s);
    let s = Store::open(&path).unwrap();
    assert!(
        s.set_auto_compact_stood_down("s-stood", None).is_ok(),
        "a store already at the current version still answers a write"
    );
}

/// **A v20 store gains the frozen prefix's fingerprint** — v21's arm, in the v13
/// fixture's shape (additive arms take hand-written DDL; see that test's note for why
/// this is the one place hand-written DDL is not drift).
///
/// What is asserted is what the arm DOES: the column appears, a row written before it
/// reads `NULL` — *nobody recorded one here*, which is deliberately not backfilled and
/// not the same fact as *the prefix is unchanged* — and a fingerprint written through
/// the API comes back, which is what the resume path's comparison reads.
#[test]
fn a_v20_store_gains_the_prefix_fingerprint_column() {
    let path = std::env::temp_dir().join(format!(
        "letibot-migrate-v20-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE session (
                     id    TEXT PRIMARY KEY,
                     title TEXT,
                     role  TEXT
                 );
                 INSERT INTO session (id, title, role) VALUES ('s-seated', 'old', 'coder');
                 CREATE TABLE schema_version (version INTEGER);
                 INSERT INTO schema_version (version) VALUES (20);",
        )
        .unwrap();
        // Prove the fixture really is pre-v21, or the arm below is tested by nothing.
        assert!(
            c.query_row("SELECT prefix_fingerprint FROM session", [], |r| r
                .get::<_, Option<String>>(0))
                .is_err(),
            "the fixture already has a prefix_fingerprint column, so it is not a v20 store"
        );
    }

    let s = Store::open(&path).unwrap();
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION, "the migration stamped the new version");

    let got: Option<String> = s
        .connection()
        .query_row(
            "SELECT prefix_fingerprint FROM session WHERE id = 's-seated'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        got, None,
        "a session seated before the column existed reads as unrecorded, which the \
             resume path answers by falling back to the session's own stable_prefix row \
             rather than by treating it as unchanged"
    );

    // And it is writeable through the API this column exists for.
    s.set_prefix_fingerprint("s-seated", "0123456789abcdef")
        .unwrap();
    let got: Option<String> = s
        .connection()
        .query_row(
            "SELECT prefix_fingerprint FROM session WHERE id = 's-seated'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got.as_deref(), Some("0123456789abcdef"));
    // A session nobody has heard of is refused by name rather than silently doing
    // nothing — the rule every setter in this file follows.
    assert!(s.set_prefix_fingerprint("s-nobody", "x").is_err());

    // Reopening is a no-op rather than a second migration.
    drop(s);
    let s = Store::open(&path).unwrap();
    assert!(
        s.set_prefix_fingerprint("s-seated", "fedcba9876543210")
            .is_ok(),
        "a store already at the current version still answers a write"
    );
}

/// **The guard's pair survives the daemon that decided it** — the whole
/// point of the columns, and the half nothing could answer before they
/// existed.
///
/// The guard's old write was `auto_compact = false` in the daemon's
/// memory: it died with the process, so a restart silently re-armed the
/// looping the guard existed to stop AND silently cleared the one fact
/// that explained a session that would not tidy itself. This closes and
/// reopens the store, which is the only thing that proves the row is on
/// disk rather than in a cache the next daemon would not have.
#[test]
fn a_stood_down_compaction_survives_the_daemon_that_decided_it() {
    let dir = std::env::temp_dir().join(format!("letibot-stood-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _clean = Clean(dir.clone());
    let path = dir.join("sessions.db");

    // The pair at the wedge's own scale: the 2026-10-09 session compacted
    // 1,849,499 ledger tokens and the summary still stood within a
    // headroom of the 1,000,000-token plan.
    {
        let s = Store::open(&path).unwrap();
        s.put_session(&SessionRecord {
            id: "s-wedge".into(),
            title: Some("the wedged session".into()),
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
        s.set_auto_compact_stood_down("s-wedge", Some((1_849_499, 1_530_411)))
            .unwrap();
    }

    // The next daemon opens the same file and reads the finding.
    let s = Store::open(&path).unwrap();
    let got = s
        .session("s-wedge")
        .unwrap()
        .expect("the row survived the reopen");
    assert_eq!(
        got.auto_compact_stood_down,
        Some((1_849_499, 1_530_411)),
        "the pair comes back in the units it was taken in — LEDGER tokens, \
             because that is what the guard decided in"
    );

    // And a session the guard never fired on stays absent, which is a
    // different fact from a pair of zeros: `None` is *the guard has not
    // fired*, and the settings row keeps it apart from `--no-auto-compact`.
    s.put_session(&SessionRecord {
        id: "s-fresh".into(),
        title: None,
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/w".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .unwrap();
    assert_eq!(
        s.session("s-fresh")
            .unwrap()
            .unwrap()
            .auto_compact_stood_down,
        None
    );
}

/// **The provider a session was SWITCHED TO is a fact on the session row, and it
/// round-trips** — `name`, `name/model`, `local`, and NULL for one that never chose.
///
/// The four spellings are four different claims at a resume, and the `local` one is
/// the easy to get wrong: `None` says *no opinion, the daemon's default stands*, while
/// `Some("local")` says *the local server, deliberately* — and a daemon whose own default
/// is a provider would hand a switched-back session straight back to it if the two were
/// collapsed.
#[test]
fn the_provider_choice_round_trips_with_its_four_spellings() {
    let s = store();
    let _ = seeded(&s);
    // Never switched: NULL, not the empty string — `set_title`'s own rule, so an
    // unset choice reads as an absence and not as a name that is blank.
    assert_eq!(s.provider_choice("sess-1").unwrap(), None);
    assert_eq!(s.session("sess-1").unwrap().unwrap().provider_choice, None);

    s.set_provider_choice("sess-1", Some("deepseek")).unwrap();
    assert_eq!(
        s.provider_choice("sess-1").unwrap().as_deref(),
        Some("deepseek")
    );
    s.set_provider_choice("sess-1", Some("glm-coding/glm-5.3"))
        .unwrap();
    // Also on the listing, which is what a resume reads.
    assert_eq!(
        s.session("sess-1")
            .unwrap()
            .unwrap()
            .provider_choice
            .as_deref(),
        Some("glm-coding/glm-5.3")
    );
    // The deliberate switch back is BY NAME, not NULL.
    s.set_provider_choice("sess-1", Some("local")).unwrap();
    assert_eq!(
        s.provider_choice("sess-1").unwrap().as_deref(),
        Some("local")
    );
    s.set_provider_choice("sess-1", None).unwrap();
    assert_eq!(s.provider_choice("sess-1").unwrap(), None);
    // And a session that does not exist is refused, not invented.
    assert!(s.set_provider_choice("no-such", Some("deepseek")).is_err());
}

/// **A v12 store gains the column as NULL**, which is exactly "never switched": a
/// session recorded before this column existed resumes on the daemon's own default,
/// which is what it always did. Walked back from a current store, for the same reason
/// the v12 fixture walks back from one: the conversation's rows stay, so the migration
/// is exercised against rows that already exist rather than against an empty file.
#[test]
fn a_v12_store_gains_the_provider_column_as_never_switched() {
    let path = std::env::temp_dir().join(format!(
        "letibot-v13-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let s = Store::open(&path).unwrap();
        let _ = seeded(&s);
    }
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("ALTER TABLE session DROP COLUMN provider_choice", [])
            .unwrap();
        c.execute("UPDATE schema_version SET version = 12", [])
            .unwrap();
    }

    let s = Store::open(&path).unwrap();
    assert_eq!(
        s.provider_choice("sess-1").unwrap(),
        None,
        "a session recorded before the column existed has no choice, and NULL says so"
    );
    // And the column is there to be written: the migration is not a no-op that
    // merely bumps the version.
    s.set_provider_choice("sess-1", Some("glm-coding/glm-5.3"))
        .unwrap();
    assert_eq!(
        s.provider_choice("sess-1").unwrap().as_deref(),
        Some("glm-coding/glm-5.3")
    );
}

#[test]
fn the_v8_migration_backfills_the_context_of_a_conversation_already_on_disk() {
    // The case that matters: a store being upgraded holds a conversation whose
    // last turn finished before the column existed. Without the backfill its
    // row reads `NULL` — "no turn has finished" — and an attaching head shows
    // no context until the next turn. The backfill sums the prefix and the
    // current transcript's items, the same rows the encoder reads.
    let path = std::env::temp_dir().join(format!(
        "letibot-backfill-v8-{}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_file(&path);
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = Clean(path.clone());

    {
        let s = Store::open(&path).unwrap();
        let (tr, _) = seeded(&s);
        // Two items: 2 tokens and 3 tokens. The prefix carries 4.
        let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
        let item = TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
        };
        let row = ledger.append("it-0", &[10, 11]).unwrap().clone();
        s.append_item(&tr, 0, &item, &row, &[10, 11]).unwrap();
        let row = ledger.append("it-1", &[12, 13, 14]).unwrap().clone();
        s.append_item(&tr, 1, &item, &row, &[12, 13, 14]).unwrap();
    }
    {
        // Reverse the v8 step: the columns go away and the version says 7. The
        // conversation's rows stay, which is what the migration has to read.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("ALTER TABLE session DROP COLUMN context_tokens", [])
            .unwrap();
        c.execute("ALTER TABLE session DROP COLUMN context_cached", [])
            .unwrap();
        c.execute("UPDATE schema_version SET version = 7", [])
            .unwrap();
    }

    let s = Store::open(&path).unwrap();
    let got = s.session("sess-1").unwrap().expect("the row survived");
    // 4 (prefix) + 2 + 3 (items) = 9: the prompt the next round would send.
    assert_eq!(
        got.context_tokens,
        Some(9),
        "the backfill is the prefix plus the current transcript's items"
    );
    // The fraction of a prompt that was never sent is not a measurement.
    assert_eq!(got.context_cached, None);

    // A session with no transcript is not invented a number: the subquery
    // finds nothing and the row stays `NULL`.
    s.put_session(&SessionRecord {
        id: "s-empty".into(),
        title: None,
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/w".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .unwrap();
    // Reopening must not backfill it: the column already exists, so the
    // `NULL` means "no turn has finished", not "needs a number".
    drop(s);
    let s = Store::open(&path).unwrap();
    let got = s.session("s-empty").unwrap().unwrap();
    assert_eq!(got.context_tokens, None, "no transcript, no number");
}

/// **A row that waits on the operator survives the store, and the picker can find it** — the
/// operator: *"todo can be dependend on ME … and then when i submit todo becomes unblocked with
/// reply attached"*. Both edges round-trip tagged (`operator`, `answered`), a row written before
/// they existed still loads, and `sessions_waiting_on_operator` counts only OPEN rows with an
/// unanswered question — a done row's leftover question, and an answered one, ask nobody anything.
#[test]
fn an_operator_question_round_trips_and_the_store_counts_who_is_waiting() {
    let s = store();
    let _seeded = seeded(&s);
    for id in ["waits", "quiet"] {
        s.put_session(&SessionRecord {
            id: id.into(),
            title: None,
            model_id: "qwen".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "deadtrickster".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    let row = |content: &str, status: TodoStatus, needs: Vec<TodoNeed>| TodoItem {
        content: content.into(),
        status,
        by: TodoBy::Model,
        when: None,
        needs,
    };
    let asked = TodoNeed::Operator {
        question: "which db?".into(),
        options: vec!["postgres".into(), "sqlite".into()],
    };
    let answered = TodoNeed::Answered {
        question: "which db?".into(),
        reply: "the operator chose \"postgres\"".into(),
    };
    s.put_todos(
        "waits",
        &[
            row("deploy", TodoStatus::Pending, vec![asked.clone()]),
            row("done already", TodoStatus::Completed, vec![asked.clone()]),
            row("answered", TodoStatus::Pending, vec![answered.clone()]),
        ],
    )
    .unwrap();
    s.put_todos(
        "quiet",
        &[row("answered", TodoStatus::Pending, vec![answered.clone()])],
    )
    .unwrap();
    let back = s.todos("waits").unwrap();
    assert_eq!(back[0].needs, vec![asked]);
    assert_eq!(back[2].needs, vec![answered]);
    let waiting = s.sessions_waiting_on_operator().unwrap();
    assert_eq!(waiting.get("waits"), Some(&1));
    assert!(!waiting.contains_key("quiet"));
    assert_eq!(operator_asks(&back).len(), 1);
}

/// A `n`-row run continuing `ledger`, as [`Store::append_items`] takes it.
fn rows_for(
    ledger: &mut TokenLedger,
    from: usize,
    n: usize,
) -> Vec<(TranscriptItem, LedgerRow, Vec<TokenId>)> {
    (from..from + n)
        .map(|i| {
            let toks: Vec<TokenId> = (0..32).map(|t| (i * 32 + t) as TokenId).collect();
            let row = ledger.append(&format!("it-{i}"), &toks).unwrap().clone();
            let item = TranscriptItem::Assistant {
                text: format!("row {i}"),
                tool_calls: vec![],
                truncated: false,
            };
            (item, row, toks)
        })
        .collect()
}

fn borrowed(
    rows: &[(TranscriptItem, LedgerRow, Vec<TokenId>)],
) -> Vec<(&TranscriptItem, &LedgerRow, &[TokenId])> {
    rows.iter().map(|(i, r, t)| (i, r, t.as_slice())).collect()
}

/// **A run of rows lands whole or not at all, and the triggers still check every row.**
///
/// `append_items` is the batched form of `append_item` that a fork's carry writes through, so
/// it must refuse exactly what the single form refuses — and refuse it for the WHOLE run: a
/// chunk that half-landed would leave the harness's `persisted` count disagreeing with the
/// store, which is the one state a resume cannot rebuild honestly.
#[test]
fn a_run_of_rows_lands_whole_or_not_at_all() {
    let s = store();
    let (tr, _) = seeded(&s);
    let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let first = rows_for(&mut ledger, 0, 10);
    s.append_items(&tr, 0, borrowed(&first)).unwrap();
    assert_eq!(s.item_count(&tr).unwrap(), 10);

    // A run whose fourth row leaves a hole in the token stream.
    let good = rows_for(&mut ledger, 10, 6);
    let mut bad = good.clone();
    bad[3].1.tok_offset += 7;
    assert!(s.append_items(&tr, 10, borrowed(&bad)).is_err());
    assert_eq!(
        s.item_count(&tr).unwrap(),
        10,
        "none of the refused run's rows landed, not even the three before the bad one"
    );

    // A run at a seq that skips is refused too.
    assert!(s.append_items(&tr, 11, borrowed(&good)).is_err());
    assert_eq!(s.item_count(&tr).unwrap(), 10);

    // And the good continuation lands, readable as the rows it wrote.
    s.append_items(&tr, 10, borrowed(&good)).unwrap();
    let back = s.load_transcript(&tr).unwrap();
    assert_eq!(back.items.len(), 16);
    assert_eq!(back.items[15].1, good[5].1);
    assert_eq!(back.items[15].2, good[5].2);
    // The connection is not left inside a transaction.
    assert!(s.connection().is_autocommit());
}

/// **MEASURED: under a competing writer, a row per transaction is what made the re-seat take
/// minutes, and a run per transaction is not.**
///
/// The rano session's `/reseat`, 2026-10-12: 971 rows written from 01:17:48 to 01:24:07, one
/// autocommit `INSERT` each, on a store a dozen daemons write to. Every row was its own trip for
/// the file's single write lock, and while another connection holds it each trip pays the busy
/// handler's back-off.
///
/// **What is asserted is the number of trips, not the time they took.** This test used to run a
/// competitor holding the lock and require the run form to be five times faster by the wall clock;
/// that held alone and failed under the whole workspace's tests, and the same 200 rows one by one
/// took anywhere from 0.3 to 30 seconds — a ratio of two durations under contention measures the
/// box as much as the code. The property the fix is made of is that a run takes the lock once per
/// [`Store::APPEND_CHUNK`] rows where rows one by one take it once each, and a commit hook counts
/// exactly that, with no competitor and no clock.
#[test]
fn a_run_takes_the_write_lock_once_per_chunk_not_once_per_row() {
    let path = std::env::temp_dir().join(format!(
        "letibot-append-commits-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    struct Clean(std::path::PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{ext}", self.0.display()));
            }
        }
    }
    let _clean = Clean(path.clone());
    let s = Store::open(&path).unwrap();
    let (tr, prefix_id) = seeded(&s);
    s.put_transcript("tr-2", "sess-1", &prefix_id).unwrap();
    // Every commit on this connection, counted: each is one trip for the write lock.
    let commits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let commits = commits.clone();
        s.connection().commit_hook(Some(move || {
            commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            false
        }));
    }
    let count = || commits.load(std::sync::atomic::Ordering::Relaxed);

    const N: usize = 200;
    let mut one = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
    let rows = rows_for(&mut one, 0, N);
    let before = count();
    for (i, (item, row, toks)) in rows.iter().enumerate() {
        s.append_item(&tr, i as u32, item, row, toks).unwrap();
    }
    let one_by_one = count() - before;

    let mut run = TokenLedger::new("tr-2", &[1, 2, 3, 4]).unwrap();
    let rows = rows_for(&mut run, 0, N);
    let before = count();
    for (k, chunk) in borrowed(&rows).chunks(Store::APPEND_CHUNK).enumerate() {
        s.append_items(
            "tr-2",
            (k * Store::APPEND_CHUNK) as u32,
            chunk.iter().copied(),
        )
        .unwrap();
    }
    let in_runs = count() - before;

    assert_eq!(s.item_count(&tr).unwrap(), N as u32);
    assert_eq!(s.item_count("tr-2").unwrap(), N as u32);
    assert!(
        one_by_one >= N,
        "the premise: a row per transaction ({one_by_one} commits for {N} rows)"
    );
    assert_eq!(
        in_runs,
        N.div_ceil(Store::APPEND_CHUNK),
        "the run form must not pay the lock per row: {in_runs} commits for {N} rows"
    );
}

fn job_store(tag: &str) -> (std::path::PathBuf, Store) {
    let dir = std::env::temp_dir().join(format!(
        "letibot-jobs-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let s = Store::open(&path).expect("a store");
    for id in ["s-a", "s-b"] {
        s.put_session(&SessionRecord {
            id: id.into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    }
    (dir, s)
}

fn job_row(handle: &str, state: &str) -> JobRecord {
    JobRecord {
        handle: handle.into(),
        command: "cargo test".into(),
        how: "asked".into(),
        state: state.into(),
        produced: 0,
        elapsed_ms: 10,
        redirect: None,
    }
}

/// **A job that ended and was never delivered is found again by the next daemon** (R91.5) — and
/// one that was delivered, or is still `running`, is not. A settlement written AFTER the delivery
/// (the watcher and the reader race) does not take the delivery back.
#[test]
fn a_settled_job_nobody_was_told_about_is_found_and_a_delivered_one_is_not() {
    let (dir, s) = job_store("undelivered");
    s.put_job("s-a", &job_row("j1", "running")).unwrap();
    s.put_job("s-a", &job_row("j2", "running")).unwrap();
    s.put_job("s-a", &job_row("j3", "running")).unwrap();
    s.put_job("s-a", &job_row("j1", "exited 0")).unwrap();
    s.put_job("s-a", &job_row("j2", "exited 1")).unwrap();
    s.deliver_job("s-a", "j2").unwrap();
    s.put_job("s-a", &job_row("j2", "exited 1")).unwrap();
    let left: Vec<String> = s
        .undelivered_jobs("s-a")
        .unwrap()
        .into_iter()
        .map(|j| j.handle)
        .collect();
    assert_eq!(left, vec!["j1".to_string()], "only j1 ended untold");
    assert!(s.undelivered_jobs("s-b").unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The highest `jN` across every session** is what a daemon seeds its counter above (R91.3), so
/// a handle minted after a restart cannot be one a session already has.
#[test]
fn the_highest_job_number_is_read_across_sessions() {
    let (dir, s) = job_store("maxjob");
    assert_eq!(s.max_job_number().unwrap(), 0, "an empty store");
    s.put_job("s-a", &job_row("j9", "exited 0")).unwrap();
    s.put_job("s-b", &job_row("j41", "running")).unwrap();
    s.put_job("s-b", &job_row("j100", "exited 0")).unwrap();
    // Not a counter's handle: a subagent's, or a name.
    s.put_job("s-b", &job_row("s-1-sub", "exited 0")).unwrap();
    s.put_job("s-b", &job_row("jx", "exited 0")).unwrap();
    assert_eq!(s.max_job_number().unwrap(), 100);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A v27 store gains `job.delivered`, and its existing rows read as delivered** — this build
/// cannot know which endings those daemons told, and re-announcing all of them would bury the one
/// that matters. A row written after the migration starts undelivered.
#[test]
fn a_v27_store_is_migrated_and_its_old_jobs_count_as_delivered() {
    let (dir, s) = job_store("v27");
    let path = dir.join("sessions.db");
    s.put_job("s-a", &job_row("j1", "exited 0")).unwrap();
    drop(s);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("ALTER TABLE job DROP COLUMN delivered", [])
            .unwrap();
        c.execute("UPDATE schema_version SET version = 27", [])
            .unwrap();
    }
    let s = Store::open(&path).unwrap();
    let v: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    assert!(
        s.undelivered_jobs("s-a").unwrap().is_empty(),
        "a job from before the column was re-announced"
    );
    s.put_job("s-a", &job_row("j2", "exited 0")).unwrap();
    assert_eq!(s.undelivered_jobs("s-a").unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **An unsealed fork is not the session's conversation; a sealed one is** (R91.2). A daemon that
/// dies while a fork's rows are being written resumes onto the parent, whole, rather than onto the
/// part of the fork that had been written.
#[test]
fn an_unsealed_fork_is_not_current_until_it_is_sealed() {
    let s = store();
    let (tr, prefix_id) = seeded(&s);
    s.put_fork_unsealed("tr-2", "sess-1", &prefix_id, &tr, 1)
        .unwrap();
    assert_eq!(
        s.current_transcript_id("sess-1").unwrap().as_deref(),
        Some(tr.as_str()),
        "a fork whose rows are not all stored must not be what a resume opens"
    );
    assert_eq!(
        s.session("sess-1").unwrap().unwrap().id,
        "sess-1",
        "the session row still reads"
    );
    s.seal_transcript("tr-2").unwrap();
    assert_eq!(
        s.current_transcript_id("sess-1").unwrap().as_deref(),
        Some("tr-2"),
        "sealed, the fork is the conversation"
    );
    assert!(s.seal_transcript("nope").is_err());
}

/// **A row that does not start where the transcript's prefix ends is refused** — the
/// assertion the insert trigger cannot make for `seq` 0, and the one the operator's store
/// needed on 2026-10-25: at that point `s-gatekeeper#t0` was seated under a 24,407-token
/// prefix, had no rows, and a newer daemon appended fourteen rows starting at **32162**
/// (its own prefix length). Every one of those rows is correctly hashed and the trigger
/// accepted all fourteen, because row 0 continues nothing — and no resume has been able to
/// rebuild that transcript since.
#[test]
fn a_first_row_that_does_not_start_after_the_prefix_is_refused() {
    let s = store();
    let (tr, _) = seeded(&s);
    let item = TranscriptItem::Assistant {
        text: "x".into(),
        tool_calls: vec![],
        truncated: false,
    };
    let toks = vec![5u32, 6];
    // The prefix is four tokens long, so row 0 starts at 4 and nowhere else.
    for wrong in [0u32, 3, 5, 32162] {
        let row = LedgerRow {
            item_id: "a".into(),
            tok_offset: wrong,
            tok_len: 2,
            h_k: [7; 32],
        };
        let err = s
            .append_item(&tr, 0, &item, &row, &toks)
            .expect_err("a row at the wrong offset must be refused");
        let said = err.to_string();
        assert!(
            said.contains(&format!("would start at token {wrong}")),
            "the refusal must name the offset it was given: {said}"
        );
        assert!(
            said.contains("ends: 4"),
            "and the one the store requires: {said}"
        );
        assert_eq!(s.item_count(&tr).unwrap(), 0, "nothing was written");
    }

    // The batched form is the same assertion once for the run — `Harness::persist`'s path.
    let row = LedgerRow {
        item_id: "b".into(),
        tok_offset: 9,
        tok_len: 2,
        h_k: [8; 32],
    };
    let err = s
        .append_items(&tr, 9, [(&item, &row, toks.as_slice())])
        .expect_err("a run that starts anywhere else is refused too");
    assert!(err.to_string().contains("ends: 4"), "{err}");
    assert_eq!(s.item_count(&tr).unwrap(), 0);

    // And the honest row lands, as many times as the prefix and the rows so far allow.
    let good = LedgerRow {
        item_id: "c".into(),
        tok_offset: 4,
        tok_len: 2,
        h_k: [9; 32],
    };
    s.append_item(&tr, 0, &item, &good, &toks).unwrap();
    let after = LedgerRow {
        item_id: "d".into(),
        tok_offset: 6,
        tok_len: 2,
        h_k: [10; 32],
    };
    s.append_item(&tr, 1, &item, &after, &toks).unwrap();
    assert_eq!(s.item_count(&tr).unwrap(), 2);
}

/// **An empty transcript is re-seated; one with rows is refused, because those rows speak
/// the prefix they were written under.** The first half is what `Harness::open` does to a
/// transcript an older build seated and nobody has written to — the second is the guard
/// that makes it safe to do it.
#[test]
fn a_transcript_is_reseated_only_while_it_holds_no_rows() {
    let s = store();
    let (tr, old) = seeded(&s);
    let mut other = prefix();
    other.system = "a newer build's prompt".into();
    other.tokens = vec![9u32; 12];
    other.h_init = hash_tokens(&other.tokens);
    let new = s.put_stable_prefix(&other).unwrap();

    assert_eq!(s.transcript_prefix_id(&tr).unwrap().as_deref(), Some(&*old));
    assert_eq!(s.stable_prefix_ids_of_len(12).unwrap(), vec![new.clone()]);
    assert!(
        s.stable_prefix_ids_of_len(4).unwrap().contains(&old),
        "the four-token prefix is one of the four-token answers"
    );

    s.set_transcript_prefix(&tr, &new).unwrap();
    assert_eq!(s.transcript_prefix_id(&tr).unwrap().as_deref(), Some(&*new));

    // Now it has a row, and the same call must refuse rather than make that row unreadable.
    let item = TranscriptItem::Assistant {
        text: "x".into(),
        tool_calls: vec![],
        truncated: false,
    };
    let toks = vec![5u32, 6, 7];
    let row = LedgerRow {
        item_id: "a".into(),
        // Twelve tokens of prefix now, which is also what the anchor requires.
        tok_offset: 12,
        tok_len: 3,
        h_k: [1; 32],
    };
    s.append_item(&tr, 0, &item, &row, &toks).unwrap();
    let err = s
        .set_transcript_prefix(&tr, &old)
        .expect_err("a transcript with rows must not have its prefix moved");
    assert!(err.to_string().contains("holds 1 row(s)"), "{err}");
    assert_eq!(s.transcript_prefix_id(&tr).unwrap().as_deref(), Some(&*new));

    // An unknown transcript is `NotFound`, not a silent success.
    assert!(s.set_transcript_prefix("no-such", &new).is_err());
    assert_eq!(s.transcript_prefix_id("no-such").unwrap(), None);
}
