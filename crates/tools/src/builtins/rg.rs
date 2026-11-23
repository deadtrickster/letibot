//! `grep`'s one-command half: where the machine the backend reads can run `rg`, the whole rung is
//! one invocation there instead of a read per page of files. On a remote host or a VM every page is
//! a round trip, and this is the round trip that stops being paid.
//!
//! The walk still runs, and the answer here is INTERSECTED with it. The walk owns which files are
//! in - git's rules, the size ceiling, the byte budget - and rg is asked to search a superset of
//! them: it is told the same rules, and a match in a path the walk never listed is dropped. So a
//! rule the two read differently costs a read that was wasted, never a match that was missed.
//!
//! Nothing is installed and nothing is asked of the operator: `rg` is used where it already is,
//! and every refusal - no `rg`, an old dialect, a failed run, a path space that does not line up -
//! falls back to the in-process scan for that rung.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use serde_json::Value;

use crate::backend::{Command, ExecBackend};

/// The largest file a search opens, from [`super::grep::FILE_CEILING`] in bytes. rg is told not to
/// read past it: a candidate over it was never in this search.
const MAX_FILESIZE: u64 = super::grep::FILE_CEILING;

/// What the probe found on one backend's machine.
#[derive(Debug, Clone)]
pub enum Tool {
    /// `rg` answered, with the version line it gave.
    Present(String),
    /// It is not there, or this backend cannot run a command at all. The reason is not kept: a
    /// search that fell back is already a silent fact, and naming it on every call would be noise
    /// in a result the model reads.
    Absent,
}

/// A backend is identified by its address and what it says about itself: two backends are the same
/// machine only if both match. A stale entry can only cost a failed run (which falls back) or a
/// probe that was not needed, never a wrong answer.
type Key = (usize, String);

#[derive(Debug)]
struct Probe {
    tool: Tool,
    /// Whether a result has already said which `rg` ran, so the version is said once per backend.
    said: bool,
}

static PROBES: LazyLock<Mutex<std::collections::HashMap<Key, Probe>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn key(backend: &dyn ExecBackend) -> Key {
    let addr = std::ptr::from_ref(backend) as *const u8 as usize;
    (addr, backend.describe())
}

/// The `rg` this backend's machine has, probing once per backend and caching the answer.
///
/// The probe is `command -v rg && rg --version` in ONE command, in the backend's own command
/// environment: a host whose non-interactive PATH has no mise shims is a host where `rg` is not
/// reachable, and that is the answer to give. Only the `--json` shape is required of it, which is
/// stable since 0.10, so no version floor is asserted here.
pub fn arm(backend: &dyn ExecBackend) -> Option<Tool> {
    let key = key(backend);
    if let Ok(probes) = PROBES.lock()
        && let Some(p) = probes.get(&key)
    {
        return Some(p.tool.clone());
    }
    let tool = probe(backend);
    if let Ok(mut probes) = PROBES.lock() {
        probes.insert(
            key,
            Probe {
                tool: tool.clone(),
                said: false,
            },
        );
    }
    // Absent is not a pushdown either way; the caller keeps the in-process scan.
    Some(tool)
}

/// The note that says which engine answered, the first time one does on this backend.
pub fn version_note(backend: &dyn ExecBackend, tool: &Tool) -> Option<String> {
    let Tool::Present(version) = tool else {
        return None;
    };
    let key = key(backend);
    let mut probes = PROBES.lock().ok()?;
    let probe = probes.get_mut(&key)?;
    if probe.said {
        return None;
    }
    probe.said = true;
    Some(format!(
        "searched by `{version}` on that host: one command, not one read per page"
    ))
}

fn probe(backend: &dyn ExecBackend) -> Tool {
    let cmd = Command {
        argv: vec![
            "sh".into(),
            "-c".into(),
            "command -v rg && rg --version".into(),
        ],
        cwd: ".".into(),
        env: Vec::new(),
    };
    match backend.run(&cmd) {
        Ok(out) if out.exit == 0 => {
            let text = String::from_utf8_lossy(&out.stdout);
            match text
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("ripgrep"))
            {
                Some(line) => Tool::Present(line.trim().to_string()),
                // Named, but not the engine whose `--json` this build parses.
                None => Tool::Absent,
            }
        }
        Ok(_) => Tool::Absent,
        Err(_) => Tool::Absent,
    }
}

/// One rung's question, in the words rg takes it in.
pub struct Question<'a> {
    /// The scope as the tool spells it, from the session's workspace.
    pub scope: &'a str,
    /// The pattern's own source, which is `Pattern`'s: the `regex` crate, one line at a time.
    pub pattern: &'a str,
    pub case_insensitive: bool,
    pub context: usize,
    pub max_matches: usize,
    /// Directories this tree's walk never enters, by name. rg is told not to open them; the caller
    /// leaves out any name a candidate sits under, since then the walk did enter it.
    pub skip: &'a [&'a str],
}

