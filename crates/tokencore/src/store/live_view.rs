//! A read-only window onto a store somebody else is writing, for tests that measure the real
//! `sessions.db`. An in-memory [`Store`] with the file `ATTACH`ed `mode=ro`; [`LiveView::load`]
//! copies a transcript in, loads it, and rolls back. The copy drops the append-only triggers.

use std::path::Path;

use rusqlite::{Connection, params};

use super::{LoadedTranscript, Result, SCHEMA_VERSION, Store, StoreError};

/// The schema name the live file is attached under.
const LIVE: &str = "live";

/// The `transcript_item` triggers of `SCHEMA_SQL`; see the module doc for why the copy
/// drops them.
const WRITER_TRIGGERS: [&str; 3] = [
    "transcript_item_no_update",
    "transcript_item_no_delete",
    "transcript_item_append_only_insert",
];

/// A read-only view of a store file, which copies one transcript at a time into memory.
pub struct LiveView {
    store: Store,
}

impl LiveView {
    /// Attach `path` read-only. Refuses a file written by a newer build, as
    /// [`Store::open`] would - including one whose reader floor this build is below.
    pub fn open(path: &Path) -> Result<Self> {
        let store = Store::open_in_memory()?;
        {
            let conn = store.connection();
            conn.pragma_update(None, "foreign_keys", false)?;
            for t in WRITER_TRIGGERS {
                conn.execute_batch(&format!("DROP TRIGGER IF EXISTS main.{t}"))?;
            }
            conn.execute(
                &format!("ATTACH DATABASE ?1 AS {LIVE}"),
                params![read_only_uri(path)],
            )?;
            // The fact this whole module rests on, checked rather than assumed: a URI the
            // connection did not parse as one would attach a FILE NAMED `file:…?mode=ro`,
            // writable, and every promise above would be false.
            if !conn.is_readonly(LIVE)? {
                return Err(StoreError::Refused(format!(
                    "{} attached writable; refusing to read the live store through it",
                    path.display()
                )));
            }
            let found: i64 = conn.query_row(
                &format!("SELECT MAX(version) FROM {LIVE}.schema_version"),
                [],
                |r| r.get(0),
            )?;
            // The same rule `Store::open` applies, floor and all: a view is a reader, so a store a
            // newer build migrated additively is one this build can still read. Read-only, it can
            // neither record a floor nor repair a missing one.
            let floor = super::floor::read_floor(&conn, LIVE)?;
            if found > SCHEMA_VERSION && !floor.is_some_and(|f| f <= SCHEMA_VERSION) {
                return Err(StoreError::SchemaTooNew {
                    found,
                    known: SCHEMA_VERSION,
                    floor,
                });
            }
        }
        Ok(LiveView { store })
    }

    /// The connection, with the live file attached as `live`. Holding it holds the process's store
    /// lock - do not call another method of this view while holding it: the in-memory store is the
    /// same one connection, so the second call would wait on the lock it already has.
    pub fn connection(&self) -> super::Conn<'_> {
        self.store.connection()
    }

    /// Every transcript id in the live file, in id order.
    pub fn transcripts(&self) -> Result<Vec<String>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(&format!("SELECT id FROM {LIVE}.transcript ORDER BY id"))?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// One transcript, read from the live file in one snapshot and loaded through
    /// [`Store::load_transcript`]. The load runs on the connection this view already holds - a
    /// second call into the store would wait on the very lock the copy is open under.
    pub fn load(&self, transcript_id: &str) -> Result<LoadedTranscript> {
        let conn = self.connection();
        // DEFERRED: the first read of `live` below takes the read snapshot, and every
        // later read in this transaction sees the same one.
        let tx = conn.unchecked_transaction()?;
        copy(
            &conn,
            "transcript",
            "WHERE id = ?1 ORDER BY rowid",
            transcript_id,
        )?;
        copy(
            &conn,
            "stable_prefix",
            &format!("WHERE id = (SELECT stable_prefix_id FROM {LIVE}.transcript WHERE id = ?1)"),
            transcript_id,
        )?;
        copy(
            &conn,
            "transcript_item",
            "WHERE transcript_id = ?1 ORDER BY seq",
            transcript_id,
        )?;
        let loaded = Store::load_transcript_on(&conn, transcript_id);
        drop(tx); // rolls back: nothing accumulates across loads
        loaded
    }
}

