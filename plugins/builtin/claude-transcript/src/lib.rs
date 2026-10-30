//! **Claude Code's own transcripts, as a tool the model can call** — the first user of the
//! `tool_` seam (`docs/plugins-plan.md` §4.8) and of `read:<path>` (§3).
//!
//! # Why this exists
//!
//! Claude Code keeps every conversation it ran on this machine as
//! `~/.claude/projects/<project>/<session>.jsonl` — one JSON object per line, no index, no query
//! language. So a model asked *"what did we decide in the letibot session yesterday"* has exactly
//! two options today: guess, or ask the operator to go and read it. `bash` is not an option worth
//! having — a `grep` over a 116 MB file into a context is the thing this tool exists to prevent,
//! and it needs the gate, which a confined seat does not have.
//!
//! The tree already has the vocabulary for this: `transcript` (`crates/tools/src/builtins/
//! transcript.rs`) answers *this* session's own store with `sessions`, a text search that returns
//! line numbers, a range read and bounded pages. This layer answers the same questions about
//! another agent's store, in the same words, so a model that has learnt one has learnt the other.
//!
//! # The four operations, and what each is bounded by
//!
//! | `what` | answers | bounded by |
//! |---|---|---|
//! | `sessions` | a project's transcripts: size, mtime, and the operator's first line | 16 projects, 12 transcripts each, 16 lines read per transcript |
//! | `search` | each match's line number, byte offset and context | 10 matches, 1 line of context, 2 000 bytes a line |
//! | `read` | a numbered line range | 60 lines (400 at most), 2 000 bytes a line |
//! | `outline` | what the operator asked and what the assistant concluded | 40 lines of each kind |
//!
//! Every one of them says when it truncated. That is not politeness: a tool whose whole reason for
//! existing is *do not put a hundred megabytes into a context* cannot answer a big file with
//! silence about the part it left out — the model would read a partial answer as a complete one.
//!
//! # How a guest with no filesystem reads one
//!
//! A plugin does no I/O (§1, principle 1), so this layer does not open a file: it **asks the host**
//! with `{"looks":[…]}` and the host runs them and calls the hook again with the results in the
//! same order. `read` (a line range, or a list of line numbers), `list` (a directory's entries with
//! size and mtime) and `grep` (matches with line index and byte offset) are the three primitives,
//! and they are the host's because *streaming a 116 MB file* is the host's job. What is here is the
//! part that is policy: which file, which lines, and how the answer reads.
//!
//! # The reach is one line in `plugin.toml`
//!
//! `capabilities = ["read:~/.claude/projects"]` — that and nothing else. The host refuses a look
//! outside it, and it refuses it on the *resolved* path, so `~/.claude/projects/-Users-me-Projects-
//! letibot/memory`, a symlink into `~/Projects`, is refused for pointing out of the tree. `~/.ssh`
//! is not under this tree and no spelling of a path makes it so.

use letibot_guest_abi as abi;
use serde::Deserialize;
use serde_json::{Value, json};

/// Projects one listing covers. A machine with more is a machine whose operator should name one.
const PROJECTS: usize = 16;
/// Transcripts one project contributes to a listing, newest first.
const FILES_PER_PROJECT: usize = 12;
/// Transcripts one answer covers across every project.
const FILES_TOTAL: usize = 24;
/// Lines read to find the operator's first line of a transcript — the first user row of a real
/// session is usually within eight, and a header or a title row is not one.
const HEAD_LINES: u64 = 16;
/// Lines of a transcript an outline reads, of each kind.
const OUTLINE_LINES: usize = 40;
/// The most one answer's payload may be. The guest's arena is a megabyte; an answer that spent it
/// would be a tool result the runtime then has to spill, which is the cost this tool avoids.
const PAYLOAD_CAP: usize = 24_000;
/// How much of one line is shown in an answer.
const SHOW: usize = 240;

/// The hook's input, as the host sends it.
#[derive(Debug, Default, Deserialize)]
struct In {
    #[serde(default)]
    args: Value,
    #[serde(default)]
    feeds: Feeds,
    /// The results of the looks asked for on the previous round, in the order they were asked.
    #[serde(default)]
    looks: Vec<Value>,
    /// What this layer carried over from the previous round — the host keeps it and hands it back
    /// unchanged (§1, principle 5: a plugin is stateless between calls).
    #[serde(default)]
    state: Value,
}

/// The host's own facts. Not capabilities: these are what the harness already knows. The host
/// also sends the session id, which this layer has no use for — a transcript is named by the file
/// it is, not by the session asking.
#[derive(Debug, Default, Clone, Deserialize)]
struct Feeds {
    #[serde(default)]
    home: String,
    #[serde(default)]
    workspace: String,
    #[serde(default)]
    now_ms: u64,
}

