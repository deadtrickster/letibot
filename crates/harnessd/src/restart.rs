//! **A daemon restart, made as painless as it can be** — what a session was running when its
//! daemon (harnessd) stopped, brought back or reminded when a daemon opens it again.
//!
//! The operator, 2026-10-12, in two steps. First: *"and here is one more thing about restart - we
//! should bring subagents back - i cant afford to wait till they finish. we should record them
//! (session persist anyway) and restart automatically"*. Then, widening it: *"on restoring agents -
//! in fact to make restarts as much painless as possible - by default we restore agents, monitors,
//! and remind the model about jobs. Now, if some threshold passed - say more than 1 hours after
//! the stop, we start to simply remind the model stating that time passed and this is what you wre
//! running and how to restart that."* And the scope of it: *"this is the case for when harnessd
//! stopped of course"*.
//!
//! # Only a daemon stop triggers it
//!
//! Every record this reads is stamped with the daemon instance that wrote it
//! (`Registry::instance`). A session opened by the daemon that already holds its records — a head
//! attaching after hours of idle, a `/resume`, a parked child coming back — finds nothing that is
//! not its own and does nothing. Only records written by a daemon that is no longer running are
//! acted on, and only once: each is re-stamped or closed as it is handled.
//!
//! # How long the daemon was down
//!
//! From the previous daemon's STOP to this daemon's START (`Registry::started_ms`), read from the
//! `daemon_life` row the previous daemon kept ([`Lifeline`]): an orderly stop writes its exact
//! time; after a crash the last heartbeat is the latest moment the daemon is KNOWN to have been
//! alive (it beats every [`BEAT`]), so the down time is an estimate that can be up to one beat too
//! long, and the notice says so. A record from a daemon with no life row at all (a build before
//! this one) is dated by the record's own last write, and said to be an estimate too.
//!
//! # Under the threshold (`Config::restore_within`, default one hour)
//!
//! * **subagents** that were mid-task are resumed (`crate::subagent_record`);
//! * **monitors** that were live are declared again with the call that declared them — same name,
//!   same condition, same owner, the TTL they were declared with — and one whose condition cannot
//!   be re-established (a job that is gone, a pid that is gone) is reported, not dropped;
//! * **jobs** are NOT re-run — a process cannot be resumed and running a command twice can do harm
//!   — and the model is reminded of each ([`job_line`]).
//!
//! # Over it
//!
//! Nothing is restored. The model gets one reminder of everything that was running, with the one
//! call that brings each back (`task_resume`, `monitor`, `bash`), and is told to decide what still
//! matters.
//!
//! Either way the reminder is a turn (it is news the model must act on) and the operator sees a
//! `restart_restored` / `restart_reminded` warning on the session listing what happened.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{
    JobRecord, Store, StoredMonitor, TodoCondition, TodoItem, TodoNeed, TodoStatus,
};

/// How often a daemon writes its heartbeat — the bound on how wrong a crash's down time can be.
pub const BEAT: Duration = Duration::from_secs(30);

/// The default restore threshold — the operator's *"say more than 1 hours after the stop"*.
pub const RESTORE_WITHIN: Duration = Duration::from_secs(3600);

/// **This daemon's life, written down** — started now, a heartbeat every [`BEAT`] until the
/// registry closes, and the stop when [`Lifeline::stopped`] is called on the orderly path.
pub struct Lifeline {
    path: Option<PathBuf>,
    registry: Arc<Registry>,
}

impl Lifeline {
    pub fn start(path: Option<PathBuf>, registry: Arc<Registry>) -> Lifeline {
        if let Some(p) = &path {
            let instance = registry.instance().to_string();
            let started = registry.started_ms();
            match Store::open(p).and_then(|s| s.life_started(&instance, started)) {
                Ok(()) => {
                    let p = p.clone();
                    let reg = registry.clone();
                    let _ = std::thread::Builder::new()
                        .name("daemon-heartbeat".into())
                        .spawn(move || heartbeat(p, reg));
                }
                Err(e) => eprintln!(
                    "  this daemon's start was not recorded ({e}); a restart after it cannot \
                     say how long it was down"
                ),
            }
        }
        Lifeline { path, registry }
    }

    /// **The orderly stop**, said with its exact time.
    pub fn stopped(&self) {
        let Some(p) = &self.path else {
            return;
        };
        let now = letibot_sessionlog::registry::now_ms();
        if let Err(e) = Store::open(p).and_then(|s| s.life_stopped(self.registry.instance(), now)) {
            eprintln!("  this daemon's stop was not recorded ({e})");
        }
    }
}