/// One matching line, in the shape `grep` renders: the path as the walk spells it, the line, and
/// the lines around it.
pub struct Found {
    pub path: String,
    pub line_no: usize,
    pub line: String,
    pub region: Vec<(usize, String)>,
    /// The first match's byte offsets in `line`, which is what centres a long line's window.
    pub span: Option<(usize, usize)>,
}

/// Why rg did not answer this rung. Every one of them means the in-process scan answers instead.
#[derive(Debug)]
pub enum Refusal {
    /// No `rg` on that machine, or the backend cannot run a command.
    NoTool(String),
    /// rg ran and refused: a pattern its dialect cannot express, or a failure of its own.
    Said(String),
    /// Every match it found fell outside the walk's candidate list. That is a path space
    /// disagreement, not an empty tree, and it must never be read as one.
    Paths,
}

/// The rung, run as one command. `keep` is the walk's candidate list, spelled as the backend
/// spells paths; what rg finds outside it is dropped.
pub fn search(
    backend: &dyn ExecBackend,
    question: &Question<'_>,
    keep: &HashSet<&str>,
    context: usize,
) -> Result<Vec<Found>, Refusal> {
    let cmd = Command {
        argv: argv(question),
        // The session's workspace, which is where the tool's paths start.
        cwd: ".".into(),
        // The home the walk reads its global excludes file from, so the two readings agree about
        // which files git leaves in. A host where this backend names no home gets its own.
        env: backend
            .home_path()
            .map(|home| vec![("HOME".to_string(), home)])
            .unwrap_or_default(),
    };
    let out = backend
        .run(&cmd)
        .map_err(|e| Refusal::NoTool(e.to_string()))?;
    match out.exit {
        // 1 is "nothing matched", which is an answer.
        0 | 1 => {}
        code => {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(Refusal::Said(match why.is_empty() {
                true => format!("rg exited {code}"),
                false => format!("rg exited {code}: {}", first_line(&why)),
            }));
        }
    }
    parse(&out.stdout, context, keep)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().to_string()
}

/// The argv, flag by flag, and why each one is there. rg's own defaults are ripgrep's opinion;
/// `grep`'s answer has to be THIS tree's, which is why the ignore and hidden flags are spelled out.
fn argv(q: &Question<'_>) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "rg".into(),
        "--json".into(),
        // No `~/.ripgreprc`, no `RIPGREP_CONFIG_PATH`: a search whose answer depends on the
        // operator's rg config is not the same search on two machines.
        "--no-config".into(),
        // A path it cannot open is not the answer; the walk already had its say about scope.
        "--no-messages".into(),
        // This tree searches hidden files (`.github/`, `.hidden/note.txt`) - and rg still skips
        // `.git` itself, which is what `default_skip` does.
        "--hidden".into(),
        // `.ignore` and `.rgignore` are not git's rules and the walk does not read them.
        "--no-ignore-dot".into(),
        // A `.gitignore` counts without a `.git` beside it, as it does for this walk.
        "--no-require-git".into(),
        // The walk reads the `.gitignore` files under its own root and no others, so rg is not
        // asked to consult the directories above it either. What that leaves rg stricter about is
        // a rule file outside the walk's reach - a global excludes file a backend rooted at one
        // project cannot read - and then rg drops a file the walk would have kept: the answer
        // stays inside the walk's list, it just has one fewer hit in it than this process finds.
        "--no-ignore-parent".into(),
        // CRLF lines are matched as `str::lines` sees them: the anchor sits before the `\r`.
        "--crlf".into(),
        format!("--max-filesize={MAX_FILESIZE}"),
        format!("--max-count={}", q.max_matches.max(1)),
    ];
    if q.case_insensitive {
        a.push("--ignore-case".into());
    }
    if q.context > 0 {
        a.push(format!("--context={}", q.context));
    }
    for name in q.skip {
        a.push(format!("--glob=!{name}"));
    }
    a.push("--regexp".into());
    a.push(q.pattern.to_string());
    a.push("--".into());
    a.push(q.scope.to_string());
    a
}

