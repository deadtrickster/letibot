//! The firecode verbs letibot drives as an owner: pool, state and gc, each a client call with a
//! deadline that answers JSON. One module, so a lifecycle step, a reviewer's dispatch and
//! `letibot holds` all speak to one CLI in one vocabulary.
//!
//! Nothing here boots or runs a VM - that is [`super::FirecodeBackend`]. These are the calls about
//! a run AFTER it exists: what owns it, whether its work came out, and what the box may reap.
//!
//! A box with no firecode is not an error here: [`Failure::Missing`] is the one answer a caller
//! turns into a notice and a no-op, so a lifecycle pass on such a box says one line and never
//! loops on a refusal nobody can fix (`docs/lifecycle.md` d.5).

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one of these calls gets. Every one of them is a local client answering a small JSON
/// document, so a call still running at the deadline is a firecode that is not going to answer.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// `firecode gc` removes layers, which is minutes of disk work and not a client round trip.
pub const GC_TIMEOUT: Duration = Duration::from_secs(900);

/// What a run is FOR, as `--purpose` spells it. The word is firecode's, not this tree's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A session placed in a VM (a package, a `--vm` session).
    Session,
    /// A merge queue reviewer.
    Reviewer,
    /// A one-shot: a gate's steps, a thing that runs once and has nothing to keep.
    Ephemeral,
    /// A test's own VM.
    Test,
}

impl Purpose {
    pub fn word(self) -> &'static str {
        match self {
            Purpose::Session => "session",
            Purpose::Reviewer => "reviewer",
            Purpose::Ephemeral => "ephemeral",
            Purpose::Test => "test",
        }
    }

    pub fn parse(word: &str) -> Option<Purpose> {
        [
            Purpose::Session,
            Purpose::Reviewer,
            Purpose::Ephemeral,
            Purpose::Test,
        ]
        .into_iter()
        .find(|p| p.word() == word)
    }

    /// Whether a VM of this purpose is worth keeping asleep for the next one.
    pub fn reusable(self) -> bool {
        matches!(self, Purpose::Reviewer)
    }
}

/// The stamp item 1 puts on every run letibot starts: `--owner letibot:<session>:<pid>`, the
/// project the work is in, and what the run is for. Flags win over the environment, so both are
/// written: an older firecode ignores a variable it does not know and would refuse a flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub session: String,
    /// The daemon (or one-shot) process that started the run.
    pub pid: u32,
    /// The project key the run works in - twelve hex, the store's own spelling
    /// (`letibot_tokencore::migrate_split::project_key`).
    pub project: Option<String>,
    pub purpose: Purpose,
    /// `--reuse`: hand this VM back to the pool instead of destroying it.
    pub reuse: bool,
}

impl Stamp {
    /// The stamp of a run started for `session` by this process.
    pub fn here(session: impl Into<String>, purpose: Purpose) -> Stamp {
        Stamp {
            session: session.into(),
            pid: std::process::id(),
            project: None,
            purpose,
            reuse: purpose.reusable(),
        }
    }

    /// The stamp with the project the run works in, keyed the way the store keys one.
    pub fn in_project(mut self, dir: &Path) -> Stamp {
        self.project = Some(project_key(dir));
        self
    }

    /// `letibot:<session>:<pid>`, the whole point of the flag.
    pub fn owner(&self) -> String {
        format!("letibot:{}:{}", self.session, self.pid)
    }

    /// The flags, in the order a person reads them.
    pub fn flags(&self) -> Vec<String> {
        let mut out = vec![
            "--owner".to_string(),
            self.owner(),
            "--purpose".to_string(),
            self.purpose.word().to_string(),
        ];
        if let Some(key) = &self.project {
            out.push("--owner-project".to_string());
            out.push(key.clone());
        }
        if self.reuse {
            out.push("--reuse".to_string());
        }
        out
    }

    /// The same facts as the environment, for a firecode older than the flags.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut out = vec![
            ("FIRECODE_OWNER".to_string(), self.owner()),
            (
                "FIRECODE_PURPOSE".to_string(),
                self.purpose.word().to_string(),
            ),
        ];
        if let Some(key) = &self.project {
            out.push(("FIRECODE_OWNER_PROJECT".to_string(), key.clone()));
        }
        if self.reuse {
            out.push(("FIRECODE_REUSABLE".to_string(), "1".to_string()));
        }
        out
    }
}

