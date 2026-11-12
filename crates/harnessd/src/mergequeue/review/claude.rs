//! The queue's other reviewer: Claude Code, headless (`claude -p`, R116).
//!
//! A letibot reviewer is a session that hands its verdict to the review desk through the
//! `review_verdict` tool. This one is a PROCESS: one prompt in, one JSON object out, and the verdict
//! is the run's `structured_output` checked against the queue's own schema ([`schema`] is
//! [`Decision::ALL`] and the three evidence fields `review_verdict` takes, and nothing else). The
//! queue drives it the way it drives a child - start, poll, stop - so a panel of reviewers cannot
//! tell which kind judged a branch.
//!
//! Three facts of the headless mode shape this file. The run's output is drained by a thread while
//! it works, because a reviewer that prints more than a pipe holds would otherwise block for ever.
//! The time limit ends a run with `SIGINT` and not `SIGTERM`: a turn ended by `SIGINT` is ended
//! cleanly, and `SIGTERM` exits 143, which reads as a crash. And `--resume <id>` continues the
//! previous run's conversation, so a re-review of a new head keeps what the reviewer already read.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use letibot_tools::gatekeeper::{Decision, Door, LookedAt, ReviewRequest, Submission};
use serde_json::{Value, json};

/// The tools a run may use unless the project names its own: what a review actually reads, and
/// nothing that can write. The three git commands are the change and its history (`git diff`,
/// `git log`, `git show`) and the revision a diff is named against; `Grep` is the search. A project
/// that wants a command outside this list names it - `cargo test` builds in the tree, so it is not
/// here by default.
pub const DEFAULT_TOOLS: [&str; 7] = [
    "Read",
    "Grep",
    "Glob",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git rev-parse:*)",
];

/// The permission mode of a run nobody sits in front of. `default` runs what is preapproved and
/// refuses the rest, which is what a review needs; `bypassPermissions` hands a reviewer every tool
/// it can name, and that is a project's decision rather than this file's default.
pub const DEFAULT_PERMISSION_MODE: &str = "default";

/// An hour - the queue's own review deadline ([`super::super::serve::REVIEW_DEADLINE_MS`]), so a
/// project that says nothing gets the limit it got before there was a Claude reviewer at all.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// How long a stopped run is given to end before `SIGKILL` - the gate's own five seconds, for the
/// same reason: a reviewer that ignores `SIGINT` must not linger as a process nobody owns.
pub(super) const STOP_GRACE_MS: u64 = 5_000;

/// What a project said about its Claude reviewers - `[queue.claude]`. Present only when the project
/// named a binary: naming it IS the switch, and a queue nobody configured runs letibot reviewers
/// exactly as it did before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `[queue.claude] binary` - the `claude` executable.
    pub binary: PathBuf,
    /// `[queue.claude] allowed_tools` - what `--allowedTools` hands the run.
    pub allowed_tools: Vec<String>,
    /// `[queue.claude] permission_mode` - what `--permission-mode` hands it.
    pub permission_mode: String,
    /// `[queue.claude] timeout_seconds`: the review row's own deadline, enforced by the queue.
    pub timeout: Duration,
}

