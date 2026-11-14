//! The hard denylist a shadow's `allow` must still pass. Lexical and conservative: anything it
//! cannot rule out is `ask`, which is today's behaviour, so a shadow that is on is never less safe.

use std::path::{Component, Path, PathBuf};

/// The tool call a permission verdict is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub args: serde_json::Value,
    pub cwd: PathBuf,
    pub workspace: PathBuf,
    pub home: PathBuf,
}

/// Commands that raise privilege.
const PRIVILEGE: [&str; 5] = ["sudo", "doas", "pkexec", "su", "run0"];

/// Commands that write to the paths they are given.
const WRITERS: [&str; 15] = [
    "rm", "mv", "cp", "tee", "touch", "mkdir", "rmdir", "ln", "chmod", "chown", "dd", "install",
    "truncate", "rsync", "unlink",
];

/// Path arguments of the file tools.
const PATH_KEYS: [&str; 4] = ["path", "file_path", "filePath", "target"];

/// `Ok` when nothing on the denylist is in the call; `Err` names the first thing that is.
pub fn allow_passes(call: &Call) -> Result<(), String> {
    let command = call.args.get("command").and_then(|c| c.as_str());
    let text = command.map_or_else(|| call.args.to_string(), str::to_string);
    if text.contains(".config/letibot/keys") {
        return Err("it touches ~/.config/letibot/keys".into());
    }
    for key in PATH_KEYS {
        if let Some(p) = call.args.get(key).and_then(|p| p.as_str())
            && is_write_tool(&call.tool)
            && !inside(&resolve(p, call), &call.workspace)
        {
            return Err(format!("`{}` writes {p}, outside the workspace", call.tool));
        }
    }
    let Some(command) = command else {
        return Ok(());
    };
    for segment in command.split(['\n', ';', '&', '|', '(', ')', '`']) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        if let Some(w) = words.iter().find(|w| PRIVILEGE.contains(&base(w))) {
            return Err(format!("it raises privilege with `{w}`"));
        }
        if let Some(g) = words.iter().position(|w| base(w) == "git")
            && words[g..].contains(&"push")
        {
            return Err("it pushes".into());
        }
        let writes = words.first().is_some_and(|w| WRITERS.contains(&base(w)))
            || (words.first().is_some_and(|w| base(w) == "sed")
                && words.iter().any(|w| w.starts_with("-i")));
        let redirect = redirect_target(segment);
        let targets: Vec<&str> = if writes {
            words
                .iter()
                .skip(1)
                .filter(|w| !w.starts_with('-'))
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        for t in targets.into_iter().chain(redirect) {
            if matches!(t, "/dev/null" | "/dev/stdout" | "/dev/stderr") {
                continue;
            }
            if !inside(&resolve(t, call), &call.workspace) {
                return Err(format!("it writes {t}, outside the workspace"));
            }
        }
    }
    Ok(())
}

fn is_write_tool(tool: &str) -> bool {
    matches!(
        tool,
        "write" | "edit" | "multiedit" | "apply_patch" | "patch" | "notebook_edit"
    )
}

/// `/usr/bin/sudo` is `sudo`.
fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// What `>` or `>>` writes to, when the segment redirects. `2>&1` and friends write no file.
fn redirect_target(segment: &str) -> Option<&str> {
    let at = segment.find('>')?;
    let rest = segment[at..].trim_start_matches('>').trim_start();
    let target = rest.split_whitespace().next()?;
    (!target.starts_with('&')).then_some(target)
}

/// A path as the call would reach it, normalised lexically: `~` is home, relative is the cwd's.
fn resolve(p: &str, call: &Call) -> PathBuf {
    let p = p.trim_matches(['"', '\'']);
    let joined = if let Some(rest) = p.strip_prefix("~/") {
        call.home.join(rest)
    } else if p == "~" {
        call.home.clone()
    } else if p.starts_with("$HOME") || p.starts_with("${HOME}") {
        // A variable this cannot expand is a path it cannot place, so it is outside.
        PathBuf::from("/")
    } else {
        call.cwd.join(p)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn inside(p: &Path, workspace: &Path) -> bool {
    p.starts_with(workspace)
}