/// The project key of a directory: twelve hex, the same number the store's split files are named
/// by, so a VM's owner project and a session's project file agree.
pub fn project_key(dir: &Path) -> String {
    let shown = std::fs::canonicalize(dir)
        .unwrap_or_else(|_| dir.to_path_buf())
        .display()
        .to_string();
    letibot_tokencore::migrate_split::project_key(&shown)
}

/// Why a call did not answer. [`Failure::Missing`] is a no-op for the caller; everything else is
/// firecode answering, and what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// There is no firecode on this box, or not the program `$FIRECODE_BIN` names.
    Missing(String),
    TimedOut {
        verb: String,
        after: Duration,
    },
    /// firecode ran and said no.
    Refused {
        verb: String,
        code: i32,
        said: String,
    },
    /// It answered with something that is not the document this build asked for.
    Unreadable {
        verb: String,
        why: String,
    },
}

impl Failure {
    /// Whether the caller's step should be skipped in silence (with one notice) rather than failed.
    pub fn is_missing(&self) -> bool {
        matches!(self, Failure::Missing(_))
    }

    /// The one line a person reads.
    pub fn sentence(&self) -> String {
        match self {
            Failure::Missing(why) => format!("firecode: {why}"),
            Failure::TimedOut { verb, after } => format!(
                "`firecode {verb}` did not answer within {} s and was killed with everything it \
                 had started",
                after.as_secs()
            ),
            Failure::Refused { verb, code, said } => format!(
                "`firecode {verb}` refused (exit {code}): {}",
                if said.is_empty() {
                    "it said nothing"
                } else {
                    said.trim()
                }
            ),
            Failure::Unreadable { verb, why } => {
                format!("`firecode {verb}` answered something this build cannot read: {why}")
            }
        }
    }
}

/// One answered call: its JSON (null when it answered no document) and the line it said, which is
/// what a refusal is built from.
struct Answer {
    json: serde_json::Value,
    said: String,
}

/// Run one verb, bounded, and read its JSON. `ok_codes` are the exit statuses that are an ANSWER
/// (0 always, and 3 for a release that refuses because work was never copied out).
fn ask(
    bin: &Path,
    args: &[String],
    verb: &str,
    within: Duration,
    fed: Option<&[u8]>,
    ok_codes: &[i32],
) -> Result<Answer, Failure> {
    super::firecode_available_at(bin.to_path_buf()).map_err(Failure::Missing)?;
    let ended = match fed {
        Some(bytes) => crate::rpc::run_bounded_fed(bin, args, within, bytes),
        None => crate::rpc::run_bounded(bin, args, within),
    };
    let out = match ended {
        crate::rpc::Ended::Answered(out) => out,
        crate::rpc::Ended::TimedOut => {
            return Err(Failure::TimedOut {
                verb: verb.to_string(),
                after: within,
            });
        }
        crate::rpc::Ended::Unstartable(why) => return Err(Failure::Missing(why)),
    };
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let said = if stdout.trim().is_empty() {
        stderr.clone()
    } else {
        format!("{stdout}{stderr}")
    };
    if code != 0 && !ok_codes.contains(&code) {
        return Err(Failure::Refused {
            verb: verb.to_string(),
            code,
            said: said.trim().to_string(),
        });
    }
    // The JSON is the last line that parses: a firecode that prints a banner before its document
    // is still answering the document.
    let json = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .unwrap_or(serde_json::Value::Null);
    Ok(Answer { json, said })
}

fn unreadable<T: serde::de::DeserializeOwned>(
    verb: &str,
    v: &serde_json::Value,
) -> Result<T, Failure> {
    serde_json::from_value(v.clone()).map_err(|e| Failure::Unreadable {
        verb: verb.to_string(),
        why: format!("{e} (in {v})"),
    })
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// state release / extract
// ---------------------------------------------------------------------------

/// What a VM release says. Exit 3 is this same document with `released: false`: the guest's work
/// was never copied out, which is a fact to act on rather than a call that failed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Release {
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    /// `out`, `nothing`, or absent.
    #[serde(default)]
    pub extraction: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub unextracted: bool,
    #[serde(default)]
    pub hint: Option<String>,
}

impl Release {
    /// Whether the guest's work is where a collector can take it: the extraction came `out`, or
    /// there was nothing to extract. Everything else is a refusal to claim the work arrived.
    pub fn work_is_out(&self) -> bool {
        match self.extraction.as_deref() {
            Some("out") => true,
            Some(other) => other == "nothing" || other == "none",
            None => !self.unextracted && self.released,
        }
    }