/// rg's JSON, in the order it wrote it, as `Found`s. A file's block runs from `begin` to `end`;
/// its match and context lines are turned into one region per match, which is the same window the
/// in-process scan collects from the file's own lines.
fn parse(stdout: &[u8], context: usize, keep: &HashSet<&str>) -> Result<Vec<Found>, Refusal> {
    let mut out: Vec<Found> = Vec::new();
    let mut seen = 0usize;
    let mut block = Block::default();
    for line in stdout.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            // A line rg wrote that is not JSON is a stream this build does not understand, and a
            // partly understood stream is not an answer.
            return Err(Refusal::Said("rg wrote a line that is not JSON".into()));
        };
        match event
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
        {
            "begin" => {
                block.finish(&mut out, context, keep, &mut seen);
                block = Block {
                    path: event_text(&event, "path").unwrap_or_default(),
                    ..Block::default()
                };
            }
            "match" | "context" => {
                let Some(text) = event_line(&event) else {
                    // A line rg could not write as text is a file this build would read lossily;
                    // its offsets would be the ones that differ, so the scan answers instead.
                    return Err(Refusal::Said(
                        "rg answered a line it could not write as text".into(),
                    ));
                };
                let Some(line_no) = event
                    .get("data")
                    .and_then(|d| d.get("line_number"))
                    .and_then(|n| n.as_u64())
                else {
                    return Err(Refusal::Said("rg wrote a match with no line number".into()));
                };
                block.lines.push(Line {
                    no: line_no as usize,
                    text,
                    is_match: event.get("type").and_then(|t| t.as_str()) == Some("match"),
                    span: event
                        .get("data")
                        .and_then(|d| d.get("submatches"))
                        .and_then(|s| s.get(0))
                        .and_then(|m| {
                            let start = m.get("start")?.as_u64()?;
                            let end = m.get("end")?.as_u64()?;
                            Some((start as usize, end as usize))
                        }),
                });
            }
            "end" => block.finish(&mut out, context, keep, &mut seen),
            // `summary` carries counts for `--stats`, which this build does not ask for.
            _ => {}
        }
    }
    block.finish(&mut out, context, keep, &mut seen);
    if seen > 0 && out.is_empty() {
        return Err(Refusal::Paths);
    }
    Ok(out)
}

#[derive(Default)]
struct Block {
    path: String,
    /// Every line rg wrote for this file, in order: matches and the context around them.
    lines: Vec<Line>,
}

/// One line of rg's answer for one file.
struct Line {
    no: usize,
    text: String,
    /// A match, or the context around one.
    is_match: bool,
    /// The first match's byte span in `text`, where the line is a match.
    span: Option<(usize, usize)>,
}

impl Block {
    fn finish(
        &mut self,
        out: &mut Vec<Found>,
        context: usize,
        keep: &HashSet<&str>,
        seen: &mut usize,
    ) {
        if self.lines.is_empty() {
            return;
        }
        let path = self
            .path
            .strip_prefix("./")
            .unwrap_or(&self.path)
            .to_string();
        for line in self.lines.iter() {
            if !line.is_match {
                continue;
            }
            *seen += 1;
            if !keep.contains(path.as_str()) {
                continue;
            }
            let region: Vec<(usize, String)> = match context {
                0 => Vec::new(),
                _ => {
                    let first = line.no.saturating_sub(context).max(1);
                    let last = line.no + context;
                    // The block is in file order, so the window is a contiguous run of it. A
                    // neighbouring match's lines are in the block too, which is what makes two
                    // close matches share the lines between them, as the scan's regions do.
                    let mut region = Vec::new();
                    for other in &self.lines {
                        if other.no >= first && other.no <= last {
                            region.push((other.no, other.text.clone()));
                        }
                    }
                    region
                }
            };
            out.push(Found {
                path: path.clone(),
                line_no: line.no,
                line: line.text.clone(),
                region,
                span: line.span,
            });
        }
        self.lines.clear();
    }
}

/// `data.path.text`, the relative path rg wrote.
fn event_text(event: &Value, field: &str) -> Option<String> {
    let v = event.get("data")?.get(field)?;
    let text = v.get("text")?.as_str()?;
    Some(text.to_string())
}

/// `data.lines.text`, without the line terminator `str::lines` would have dropped. rg writes
/// `bytes` instead when the line is not UTF-8.
fn event_line(event: &Value) -> Option<String> {
    let lines = event.get("data")?.get("lines")?;
    if let Some(t) = lines.get("text").and_then(|t| t.as_str()) {
        let t = t.strip_suffix('\n').unwrap_or(t);
        return Some(t.strip_suffix('\r').unwrap_or(t).to_string());
    }
    let raw = lines.get("bytes").and_then(|b| b.as_str())?;
    let bytes = crate::firecode::decode_base64(raw.as_bytes()).ok()?;
    let text = crate::builtins::text_of(&bytes).0;
    let text = text.strip_suffix('\n').unwrap_or(&text);
    Some(text.strip_suffix('\r').unwrap_or(text).to_string())
}