impl Settings {
    /// The settings a table asked for, with the defaults beside them.
    pub fn new(binary: PathBuf) -> Self {
        Settings {
            binary,
            allowed_tools: DEFAULT_TOOLS.iter().map(|t| t.to_string()).collect(),
            permission_mode: DEFAULT_PERMISSION_MODE.to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// The verdict schema a run is asked for: the queue's own verdict, field for field - the words of
/// [`Decision::ALL`] and the three evidence fields - so the two reviewer kinds cannot answer
/// differently and a panel can mix them.
pub fn schema() -> Value {
    let words: Vec<&str> = Decision::ALL.iter().map(|d| d.as_str()).collect();
    json!({
        "type": "object",
        "properties": {
            "decision": { "type": "string", "enum": words },
            "reasons": { "type": "array", "items": { "type": "string" } },
            "files": { "type": "array", "items": { "type": "string" } },
            "commands": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["decision", "reasons", "files", "commands"],
        "additionalProperties": false
    })
}

/// One run's argv, without the binary: the prompt, the three flags the queue needs, and `--resume`
/// when this review already has a conversation. `--resume` goes before the tool list because
/// `--allowedTools` takes every argument until the next flag.
pub fn argv(settings: &Settings, prompt: &str, resume: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(id) = resume {
        out.push("--resume".to_string());
        out.push(id.to_string());
    }
    out.push("-p".to_string());
    out.push(prompt.to_string());
    out.push("--output-format".to_string());
    out.push("json".to_string());
    out.push("--json-schema".to_string());
    out.push(schema().to_string());
    out.push("--permission-mode".to_string());
    out.push(settings.permission_mode.clone());
    out.push("--allowedTools".to_string());
    out.extend(settings.allowed_tools.iter().cloned());
    out
}

/// What one run's stdout says: the conversation's id, which is what a re-review resumes, and the
/// verdict the queue writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub session_id: Option<String>,
    pub verdict: Submission,
}

/// Read a run's stdout as the queue's verdict. A result without `structured_output` is an error -
/// never a default and never a pass: a reviewer that answered in prose has judged nothing, and
/// reading that as `accept` would land a branch nobody accepted.
pub fn read_stdout(stdout: &str) -> Result<Answer, String> {
    let value = json_in(stdout).ok_or_else(|| {
        format!(
            "the run printed no JSON result at all ({} bytes)",
            stdout.len()
        )
    })?;
    let session_id = value
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(structure) = value.get("structured_output") else {
        let said = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let failed = value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let tail = match (failed, said.is_empty()) {
            (true, false) => format!("; the run says it failed: {}", clip(said)),
            (true, true) => "; the run says it failed and printed no reason".to_string(),
            (false, false) => format!("; its answer was: {}", clip(said)),
            (false, true) => String::new(),
        };
        return Err(format!(
            "the run ended without `structured_output`, which is the only door a verdict goes \
             through here - a reviewer that answers without it has judged nothing{tail}"
        ));
    };
    let Some(word) = structure.get("decision").and_then(Value::as_str) else {
        return Err(
            "the run's `structured_output` names no `decision`, so there is no verdict in it"
                .to_string(),
        );
    };
    let Some(decision) = Decision::parse(word.trim()) else {
        let words: Vec<&str> = Decision::ALL.iter().map(|d| d.as_str()).collect();
        return Err(format!(
            "the run's `structured_output` decided `{word}`, which is not one of {} - a word \
             outside the set is refused rather than read as `accept`, because a typo must not land \
             a branch",
            words.join(", ")
        ));
    };
    Ok(Answer {
        session_id,
        verdict: Submission {
            decision,
            reasons: strings(structure.get("reasons")),
            looked_at: LookedAt {
                files: strings(structure.get("files")),
                commands: strings(structure.get("commands")),
            },
        },
    })
}

/// The last JSON object in a run's output: one object under `--output-format json`, one per line
/// under `stream-json`, so the reader takes the whole text and then the newest line that parses.
fn json_in(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text.trim())
        && v.is_object()
    {
        return Some(v);
    }
    text.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(Value::is_object)
}

/// A field of the verdict that is a list of strings; anything else is nothing said.
fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn clip(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped = letibot_transcript::text::clip_chars(&one_line, 300);
    clipped.to_string()
}

/// What one poll of a run found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Polled {
    /// Still working; nothing to write yet.
    Running,
    /// The run answered with a verdict.
    Verdict(Answer),
    /// The run ended without one - its sentence says what came instead.
    Failed(String),
    /// The row's time was up: the run has been ended with `SIGINT`, and this says so.
    TimedOut(String),
}

/// One run in flight: the process, and the two threads draining what it prints.
struct Run {
    handle: String,
    binary: PathBuf,
    child: Child,
    stdout: Option<std::thread::JoinHandle<Vec<u8>>>,
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
    /// When `SIGINT` was sent, Unix ms. `None` while nobody has asked it to stop.
    stopped: Option<u64>,
}

impl Run {
    /// `SIGINT`, which ends a Claude Code turn cleanly; `SIGTERM` would exit 143, which reads as a
    /// crash rather than the queue's own act. Sent on every request while the run is alive, because
    /// a shell's own handler consumes one that lands before its command starts (measured, 13 of
    /// 400) and `stopped` keeps the FIRST request, which is the grace's clock.
    fn end(&mut self, now_ms: u64) {
        if self.stopped.is_none() {
            self.stopped = Some(now_ms);
        }
        let pgid = self.child.id() as i32;
        // SAFETY: the pid is this process's own child, spawned into a group of its own
        // (`process_group(0)`), and `try_wait` said it was alive when the caller asked. A group
        // already gone answers ESRCH, which is the outcome wanted.
        unsafe {
            libc::killpg(pgid, libc::SIGINT);
        }
    }

