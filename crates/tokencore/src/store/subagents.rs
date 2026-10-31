//! **The subagents a session started, as rows that outlive the daemon** (schema 26).
//!
//! The operator, 2026-10-12: *"and here is one more thing about restart - we should bring
//! subagents back - i cant afford to wait till they finish. we should record them (session
//! persist anyway) and restart automatically"*.
//!
//! A child's SESSION was always in the store — its transcript, its workspace, its parent — but
//! nothing said that the child was in the middle of its task, what it had been asked, or how it
//! was spawned. The task journal (`harnessd::tasks`) is a dashboard file rewritten from memory and
//! the slots `task_result` reads are in-memory too, so a restarted daemon came back with the
//! child's conversation and no idea that anybody was waiting on it. This table is that record: one
//! row per child, written when it is started and moved when its first turn settles, and read by
//! the parent when a daemon opens it again.
//!
//! # What a row's `state` means
//!
//! * `running` — **the daemon that owned the child did not see its task end**: the turn was in
//!   flight when that daemon crashed, was killed, or was stopped in an orderly way (an orderly stop
//!   leaves the row `running` on purpose — see `harnessd`'s `SubagentRecord`). The parent's next
//!   open resumes it.
//! * `done` / `failed` — the first turn settled, and `outcome` is the answer or the reason.
//!   `delivered` says whether the parent's model has been handed that settlement; a row that is
//!   settled and undelivered is delivered on the next open, and never respawned.
//! * `killed` — stopped by a person or by a parent's stop. **Never resumed.**
//! * `abandoned` — a resume was refused (its worktree is gone, or it crashed the daemon too many
//!   times in a row). `outcome` says why, and the parent was told.
//!
//! `instance` is the daemon instance (`Registry::instance`) that last owned the row, which is how
//! a parent reopened inside the SAME daemon — a parked session coming back, a head's `/resume` —
//! tells "this child is live in this process" from "this child was lost with the last one".
//!
//! `resumes` counts the restarts a child has been brought back from **without an orderly stop in
//! between**: an orderly shutdown resets it to zero, so it counts crashes, and a child at the
//! limit is abandoned rather than resumed into the crash a fourth time.
//!
//! # And beside it, the rest of what a restart needs (the operator, widening the ask: *"to make
//! restarts as much painless as possible - by default we restore agents, monitors, and remind the
//! model about jobs"*)
//!
//! * `session_monitor` — each `monitor` declaration's own arguments, so a restart can make the
//!   same call again;
//! * `job_context` — a background job's slug, directory and owning daemon, which the `job` row
//!   does not hold, so a reminder can say how to run it again;
//! * `daemon_life` — each daemon's start, last heartbeat and orderly stop, so how long a daemon
//!   was down is a reading and not a guess.
//!
//! Additive: new tables, and nothing in them is read by a build that predates them.

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

/// **One declared monitor, as the store remembers it.**
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

/// **A background job's context** — see the `job_context` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobContext {
    pub handle: String,
    pub slug: Option<String>,
    pub cwd: String,
    pub instance: String,
    pub started_ms: u64,
}

/// **A child the store names and the subagent record does not** — a spawn whose row was lost.
///
/// The record is written before a child's thread exists (`harnessd`'s `record_start`), so a
/// store that refused that write leaves a child with no row at all: nothing lists it, nothing
/// answers for it, and nothing can resume it. Its own session row is a different write, made by
/// the child when it opens, so it is usually still there — and it is all a reader has.
#[derive(Debug, Clone, PartialEq)]
pub struct UnrecordedChild {
    pub id: String,
    pub title: Option<String>,
    pub role: Option<String>,
    /// Where it worked: its worktree, or its parent's tree.
    pub workspace_root: String,
    pub created_ms: i64,
    /// **The current transcript's last row**, by [`StoredSession::last_item`]'s rule — an
    /// `Assistant` with no calls after it is a finished turn, anything else is a turn that was
    /// still going when the rows stopped. `None` for a child that wrote no row at all.
    pub last_item: Option<letibot_transcript::TranscriptItem>,
}

/// **One daemon's life** — see the `daemon_life` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonLife {
    pub instance: String,
    pub started_ms: u64,
    pub beat_ms: u64,
    pub stopped_ms: Option<u64>,
}

