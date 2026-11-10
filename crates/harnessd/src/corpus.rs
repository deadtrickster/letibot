//! Where the gate's decisions go so they outlive the daemon. > *"we should assemble my
//! decisions and model decisions into corpus across our > letibot/leticode runs.
//!
//! The corpus is one file per project (§ The corpus, `docs/store-split.md`): the sink here
//! writes this daemon's own project file, and the sweep below reads every one of them.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::split::PROJECTS_DIR;
use letibot_tokencore::store::{CorpusCounts, Store, StoredAdjudication};
use letibot_tools::{CorpusRow, CorpusSink, OperatorOverride};

pub struct StoreCorpus {
    store: Mutex<Store>,
    /// Decisions this sink could not write. Read by the disclosure: a corpus that
    /// is quietly dropping rows must not read as a corpus that is being kept.
    lost: AtomicU64,
    /// Rulings that found no row. Counted separately because it means something
    /// different - the decision was never written, or it belongs to a store this
    /// daemon is not the one holding.
    orphan_rulings: AtomicU64,
}

impl StoreCorpus {
    /// Take the session store's handle, for the gate's lifetime. The corpus is a project file's,
    /// so a set's handle is asked for the table it reads.
    pub fn open(
        at: impl Into<crate::config::StoreAt>,
    ) -> Result<Self, letibot_tokencore::store::StoreError> {
        let at: crate::config::StoreAt = at.into();
        Ok(StoreCorpus {
            store: Mutex::new(at.open_existing()?),
            lost: AtomicU64::new(0),
            orphan_rulings: AtomicU64::new(0),
        })
    }

    /// What the corpus holds, and how much of it did not arrive. Reported together because a
    /// count shown without its losses is the denominator defect in the one place it matters
    /// most: a corpus quietly dropping rows must not read as one that is being kept.
    pub fn counts(&self) -> (letibot_tokencore::store::CorpusCounts, u64, u64) {
        let c = self
            .store
            .lock()
            .ok()
            .and_then(|s| s.corpus_counts().ok())
            .unwrap_or_default();
        (
            c,
            self.lost.load(Ordering::Relaxed),
            self.orphan_rulings.load(Ordering::Relaxed),
        )
    }
}

impl CorpusSink for StoreCorpus {
    /// From the table, so a resumed session's gate carries on numbering where the last daemon
    /// stopped. Unreadable store, or a session with no rows yet, is `0` - the same answer a
    /// fresh session gives, and the honest one when nothing can be read.
    fn decisions_recorded(&self, session_id: &str) -> u64 {
        self.store
            .lock()
            .ok()
            .and_then(|s| s.max_adjudication_seq(session_id).ok())
            .unwrap_or(0)
    }