/// What one call asked for.
#[derive(Debug)]
struct Args {
    what: String,
    project: Option<String>,
    session: Option<String>,
    matching: Option<String>,
    from: u64,
    lines: u64,
    limit: u64,
    context: u64,
    all_projects: bool,
}

impl Args {
    fn read(args: &Value) -> Args {
        let s = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let n = |k: &str, default: u64, low: u64, high: u64| {
            args.get(k)
                .and_then(Value::as_u64)
                .map(|v| v.clamp(low, high))
                .unwrap_or(default)
        };
        Args {
            what: s("what").unwrap_or_else(|| "sessions".to_string()),
            project: s("project"),
            session: s("session"),
            matching: s("match"),
            from: n("from", 1, 1, u32::MAX as u64),
            lines: n("lines", 60, 1, 400),
            limit: n("limit", 10, 1, 40),
            context: n("context", 1, 0, 8),
            all_projects: args
                .get("all_projects")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

/// One step of a call: ask the host for data, or answer.
enum Step {
    Ask {
        looks: Vec<Value>,
        state: Value,
    },
    Done {
        outcome: &'static str,
        payload: String,
    },
}

impl Step {
    fn json(self) -> Value {
        match self {
            Step::Ask { looks, state } => json!({"looks": looks, "state": state}),
            Step::Done { outcome, payload } => {
                json!({"outcome": outcome, "payload": cap(payload)})
            }
        }
    }
}

/// **The ceiling on one answer**, said out loud when it is reached.
///
/// Every operation is bounded already, and the bounds multiply: 400 lines at 720 characters is a
/// quarter of a megabyte. This is the last gate before the answer becomes a tool result the
/// runtime has to spill — which is the cost this tool exists to avoid — so it cuts on a character
/// boundary and tells the model what to ask for instead.
fn cap(payload: String) -> String {
    if payload.len() <= PAYLOAD_CAP {
        return payload;
    }
    let mut end = PAYLOAD_CAP;
    while !payload.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n… (the answer was cut at {PAYLOAD_CAP} bytes; this call's answer is longer than \
         one result should be. A narrower `match`, a smaller `lines`, or a `session` reaches the \
         rest.)",
        &payload[..end]
    )
}

fn ok(payload: impl Into<String>) -> Step {
    Step::Done {
        outcome: "ok",
        payload: payload.into(),
    }
}

fn failed(why: impl Into<String>, payload: impl Into<String>) -> Step {
    Step::Done {
        outcome: "failed",
        payload: format!("{}\n\n{}", why.into(), payload.into()),
    }
}

fn refused(why: impl Into<String>, payload: impl Into<String>) -> Step {
    Step::Done {
        outcome: "refused",
        payload: format!("{}\n\n{}", why.into(), payload.into()),
    }
}

/// The hook: `tool_claude_transcript`.
#[unsafe(no_mangle)]
pub extern "C" fn tool_claude_transcript(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

fn answer(bytes: &[u8]) -> Option<String> {
    let input: In = match serde_json::from_slice(bytes) {
        Ok(input) => input,
        // The host writes this input, so bytes that are not the contract are the host's defect and
        // not a pass: `-1` would say *this layer has nothing to add*, which is not what happened.
        Err(e) => {
            return Some(
                failed(
                    "the tool input was not JSON this layer can read",
                    format!(
                        "{e}. The input is `{{\"v\":1,\"name\":…,\"args\":{{…}},\"feeds\":{{…}},\
                         \"looks\":[…],\"state\":…}}`; `plugin test` on this layer shows what the \
                         host sends."
                    ),
                )
                .json()
                .to_string(),
            );
        }
    };
    let args = Args::read(&input.args);
    let step = if input.looks.is_empty() {
        begin(&args, &input.feeds)
    } else {
        carry(&args, &input.feeds, &input.looks, &input.state)
    };
    Some(step.json().to_string())
}

/// Round zero: nothing has been looked at, so decide what to ask for.
fn begin(args: &Args, feeds: &Feeds) -> Step {
    let state = json!({"op": args.what, "step": 0});
    carry(args, feeds, &[], &state)
}

/// Every round after the first: `looks` is the answer to the previous round's questions, and
/// `state` is what this layer left there.
fn carry(args: &Args, feeds: &Feeds, looks: &[Value], state: &Value) -> Step {
    let step = state.get("step").and_then(Value::as_u64).unwrap_or(0);
    match args.what.as_str() {
        "sessions" => sessions(args, feeds, looks, state, step),
        "search" => search(args, feeds, looks, state, step),
        "read" => read_op(args, feeds, looks, state, step),
        "outline" => outline(args, feeds, looks, state, step),
        other => failed(
            format!("`{other}` is not something this tool does"),
            "There are four: `sessions` (the default — a project's transcripts), `search` (find \
             text), `read` (a line range) and `outline` (what was asked and what was concluded).",
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// `sessions`
// ---------------------------------------------------------------------------------------------

fn sessions(args: &Args, feeds: &Feeds, looks: &[Value], state: &Value, step: u64) -> Step {
    match step {
        0 => {
            let dir = match listing_root(args, feeds) {
                Ok(dir) => dir,
                Err(why) => return refused(why.0, why.1),
            };
            let kind = if args.all_projects { "root" } else { "project" };
            Step::Ask {
                looks: vec![json!({"list": dir, "limit": 64})],
                state: json!({"op": "sessions", "step": 1, "kind": kind, "dir": dir}),
            }
        }
        1 if state["kind"] == json!("root") => {
            let dir = string(state, "dir");
            let projects = subdirs(&looks[0], &dir);
            if projects.is_empty() {
                return ok(format!(
                    "no project transcripts under {dir} — Claude Code writes one directory per \
                     project it ran in, and this one holds none. If the operator works somewhere \
                     else, pass `project` with that path."
                ));
            }
            let listed = projects.len();
            let projects: Vec<String> = projects.into_iter().take(PROJECTS).collect();
            Step::Ask {
                looks: projects
                    .iter()
                    .map(|p| json!({"list": p, "limit": 128}))
                    .collect(),
                state: json!({
                    "op": "sessions", "step": 2, "kind": "projects",
                    "dir": dir, "projects": projects, "listed": listed,
                }),
            }
        }
        1 => {
            // A project was named: its listing is in hand, so go straight to the heads.
            let dir = string(state, "dir");
            let files = transcripts(&looks[0], &dir);
            if files.is_empty() {
                return ok(format!(
                    "no transcripts in {dir} — the directory exists and holds no `.jsonl`, which \
                     means a session was started there and never wrote a turn."
                ));
            }
            heads(&files)
        }
        2 => {
            // Every project was listed; now one head per transcript, newest first.
            let projects: Vec<String> = strings(state, "projects");
            let mut files = Vec::new();
            for (i, look) in looks.iter().enumerate() {
                if let Some(dir) = projects.get(i) {
                    files.extend(transcripts(look, dir));
                }
            }
            files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
            files.truncate(FILES_TOTAL);
            if files.is_empty() {
                return ok("no transcripts in any project under this root.");
            }
            heads(&files)
        }
        _ => {
            let files: Vec<File> = state
                .get("files")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .map(|f| File {
                            path: string(f, "path"),
                            project: string(f, "project"),
                            size: f.get("size").and_then(Value::as_u64).unwrap_or(0),
                            mtime: f.get("mtime").and_then(Value::as_u64).unwrap_or(0),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut out = format!(
                "{} transcript(s), newest first. `~/.claude/projects` names a project by its path \
                 with the separators turned into `-`.\n",
                files.len()
            );
            let now = feeds.now_ms;
            for (i, file) in files.iter().enumerate() {
                let head = looks.get(i);
                out.push_str(&format!(
                    "\n  {}/{}  {}  {}\n",
                    file.project,
                    file_name(&file.path),
                    size(file.size),
                    stamp(file.mtime, now),
                ));
                match head.map(first_operator_line).unwrap_or(None) {
                    Some(line) => out.push_str(&format!("      \"{}\"\n", clip(&line, SHOW))),
                    None => out.push_str("      (no operator line in the first lines)\n"),
                }
            }
            let listed = state.get("listed").and_then(Value::as_u64).unwrap_or(0) as usize;
            let projects: Vec<String> = strings(state, "projects");
            if listed > projects.len() {
                out.push_str(&format!(
                    "\n{} more project(s) were not listed — pass `project` to read one.\n",
                    listed - projects.len()
                ));
            }
            out.push_str(
                "\n`read` a transcript by `session` (the file's name without `.jsonl`, or the \
                 first characters of it) for its lines, `search` for text across them, `outline` \
                 for what was asked and concluded.\n",
            );
            ok(out)
        }
    }
}

/// Ask the host for the head of every transcript, so the next round can name the first line.
fn heads(files: &[File]) -> Step {
    Step::Ask {
        looks: files
            .iter()
            .map(|f| {
                json!({
                    "read": f.path,
                    "from": 1,
                    "lines": HEAD_LINES,
                    "max_line_bytes": 800,
                })
            })
            .collect(),
        state: json!({
            "op": "sessions", "step": 3, "kind": "heads",
            "files": files.iter().map(|f| json!({
                "path": f.path, "project": f.project, "size": f.size, "mtime": f.mtime,
            })).collect::<Vec<Value>>(),
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// `search`
// ---------------------------------------------------------------------------------------------

fn search(args: &Args, feeds: &Feeds, looks: &[Value], state: &Value, step: u64) -> Step {
    let Some(_needle) = &args.matching else {
        return refused(
            "`search` needs `match` — the text to look for",
            "The search is a plain case-insensitive substring, not a regex. `sessions` lists what \
             there is to search, and `read` answers a line range once a search has named one.",
        );
    };
    match step {
        0 => {
            let dir = match listing_root(args, feeds) {
                Ok(dir) => dir,
                Err(why) => return refused(why.0, why.1),
            };
            let kind = if args.all_projects { "root" } else { "project" };
            Step::Ask {
                looks: vec![json!({"list": dir, "limit": 64})],
                state: json!({"op": "search", "step": 1, "kind": kind, "dir": dir}),
            }
        }
        1 if state["kind"] == json!("root") => {
            let dir = string(state, "dir");
            let projects: Vec<String> = subdirs(&looks[0], &dir)
                .into_iter()
                .take(PROJECTS)
                .collect();
            if projects.is_empty() {
                return ok(format!(
                    "no project transcripts under {dir}, so nothing to search."
                ));
            }
            Step::Ask {
                looks: projects
                    .iter()
                    .map(|p| json!({"list": p, "limit": 128}))
                    .collect(),
                state: json!({"op": "search", "step": 2, "kind": "projects", "dir": dir, "projects": projects}),
            }
        }
        1 => {
            let dir = string(state, "dir");
            let mut files = transcripts(&looks[0], &dir);
            files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
            files.truncate(FILES_PER_PROJECT);
            if files.is_empty() {
                return ok(format!("no transcripts in {dir}, so nothing to search."));
            }
            grep_all(args, files)
        }
        2 => {
            let projects: Vec<String> = strings(state, "projects");
            let mut files = Vec::new();
            for (i, look) in looks.iter().enumerate() {
                if let Some(dir) = projects.get(i) {
                    let mut these = transcripts(look, dir);
                    these.sort_by(|a, b| b.mtime.cmp(&a.mtime));
                    these.truncate(FILES_PER_PROJECT);
                    files.extend(these);
                }
            }
            files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
            files.truncate(FILES_TOTAL);
            if files.is_empty() {
                return ok("no transcripts in any project under this root, so nothing to search.");
            }
            grep_all(args, files)
        }
        _ => render_search(args, looks, state),
    }
}

/// Ask the host to sweep every transcript for the needle.
fn grep_all(args: &Args, files: Vec<File>) -> Step {
    let needle = args.matching.clone().unwrap_or_default();
    Step::Ask {
        looks: files
            .iter()
            .map(|f| {
                json!({
                    "grep": f.path,
                    "match": needle,
                    "limit": args.limit,
                    "context": args.context,
                    "ignore_case": true,
                })
            })
            .collect(),
        state: json!({
            "op": "search", "step": 3, "kind": "matches", "match": needle,
            "files": files.iter().map(|f| json!({
                "path": f.path, "project": f.project, "size": f.size, "mtime": f.mtime,
            })).collect::<Vec<Value>>(),
        }),
    }
}

fn render_search(args: &Args, looks: &[Value], state: &Value) -> Step {
    let files: Vec<File> = state
        .get("files")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|f| File {
                    path: string(f, "path"),
                    project: string(f, "project"),
                    size: f.get("size").and_then(Value::as_u64).unwrap_or(0),
                    mtime: f.get("mtime").and_then(Value::as_u64).unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    let needle = string(state, "match");
    let mut hits: Vec<(usize, Value)> = Vec::new();
    let mut scanned = 0u64;
    let mut more = false;
    let mut unreadable: Vec<String> = Vec::new();
    for (i, look) in looks.iter().enumerate() {
        if look.get("exit").and_then(Value::as_u64) != Some(0) {
            if let Some(file) = files.get(i) {
                unreadable.push(format!(
                    "{}: {}",
                    file.project,
                    look.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unreadable")
                ));
            }
            continue;
        }
        scanned += look.get("scanned").and_then(Value::as_u64).unwrap_or(0);
        if look.get("more").and_then(Value::as_bool) == Some(true) {
            more = true;
        }
        for m in look
            .get("matches")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            hits.push((i, m.clone()));
        }
    }
    if hits.is_empty() {
        let mut out = format!(
            "nothing matched `{needle}`. {scanned} line(s) were read across {} transcript(s), so \
             the search ran — this is an empty result, not a failure.\n\nThe search is a plain \
             case-insensitive substring, not a regex: a shorter `match` catches more, and \
             punctuation in the middle of a phrase will miss.\n",
            looks.len()
        );
        if !unreadable.is_empty() {
            out.push_str(&format!(
                "\n{} transcript(s) could not be read: {}\n",
                unreadable.len(),
                unreadable.join("; ")
            ));
        }
        return ok(out);
    }
    let mut out = format!(
        "{} match(es) for `{needle}` in {} transcript(s), {scanned} line(s) read{}.\n",
        hits.len(),
        looks.len(),
        if more {
            ", and the sweep stopped at the match limit"
        } else {
            ""
        },
    );
    let shown = hits.len().min(args.limit as usize);
    for (i, m) in hits.iter().take(shown) {
        let file = &files[*i];
        out.push_str(&format!(
            "\n  {}/{}:{}  byte {}\n",
            file.project,
            file_name(&file.path),
            m.get("line").and_then(Value::as_u64).unwrap_or(0),
            m.get("byte").and_then(Value::as_u64).unwrap_or(0),
        ));
        for line in m
            .get("before")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.push_str(&format!(
                "      {} | {}\n",
                n_of(line),
                clip(&text_of(line), SHOW)
            ));
        }
        out.push_str(&format!(
            "    > {} | {}{}\n",
            m.get("line").and_then(Value::as_u64).unwrap_or(0),
            clip(m.get("text").and_then(Value::as_str).unwrap_or(""), SHOW),
            if m.get("cut").and_then(Value::as_bool) == Some(true) {
                " …(line cut)"
            } else {
                ""
            }
        ));
        for line in m
            .get("after")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.push_str(&format!(
                "      {} | {}\n",
                n_of(line),
                clip(&text_of(line), SHOW)
            ));
        }
    }
    if hits.len() > shown {
        out.push_str(&format!(
            "\n{} more match(es) were not shown. A narrower `match`, a `session`, or `limit` \
             continues — and `read` takes the line number above if what you want is the context \
             around one hit rather than all of them.\n",
            hits.len() - shown
        ));
    }
    if !unreadable.is_empty() {
        out.push_str(&format!(
            "\n{} transcript(s) could not be read: {}\n",
            unreadable.len(),
            unreadable.join("; ")
        ));
    }
    ok(out)
}

// ---------------------------------------------------------------------------------------------
// `read`
// ---------------------------------------------------------------------------------------------

fn read_op(args: &Args, feeds: &Feeds, looks: &[Value], state: &Value, step: u64) -> Step {
    match step {
        0 => {
            let dir = match project_dir(args, feeds) {
                Ok(dir) => dir,
                Err(why) => return refused(why.0, why.1),
            };
            match &args.session {
                Some(id) if is_full_id(id) => Step::Ask {
                    looks: vec![json!({
                        "read": format!("{dir}/{id}.jsonl"),
                        "from": args.from,
                        "lines": args.lines,
                    })],
                    state: json!({"op": "read", "step": 2, "file": format!("{dir}/{id}.jsonl")}),
                },
                _ => Step::Ask {
                    looks: vec![json!({"list": dir, "limit": 128})],
                    state: json!({"op": "read", "step": 1, "dir": dir}),
                },
            }
        }
        1 => {
            let dir = string(state, "dir");
            let files = transcripts(&looks[0], &dir);
            let file = match pick(&files, args.session.as_deref()) {
                Ok(file) => file,
                Err(why) => return why,
            };
            Step::Ask {
                looks: vec![json!({
                    "read": file.path,
                    "from": args.from,
                    "lines": args.lines,
                })],
                state: json!({"op": "read", "step": 2, "file": file.path}),
            }
        }
        _ => {
            let file = string(state, "file");
            let look = &looks[0];
            if look.get("exit").and_then(Value::as_u64) != Some(0) {
                return failed(
                    format!("`{file}` could not be read"),
                    look.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("the host did not say why")
                        .to_string(),
                );
            }
            let lines = look
                .get("lines")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if lines.is_empty() {
                return ok(format!(
                    "`{file}` has no line {} — the file ends before it. Line numbers are \
                     1-based and count every row, including the ones that are not conversation.",
                    args.from
                ));
            }
            let mut out = format!(
                "`{}`, line {} onwards ({} line(s){}):\n\n",
                file,
                args.from,
                lines.len(),
                if look.get("more").and_then(Value::as_bool) == Some(true) {
                    ", and the file goes on"
                } else {
                    ""
                },
            );
            for line in &lines {
                out.push_str(&format!(
                    "{:>6} | {}\n",
                    n_of(line),
                    clip(&text_of(line), SHOW * 3),
                ));
            }
            out.push_str(
                "\nEach line is one JSON object of Claude Code's own store. `search` finds a line \
                 by text and names its byte offset; `outline` reads the conversation without the \
                 tool traffic.\n",
            );
            ok(out)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `outline`
// ---------------------------------------------------------------------------------------------

fn outline(args: &Args, feeds: &Feeds, looks: &[Value], state: &Value, step: u64) -> Step {
    match step {
        0 => {
            let dir = match project_dir(args, feeds) {
                Ok(dir) => dir,
                Err(why) => return refused(why.0, why.1),
            };
            match &args.session {
                Some(id) if is_full_id(id) => {
                    let file = format!("{dir}/{id}.jsonl");
                    greps(file)
                }
                _ => Step::Ask {
                    looks: vec![json!({"list": dir, "limit": 128})],
                    state: json!({"op": "outline", "step": 1, "dir": dir}),
                },
            }
        }
        1 => {
            let dir = string(state, "dir");
            let files = transcripts(&looks[0], &dir);
            match pick(&files, args.session.as_deref()) {
                Ok(file) => greps(file.path),
                Err(why) => why,
            }
        }
        2 => {
            // Both greps are in hand: the lines they named, in one read.
            let file = string(state, "file");
            let mut at: Vec<u64> = Vec::new();
            for look in looks {
                for m in look
                    .get("matches")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(n) = m.get("line").and_then(Value::as_u64) {
                        at.push(n);
                    }
                }
            }
            at.sort_unstable();
            at.dedup();
            let total = at.len();
            at.truncate(OUTLINE_LINES);
            if at.is_empty() {
                return ok(format!(
                    "`{file}` has no user or assistant rows in it — a transcript that never got \
                     past its header, or one written by a version that spelled the rows another \
                     way."
                ));
            }
            Step::Ask {
                looks: vec![json!({"read": file, "at": at})],
                state: json!({"op": "outline", "step": 3, "file": file, "of": total}),
            }
        }
        _ => {
            let file = string(state, "file");
            let total = state.get("of").and_then(Value::as_u64).unwrap_or(0) as usize;
            let look = &looks[0];
            if look.get("exit").and_then(Value::as_u64) != Some(0) {
                return failed(
                    format!("`{file}` could not be read"),
                    look.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("the host did not say why")
                        .to_string(),
                );
            }
            let lines = look
                .get("lines")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut asks = 0usize;
            let mut said = 0usize;
            let mut out = format!("`{file}` — what was asked and what was concluded:\n");
            for line in &lines {
                let Some(row) = row_of(&text_of(line)) else {
                    continue;
                };
                match row {
                    Row::Ask { text } => {
                        asks += 1;
                        out.push_str(&format!("\n  OPERATOR\n    {}\n", clip(&text, SHOW * 2)));
                    }
                    Row::Conclusion { text } => {
                        said += 1;
                        out.push_str(&format!("\n  assistant\n    {}\n", clip(&text, SHOW * 2)));
                    }
                }
            }
            if asks == 0 && said == 0 {
                out.push_str("\n  (nothing readable — the rows that matched did not parse)\n");
            }
            out.push_str(&format!(
                "\n{asks} ask(s) and {said} conclusion(s). Tool calls, tool results and the \
                 model's reasoning are left out on purpose — `search` and `read` have them.\n"
            ));
            if total > OUTLINE_LINES {
                out.push_str(&format!(
                    "{} row(s) of each kind were not shown: the outline stops at {}. `search` for \
                     a word you remember finds a later one.\n",
                    total - OUTLINE_LINES,
                    OUTLINE_LINES
                ));
            }
            ok(out)
        }
    }
}

/// Both kinds in one answer: the host runs the two greps and this round reads what they found.
fn greps(file: String) -> Step {
    Step::Ask {
        looks: vec![
            json!({"grep": file, "match": "\"type\":\"user\"", "limit": OUTLINE_LINES, "context": 0}),
            json!({"grep": file, "match": "\"type\":\"assistant\"", "limit": OUTLINE_LINES, "context": 0}),
        ],
        state: json!({"op": "outline", "step": 2, "file": file}),
    }
}

// ---------------------------------------------------------------------------------------------
// Claude Code's own line, as this tool reads it
// ---------------------------------------------------------------------------------------------

/// What one row of a transcript is, as far as an outline cares.
enum Row {
    Ask { text: String },
    Conclusion { text: String },
}

/// **One line of `~/.claude/projects/<project>/<uuid>.jsonl`**, read for what a reader would see.
///
/// The store is not documented and does not need to be: this reads the fields that are stable in
/// every version the tool has written — `type`, `isSidechain`, `userType`, `message.role` and
/// `message.content` — and answers `None` for a row that is not conversation. A row whose `text`
/// was cut by the host is not parsed at all rather than parsed into a half sentence.
fn row_of(text: &str) -> Option<Row> {
    if !text.trim_start().starts_with('{') || !text.trim_end().ends_with('}') {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    // A sidechain is a subagent's own conversation, replayed into the same file. An outline of
    // what the operator asked does not want forty of them interleaved with it.
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let kind = value.get("type").and_then(Value::as_str)?;
    let message = value.get("message")?;
    match kind {
        "user" => {
            // `userType: "internal"` is the harness talking to itself; a `content` that is a list
            // is a tool result being handed back. Neither is the operator asking anything.
            if let Some(t) = value.get("userType").and_then(Value::as_str) {
                if t != "external" {
                    return None;
                }
            }
            if message.get("role").and_then(Value::as_str) != Some("user") {
                return None;
            }
            let text = match message.get("content")? {
                Value::String(s) => s.clone(),
                Value::Array(parts) => text_parts(parts)?,
                _ => return None,
            };
            (!text.trim().is_empty()).then_some(Row::Ask { text })
        }
        "assistant" => {
            let parts = message.get("content")?.as_array()?;
            let text = text_parts(parts)?;
            (!text.trim().is_empty()).then_some(Row::Conclusion { text })
        }
        _ => None,
    }
}

/// The `text` blocks of a content list, joined — the model's words, without its thinking, its
/// tool calls or the results coming back.
fn text_parts(parts: &[Value]) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = part.get("text").and_then(Value::as_str)
        {
            out.push(text);
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// The operator's first line of a transcript: the first `user` row whose content is text.
fn first_operator_line(look: &Value) -> Option<String> {
    if look.get("exit").and_then(Value::as_u64) != Some(0) {
        return None;
    }
    for line in look.get("lines").and_then(Value::as_array)? {
        if line.get("cut").and_then(Value::as_bool) == Some(true) {
            // A cut line cannot be parsed, and half a sentence is worse than the next row.
            continue;
        }
        if let Some(Row::Ask { text }) = row_of(&text_of(line)) {
            return Some(text.replace('\n', " "));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Paths and names
// ---------------------------------------------------------------------------------------------

/// One transcript file, as a listing named it.
#[derive(Debug, Clone)]
struct File {
    path: String,
    project: String,
    size: u64,
    mtime: u64,
}

/// **The directory one call lists first**: a project's, or — with `all_projects` — the root that
/// holds every project.
fn listing_root(args: &Args, feeds: &Feeds) -> Result<String, (String, String)> {
    if args.all_projects {
        root(feeds)
    } else {
        project_dir(args, feeds)
    }
}

/// **Where a project's transcripts are.** `args.project` is a path (`/Users/me/Projects/x`) or the
/// directory name Claude Code gave it; absent is this session's workspace.
///
/// The name is the path with every character that is not a letter, a digit or `-` turned into `-`,
/// which is what `~/.claude/projects` holds on this box (`/Users/dead/.emacs.d` →
/// `-Users-dead--emacs-d`). It is a best-effort match and that is safe: a wrong guess is answered
/// with the directory that is missing, and `sessions` lists the names that exist.
fn project_dir(args: &Args, feeds: &Feeds) -> Result<String, (String, String)> {
    let root = root(feeds)?;
    let named = args
        .project
        .clone()
        .unwrap_or_else(|| feeds.workspace.clone());
    if named.is_empty() {
        return Err((
            "this call names no project, and the host did not say which workspace it is in"
                .to_string(),
            "Pass `project` with a path Claude Code has run in.".to_string(),
        ));
    }
    let slug = slug(&named, &feeds.home);
    Ok(format!("{root}/{slug}"))
}

/// `~/.claude/projects`, from the home directory the host handed over.
fn root(feeds: &Feeds) -> Result<String, (String, String)> {
    if feeds.home.is_empty() {
        return Err((
            "the host did not say where home is, and Claude Code's store is under it".to_string(),
            "This tool reads `~/.claude/projects`; without `$HOME` there is no `~` to expand."
                .to_string(),
        ));
    }
    Ok(format!("{}/.claude/projects", feeds.home))
}

fn slug(path: &str, home: &str) -> String {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        path.to_string()
    };
    expanded
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The `.jsonl` files a project listing named.
fn transcripts(look: &Value, dir: &str) -> Vec<File> {
    let project = dir.rsplit('/').next().unwrap_or(dir).to_string();
    look.get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.get("dir").and_then(Value::as_bool) != Some(true))
        .filter_map(|e| {
            let name = e.get("name").and_then(Value::as_str)?;
            name.ends_with(".jsonl").then(|| File {
                path: format!("{dir}/{name}"),
                project: project.clone(),
                size: e.get("size").and_then(Value::as_u64).unwrap_or(0),
                mtime: e.get("mtime").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect()
}

/// The directories a root listing named.
fn subdirs(look: &Value, dir: &str) -> Vec<String> {
    look.get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.get("dir").and_then(Value::as_bool) == Some(true))
        .filter_map(|e| e.get("name").and_then(Value::as_str))
        .map(|name| format!("{dir}/{name}"))
        .collect()
}

/// One transcript out of a project, by id or by the start of one. Absent is the newest.
fn pick(files: &[File], want: Option<&str>) -> Result<File, Step> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    match want {
        None => sorted.first().cloned().ok_or_else(|| {
            ok("this project has no transcript to read — the directory holds no `.jsonl`.")
        }),
        Some(want) => {
            let want = want.trim_end_matches(".jsonl");
            let hits: Vec<&File> = sorted
                .iter()
                .filter(|f| file_name(&f.path).starts_with(want))
                .collect();
            match hits.len() {
                1 => Ok(hits[0].clone()),
                0 => Err(failed(
                    format!("no transcript in this project starts with `{want}`"),
                    format!(
                        "The ones there are:\n{}",
                        sorted
                            .iter()
                            .take(FILES_PER_PROJECT)
                            .map(|f| format!(
                                "  {}  {}  {}",
                                file_name(&f.path),
                                size(f.size),
                                stamp(f.mtime, 0)
                            ))
                            .collect::<Vec<String>>()
                            .join("\n")
                    ),
                )),
                _ => Err(failed(
                    format!("`{want}` names {} transcripts", hits.len()),
                    format!(
                        "A session id is a UUID and the first eight characters are enough:\n{}",
                        hits.iter()
                            .map(|f| format!("  {}", file_name(&f.path)))
                            .collect::<Vec<String>>()
                            .join("\n")
                    ),
                )),
            }
        }
    }
}

fn is_full_id(id: &str) -> bool {
    id.len() == 36 && id.chars().filter(|c| *c == '-').count() == 4
}

fn file_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".jsonl")
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// Reading a look's result, and the small words an answer is made of
// ---------------------------------------------------------------------------------------------

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn n_of(line: &Value) -> u64 {
    line.get("n").and_then(Value::as_u64).unwrap_or(0)
}

fn text_of(line: &Value) -> String {
    line.get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `text` cut to `cap` characters, on a character boundary, with the cut said out loud.
fn clip(text: &str, cap: usize) -> String {
    let flat = text.replace('\n', " ⏎ ");
    if flat.chars().count() <= cap {
        return flat;
    }
    let cut: String = flat.chars().take(cap).collect();
    format!("{cut}… ({} more characters)", flat.chars().count() - cap)
}

/// A byte count a person can read at a glance.
fn size(bytes: u64) -> String {
    match bytes {
        0..=999 => format!("{bytes} B"),
        n if n < 1024 * 1024 => format!("{:.1} kB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}

/// **When a file last changed**, in the caller's own units: the clock, and how long ago.
///
/// The arithmetic is here rather than in a dependency because a guest is embedded in a binary and
/// a date library for two lines is not worth the bytes. `now_ms == 0` is a caller that has no
/// clock to compare with, and then only the stamp is printed.
fn stamp(ms: u64, now_ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let text = format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    );
    if now_ms == 0 || ms == 0 {
        return text;
    }
    let delta = now_ms.saturating_sub(ms) / 1000;
    let ago = match delta {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", delta / 60),
        3600..=86_399 => format!("{} h ago", delta / 3600),
        _ => format!("{} d ago", delta / 86_400),
    };
    format!("{text} ({ago})")
}

/// Days since 1970-01-01 to a civil date — Howard Hinnant's `civil_from_days`, which is exact for
/// every date this tool will ever see and needs no table.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The layer's policy, tested natively — see `tests.rs`.
#[cfg(test)]
mod tests;