/// **One child, as the store remembers it** — see the module docs for each field's meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentRun {
    pub child_id: String,
    pub parent_id: String,
    pub state: String,
    /// The subtask, verbatim — what the child was asked, which a respawn needs and which the
    /// child's own transcript holds too.
    pub prompt: String,
    pub spec_json: String,
    pub instance: String,
    pub resumes: u32,
    pub outcome: Option<String>,
    pub delivered: bool,
    pub started_ms: u64,
    pub updated_ms: u64,
}

impl Store {
    /// **A child has been started** (or brought back): its row, replacing any earlier one for
    /// the same handle. `resumes` and `instance` are the caller's.
    pub fn put_subagent_run(&self, run: &SubagentRun) -> Result<()> {
        self.exec(
            "INSERT INTO subagent_run
               (child_id, parent_id, state, prompt, spec_json, instance, resumes, outcome,
                delivered, started_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(child_id) DO UPDATE SET
               parent_id = ?2, state = ?3, prompt = ?4, spec_json = ?5, instance = ?6,
               resumes = ?7, outcome = ?8, delivered = ?9, updated_ms = ?11",
            params![
                run.child_id,
                run.parent_id,
                run.state,
                run.prompt,
                run.spec_json,
                run.instance,
                run.resumes as i64,
                run.outcome,
                run.delivered as i64,
                run.started_ms as i64,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// **A child's state moved**: `state` and `outcome`, and `delivered` back to false — a
    /// settlement written now is one nobody has been handed yet. A handle with no row is a
    /// child this store never recorded, and nothing is written.
    pub fn settle_subagent_run(
        &self,
        child_id: &str,
        state: &str,
        outcome: Option<&str>,
    ) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET state = ?2, outcome = ?3, delivered = 0, updated_ms = ?4
              WHERE child_id = ?1",
            params![child_id, state, outcome, now_ms()],
        )?;
        Ok(())
    }

    /// **An orderly stop interrupted this child**: it stays `running`, so the next open
    /// resumes it, and its crash count goes back to zero — an orderly stop is not a crash.
    pub fn interrupt_subagent_run(&self, child_id: &str) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET state = 'running', resumes = 0, updated_ms = ?2
              WHERE child_id = ?1 AND state = 'running'",
            params![child_id, now_ms()],
        )?;
        Ok(())
    }

    /// **A settled child has started another turn** — a parent's `task_message`, a later prompt,
    /// a wake or its plan-check, served on the child's own thread after its first answer. The row
    /// goes back to `running` so that a daemon stopping in the middle of that turn leaves a child
    /// the next daemon resumes, instead of a `done` row that says nothing was in flight (the SLT
    /// sweep of 2026-10-14: woken by its parent at 02:01:41, interrupted by the restart at
    /// 02:06:05, and left by the next daemon as finished). The outcome and the delivered flag are
    /// the first turn's and are kept.
    ///
    /// Answers the state it was in (`done` or `failed`) when it moved, so the turn's end can put it
    /// back; `None` when nothing moved — no row, or a row that is not settled (still running from
    /// its first turn, killed, abandoned, paused).
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

    /// **A later turn of a settled child ended on its own** (not by the daemon stopping): the row
    /// goes back to the state it had before [`Store::reopen_subagent_run`] moved it. Only a row
    /// still `running` is touched — one a stop, a kill or a restart has moved since is theirs.
    pub fn rest_subagent_run(&self, child_id: &str, state: &str) -> Result<()> {
        self.exec(
            "UPDATE subagent_run SET state = ?2, updated_ms = ?3
              WHERE child_id = ?1 AND state = 'running'",
            params![child_id, state, now_ms()],
        )?;
        Ok(())
    }

    /// **The parent's model has been handed this child's settlement.**
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