    /// The line a person reads when the release refused: what is missing and the command that
    /// would take it.
    pub fn sentence(&self, name: &str) -> String {
        match self.extraction.as_deref() {
            Some("out") | Some("nothing") | Some("none") => format!(
                "{name}: released, work {}",
                match self.bytes {
                    Some(n) => format!("out ({} bytes)", n),
                    None => "out".to_string(),
                }
            ),
            _ => format!(
                "{name}: its work was never copied out{}; {}",
                match self.reason.as_deref() {
                    Some(r) => format!(" ({r})"),
                    None => String::new(),
                },
                self.hint
                    .as_deref()
                    .unwrap_or("the reaper retries the extraction once before it flags it")
            ),
        }
    }
}

/// Which VM a verb is about: by name, or by the project it was booted on. Both spellings are
/// firecode's; a backend that never learned the name it was given a run by uses the project.
#[derive(Debug, Clone, Copy)]
pub enum Which<'a> {
    Name(&'a str),
    Project(&'a Path),
}

impl Which<'_> {
    fn args(&self) -> Vec<String> {
        match self {
            Which::Name(n) => vec![n.to_string()],
            Which::Project(p) => vec!["--project".to_string(), p.display().to_string()],
        }
    }

    /// The name, when the caller knew it.
    pub fn name(&self) -> Option<&str> {
        match self {
            Which::Name(n) => Some(n),
            Which::Project(_) => None,
        }
    }
}

/// `firecode state release <name> --json`, or `--project <dir>`.
pub fn release(bin: &Path, which: Which<'_>) -> Result<Release, Failure> {
    let mut a = vec!["state".to_string(), "release".to_string()];
    a.extend(which.args());
    a.push("--json".to_string());
    // Exit 3 is a document, not a failure: it is the refusal this call exists to read.
    let answer = ask(bin, &a, "state release", CALL_TIMEOUT, None, &[0, 3])?;
    if answer.json.is_null() {
        return Err(Failure::Unreadable {
            verb: "state release".into(),
            why: format!("it said {:?} and no JSON", answer.said.trim()),
        });
    }
    unreadable("state release", &answer.json)
}

/// What an extraction says.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Extract {
    #[serde(default)]
    pub extracted: bool,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `firecode extract <run> --bundle <path> --json`: the retry a release that refused leaves the
/// reaper (`docs/lifecycle.md` d.8).
pub fn extract(bin: &Path, run: &str, bundle: &Path) -> Result<Extract, Failure> {
    let a = vec![
        "extract".to_string(),
        run.to_string(),
        "--bundle".to_string(),
        bundle.display().to_string(),
        "--json".to_string(),
    ];
    let answer = ask(bin, &a, "extract", CALL_TIMEOUT, None, &[0])?;
    unreadable("extract", &answer.json)
}

// ---------------------------------------------------------------------------
// pool take / return / sleep / status
// ---------------------------------------------------------------------------

/// What a pool take says: a sleeping VM of the right purpose, or why there is none.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Taken {
    #[serde(default)]
    pub taken: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// What it was before: `sleeping` for a reused VM.
    #[serde(default)]
    pub was: Option<String>,
    #[serde(default)]
    pub resume: bool,
    #[serde(default, alias = "path")]
    pub project_path: Option<PathBuf>,
    /// `empty`, `exhausted`, … when nothing was taken.
    #[serde(default)]
    pub reason: Option<String>,
}

/// `firecode pool take --project <dir> --purpose <p> --json`. `taken: false` is an answer, not a
/// failure: the caller boots fresh.
pub fn take(bin: &Path, project: &Path, purpose: Purpose) -> Result<Taken, Failure> {
    let a = vec![
        "pool".to_string(),
        "take".to_string(),
        "--project".to_string(),
        project.display().to_string(),
        "--purpose".to_string(),
        purpose.word().to_string(),
        "--json".to_string(),
    ];
    let answer = ask(bin, &a, "pool take", CALL_TIMEOUT, None, &[0])?;
    unreadable("pool take", &answer.json)
}

/// What handing a VM back says. `returned: false` with `running_processes` is the refusal item 4
/// reads: a reviewer's VM whose guest still has a process is killed, and nothing else is.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Returned {
    #[serde(default)]
    pub returned: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    /// `sleeping` after a `--sleep` return.
    #[serde(default)]
    pub state: Option<String>,
    /// `out`, `in` or `nothing`.
    #[serde(default)]
    pub work: Option<String>,
    #[serde(default)]
    pub running_processes: Option<u32>,
    /// The programs, as one line.
    #[serde(default)]
    pub processes: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
}