/// `INSERT INTO main.table SELECT … FROM live.table <tail>`, over the columns both have  -
/// so a live file one migration behind still copies, with the newer columns defaulted.
fn copy(conn: &Connection, table: &str, tail: &str, arg: &str) -> Result<usize> {
    let cols = |schema: &str| -> Result<Vec<String>> {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1, ?2) ORDER BY cid")?;
        let names = stmt
            .query_map(params![table, schema], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names)
    };
    let live = cols(LIVE)?;
    let shared: Vec<String> = cols("main")?
        .into_iter()
        .filter(|c| live.contains(c))
        .map(|c| format!("\"{c}\""))
        .collect();
    let list = shared.join(", ");
    Ok(conn.execute(
        &format!("INSERT INTO main.{table} ({list}) SELECT {list} FROM {LIVE}.{table} {tail}"),
        params![arg],
    )?)
}

/// `file:<path>?mode=ro`, with the three characters a URI path cannot carry literally
/// escaped.
fn read_only_uri(path: &Path) -> String {
    let mut s = String::from("file:");
    for ch in path.to_string_lossy().chars() {
        match ch {
            '%' => s.push_str("%25"),
            '?' => s.push_str("%3f"),
            '#' => s.push_str("%23"),
            c => s.push(c),
        }
    }
    s.push_str("?mode=ro");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{TokenLedger, hash_tokens};
    use crate::store::{SessionRecord, StablePrefixRecord};
    use letibot_transcript::TranscriptItem;

    /// A scratch dir removed on drop, panic or not.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A store FILE with one two-row transcript in it, written through the ordinary writer,
    /// at a path carrying every character [`read_only_uri`] escapes.
    fn fixture() -> (Scratch, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "letibot-liveview-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let scratch = Scratch(dir.clone());
        let path = dir.join("sessions #1?%20.db");
        let s = Store::open(&path).unwrap();
        let tokens = vec![1u32, 2, 3, 4];
        let prefix_id = s
            .put_stable_prefix(&StablePrefixRecord {
                dialect_sha: "aa".repeat(32),
                system: "you are a harness".into(),
                tools_json: vec![],
                h_init: hash_tokens(&tokens),
                tokens: tokens.clone(),
                vocab_source: "/models/x.gguf".into(),
                effort: None,
            })
            .unwrap();
        s.put_session(&SessionRecord {
            id: "sess-1".into(),
            title: None,
            model_id: "qwen".into(),
            dialect_sha: "aa".repeat(32),
            workspace_root: "/w".into(),
            owner: "deadtrickster".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
        s.put_transcript("tr-1", "sess-1", &prefix_id).unwrap();
        let mut ledger = TokenLedger::new("tr-1", &tokens).unwrap();
        for seq in 0..2u32 {
            let item = TranscriptItem::Assistant {
                text: format!("row {seq}"),
                tool_calls: vec![],
                truncated: false,
            };
            let toks = vec![10 + seq, 20 + seq];
            let row = ledger.append(&format!("it-{seq}"), &toks).unwrap().clone();
            s.append_item("tr-1", seq, &item, &row, &toks).unwrap();
        }
        (scratch, path)
    }

    /// The view loads what the writer wrote, and cannot write back. The load is
    /// compared with `Store::load_transcript` on the file itself; the refusal is SQLite's
    /// own, not a convention; and the file's bytes are the same after the view is gone.
    #[test]
    fn a_view_loads_what_the_writer_wrote_and_cannot_write_to_the_file() {
        let (_scratch, path) = fixture();
        let direct = Store::open(&path).unwrap().load_transcript("tr-1").unwrap();
        let before = std::fs::read(&path).unwrap();

        let view = LiveView::open(&path).expect("a view of the store");
        assert!(view.connection().is_readonly(LIVE).unwrap());
        assert_eq!(view.transcripts().unwrap(), vec!["tr-1".to_string()]);

        let seen = view.load("tr-1").unwrap();
        assert_eq!(seen.ledger_input(), direct.ledger_input());
        assert_eq!(seen.prefix_tokens, direct.prefix_tokens);
        assert_eq!(seen.h_init, direct.h_init);
        assert_eq!(seen.session_id, direct.session_id);
        // A second load of the same transcript works: the first one left nothing behind.
        assert_eq!(view.load("tr-1").unwrap().items.len(), 2);
        assert!(matches!(view.load("nope"), Err(StoreError::NotFound(_))));

        let err = view
            .connection()
            .execute_batch("CREATE TABLE live.scribble (x)")
            .expect_err("a write through the view must be refused");
        assert!(err.to_string().contains("readonly"), "{err}");
        drop(view);

        assert_eq!(std::fs::read(&path).unwrap(), before, "the file changed");
    }
}