    /// **Every child any session started**, oldest first — what the daemon's session list reads
    /// the children's names from (their slugs are in `spec_json`, the spawn's own description).
    pub fn subagent_runs(&self) -> Result<Vec<SubagentRun>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!("{SELECT_RUN} ORDER BY started_ms, child_id"))?;
        let rows = st.query_map([], run_of)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// **Every child this session started**, oldest first — what a parent reads when it is
    /// opened.
    pub fn subagent_runs_of(&self, parent_id: &str) -> Result<Vec<SubagentRun>> {
        let conn = self.conn();
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

    /// **When this store began recording children** — the oldest `subagent_run` row's start, or
    /// `None` for a store that has never recorded one.
    ///
    /// The window a reader of [`Store::children_without_runs`] needs: before this instant no
    /// build recorded children at all, so a child from before it has no row because none was
    /// ever written for anybody — not because a write was lost. Measured on the operator's box,
    /// 2026-11-01: one session had 517 child sessions older than its first record and exactly
    /// one inside the window, and that one is the child the daemon lost.
    pub fn first_subagent_run_ms(&self) -> Result<Option<u64>> {
        let at: Option<i64> =
            self.conn()
                .query_row("SELECT MIN(started_ms) FROM subagent_run", [], |r| r.get(0))?;
        Ok(at.map(|v| v as u64))
    }

    /// **The children of `parent_id` the store names and no `subagent_run` row does**, newest
    /// first, and only those created at or after `since_ms` (see
    /// [`Store::first_subagent_run_ms`] for what that window means).
    ///
    /// The read is the *second witness*: a spawn whose record was lost still opened a session,
    /// and that session's row says it is this parent's child, where it worked, and — from its
    /// current transcript's last row — how its last turn ended.
    ///
    /// **The window is what keeps this small.** Children from before this store recorded any
    /// are not this reader's subject, and on the operator's box they are the overwhelming
    /// majority: 517 of one session's 573 child sessions predate its first record.
    pub fn children_without_runs(
        &self,
        parent_id: &str,
        since_ms: u64,
    ) -> Result<Vec<UnrecordedChild>> {
        // `as i64` on a `u64` past `i64::MAX` wraps NEGATIVE, which would make the window
        // everything rather than nothing — the direction that hides a bug behind a wall of rows.
        let since = i64::try_from(since_ms).unwrap_or(i64::MAX);
        self.children_without_runs_matching(
            "s.parent_session_id = ?1 AND s.created_at >= ?2",
            &[&parent_id, &since],
        )
    }

    /// [`Store::children_without_runs`] for one id — `None` unless it is a child of `parent_id`
    /// the record does not hold. Keyed, so reading one child does not list its siblings.
    pub fn child_without_run(
        &self,
        parent_id: &str,
        child_id: &str,
    ) -> Result<Option<UnrecordedChild>> {
        Ok(self
            .children_without_runs_matching(
                "s.parent_session_id = ?1 AND s.id = ?2",
                &[&parent_id, &child_id],
            )?
            .into_iter()
            .next())
    }

    fn children_without_runs_matching(
        &self,
        filter: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<UnrecordedChild>> {
        // The last row is read with the same "newest transcript, then highest seq" rule
        // `sessions_matching` uses, and for the same reason: a compaction fork is written in the
        // same millisecond as the transcript it forked from, so the rowid is what breaks the tie.
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT s.id, s.title, s.role, s.workspace_root, s.created_at,
                    (SELECT i.item_json FROM transcript_item i
                      WHERE i.transcript_id = (SELECT t.id FROM transcript t
                          WHERE t.session_id = s.id
                          ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1)
                      ORDER BY i.seq DESC LIMIT 1)
               FROM session s
              WHERE {filter}
                AND NOT EXISTS (SELECT 1 FROM subagent_run r WHERE r.child_id = s.id)
              ORDER BY s.created_at DESC, s.rowid DESC"
        ))?;
        let rows = st.query_map(args, |r| {
            Ok(UnrecordedChild {
                id: r.get(0)?,
                title: r.get(1)?,
                role: r.get(2)?,
                workspace_root: r.get(3)?,
                created_ms: r.get(4)?,
                last_item: r
                    .get::<_, Option<String>>(5)?
                    .and_then(|j| serde_json::from_str(&j).ok()),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

impl Store {
    /// **A monitor was declared (or renewed)**: its row, live, owned by `instance`.
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

    /// **A monitor is no longer live** — retired, settled, expired, or reported after a restart.
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
        let conn = self.conn();
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

    /// **A background job's context**, written when it is backgrounded.
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

    /// **A daemon started**: its row, with the heartbeat at the start.
    pub fn life_started(&self, instance: &str, at_ms: u64) -> Result<()> {
        self.exec(
            "INSERT INTO daemon_life (instance, started_ms, beat_ms, stopped_ms)
             VALUES (?1, ?2, ?2, NULL)
             ON CONFLICT(instance) DO UPDATE SET started_ms = ?2, beat_ms = ?2, stopped_ms = NULL",
            params![instance, at_ms as i64],
        )?;
        Ok(())
    }

    /// **The daemon is alive** at `at_ms`.
    pub fn life_beat(&self, instance: &str, at_ms: u64) -> Result<()> {
        self.exec(
            "UPDATE daemon_life SET beat_ms = ?2 WHERE instance = ?1",
            params![instance, at_ms as i64],
        )?;
        Ok(())
    }

    /// **The daemon stopped in an orderly way** at `at_ms`.
    pub fn life_stopped(&self, instance: &str, at_ms: u64) -> Result<()> {
        self.exec(
            "UPDATE daemon_life SET beat_ms = ?2, stopped_ms = ?2 WHERE instance = ?1",
            params![instance, at_ms as i64],
        )?;
        Ok(())
    }

    /// **Every daemon's life this store has seen** — so a reader can ask *did a daemon other than me
    /// ever stop here?*, which is what makes a restart a fact rather than an inference from a
    /// session's creation time (a seeded or long-lived session is older than this daemon without
    /// anything having restarted).
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
                          resumes, outcome, delivered, started_ms, updated_ms FROM subagent_run";

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
        delivered: r.get::<_, i64>(8)? != 0,
        started_ms: r.get::<_, i64>(9)? as u64,
        updated_ms: r.get::<_, i64>(10)? as u64,
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
            delivered: false,
            started_ms: 1,
            updated_ms: 1,
        }
    }

    /// **A child whose record was never written is still named by its own session** — the second
    /// witness, and the window that keeps it to the children a record was due for.
    ///
    /// MEASURED 2026-11-01 on the operator's daemon: `s-…-sub-1793488007308855461` ran for six
    /// and a half minutes, wrote 1456 rows, left four commits in its worktree — and had no
    /// `subagent_run` row at all, because the write came back `sqlite: database is locked` and
    /// `record_start` prints and carries on. No record, so nothing listed it, nothing answered
    /// for it, and `task_resume` could not bring it back.
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
            })
            .unwrap();
        // A child the record holds, and two the record does not.
        let mut kept = run("s-1-sub-kept", "s-1");
        kept.started_ms = 5_000;
        store.put_subagent_run(&kept).unwrap();
        for (id, workspace) in [
            ("s-1-sub-lost", "/w/agent-lost"),
            ("s-1-sub-answer", "/w/agent-answer"),
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
        let all = store.children_without_runs("s-1", 0).unwrap();
        let ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["s-1-sub-answer", "s-1-sub-lost"], "{all:?}");
        assert_eq!(all[0].last_item, Some(answer));
        assert_eq!(all[1].last_item, None, "a child that wrote nothing");
        assert_eq!(all[1].workspace_root, "/w/agent-lost");
        assert_eq!(all[1].role.as_deref(), Some("coder"));

        // **The window**: nothing created before this store began recording children is this
        // reader's subject, and a window past every child finds none of them.
        assert!(
            store
                .children_without_runs("s-1", u64::MAX)
                .unwrap()
                .is_empty()
        );

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

        // And a store that has never recorded a child says so, which is the window a fresh box
        // reads as "everything".
        let fresh = Store::open_in_memory().unwrap();
        assert_eq!(fresh.first_subagent_run_ms().unwrap(), None);
    }

    /// The record's whole life: started, settled, delivered — and an orderly stop that leaves a
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

    /// **A later turn of a settled child is in flight, and the row says so** — and puts the
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

    /// **Every write here outlasts a lock held past the busy timeout** (R91.6). They went to
    /// `self.conn().execute` directly, so each had one 30 s busy-timeout attempt — the single
    /// lottery ticket `busy.rs` describes — and a lost one cost a redelivery
    /// (`deliver_subagent_run`), a crash counted against `RESUME_LIMIT`
    /// (`interrupt_subagent_run`) or a child that could never be restored (`put_subagent_run`).
    /// The connection's timeout is cut to 50 ms and another connection holds the write lock for
    /// 600 ms; every write must wait it out.
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

        let writes: Vec<(&str, Box<dyn Fn(&Store) -> Result<()>>)> = vec![
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