impl Returned {
    /// Whether the guest still has something running, which is the one refusal `--kill` answers.
    pub fn still_running(&self) -> bool {
        !self.returned && self.running_processes.unwrap_or(0) > 0
    }

    pub fn sentence(&self, name: &str) -> String {
        if self.returned {
            format!(
                "{name}: {}",
                match (self.state.as_deref(), self.work.as_deref()) {
                    (Some(state), Some(work)) => format!("{state}, work {work}"),
                    (Some(state), None) => state.to_string(),
                    _ => "returned to the pool".to_string(),
                }
            )
        } else {
            format!(
                "{name}: not returned{}; {}",
                match self.running_processes {
                    Some(n) => format!(
                        " - {n} process(es) still running in the guest{}",
                        match self.processes.as_deref() {
                            Some(p) if !p.is_empty() => format!(" ({p})"),
                            _ => String::new(),
                        }
                    ),
                    None => String::new(),
                },
                self.reason.as_deref().unwrap_or("firecode gave no reason")
            )
        }
    }
}

/// `firecode pool return <name> --json [--sleep|--kill]`.
pub fn give_back(bin: &Path, name: &str, kill: bool) -> Result<Returned, Failure> {
    let mut a = vec![
        "pool".to_string(),
        "return".to_string(),
        name.to_string(),
        "--json".to_string(),
    ];
    a.push(if kill { "--kill" } else { "--sleep" }.to_string());
    // Exit 1 is the "work is still running in the guest" refusal, which is an answer.
    let answer = ask(bin, &a, "pool return", CALL_TIMEOUT, None, &[0, 1])?;
    unreadable("pool return", &answer.json)
}

/// What putting idle VMs to sleep did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Slept {
    #[serde(default)]
    pub slept: Vec<String>,
    #[serde(default)]
    pub kept: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `firecode pool sleep --idle <secs> --json`.
pub fn pool_sleep(bin: &Path, idle_secs: u64) -> Result<Slept, Failure> {
    let a = vec![
        "pool".to_string(),
        "sleep".to_string(),
        "--idle".to_string(),
        idle_secs.to_string(),
        "--json".to_string(),
    ];
    let answer = ask(bin, &a, "pool sleep", CALL_TIMEOUT, None, &[0])?;
    if answer.json.is_null() {
        return Ok(Slept::default());
    }
    unreadable("pool sleep", &answer.json)
}

/// One VM the pool holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PoolRow {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
}

/// `firecode pool status --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PoolStatus {
    #[serde(default)]
    pub pool: Vec<PoolRow>,
    #[serde(default)]
    pub limits: serde_json::Value,
}

pub fn pool_status(bin: &Path) -> Result<PoolStatus, Failure> {
    let a = args(&["pool", "status", "--json"]);
    let answer = ask(bin, &a, "pool status", CALL_TIMEOUT, None, &[0])?;
    unreadable("pool status", &answer.json)
}

// ---------------------------------------------------------------------------
// state reap / ls, gc
// ---------------------------------------------------------------------------

/// What a reap did with the runs whose owner is no longer live.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Reap {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub reaped: Vec<String>,
    #[serde(default)]
    pub kept: Vec<String>,
    /// Runs kept because their work was never copied out.
    #[serde(default)]
    pub kept_unextracted: u64,
}

impl Reap {
    /// The one line the reaper's journal keeps.
    pub fn sentence(&self) -> String {
        format!(
            "firecode reaped {} run(s) whose owner is gone; kept {}{}",
            self.reaped.len(),
            self.kept.len(),
            match self.kept_unextracted {
                0 => String::new(),
                n => format!(", {n} of them because their work was never copied out"),
            }
        )
    }
}

/// `firecode state reap --owners - --json`, the live owner ids on stdin. `-` and not an argv
/// because a box with two hundred sessions has no room for them in a command line.
pub fn reap(bin: &Path, live: &[String]) -> Result<Reap, Failure> {
    let a = args(&["state", "reap", "--owners", "-", "--json"]);
    let mut fed = live.join("\n");
    fed.push('\n');
    let answer = ask(
        bin,
        &a,
        "state reap",
        CALL_TIMEOUT,
        Some(fed.as_bytes()),
        &[0],
    )?;
    unreadable("state reap", &answer.json)
}

