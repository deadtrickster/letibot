//! The subagents a session started, as rows that outlive the daemon, so a restart can list the
//! children it owes an answer to. The window is the oldest record this store holds: before it a
//! missing row says nothing.

use letibot_transcript::TranscriptItem;
use rusqlite::{OptionalExtension, params};

use super::{Result, Store, now_ms};

pub const SUBAGENTS_SQL: &str = r#"
-- **The subagents a session started (schema 26)** — one row per child, see
-- `store::subagents` for what each state means. `spec_json` is the spawn's own description
-- (role, downgrade, placement, model, worktree, VM source and check) as the daemon wrote it,
-- opaque to the store. No foreign key to `session`: the row is written BEFORE the child's
-- session row exists, because a daemon that dies while the child is opening must still know
-- it had one.
CREATE TABLE IF NOT EXISTS subagent_run (
    child_id    TEXT PRIMARY KEY,
    parent_id   TEXT NOT NULL,
    state       TEXT NOT NULL,
    prompt      TEXT NOT NULL,
    spec_json   TEXT NOT NULL,
    instance    TEXT NOT NULL,
    resumes     INTEGER NOT NULL DEFAULT 0,
    outcome     TEXT,
    later_answer TEXT,
    delivered   INTEGER NOT NULL DEFAULT 0,
    started_ms  INTEGER NOT NULL,
    updated_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS subagent_run_parent ON subagent_run(parent_id);

-- **The monitors a session declared (schema 26)**, so a restart can re-arm them. `args_json` is
-- the `monitor` call's own arguments, verbatim — re-arming is that call made again. `live` is 0
-- once the monitor retired, fired for the last time or expired, as the session last saw it.
CREATE TABLE IF NOT EXISTS session_monitor (
    session_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    args_json   TEXT NOT NULL,
    instance    TEXT NOT NULL,
    ttl_ms      INTEGER NOT NULL,
    live        INTEGER NOT NULL,
    declared_ms INTEGER NOT NULL,
    updated_ms  INTEGER NOT NULL,
    PRIMARY KEY (session_id, name)
);

-- **What the `job` row does not hold about a background job (schema 26)**: the name the caller
-- gave it, the directory it ran in, and the daemon that started it — what a reminder after a
-- restart needs to say how to run it again.
CREATE TABLE IF NOT EXISTS job_context (
    session_id  TEXT NOT NULL,
    handle      TEXT NOT NULL,
    slug        TEXT,
    cwd         TEXT NOT NULL,
    instance    TEXT NOT NULL,
    started_ms  INTEGER NOT NULL,
    PRIMARY KEY (session_id, handle)
);

-- **Each daemon's life (schema 26)**: when it started, its last heartbeat, and — after an orderly
-- stop — when it stopped. How long a session's daemon was down is read from here.
CREATE TABLE IF NOT EXISTS daemon_life (
    instance    TEXT PRIMARY KEY,
    started_ms  INTEGER NOT NULL,
    beat_ms     INTEGER NOT NULL,
    stopped_ms  INTEGER
);
"#;

/// One declared monitor, as the store remembers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMonitor {
    pub session_id: String,
    pub name: String,
    pub args_json: String,
    pub instance: String,
    pub ttl_ms: u64,
    pub live: bool,
    pub declared_ms: u64,
    pub updated_ms: u64,
}

/// A background job's context - see the `job_context` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobContext {
    pub handle: String,
    pub slug: Option<String>,
    pub cwd: String,
    pub instance: String,
    pub started_ms: u64,
}

/// What the store can say about a child it holds no `subagent_run` row for: the two classes,
/// and the reason the window exists at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRecord {
    /// Created at or after this store's first `subagent_run` row - the store was recording
    /// children by then, so this child's row was due and is missing.
    Due,
    /// Created before it: no build was recording children then, so having no row is the ordinary
    /// state and says nothing about a write.
    BeforeTheStore,
}

/// A child the store names and the subagent record does not: a spawn whose row is missing, or
/// a child older than the record itself.
#[derive(Debug, Clone, PartialEq)]
pub struct UnrecordedChild {
    pub id: String,
    pub title: Option<String>,
    pub role: Option<String>,
    /// Where it worked: its worktree, or its parent's tree.
    pub workspace_root: String,
    pub created_ms: i64,
    /// Which of the two classes this child is in - see [`NoRecord`]. A sentence a model reads
    /// may say *why* the row is missing only for [`NoRecord::Due`].
    pub why: NoRecord,
    /// The current transcript's last row, by [`StoredSession::last_item`]'s rule - an
    /// `Assistant` with no calls after it is a finished turn, anything else is a turn that was
    /// still going when the rows stopped. `None` for a child that wrote no row at all.
    pub last_item: Option<letibot_transcript::TranscriptItem>,
}