    /// The two streams, once the process has ended: their threads are finished by then.
    fn output(&mut self) -> (String, String) {
        let join = |h: Option<std::thread::JoinHandle<Vec<u8>>>| {
            h.and_then(|h| h.join().ok())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        (join(self.stdout.take()), join(self.stderr.take()))
    }
}

/// The Claude reviewers this daemon has running, by handle. One per daemon and shared with every
/// worker for the reason the review desk is one: a row one pass started is polled by the next.
#[derive(Default)]
pub struct Runs {
    inner: Mutex<Vec<Run>>,
}

impl Runs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start the project's binary in `cwd` as the reviewer of `entry_id`. `resume` is this review's
    /// conversation when it has one (see `Store::claude_review`), so the reviewer keeps what it
    /// already read instead of starting from the branch again.
    pub fn start(
        &self,
        entry_id: &str,
        settings: &Settings,
        prompt: &str,
        cwd: &Path,
        resume: Option<&str>,
    ) -> Result<String, String> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let handle = format!(
            "claude-{entry_id}-{}",
            SEQ.fetch_add(1, Ordering::Relaxed) + 1
        );
        let args = argv(settings, prompt, resume);
        let mut child = Command::new(&settings.binary)
            .args(&args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| {
                format!(
                    "the reviewer could not start: {} in {} ({e})",
                    settings.binary.display(),
                    cwd.display()
                )
            })?;
        let stdout = child.stdout.take().map(drain);
        let stderr = child.stderr.take().map(drain);
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Run {
                handle: handle.clone(),
                binary: settings.binary.clone(),
                child,
                stdout,
                stderr,
                stopped: None,
            });
        Ok(handle)
    }

    /// Whether `handle` is a run of this daemon's - the question that tells a process reviewer from
    /// a session one, which the queue's liveness check has to ask before it calls a row dead.
    pub fn owns(&self, handle: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|r| r.handle == handle)
    }

    /// Ask `handle` what it has for the queue, and end it when the row's time is up. The deadline is
    /// the ROW's (`ReviewRow::deadline_ms`) rather than this module's clock, so a project's
    /// `[queue.claude] timeout_seconds` and the queue's limit are one number.
    pub fn poll(&self, handle: &str, deadline_ms: Option<u64>, now_ms: u64) -> Polled {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(i) = inner.iter().position(|r| r.handle == handle) else {
            return Polled::Failed(format!(
                "the reviewer `{handle}` is not a run this daemon holds: it never started, or a \
                 pass already collected it"
            ));
        };
        match inner[i].child.try_wait() {
            Ok(None) => {
                // Already asked to end - by this deadline on an earlier poll, or by a pass that
                // dropped the row. Ask AGAIN: one `SIGINT` that a shell's handler consumes is gone
                // for good, and this poll is the only door left, so a run ended once and never
                // repeated wedges the row here for as long as the process lives.
                if inner[i].stopped.is_some() {
                    inner[i].end(now_ms);
                    return Polled::Running;
                }
                if !deadline_ms.is_some_and(|d| now_ms > d) {
                    return Polled::Running;
                }
                inner[i].end(now_ms);
                Polled::TimedOut(format!(
                    "the reviewer `{handle}` was still at it at its time limit and the run was \
                     ended with SIGINT, so no verdict came back - nothing about the branch was \
                     judged"
                ))
            }
            Ok(Some(status)) => {
                let mut run = inner.remove(i);
                let (out, err) = run.output();
                match read_stdout(&out) {
                    Ok(answer) => Polled::Verdict(answer),
                    Err(why) => {
                        let tail = clip(&err);
                        Polled::Failed(format!(
                            "the reviewer `{handle}` ({} in the tree) ended ({}) and {why}{}",
                            run.binary.display(),
                            status_clause(status),
                            match tail.is_empty() {
                                true => String::new(),
                                false => format!(" - its stderr said: {tail}"),
                            }
                        ))
                    }
                }
            }
            Err(e) => Polled::Failed(format!(
                "the reviewer `{handle}`'s process could not be asked what it is doing: {e}"
            )),
        }
    }

    /// End a run the queue no longer wants - the row moved on without it. `SIGINT`, repeated on
    /// every call while it is alive; the run stays in the table until its process is gone, when
    /// [`Runs::keep`] drops it.
    pub fn stop(&self, handle: &str, now_ms: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(run) = inner.iter_mut().find(|r| r.handle == handle) {
            run.end(now_ms);
        }
    }

    /// Keep exactly these runs: every other is stopped, and dropped once its process has ended. A
    /// run that ignored `SIGINT` past the grace is killed, so nothing outlives the queue's interest
    /// in it for longer than it takes to say so.
    pub fn keep(&self, handles: &[String], now_ms: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut kept: Vec<Run> = Vec::new();
        for mut run in inner.drain(..) {
            if handles.contains(&run.handle) {
                kept.push(run);
                continue;
            }
            run.end(now_ms);
            match run.child.try_wait() {
                // Gone: the threads are finished, the run is nobody's, and the entry leaves here.
                Ok(Some(_)) => {}
                // Still there after the grace: it ignored the polite end.
                Ok(None) => {
                    if run
                        .stopped
                        .is_some_and(|at| now_ms.saturating_sub(at) > STOP_GRACE_MS)
                    {
                        let _ = run.child.kill();
                    }
                    kept.push(run);
                }
                Err(_) => {}
            }
        }
        *inner = kept;
    }
}

/// A reader thread that keeps a pipe from filling: the run's output is read as it comes, and the
/// bytes are joined when the process has ended.
fn drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

/// How a process ended, in the words the row keeps: its own exit code, or the signal that ended it.
fn status_clause(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(c), _) => format!("exit code {c}"),
        (None, Some(s)) => format!("killed by signal {s}{}", signal_name(s)),
        (None, None) => "exit code unknown".to_string(),
    }
}

fn signal_name(s: i32) -> &'static str {
    match s {
        libc::SIGINT => " (SIGINT - the queue's own stop)",
        libc::SIGTERM => " (SIGTERM)",
        libc::SIGKILL => " (SIGKILL)",
        _ => "",
    }
}

/// The prompt a headless run is asked with: the queue's own review prompt, with the door named the
/// way this reviewer can obey it (it holds no `review_verdict`) and a `review_brief` layer's words
/// where the host's policy stood.
pub fn prompt(req: &ReviewRequest, shaped: Option<&letibot_plugin::review::Brief>) -> String {
    letibot_tools::gatekeeper::review_prompt_with(req, Door::StructuredOutput, shaped)
}

#[cfg(test)]
mod tests;