/// One row of `firecode state ls --json`: a VM firecode still has state for.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct VmRow {
    #[serde(default, alias = "id")]
    pub name: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    /// The stamp this tree wrote: `letibot:<session>:<pid>`.
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// Bytes of state this VM holds.
    #[serde(default, alias = "size_bytes")]
    pub size: Option<u64>,
    /// When it was last used, as firecode wrote it.
    #[serde(default, alias = "last_used_ms", alias = "used")]
    pub last_used: Option<String>,
    /// `out`, `in`, `nothing` or `unknown` - whether the guest's work left it.
    #[serde(default)]
    pub work: Option<String>,
    #[serde(default)]
    pub flagged: bool,
}

impl VmRow {
    /// The session id the stamp names, when it is letibot's.
    pub fn owner_session(&self) -> Option<&str> {
        let owner = self.owner.as_deref()?;
        let rest = owner.strip_prefix("letibot:")?;
        rest.split(':').next().filter(|s| !s.is_empty())
    }

    pub fn name_or_project(&self) -> String {
        self.name
            .clone()
            .or_else(|| {
                self.project.as_ref().and_then(|p| {
                    Path::new(p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
            })
            .unwrap_or_else(|| "unnamed".to_string())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Ls {
    #[serde(default)]
    pub sessions: Vec<VmRow>,
}

pub fn state_ls(bin: &Path) -> Result<Ls, Failure> {
    let a = args(&["state", "ls", "--json"]);
    let answer = ask(bin, &a, "state ls", CALL_TIMEOUT, None, &[0])?;
    unreadable("state ls", &answer.json)
}

/// What a `gc --state` did, and - the fact item 5 reports - whether it got under the budget.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Gc {
    #[serde(default)]
    pub freed_bytes: u64,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default, alias = "reached")]
    pub target_reached: Option<bool>,
    /// Why it stopped short, when it did.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub kept: serde_json::Value,
}

impl Gc {
    /// The line `letibot holds` and the daemon's log carry: what went, and whether the budget was
    /// reached.
    pub fn sentence(&self) -> String {
        let freed = crate::firecode::human(self.freed_bytes);
        match self.target_reached {
            Some(true) => format!(
                "firecode gc freed {freed} of state ({} run(s)) and is under its budget",
                self.removed.len()
            ),
            Some(false) => format!(
                "firecode gc freed {freed} of state ({} run(s)) and is STILL OVER its budget: {}",
                self.removed.len(),
                self.reason.as_deref().unwrap_or("firecode gave no reason")
            ),
            None => format!(
                "firecode gc freed {freed} of state ({} run(s))",
                self.removed.len()
            ),
        }
    }
}

/// `firecode gc --state --budget <size> --max-age <span> --json`.
pub fn gc_state(bin: &Path, budget: &str, max_age: &str) -> Result<Gc, Failure> {
    let a = vec![
        "gc".to_string(),
        "--state".to_string(),
        "--budget".to_string(),
        budget.to_string(),
        "--max-age".to_string(),
        max_age.to_string(),
        "--json".to_string(),
    ];
    let answer = ask(bin, &a, "gc --state", GC_TIMEOUT, None, &[0])?;
    unreadable("gc --state", &answer.json)
}

// ---------------------------------------------------------------------------
// The work firecode never copied out
// ---------------------------------------------------------------------------

/// A VM whose work `state release` refused to call out, kept so the reaper can retry the
/// extraction once before it flags the run. One file per VM, beside its copy, because the record
/// must outlive the process that wrote it (a daemon restart is the normal case).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Unextracted {
    pub name: String,
    /// The run id `firecode extract` takes.
    pub run: String,
    pub project: String,
    #[serde(default)]
    pub hint: Option<String>,
    /// How many extraction attempts have been made. One, and the record is flagged.
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub since_ms: u64,
}

impl Unextracted {
    /// Where a VM named `name`'s record lives: beside the copies under the cache.
    pub fn path_in(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.unextracted.json"))
    }

    /// Write the record, creating the directory. A record that cannot be written is not worth an
    /// error at a session's end: the line the operator reads is the one that matters.
    pub fn save(&self, dir: &Path) -> bool {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        let path = Self::path_in(dir, &self.name);
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => std::fs::write(path, bytes).is_ok(),
            Err(_) => false,
        }
    }

    /// Remove a VM's record, after its work came out.
    pub fn forget(dir: &Path, name: &str) {
        let _ = std::fs::remove_file(Self::path_in(dir, name));
    }

    /// Every record under `dir`, oldest first.
    pub fn all(dir: &Path) -> Vec<Unextracted> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<Unextracted> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().ends_with(".unextracted.json"))
                    .unwrap_or(false)
            })
            .filter_map(|p| serde_json::from_slice::<Unextracted>(&std::fs::read(p).ok()?).ok())
            .collect();
        out.sort_by_key(|r| r.since_ms);
        out
    }
}