/// One daemon's life - see the `daemon_life` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonLife {
    pub instance: String,
    pub started_ms: u64,
    pub beat_ms: u64,
    pub stopped_ms: Option<u64>,
}

/// One child, as the store remembers it - see the module docs for each field's meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentRun {
    pub child_id: String,
    pub parent_id: String,
    pub state: String,
    /// The subtask, verbatim - what the child was asked, which a respawn needs and which the
    /// child's own transcript holds too.
    pub prompt: String,
    pub spec_json: String,
    pub instance: String,
    pub resumes: u32,
    /// What the child's FIRST turn said, kept where every reader that wants `task_result`'s
    /// original answer has always found it.
    pub outcome: Option<String>,
    /// The newest answer of a turn the child ran after that one - a settlement of its own,
    /// which a parent's watcher hands over separately. `None` for a child that answered once.
    pub later_answer: Option<String>,
    pub delivered: bool,
    pub started_ms: u64,
    pub updated_ms: u64,
}

impl Store {
    /// A child has been started (or brought back): its row, replacing any earlier one for
    /// the same handle. `resumes` and `instance` are the caller's.
    pub fn put_subagent_run(&self, run: &SubagentRun) -> Result<()> {
        self.exec(
            "INSERT INTO subagent_run
               (child_id, parent_id, state, prompt, spec_json, instance, resumes, outcome,
                later_answer, delivered, started_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(child_id) DO UPDATE SET
               parent_id = ?2, state = ?3, prompt = ?4, spec_json = ?5, instance = ?6,
               resumes = ?7, outcome = ?8, later_answer = ?9, delivered = ?10, updated_ms = ?12",
            params![
                run.child_id,
                run.parent_id,
                run.state,
                run.prompt,
                run.spec_json,
                run.instance,
                run.resumes as i64,
                run.outcome,
                run.later_answer,
                run.delivered as i64,
                run.started_ms as i64,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// A child's state moved: `state` and `outcome`, and `delivered` back to false. A handle with
    /// no row is a child this store never recorded, and nothing is written. A later answer standing
    /// before this settlement is dropped so no reader hands out the older turn.
    pub fn settle_subagent_run(
        &self,
        child_id: &str,
        state: &str,
        outcome: Option<&str>,
    ) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET state = ?2, outcome = ?3, later_answer = NULL,
                    delivered = 0, updated_ms = ?4
              WHERE child_id = ?1",
            params![child_id, state, outcome, now_ms()],
        )?;
        Ok(())
    }

    /// A turn the child ran after its first answered `answer`: the row keeps it beside `outcome`
    /// (the first answer), and `delivered` goes back to false. The newest later turn replaces the
    /// previous; a handle with no row is not written.
    pub fn settle_subagent_run_again(&self, child_id: &str, answer: &str) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET later_answer = ?2, delivered = 0, updated_ms = ?3
              WHERE child_id = ?1",
            params![child_id, answer, now_ms()],
        )?;
        Ok(())
    }