fn heartbeat(path: PathBuf, registry: Arc<Registry>) {
    let Ok(store) = Store::open(&path) else {
        return;
    };
    let mut last = std::time::Instant::now();
    while !registry.is_closed() {
        std::thread::sleep(Duration::from_millis(500));
        if last.elapsed() >= BEAT {
            last = std::time::Instant::now();
            let _ = store.life_beat(registry.instance(), letibot_sessionlog::registry::now_ms());
        }
    }
}

/// **When the previous daemon stopped, and whether that is a fact or an estimate.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub at_ms: u64,
    /// `None` for an orderly stop's own time; otherwise what the time was estimated from.
    pub estimated_from: Option<&'static str>,
}

/// The stop of `instance`, read from its life row; `fallback_ms` (the latest write among its
/// records) when it has none.
pub fn stop_of(store: &Store, instance: &str, fallback_ms: u64) -> Stopped {
    match store.life(instance).ok().flatten() {
        Some(l) => match l.stopped_ms {
            Some(at) => Stopped {
                at_ms: at,
                estimated_from: None,
            },
            None => Stopped {
                at_ms: l.beat_ms,
                estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
            },
        },
        None => Stopped {
            at_ms: fallback_ms,
            estimated_from: Some("the last record it wrote (it kept no heartbeat)"),
        },
    }
}

