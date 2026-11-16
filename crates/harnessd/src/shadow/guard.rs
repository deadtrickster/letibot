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

/// Paths a read the shadow grants must not reach, beyond the secret stores: the daemon's own keys,
/// configuration and store, and the other agents' credentials and transcripts.
const OWN_CONFIG: [&str; 6] = [
    ".config/letibot",
    ".local/share/letibot",
    ".claude",
    ".codex",
    ".config/claude",
    "bin/fleet",
];

/// `Region::Device`: kernel and process state, never a file a review reads.
const DEVICE: [&str; 3] = ["/proc", "/sys", "/dev"];

/// `Ok` when the shadow may grant a read of `path`: absolute, not a secret store, credential
/// file, agent store or device, and no such place below it. Judged at the lexical path and at its
/// canonical key, since the grant binds the key and a symlink would carry the read elsewhere.
pub fn read_passes(path: &str, home: &Path) -> Result<(), String> {
    if !home.is_absolute() || home == Path::new("/") {
        return Err("no home directory is known, so a secret below it cannot be ruled out".into());
    }
    let p = Path::new(path.trim());
    if !p.is_absolute() {
        return Err(format!("`{path}` is not an absolute path"));
    }
    let lexical = lexical(p);
    let key = letibot_tools::view::key(&lexical);
    passes_at(path, &lexical, home)?;
    if key != lexical {
        passes_at(path, &key, home)
            .map_err(|e| format!("{e} (it resolves to `{}`)", key.display()))?;
    }
    Ok(())
}

fn passes_at(path: &str, p: &Path, home: &Path) -> Result<(), String> {
    if let Some(d) = DEVICE.iter().find(|d| p.starts_with(d)) {
        return Err(format!("`{path}` is under `{d}`, kernel and process state"));
    }
    if let Some(store) =
        letibot_tools::intent::Surroundings::secret_store_of(&p.to_string_lossy(), home.to_str())
    {
        return Err(format!("`{path}` is in the secret store `{store}`"));
    }
    for c in p.components() {
        let c = c.as_os_str().to_string_lossy();
        if c == "keys" {
            return Err(format!("`{path}` names a keys directory"));
        }
        if c.contains("credentials") {
            return Err(format!("`{path}` names a credentials file"));
        }
    }
    // A directory above a secret would put it in the view too: home itself, `/`, `/etc`.
    let mut below: Vec<PathBuf> = OWN_CONFIG.iter().map(|d| home.join(d)).collect();
    for d in letibot_tools::intent::secret_dirs() {
        match d.strip_prefix('/') {
            Some(_) => below.push(PathBuf::from(d)),
            None => below.push(home.join(d)),
        }
    }
    if let Some(d) = below.iter().find(|d| d.starts_with(p) || p.starts_with(d)) {
        return Err(format!("`{path}` reaches `{}`", d.display()));
    }
    Ok(())
}

/// The roots a shadow-granted read may land in: the asker's workspace and the worktrees of its
/// repository, read at ask time. Nothing is configured, so a box path is never a root by default.
pub fn read_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut out = vec![letibot_tools::view::key(workspace)];
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["worktree", "list", "--porcelain"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    if let Ok(o) = listed
        && o.status.success()
    {
        for l in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some(w) = l.strip_prefix("worktree ") {
                let k = letibot_tools::view::key(Path::new(w));
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
    }
    out
}

/// `Ok` when `path`'s canonical key is inside one of `roots`.
pub fn within_roots(path: &str, roots: &[PathBuf]) -> Result<(), String> {
    let key = letibot_tools::view::key(&lexical(Path::new(path.trim())));
    if roots.iter().any(|r| key.starts_with(r)) {
        return Ok(());
    }
    Err(format!(
        "`{path}` is outside the asker's repository ({})",
        roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
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