    /// An orderly stop interrupted this child: it stays `running`, so the next open
    /// resumes it, and its crash count goes back to zero - an orderly stop is not a crash.
    pub fn interrupt_subagent_run(&self, child_id: &str) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET state = 'running', resumes = 0, updated_ms = ?2
              WHERE child_id = ?1 AND state = 'running'",
            params![child_id, now_ms()],
        )?;
        Ok(())
    }

    /// A settled child has started another turn: a parent's `task_message`, a later prompt, a
    /// wake or its plan-check, served on the child's own thread after its first task ended.
    pub fn reopen_subagent_run(&self, child_id: &str) -> Result<Option<String>> {
        let Some(run) = self.subagent_run(child_id)? else {
            return Ok(None);
        };
        if !matches!(run.state.as_str(), "done" | "failed") {
            return Ok(None);
        }
        let moved = self.exec(
            "UPDATE subagent_run SET state = 'running', resumes = 0, updated_ms = ?3
              WHERE child_id = ?1 AND state = ?2",
            params![child_id, run.state, now_ms()],
        )?;
        Ok((moved > 0).then_some(run.state))
    }

    /// A later turn of a settled child ended on its own (not by the daemon stopping): the row
    /// goes back to the state it had before [`Store::reopen_subagent_run`] moved it. Only a row
    /// still `running` is touched - one a stop, a kill or a restart has moved since is theirs.
    pub fn rest_subagent_run(&self, child_id: &str, state: &str) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET state = ?2, updated_ms = ?3
              WHERE child_id = ?1 AND state = 'running'",
            params![child_id, state, now_ms()],
        )?;
        Ok(())
    }

    /// The parent's model has been handed this child's settlement.
    pub fn deliver_subagent_run(&self, child_id: &str) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET delivered = 1, updated_ms = ?2 WHERE child_id = ?1",
            params![child_id, now_ms()],
        )?;
        Ok(())
    }

    /// One child's row.
    pub fn subagent_run(&self, child_id: &str) -> Result<Option<SubagentRun>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{SELECT_RUN} WHERE child_id = ?1"),
                params![child_id],
                run_of,
            )
            .optional()?)
    }

    /// Every child any session started, oldest first - what the daemon's session list reads
    /// the children's names from (their slugs are in `spec_json`, the spawn's own description).
    pub fn subagent_runs(&self) -> Result<Vec<SubagentRun>> {
        let conn = self.at("subagent_run");
        let mut st = conn.prepare(&format!("{SELECT_RUN} ORDER BY started_ms, child_id"))?;
        let rows = st.query_map([], run_of)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every child this session started, oldest first - what a parent reads when it is
    /// opened.
    pub fn subagent_runs_of(&self, parent_id: &str) -> Result<Vec<SubagentRun>> {
        let conn = self.at("subagent_run");
        let mut st = conn.prepare(&format!(
            "{SELECT_RUN} WHERE parent_id = ?1 ORDER BY started_ms, child_id"
        ))?;
        let rows = st.query_map(params![parent_id], run_of)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// When this store began recording children: the oldest `subagent_run` row's start, or `None`
    /// for a store that never did. This is the window a missing row is read against.
    pub fn first_subagent_run_ms(&self) -> Result<Option<u64>> {
        let at: Option<i64> = self.at("subagent_run").query_row(
            "SELECT MIN(started_ms) FROM subagent_run",
            [],
            |r| r.get(0),
        )?;
        Ok(at.map(|v| v as u64))
    }

    /// The children of `parent_id` the store names and no `subagent_run` row does, newest first:
    /// the ones whose record was DUE, created at or after the store's first record.
    pub fn children_without_runs(&self, parent_id: &str) -> Result<Vec<UnrecordedChild>> {
        self.unrecorded_of(parent_id, None, true)
    }

    /// [`Store::children_without_runs`] for one id: `None` unless it is a child of `parent_id`
    /// the record does not hold. Keyed, so reading one child does not list its siblings.
    pub fn child_without_run(
        &self,
        parent_id: &str,
        child_id: &str,
    ) -> Result<Option<UnrecordedChild>> {
        Ok(self
            .unrecorded_of(parent_id, Some(child_id), false)?
            .into_iter()
            .next())
    }

    /// The one read both doors take: every child of `parent_id` the store names and no
    /// `subagent_run` row does, each carrying the class that says why the row is missing.
    fn unrecorded_of(
        &self,
        parent_id: &str,
        child_id: Option<&str>,
        due_only: bool,
    ) -> Result<Vec<UnrecordedChild>> {
        // `None` is "nothing is due", not "everything is": a store that never recorded a child
        // is bound as `i64::MAX`, which puts every child before the window.
        let since = match self.first_subagent_run_ms()? {
            // `u64` past `i64::MAX` wraps NEGATIVE, which would make the window everything rather
            // than nothing.
            Some(at) => i64::try_from(at).unwrap_or(i64::MAX),
            None => i64::MAX,
        };
        // The children are the index's and the record of which have a run is this file's, so the
        // one query the single-file store could write is two reads here: the recorded ids first,
        // then the children the index names that are not among them.
        let recorded: std::collections::HashSet<String> = {
            let conn = self.at("subagent_run");
            let mut st = conn.prepare("SELECT child_id FROM subagent_run")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        // The last row is read with the same "newest transcript, then highest seq" rule
        // `sessions_matching` uses, and for the same reason: a compaction fork is written in the
        // same millisecond as the transcript it forked from, so the rowid is what breaks the tie.
        //
        // The rows are COLLECTED before anything is read from another file: on a single-file store
        // the two reads are one connection, and a second guard on it, taken while the first is
        // held, is a deadlock rather than a wait.
        let mut children: Vec<UnrecordedChild> = {
            let conn = self.conn();
            let mut st = conn.prepare(
                "SELECT s.id, s.title, s.role, s.workspace_root, s.created_at,
                        CASE WHEN s.created_at >= ?2 THEN 1 ELSE 0 END
                   FROM session s
                  WHERE s.parent_session_id = ?1
                    -- the same `?2` the CASE reads: the list keeps only the children a row was due
                    -- for, the keyed read takes whichever class the child it was asked about is in
                    AND (?3 = 1 OR s.created_at >= ?2)
                    AND (?4 IS NULL OR s.id = ?4)
                  ORDER BY s.created_at DESC, s.rowid DESC",
            )?;
            let rows = st.query_map(
                params![parent_id, since, if due_only { 0i64 } else { 1 }, child_id],
                |r| {
                    let created_ms: i64 = r.get(4)?;
                    Ok(UnrecordedChild {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        role: r.get(2)?,
                        workspace_root: r.get(3)?,
                        created_ms,
                        why: if r.get::<_, i64>(5)? != 0 {
                            NoRecord::Due
                        } else {
                            NoRecord::BeforeTheStore
                        },
                        last_item: None,
                    })
                },
            )?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };
        let mut out = Vec::new();
        for mut child in children.drain(..) {
            if recorded.contains(&child.id) {
                continue;
            }
            child.last_item = self.last_item_of(&child.id)?;
            out.push(child);
        }
        Ok(out)
    }

    /// The newest non-offer row of a session's newest transcript: what a list shows of a
    /// conversation it is not opening, and the one read [`Store::unrecorded_of`] needs twice.
    fn last_item_of(&self, session_id: &str) -> Result<Option<TranscriptItem>> {
        let conn = self.at("transcript_item");
        let json: Option<String> = conn
            .query_row(
                "SELECT i.item_json FROM transcript_item i
                  WHERE i.transcript_id = (SELECT t.id FROM transcript t
                      WHERE t.session_id = ?1
                      ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1)
                    AND i.kind <> 'offer'
                  ORDER BY i.seq DESC LIMIT 1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }
}

impl Store {
    /// A monitor was declared (or renewed): its row, live, owned by `instance`.
    pub fn put_monitor(
        &self,
        session_id: &str,
        name: &str,
        args_json: &str,
        instance: &str,
        ttl_ms: u64,
    ) -> Result<()> {
        let now = now_ms();
        self.exec(
            "INSERT INTO session_monitor
               (session_id, name, args_json, instance, ttl_ms, live, declared_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)
             ON CONFLICT(session_id, name) DO UPDATE SET
               args_json = ?3, instance = ?4, ttl_ms = ?5, live = 1, declared_ms = ?6,
               updated_ms = ?6",
            params![session_id, name, args_json, instance, ttl_ms as i64, now],
        )?;
        Ok(())
    }

    /// A monitor is no longer live - retired, settled, expired, or reported after a restart.
    pub fn end_monitor(&self, session_id: &str, name: &str) -> Result<()> {
        self.exec(
            "UPDATE session_monitor SET live = 0, updated_ms = ?3
              WHERE session_id = ?1 AND name = ?2",
            params![session_id, name, now_ms()],
        )?;
        Ok(())
    }

    /// Every monitor row of a session, live or not, by name.
    pub fn monitors_of(&self, session_id: &str) -> Result<Vec<StoredMonitor>> {
        let conn = self.at("session_monitor");
        let mut st = conn.prepare(
            "SELECT session_id, name, args_json, instance, ttl_ms, live, declared_ms, updated_ms
               FROM session_monitor WHERE session_id = ?1 ORDER BY name",
        )?;
        let rows = st.query_map(params![session_id], |r| {
            Ok(StoredMonitor {
                session_id: r.get(0)?,
                name: r.get(1)?,
                args_json: r.get(2)?,
                instance: r.get(3)?,
                ttl_ms: r.get::<_, i64>(4)? as u64,
                live: r.get::<_, i64>(5)? != 0,
                declared_ms: r.get::<_, i64>(6)? as u64,
                updated_ms: r.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// A background job's context, written when it is backgrounded.
    pub fn put_job_context(&self, session_id: &str, ctx: &JobContext) -> Result<()> {
        self.exec(
            "INSERT INTO job_context (session_id, handle, slug, cwd, instance, started_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(session_id, handle) DO UPDATE SET
               slug = ?3, cwd = ?4, instance = ?5, started_ms = ?6",
            params![
                session_id,
                ctx.handle,
                ctx.slug,
                ctx.cwd,
                ctx.instance,
                ctx.started_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn job_context(&self, session_id: &str, handle: &str) -> Result<Option<JobContext>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT handle, slug, cwd, instance, started_ms FROM job_context
                  WHERE session_id = ?1 AND handle = ?2",
                params![session_id, handle],
                |r| {
                    Ok(JobContext {
                        handle: r.get(0)?,
                        slug: r.get(1)?,
                        cwd: r.get(2)?,
                        instance: r.get(3)?,
                        started_ms: r.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .optional()?)
    }

    /// A daemon started: its row, with the heartbeat at the start.
    pub fn life_started(&self, instance: &str, at_ms: u64) -> Result<()> {
        self.exec(
            "INSERT INTO daemon_life (instance, started_ms, beat_ms, stopped_ms)
             VALUES (?1, ?2, ?2, NULL)
             ON CONFLICT(instance) DO UPDATE SET started_ms = ?2, beat_ms = ?2, stopped_ms = NULL",
            params![instance, at_ms as i64],
        )?;
        Ok(())
    }

    /// The daemon is alive at `at_ms`.
    pub fn life_beat(&self, instance: &str, at_ms: u64) -> Result<()> {
        self.exec(
            "UPDATE daemon_life SET beat_ms = ?2 WHERE instance = ?1",
            params![instance, at_ms as i64],
        )?;
        Ok(())
    }

    /// The daemon stopped in an orderly way at `at_ms`.
    pub fn life_stopped(&self, instance: &str, at_ms: u64) -> Result<()> {
        self.exec(
            "UPDATE daemon_life SET beat_ms = ?2, stopped_ms = ?2 WHERE instance = ?1",
            params![instance, at_ms as i64],
        )?;
        Ok(())
    }

    /// Every daemon's life this store has seen, so a reader can ask whether a daemon other than
    /// this one ever stopped here, which is what makes a restart a fact rather than a guess.
    pub fn lives(&self) -> Result<Vec<DaemonLife>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT instance, started_ms, beat_ms, stopped_ms FROM daemon_life ORDER BY started_ms",
        )?;
        let rows = st.query_map([], |r| {
            Ok(DaemonLife {
                instance: r.get(0)?,
                started_ms: r.get::<_, i64>(1)? as u64,
                beat_ms: r.get::<_, i64>(2)? as u64,
                stopped_ms: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn life(&self, instance: &str) -> Result<Option<DaemonLife>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT instance, started_ms, beat_ms, stopped_ms FROM daemon_life
                  WHERE instance = ?1",
                params![instance],
                |r| {
                    Ok(DaemonLife {
                        instance: r.get(0)?,
                        started_ms: r.get::<_, i64>(1)? as u64,
                        beat_ms: r.get::<_, i64>(2)? as u64,
                        stopped_ms: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    })
                },
            )
            .optional()?)
    }
}

const SELECT_RUN: &str = "SELECT child_id, parent_id, state, prompt, spec_json, instance, \
                          resumes, outcome, later_answer, delivered, started_ms, updated_ms \
                          FROM subagent_run";

fn run_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<SubagentRun> {
    Ok(SubagentRun {
        child_id: r.get(0)?,
        parent_id: r.get(1)?,
        state: r.get(2)?,
        prompt: r.get(3)?,
        spec_json: r.get(4)?,
        instance: r.get(5)?,
        resumes: r.get::<_, i64>(6)? as u32,
        outcome: r.get(7)?,
        later_answer: r.get(8)?,
        delivered: r.get::<_, i64>(9)? != 0,
        started_ms: r.get::<_, i64>(10)? as u64,
        updated_ms: r.get::<_, i64>(11)? as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(child: &str, parent: &str) -> SubagentRun {
        SubagentRun {
            child_id: child.into(),
            parent_id: parent.into(),
            state: "running".into(),
            prompt: "do the thing".into(),
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

    /// A child whose record was never written is still named by its own session: the second
    /// witness, and the window that keeps it to the children a record was due for.
    #[test]
    fn a_child_with_no_record_is_named_by_its_own_session() {
        use crate::ledger::hash_tokens;
        use letibot_transcript::TranscriptItem;

        let store = Store::open_in_memory().unwrap();
        let tokens = vec![1u32, 2, 3, 4];
        let prefix = store
            .put_stable_prefix(&crate::store::StablePrefixRecord {
                dialect_sha: "aa".repeat(32),
                system: "you are a harness".into(),
                tools_json: vec![],
                h_init: hash_tokens(&tokens),
                tokens,
                vocab_source: "/models/x.gguf".into(),
                effort: None,
            })
            .unwrap();
        // A child the record holds, two the record does not, and one that is older than the
        // record itself - this store began recording children at 5 000.
        let mut kept = run("s-1-sub-kept", "s-1");
        kept.started_ms = 5_000;
        store.put_subagent_run(&kept).unwrap();
        for (id, workspace) in [
            ("s-1-sub-lost", "/w/agent-lost"),
            ("s-1-sub-answer", "/w/agent-answer"),
            ("s-1-sub-old", "/w/agent-old"),
        ] {
            store
                .put_session(&crate::store::SessionRecord {
                    id: id.into(),
                    title: Some(id.into()),
                    model_id: "qwen".into(),
                    dialect_sha: "aa".repeat(32),
                    workspace_root: workspace.into(),
                    owner: "deadtrickster".into(),
                    role: Some("coder".into()),
                    approvers: vec![],
                    parent_session_id: Some("s-1".into()),
                })
                .unwrap();
            store
                .put_transcript(&format!("t-{id}"), id, &prefix)
                .unwrap();
        }
        // `put_session` stamps `created_at` with the clock, and this child is from before the
        // store recorded anything at all - the state 517 of one session's 573 children are in.
        store
            .connection()
            .execute(
                "UPDATE session SET created_at = 1 WHERE id = ?1",
                ["s-1-sub-old"],
            )
            .unwrap();
        // One of them got as far as an answer; the other wrote no row at all.
        let answer = TranscriptItem::Assistant {
            text: "the sweep is in".into(),
            tool_calls: vec![],
            truncated: false,
        };
        store
            .append_item(
                "t-s-1-sub-answer",
                0,
                &answer,
                &crate::store::LedgerRow {
                    item_id: "a".into(),
                    tok_offset: 4,
                    tok_len: 1,
                    h_k: [1; 32],
                },
                &[9],
            )
            .unwrap();

        assert_eq!(store.first_subagent_run_ms().unwrap(), Some(5_000));
        let all = store.children_without_runs("s-1").unwrap();
        let ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["s-1-sub-answer", "s-1-sub-lost"], "{all:?}");
        assert!(all.iter().all(|c| c.why == NoRecord::Due), "{all:?}");
        assert_eq!(all[0].last_item, Some(answer));
        assert_eq!(all[1].last_item, None, "a child that wrote nothing");
        assert_eq!(all[1].workspace_root, "/w/agent-lost");
        assert_eq!(all[1].role.as_deref(), Some("coder"));

        // The window, and it is ONE rule in both doors: a child that predates this store's first
        // record is not carried by the list, because a list that ignored the window would
        // report every child a store ever held.
        assert!(!ids.contains(&"s-1-sub-old"), "{all:?}");
        let old = store
            .child_without_run("s-1", "s-1-sub-old")
            .unwrap()
            .expect("the keyed read answers for a child older than the record");
        assert_eq!(old.why, NoRecord::BeforeTheStore);

        // Keyed: the record wins, a stranger's id is not this parent's child, and the lost one
        // answers on its own.
        assert!(
            store
                .child_without_run("s-1", "s-1-sub-kept")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .child_without_run("s-2", "s-1-sub-lost")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .child_without_run("s-1", "s-1-sub-lost")
                .unwrap()
                .map(|c| c.id),
            Some("s-1-sub-lost".into())
        );

        // A store that has never recorded a child has no window, and that means NOTHING is
        // due - not that every child it holds is lost. Read the other way it turned a fresh
        // box's whole history into a wall of children carrying a cause nobody measured.
        let fresh = Store::open_in_memory().unwrap();
        assert_eq!(fresh.first_subagent_run_ms().unwrap(), None);
        fresh
            .put_session(&crate::store::SessionRecord {
                id: "s-2-sub-only".into(),
                title: None,
                model_id: "qwen".into(),
                dialect_sha: "aa".repeat(32),
                workspace_root: "/w/agent-only".into(),
                owner: "deadtrickster".into(),
                role: None,
                approvers: vec![],
                parent_session_id: Some("s-2".into()),
            })
            .unwrap();
        assert!(
            fresh.children_without_runs("s-2").unwrap().is_empty(),
            "a store that never recorded a child has no child whose record was due"
        );
        assert_eq!(
            fresh
                .child_without_run("s-2", "s-2-sub-only")
                .unwrap()
                .map(|c| c.why),
            Some(NoRecord::BeforeTheStore)
        );
    }

    /// The record's whole life: started, settled, delivered - and an orderly stop that leaves a
    /// running child running with its crash count reset, but never revives a settled one.
    #[test]
    fn a_child_row_is_started_settled_and_delivered() {
        let store = Store::open_in_memory().unwrap();
        let mut r = run("s-1-sub-1", "s-1");
        r.resumes = 2;
        store.put_subagent_run(&r).unwrap();
        store.put_subagent_run(&run("s-2-sub-1", "s-2")).unwrap();

        let mine = store.subagent_runs_of("s-1").unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].prompt, "do the thing");
        assert_eq!(mine[0].resumes, 2);

        store.interrupt_subagent_run("s-1-sub-1").unwrap();
        let got = store.subagent_run("s-1-sub-1").unwrap().unwrap();
        assert_eq!((got.state.as_str(), got.resumes), ("running", 0));

        store
            .settle_subagent_run("s-1-sub-1", "done", Some("the answer"))
            .unwrap();
        store.interrupt_subagent_run("s-1-sub-1").unwrap();
        let got = store.subagent_run("s-1-sub-1").unwrap().unwrap();
        assert_eq!(
            got.state, "done",
            "an orderly stop never revives a settled child"
        );
        assert_eq!(got.outcome.as_deref(), Some("the answer"));
        assert!(!got.delivered);

        store.deliver_subagent_run("s-1-sub-1").unwrap();
        assert!(store.subagent_run("s-1-sub-1").unwrap().unwrap().delivered);
        assert!(store.subagent_run("nobody").unwrap().is_none());
    }

    /// A later turn of a settled child is in flight, and the row says so - and puts the
    /// settlement back when the turn ends, with the first answer and its delivery untouched. A row
    /// that is not settled is never moved.
    #[test]
    fn a_later_turn_reopens_a_settled_row_and_rests_it_again() {
        let store = Store::open_in_memory().unwrap();
        store.put_subagent_run(&run("s-1-sub-1", "s-1")).unwrap();
        // Still on its first turn: nothing to reopen.
        assert_eq!(store.reopen_subagent_run("s-1-sub-1").unwrap(), None);
        store
            .settle_subagent_run("s-1-sub-1", "done", Some("first answer"))
            .unwrap();
        store.deliver_subagent_run("s-1-sub-1").unwrap();

        assert_eq!(
            store.reopen_subagent_run("s-1-sub-1").unwrap().as_deref(),
            Some("done")
        );
        let got = store.subagent_run("s-1-sub-1").unwrap().unwrap();
        assert_eq!(got.state, "running");
        assert_eq!(got.outcome.as_deref(), Some("first answer"));
        assert!(got.delivered, "the first answer was told and stays told");

        store.rest_subagent_run("s-1-sub-1", "done").unwrap();
        let got = store.subagent_run("s-1-sub-1").unwrap().unwrap();
        assert_eq!((got.state.as_str(), got.delivered), ("done", true));

        // A row moved by something else in between is not put back.
        store.reopen_subagent_run("s-1-sub-1").unwrap();
        store
            .settle_subagent_run("s-1-sub-1", "killed", Some("stopped"))
            .unwrap();
        store.rest_subagent_run("s-1-sub-1", "done").unwrap();
        assert_eq!(
            store.subagent_run("s-1-sub-1").unwrap().unwrap().state,
            "killed"
        );
        assert_eq!(store.reopen_subagent_run("s-1-sub-1").unwrap(), None);
        assert_eq!(store.reopen_subagent_run("nobody").unwrap(), None);
    }

    /// A later turn's answer is kept BESIDE the first one. `outcome` is what the child's first
    /// turn said and a reader that wants it must still find it there, so the later answers go in a
    /// column of their own and the newest one wins.
    #[test]
    fn a_later_answer_is_kept_beside_the_first_one() {
        let store = Store::open_in_memory().unwrap();
        store.put_subagent_run(&run("s-1-sub-1", "s-1")).unwrap();
        store
            .settle_subagent_run("s-1-sub-1", "done", Some("first answer"))
            .unwrap();
        store.deliver_subagent_run("s-1-sub-1").unwrap();

        store
            .settle_subagent_run_again("s-1-sub-1", "second answer")
            .unwrap();
        let got = store.subagent_run("s-1-sub-1").unwrap().unwrap();
        assert_eq!(got.outcome.as_deref(), Some("first answer"));
        assert_eq!(got.later_answer.as_deref(), Some("second answer"));
        assert!(
            !got.delivered,
            "a settlement written now is one nobody has been handed yet"
        );
        assert_eq!(got.state, "done", "the row's state is not this write's");

        // The newest later turn replaces the one before it, and a handle with no row is untouched.
        store
            .settle_subagent_run_again("s-1-sub-1", "third answer")
            .unwrap();
        assert_eq!(
            store
                .subagent_run("s-1-sub-1")
                .unwrap()
                .unwrap()
                .later_answer
                .as_deref(),
            Some("third answer")
        );
        store.settle_subagent_run_again("nobody", "x").unwrap();
    }

    /// A fresh settlement is the child's last word, so a later answer written before it is
    /// older news: left standing it outranks the outcome, and every reader that asks for the
    /// newest answer gets the turn before the one the child just ended on.
    #[test]
    fn a_settlement_drops_the_later_answer_before_it() {
        let store = Store::open_in_memory().unwrap();
        store.put_subagent_run(&run("s-1-sub-1", "s-1")).unwrap();
        store
            .settle_subagent_run("s-1-sub-1", "done", Some("A"))
            .unwrap();
        store.settle_subagent_run_again("s-1-sub-1", "B").unwrap();
        // A resumed child, or a reap after a restart, settles the row again.
        store
            .settle_subagent_run("s-1-sub-1", "done", Some("C"))
            .unwrap();
        let got = store.subagent_run("s-1-sub-1").unwrap().unwrap();
        assert_eq!(got.outcome.as_deref(), Some("C"));
        assert_eq!(
            got.later_answer, None,
            "the answer the new settlement supersedes is not the newest one"
        );
    }

    #[test]
    fn monitors_contexts_and_lives_round_trip() {
        let store = Store::open_in_memory().unwrap();
        store
            .put_monitor(
                "s-1",
                "build",
                r#"{"name":"build","path":"out"}"#,
                "i-1",
                300_000,
            )
            .unwrap();
        let m = store.monitors_of("s-1").unwrap();
        assert_eq!(m.len(), 1);
        assert!(m[0].live);
        store.end_monitor("s-1", "build").unwrap();
        assert!(!store.monitors_of("s-1").unwrap()[0].live);

        let ctx = JobContext {
            handle: "j3".into(),
            slug: Some("tests".into()),
            cwd: "/w".into(),
            instance: "i-1".into(),
            started_ms: 5,
        };
        store.put_job_context("s-1", &ctx).unwrap();
        assert_eq!(store.job_context("s-1", "j3").unwrap(), Some(ctx));

        store.life_started("i-1", 100).unwrap();
        store.life_beat("i-1", 150).unwrap();
        let l = store.life("i-1").unwrap().unwrap();
        assert_eq!((l.beat_ms, l.stopped_ms), (150, None));
        store.life_stopped("i-1", 170).unwrap();
        assert_eq!(store.life("i-1").unwrap().unwrap().stopped_ms, Some(170));
    }

    /// Every write here outlasts a lock held past the busy timeout: they went to `execute`
    /// directly, so each had one busy-timeout attempt and a long-held write failed.
    #[test]
    fn every_write_outlasts_a_lock_held_past_the_busy_timeout() {
        let path = std::env::temp_dir().join(format!(
            "letibot-subagents-busy-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let clean = |p: &std::path::Path| {
            for s in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{s}", p.display()));
            }
        };
        clean(&path);
        let store = Store::open(&path).unwrap();
        store.put_subagent_run(&run("s-1-sub-1", "s-1")).unwrap();
        store
            .connection()
            .busy_timeout(std::time::Duration::from_millis(50))
            .unwrap();

        type WriteFn = Box<dyn Fn(&Store) -> Result<()>>;
        let writes: Vec<(&str, WriteFn)> = vec![
            (
                "put_subagent_run",
                Box::new(|s| s.put_subagent_run(&run("s-1-sub-2", "s-1"))),
            ),
            (
                "settle_subagent_run",
                Box::new(|s| s.settle_subagent_run("s-1-sub-1", "done", None)),
            ),
            (
                "interrupt_subagent_run",
                Box::new(|s| s.interrupt_subagent_run("s-1-sub-2")),
            ),
            (
                "settle_subagent_run_again",
                Box::new(|s| s.settle_subagent_run_again("s-1-sub-1", "again")),
            ),
            (
                "deliver_subagent_run",
                Box::new(|s| s.deliver_subagent_run("s-1-sub-1")),
            ),
            (
                "reopen_subagent_run",
                Box::new(|s| s.reopen_subagent_run("s-1-sub-1").map(|_| ())),
            ),
            (
                "rest_subagent_run",
                Box::new(|s| s.rest_subagent_run("s-1-sub-1", "done")),
            ),
            (
                "put_monitor",
                Box::new(|s| s.put_monitor("s-1", "m", "{}", "i-1", 1)),
            ),
            ("end_monitor", Box::new(|s| s.end_monitor("s-1", "m"))),
            (
                "put_job_context",
                Box::new(|s| {
                    s.put_job_context(
                        "s-1",
                        &JobContext {
                            handle: "j1".into(),
                            slug: None,
                            cwd: "/w".into(),
                            instance: "i-1".into(),
                            started_ms: 1,
                        },
                    )
                }),
            ),
            ("life_started", Box::new(|s| s.life_started("i-1", 1))),
            ("life_beat", Box::new(|s| s.life_beat("i-1", 2))),
            ("life_stopped", Box::new(|s| s.life_stopped("i-1", 3))),
        ];
        for (name, write) in writes {
            let (go, wait) = std::sync::mpsc::channel::<()>();
            let (held, is_held) = std::sync::mpsc::channel::<()>();
            let p = path.clone();
            let holder = std::thread::spawn(move || {
                let c = rusqlite::Connection::open(&p).unwrap();
                c.execute_batch("BEGIN IMMEDIATE; UPDATE daemon_life SET beat_ms = beat_ms;")
                    .unwrap();
                held.send(()).unwrap();
                let _ = wait.recv();
                c.execute_batch("COMMIT").unwrap();
            });
            is_held.recv().unwrap();
            let releaser = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                let _ = go.send(());
            });
            let r = write(&store);
            releaser.join().unwrap();
            holder.join().unwrap();
            if let Err(e) = r {
                clean(&path);
                panic!("`{name}` died of a busy store instead of waiting: {e}");
            }
        }
        assert!(store.subagent_run("s-1-sub-1").unwrap().unwrap().delivered);
        assert_eq!(store.life("i-1").unwrap().unwrap().stopped_ms, Some(3));
        clean(&path);
    }
}