/// Whether firecode is on this box at all, and where - the one question every caller asks before
/// it says one notice and leaves its step alone.
pub fn installed() -> Option<PathBuf> {
    super::firecode_available().ok()
}

// ---------------------------------------------------------------------------
// The reaper's firecode half
// ---------------------------------------------------------------------------

/// What one pass of the firecode half did, in the lines the daemon's log keeps. `None` is a box
/// with no firecode: the pass is skipped whole, with one notice, and never retried in a loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    pub lines: Vec<String>,
    /// Steps firecode refused, which a person reads once and nothing retries.
    pub refusals: Vec<String>,
}

impl Pass {
    pub fn none() -> Pass {
        Pass {
            lines: Vec::new(),
            refusals: Vec::new(),
        }
    }
}

/// Item 5: one firecode pass - reap the runs whose owner is gone, sleep the idle pool, and retry
/// the one extraction a refused release left. `live` is every live owner id in [`Stamp::owner`]'s
/// spelling; a box with no firecode is a no-op with one line.
pub fn reap_pass(bin: &Path, live: &[String], cache: &Path) -> Pass {
    let mut pass = Pass::none();
    match reap(bin, live) {
        Ok(r) => pass.lines.push(r.sentence()),
        Err(e) if e.is_missing() => {
            pass.lines.push(e.sentence());
            return pass;
        }
        Err(e) => pass.refusals.push(e.sentence()),
    }
    match pool_sleep(bin, IDLE_SLEEP_SECS) {
        Ok(s) if !s.slept.is_empty() => {
            pass.lines.push(format!(
                "firecode put {} idle VM(s) to sleep: {}",
                s.slept.len(),
                s.slept.join(", ")
            ));
        }
        Ok(_) => {}
        Err(e) => pass.refusals.push(e.sentence()),
    }
    // The extractions a release refused leave: one retry each, then the run is flagged and a
    // person is told. Nothing here deletes a copy.
    for mut record in Unextracted::all(cache) {
        if record.flagged {
            continue;
        }
        if record.attempts >= 1 {
            record.flagged = true;
            record.save(cache);
            pass.refusals.push(format!(
                "firecode: {} still holds work that was never copied out{}",
                record.name,
                record
                    .hint
                    .as_deref()
                    .map(|h| format!(" ({h})"))
                    .unwrap_or_default()
            ));
            continue;
        }
        record.attempts += 1;
        let bundle = cache.join(format!("{}.rescue.bundle", record.name));
        match extract(bin, &record.run, &bundle) {
            Ok(x) if x.extracted => {
                Unextracted::forget(cache, &record.name);
                pass.lines.push(format!(
                    "firecode: {}'s work was extracted after all ({})",
                    record.name,
                    match x.bytes {
                        Some(n) => format!("{n} bytes"),
                        None => bundle.display().to_string(),
                    }
                ));
            }
            Ok(x) => {
                record.hint = x.reason.clone().or(record.hint);
                record.save(cache);
            }
            Err(e) => {
                record.hint = Some(e.sentence());
                record.save(cache);
            }
        }
    }
    pass
}

/// `pool sleep --idle 300`: five minutes of nothing and a pooled VM goes to sleep (`docs` item 5).
pub const IDLE_SLEEP_SECS: u64 = 300;

/// How often the hourly `gc --state` runs.
pub const GC_EVERY: Duration = Duration::from_secs(3600);

/// What the last `gc --state` said, with when it said it. It is a file so `letibot holds` can
/// report a box that is still over its budget without running a gc of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GcNote {
    #[serde(default)]
    pub at_ms: u64,
    #[serde(default)]
    pub gc: Gc,
}

/// Where the note lives: beside the store's other box-wide files.
pub fn gc_note_path(dir: &Path) -> PathBuf {
    dir.join("firecode-gc.json")
}

pub fn save_gc_note(dir: &Path, note: &GcNote) -> bool {
    std::fs::create_dir_all(dir).is_ok()
        && serde_json::to_vec_pretty(note)
            .ok()
            .is_some_and(|bytes| std::fs::write(gc_note_path(dir), bytes).is_ok())
}

pub fn read_gc_note(dir: &Path) -> Option<GcNote> {
    serde_json::from_slice(&std::fs::read(gc_note_path(dir)).ok()?).ok()
}

#[cfg(test)]
mod tests;
