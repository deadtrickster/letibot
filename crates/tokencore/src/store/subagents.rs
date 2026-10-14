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
            .conn
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
        let mut st = self
            .conn
            .prepare(&format!("{SELECT_RUN} ORDER BY started_ms, child_id"))?;
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
        let mut st = self.conn.prepare(&format!(
            "{SELECT_RUN} WHERE parent_id = ?1 ORDER BY started_ms, child_id"
        ))?;
        let rows = st.query_map(params![parent_id], run_of)?;
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
        let mut st = self.conn.prepare(
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
            .conn
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

    pub fn life(&self, instance: &str) -> Result<Option<DaemonLife>> {
        Ok(self
            .conn
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
    /// `self.conn.execute` directly, so each had one 30 s busy-timeout attempt — the single
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