/// A duration as a person reads it: `3h12m`, `4m05s`, `12s`.
pub fn span(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// **The first line of every restart notice**: how long the daemon was down, and against which
/// threshold.
pub fn header(stop: &Stopped, back_ms: u64, threshold: Duration, restored: bool) -> String {
    let down = Duration::from_millis(back_ms.saturating_sub(stop.at_ms));
    let estimate = match stop.estimated_from {
        Some(from) => format!(", estimated from {from}"),
        None => String::new(),
    };
    let when = format!(
        "stopped at {}{estimate}; back at {}",
        crate::subagent_record::utc(stop.at_ms),
        crate::subagent_record::utc(back_ms)
    );
    if restored {
        format!(
            "[restart] the daemon was down for {} ({when}) — under the {} restore threshold, so \
             what this session was running has been restored where it could be:",
            span(down),
            span(threshold)
        )
    } else {
        format!(
            "[restart] the daemon was down for {} ({when}) — longer than the {} restore \
             threshold, so NOTHING was restored automatically. This is what you were running \
             when it stopped; decide what still matters and bring back only that:",
            span(down),
            span(threshold)
        )
    }
}

/// **The call that declares a monitor again**, as the model would write it.
pub fn monitor_call(m: &StoredMonitor) -> String {
    format!("`monitor {}`", m.args_json)
}

/// **One background job that was running at the stop, as the model is reminded of it.**
///
/// `ran_ms` is how long it had run when the daemon stopped. `waiting` is the open rows that waited
/// on it — named, with what happened to their wait.
pub fn job_line(
    job: &JobRecord,
    slug: Option<&str>,
    cwd: Option<&str>,
    ran_ms: Option<u64>,
    waiting: &[String],
) -> String {
    let name = match slug {
        Some(s) => format!("`{}` (slug `{s}`)", job.handle),
        None => format!("`{}`", job.handle),
    };
    let command = if job.command.is_empty() {
        "(its command was not recorded)".to_string()
    } else {
        format!("`{}`", job.command)
    };
    let mut line = format!("- job {name}: {command}");
    if let Some(cwd) = cwd {
        line.push_str(&format!(" in `{cwd}`"));
    }
    if let Some(ms) = ran_ms {
        line.push_str(&format!(
            ", had run {} when the daemon stopped",
            span(Duration::from_millis(ms))
        ));
    }
    line.push_str(
        ". It was NOT run again: its process belonged to the old daemon — an orderly stop ends it \
         with its session's scope, and a crash may have left it running unwatched, so check \
         (`pkill` with `action: \"list\"` lists by command line and kills nothing) before running it twice. Its captured \
         output was in the old daemon's memory and is gone",
    );
    match &job.redirect {
        Some(file) => line.push_str(&format!(
            "; it was also writing to `{file}`, which survives — read that"
        )),
        None => line.push_str(", and it wrote to no file of its own"),
    }
    if !job.command.is_empty() {
        let mut call = serde_json::json!({"command": job.command, "background": true});
        if let Some(cwd) = cwd {
            call["cwd"] = serde_json::Value::String(cwd.to_string());
        }
        if let Some(s) = slug {
            call["slug"] = serde_json::Value::String(s.to_string());
        }
        line.push_str(&format!(". To run it again: `bash {call}`"));
    }
    line.push('.');
    if !waiting.is_empty() {
        line.push_str(&format!(
            "\n  Rows that waited on it: {}. Their wait is RELEASED — a job this daemon never ran \
             reads as ended, so they are no longer blocked on it; if they still need its result, \
             run it again and `wait_on` the new handle.",
            waiting.join("; ")
        ));
    }
    line
}

/// **The open rows of a board that wait on `handle` (or its slug)** — by `when: job` or by a
/// `wait_on` edge — as the reminder names them.
pub fn rows_waiting_on(rows: &[TodoItem], handle: &str, slug: Option<&str>) -> Vec<String> {
    let names = |h: &str| h == handle || slug.is_some_and(|s| s == h);
    rows.iter()
        .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
        .filter(|r| {
            matches!(&r.when, Some(TodoCondition::Job { handle }) if names(handle))
                || r.needs
                    .iter()
                    .any(|n| matches!(n, TodoNeed::Job { handle } if names(handle)))
        })
        .map(|r| format!("`{}`", r.content))
        .collect()
}

/// **A monitor declaration as the harness records it** — the `monitor` tool, wrapped so a
/// successful declaration is written down with its own arguments, a renewal moves its TTL, and a
/// retirement closes it. Everything else is the tool's own.
pub struct RecordingMonitor {
    pub inner: letibot_tools::builtins::monitor::Monitor,
    pub path: Option<PathBuf>,
    pub session_id: String,
    pub instance: String,
}

impl letibot_tools::Tool for RecordingMonitor {
    fn schema(&self) -> letibot_tools::schema::ToolSchema {
        self.inner.schema()
    }

    fn access_for(&self, args: &serde_json::Value) -> Option<letibot_tools::schema::Access> {
        self.inner.access_for(args)
    }

    fn invoke(
        &self,
        ctx: &mut letibot_tools::runtime::InvokeCtx<'_>,
        args: &serde_json::Value,
    ) -> letibot_tools::runtime::Invocation {
        let out = self.inner.invoke(ctx, args);
        if out.outcome != letibot_transcript::ToolOutcome::Ok {
            return out;
        }
        let (Some(path), Some(name)) = (&self.path, args.get("name").and_then(|v| v.as_str()))
        else {
            return out;
        };
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("watch");
        let wrote = Store::open(path).and_then(|s| match action {
            "retire" => s.end_monitor(&self.session_id, name),
            "renew" => {
                let ttl = ttl_of(args);
                let known = s
                    .monitors_of(&self.session_id)?
                    .into_iter()
                    .find(|m| m.name == name);
                match known {
                    Some(m) => {
                        let mut a: serde_json::Value =
                            serde_json::from_str(&m.args_json).unwrap_or_default();
                        a["ttl_ms"] = serde_json::json!(ttl);
                        s.put_monitor(&self.session_id, name, &a.to_string(), &self.instance, ttl)
                    }
                    None => Ok(()),
                }
            }
            _ => s.put_monitor(
                &self.session_id,
                name,
                &args.to_string(),
                &self.instance,
                ttl_of(args),
            ),
        });
        if let Err(e) = wrote {
            eprintln!(
                "  {} · monitor `{name}` was not recorded ({e}); a restart will not re-arm it",
                self.session_id
            );
        }
        out
    }
}

fn ttl_of(args: &serde_json::Value) -> u64 {
    args.get("ttl_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(letibot_tools::exec::monitor::DEFAULT_TTL.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_says_how_long_and_which_side_of_the_threshold() {
        let stop = Stopped {
            at_ms: 1_791_806_400_000,
            estimated_from: None,
        };
        let back = stop.at_ms + 5 * 60_000;
        let h = header(&stop, back, RESTORE_WITHIN, true);
        assert!(h.contains("down for 5m00s"), "{h}");
        assert!(h.contains("under the 1h00m restore threshold"), "{h}");
        let crash = Stopped {
            at_ms: stop.at_ms,
            estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
        };
        let h = header(&crash, stop.at_ms + 2 * 3_600_000, RESTORE_WITHIN, false);
        assert!(h.contains("down for 2h00m"), "{h}");
        assert!(h.contains("estimated from its last heartbeat"), "{h}");
        assert!(h.contains("NOTHING was restored"), "{h}");
    }

    #[test]
    fn a_job_reminder_names_the_rerun_call_and_where_its_output_went() {
        let job = JobRecord {
            handle: "j7".into(),
            command: "cargo test > /tmp/t.log".into(),
            how: String::new(),
            state: "running".into(),
            produced: 0,
            elapsed_ms: 0,
            redirect: Some("/tmp/t.log".into()),
        };
        let line = job_line(
            &job,
            Some("tests"),
            Some("/w"),
            Some(125_000),
            &["`ship it`".into()],
        );
        assert!(line.contains("had run 2m05s"), "{line}");
        assert!(line.contains("`/tmp/t.log`, which survives"), "{line}");
        assert!(
            line.contains(
                r#"`bash {"command":"cargo test > /tmp/t.log","background":true,"cwd":"/w","slug":"tests"}`"#
            ),
            "{line}"
        );
        assert!(line.contains("Their wait is RELEASED"), "{line}");
    }
}