    fn decided(&self, row: &CorpusRow) {
        // The trail is stored as JSON rather than as `AuthorisationTrail::render`'s prose: the
        // prose is what the *model* was shown and is already in `shown`, and a corpus reader that
        // wants to re-render it under a changed brief format needs the structure.
        let trail_json = serde_json::to_string(&row.trail)
            .unwrap_or_else(|e| format!(r#"{{"unserialisable":{}}}"#, json_string(&e.to_string())));
        // Same rule for the arguments: a value that will not serialise is recorded
        // as that, never as `{}`. An empty argument object is a real call shape.
        let arguments_json = serde_json::to_string(&row.arguments)
            .unwrap_or_else(|e| format!(r#"{{"unserialisable":{}}}"#, json_string(&e.to_string())));
        let options_json = serde_json::to_string(&row.options).unwrap_or_else(|_| "[]".into());

        let rec = letibot_tokencore::store::NewAdjudication {
            request_id: row.request_id.clone(),
            session_id: row.session_id.clone(),
            turn_id: row.turn_id.clone(),
            action: row.action.clone(),
            baseline: row.baseline.clone(),
            tier: row.tier.to_string(),
            trail_json,
            shown: row.shown.clone(),
            tool: row.tool.clone(),
            arguments_json,
            mode: row.mode.clone(),
            options_json,
            agent: row.agent.clone(),
            model_verdict: row.model_verdict.clone(),
            verdict: row.verdict.clone(),
            verdict_by: row.verdict_by.clone(),
            verdict_basis: row.verdict_basis.clone(),
            p_allow: row.p_allow,
            oracle_ms: Some(row.decision_ms as i64),
            // The oracle's own id is inside `verdict_by` (`model:<id>`) and is not separately reported
            // today. Left NULL rather than parsed out of that string: a column filled by splitting
            // prose is the column that breaks when the prose changes.
            oracle_model: None,
            // Carried from the row rather than re-derived: the key a decision is
            // recorded under and the key a later call is looked up by must be one
            // derivation.
            shape: row.shape.clone(),
            shape_class: row.shape_class.clone(),
            brief_sha: Some(row.brief_format.to_string()),
            // The flag the counts read. `Row::consulted` is
            // `ModelAdvice::consulted`, so this column is the fact rather than a regex
            // over prose - and `CorpusCounts::measured` counts it.
            consulted: Some(row.consulted),
            // 's label, beside the flag: which of the four `Unsure`s the answer was
            // so *how often does the guard run out of room* is a `GROUP BY` and not a regex
            // over `model_verdict`.
            oracle_reading: row.oracle_reading.map(str::to_string),
            reply: row.reply.clone(),
            effect: row.effect.to_string(),
            asked: row.asked,
        };

        // Written, not merely attempted. `record_adjudication` is `INSERT OR IGNORE`, so a key
        // that is already there answers `Ok(0)`; reading only `is_ok` counted that as kept and
        // is how a whole session's corpus went missing without `lost` ever moving.
        let ok = self
            .store
            .lock()
            .ok()
            .and_then(|s| s.record_adjudication(&rec).ok())
            .unwrap_or(false);
        if !ok {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // At `supervised` the ruling arrives WITH the decision, because the person answered
        // this call with the model's verdict in front of them. Two statements rather than an
        // eleven-argument insert with two more optional columns
        if let Some(what) = &row.operator {
            self.ruled(&row.request_id, what);
        }
    }

    fn ruled(&self, request_id: &str, what: &OperatorOverride) {
        let note = match what {
            OperatorOverride::Granted { note }
            | OperatorOverride::Upheld { note }
            | OperatorOverride::Revoked { note } => note.as_str(),
        };
        let found = self
            .store
            .lock()
            .ok()
            .and_then(|s| {
                s.record_operator_ruling(request_id, what.as_str(), note)
                    .ok()
            })
            .unwrap_or(false);
        if !found {
            self.orphan_rulings.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Every file a whole-corpus question reads, in key order (§ The corpus): a data directory's
/// `projects/*.db`, or the one file of a fixture's single-file store. Ordered by key so a listing
/// cut short is a stable prefix.
pub fn project_files(at: &crate::config::StoreAt) -> Vec<std::path::PathBuf> {
    let dir = match at {
        crate::config::StoreAt::File(path) => return vec![path.clone()],
        crate::config::StoreAt::Set { dir, .. } => dir,
    };
    let mut files = Vec::new();
    // Mid-transition, the project tables of every key not carved out of `sessions.db` yet are
    // still in it, so a sweep that read only `projects/` would leave those decisions out.
    let legacy = dir.join(letibot_tokencore::store::split::LEGACY_FILE);
    if legacy.is_file()
        && !dir
            .join(letibot_tokencore::store::split::SHARED_FILE)
            .is_file()
    {
        files.push(legacy);
    }
    let Ok(entries) = std::fs::read_dir(dir.join(PROJECTS_DIR)) else {
        return files;
    };
    files.extend(
        entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "db")),
    );
    files.sort();
    files
}

/// The corpus's counts, summed over every file, with the files that would not open NAMED. The
/// sweep is the accepted cost (§ The corpus) and a file it could not read is said, never counted
/// as empty.
pub fn sweep_counts(at: &crate::config::StoreAt) -> (CorpusCounts, Vec<String>) {
    let mut total = CorpusCounts::default();
    let mut unreadable = Vec::new();
    for path in project_files(at) {
        match Store::open_reader(&path).and_then(|s| s.corpus_counts()) {
            Ok(c) => {
                total.total += c.total;
                total.decided_by_operator += c.decided_by_operator;
                total.measured += c.measured;
                total.disagreements += c.disagreements;
                total.model_decided += c.model_decided;
            }
            Err(e) => unreadable.push(named(&path, &e.to_string())),
        }
    }
    (total, unreadable)
}

/// The corpus's newest decisions: each file's newest `per_file`, merged, sorted and cut to `take`,
/// because `last` and `offset` mean one page of one corpus and not a page per file.
pub fn sweep_recent(
    at: &crate::config::StoreAt,
    per_file: usize,
    take: usize,
) -> (Vec<StoredAdjudication>, Vec<String>) {
    let mut rows = Vec::new();
    let mut unreadable = Vec::new();
    for path in project_files(at) {
        match Store::open_reader(&path).and_then(|s| s.corpus(false, per_file)) {
            Ok(mut some) => rows.append(&mut some),
            Err(e) => unreadable.push(named(&path, &e.to_string())),
        }
    }
    rows.sort_by(|a, b| {
        b.decided_ms
            .cmp(&a.decided_ms)
            .then_with(|| b.request_id.cmp(&a.request_id))
    });
    rows.truncate(take);
    (rows, unreadable)
}

/// The one file a scoped question reads (§ The corpus).
pub enum Scoped {
    /// This daemon's own project file: the handle already holds the connection.
    Own,
    /// Another folder's project file, which the sweep's read-only door opens.
    Elsewhere(std::path::PathBuf),
}

/// Which project file a session's decisions are in, resolved through the shared index alone. The
/// key the migration recorded when there is one, and otherwise the folder hash of the tree's ROOT
/// workspace - a child's own workspace is its worktree and names no file (§ Migration 1).
pub fn session_file(
    at: &crate::config::StoreAt,
    index: &Store,
    session: &str,
) -> Result<Scoped, String> {
    let crate::config::StoreAt::Set { dir, key } = at else {
        // A single-file store is one file: there is nothing to resolve and nothing to sweep.
        return Ok(Scoped::Own);
    };
    let root = root_of(index, session).ok_or_else(|| {
        format!(
            "session `{session}` is not in the index at {}, so no project file's rows are its",
            at.display()
        )
    })?;
    let want = recorded_project(index, &root).unwrap_or_else(|| project_key(&root.workspace_root));
    // `file_of` answers `sessions.db` for a key no carve has taken yet, which is where its rows
    // still are - the daemon's own key is the same question asked about itself.
    let path = letibot_tokencore::store::split::file_of(dir, &want, "adjudication");
    if want.eq_ignore_ascii_case(key) {
        return match path.is_file() {
            true => Ok(Scoped::Own),
            false => Err(format!(
                "session `{session}` is in this daemon's project {key}, and {} is not there - \
                 the file its decisions would be read from",
                path.display()
            )),
        };
    }
    match path.is_file() {
        true => Ok(Scoped::Elsewhere(path)),
        false => Err(format!(
            "session `{session}` is in project {want}, and {} is not there - the file its \
             decisions would be read from",
            path.display()
        )),
    }
}

/// The session at the top of this one's tree, from the index's `parent_session_id`. A parent the
/// index does not hold stops the walk, which is the answer a resume gives that child too.
fn root_of(index: &Store, session: &str) -> Option<letibot_tokencore::store::StoredSession> {
    let mut here = index.session(session).ok().flatten()?;
    for _ in 0..64 {
        let Some(parent) = here.parent_session_id.clone() else {
            return Some(here);
        };
        let Some(up) = index.session(&parent).ok().flatten() else {
            return Some(here);
        };
        here = up;
    }
    Some(here)
}

/// The key the migration recorded for a session: `session.project`. `None` for a session written
/// since the split, which `put_session` records without it, and for a store from before P2.
fn recorded_project(
    index: &Store,
    session: &letibot_tokencore::store::StoredSession,
) -> Option<String> {
    let conn = index.connection_of("session");
    conn.query_row(
        "SELECT project FROM session WHERE id = ?1",
        [&session.id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// A file a sweep could not read, as the answer names it: the path, and why.
fn named(path: &Path, why: &str) -> String {
    format!("{} ({why})", path.display())
}

/// A JSON string literal, for the one place above that builds JSON by hand.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod shape_cache_tests {
    use super::*;
    use letibot_tools::adjudicate::{
        AdjudicatedGate, AdjudicationDecision, AdjudicationRequest, AskAdjudicator,
    };
    use letibot_tools::runtime::{Gate, GateCall, GateDecision};
    use letibot_tools::schema::Access;

    /// The whole loop, end to end: approve once, restart, do not be asked again. Every
    /// piece of this was unit-tested separately and the operator still had a session that asked
    /// them about the same `grep` after a restart - because the pieces were never joined up.
    #[test]
    fn a_shape_approved_before_a_restart_survives_it() {
        let dir = std::env::temp_dir().join(format!("letibot-shape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("sessions.db");

        let session = "s-shape-1";
        let workspace = "/w";
        {
            let s = Store::open(&path).expect("store");
            s.put_session(&letibot_tokencore::store::SessionRecord {
                id: session.into(),
                title: None,
                model_id: "m".into(),
                dialect_sha: "d".into(),
                workspace_root: workspace.into(),
                owner: "dead".into(),
                approvers: vec![],
                role: None,
                parent_session_id: None,
            })
            .expect("session");
        }

        let sink = std::sync::Arc::new(StoreCorpus::open(path.clone()).expect("corpus"));

        // Session one: the operator is asked, and says yes.
        let asked_first = {
            let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let a = asked.clone();
            let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
                "human:dead",
                move |req: &AdjudicationRequest| {
                    a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Some(AdjudicationDecision::selected(
                        req,
                        "allow_once",
                        "human:dead",
                        "yes",
                    ))
                },
            )))
            .with_identity(session, "dead")
            .with_surroundings(surroundings(workspace))
            .with_corpus_sink(std::sync::Arc::clone(&sink) as std::sync::Arc<dyn CorpusSink>);

            // Exec, so that asking is what this measures : a `grep` over the
            // workspace is a read, clause 4 settles it with nobody asked, and a run
            // through this test would then be green about a prompt that never happened.
            let args = serde_json::json!({"command": "sed -n 1p /w/a.rs"});
            assert!(matches!(
                g.admit(&call(&args, workspace)),
                GateDecision::Admit
            ));
            asked.load(std::sync::atomic::Ordering::Relaxed)
        };
        assert_eq!(asked_first, 1, "the first time must ask");

        // The daemon's own warm start, from the store the sink just wrote to.
        let warm = {
            let s = Store::open(&path).expect("reopen");
            s.approved_shapes(workspace).expect("shapes")
        };
        assert_eq!(
            warm.len(),
            1,
            "the approval did not reach the store: {warm:?}"
        );

        // Session two: a different pattern, count and file - one shape.
        let asked_second = {
            let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let a = asked.clone();
            let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
                "human:dead",
                move |req: &AdjudicationRequest| {
                    a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Some(AdjudicationDecision::selected(
                        req,
                        "allow_once",
                        "human:dead",
                        "yes",
                    ))
                },
            )))
            .with_identity("s-shape-2", "dead")
            .with_surroundings(surroundings(workspace));
            assert_eq!(g.seed_shapes(warm), 1);

            let args = serde_json::json!({"command": "sed -n 2p /w/b.rs"});
            assert!(matches!(
                g.admit(&call(&args, workspace)),
                GateDecision::Admit
            ));
            asked.load(std::sync::atomic::Ordering::Relaxed)
        };
        assert_eq!(
            asked_second, 0,
            "the operator was asked again about a shape they approved before the restart"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn surroundings(workspace: &str) -> letibot_tools::intent::Surroundings {
        letibot_tools::intent::Surroundings {
            // Replaying recorded rows: the scratch of the session that wrote them
            // is long gone, and inventing this daemon's would classify an old path
            // by a directory that did not exist when the row was made.
            scratch: None,
            home: Some("/home/dead".into()),
            workspace: Some(workspace.into()),
            shell: letibot_tools::intent::ShellTrust::Pinned {
                how: "test fixture".into(),
            },
            seen_hosts: Default::default(),
            // A replay of recorded rows: nothing observed an environment, and the disk a
            // bare name would have run in is not this one.
            session: Default::default(),
            programs: Default::default(),
            shell_path: None,
        }
    }

    fn call<'a>(args: &'a serde_json::Value, workspace: &'a str) -> GateCall<'a> {
        GateCall {
            name: "bash",
            access: Access::Exec,
            args,
            turn_id: "t#1",
            call_id: "c1",
            workspace,
            target_exists: None,
            scripts: &[],
        }
    }
}
