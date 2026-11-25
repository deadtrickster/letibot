//! The hard denylist a shadow's `allow` must still pass. Lexical first, and conservative: anything
//! it cannot rule out is `ask`, which is today's behaviour, so a shadow that is on is never less safe.

use std::path::{Component, Path, PathBuf};

/// The tool call a permission verdict is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub args: serde_json::Value,
    pub cwd: PathBuf,
    pub workspace: PathBuf,
    pub home: PathBuf,
    /// The roots the operator granted this session writable, read when the call was judged. A
    /// folder granted writable is a workspace to this denylist too, which is what lets a delete
    /// inside one reach the stage that answers it instead of falling to the person.
    pub granted: Vec<PathBuf>,
    /// Temp space this session's own shell has beyond the places layer A calls temp: `$TMPDIR`,
    /// which a box or a seat may point anywhere. Read by [`temp_roots`], never by a tool's word.
    pub temp: Vec<PathBuf>,
}

/// Commands that raise privilege.
const PRIVILEGE: [&str; 5] = ["sudo", "doas", "pkexec", "su", "run0"];

/// Whether the lexical screen refuses a `git push`. Every stage but `publish` does, because none of
/// them was promoted knowing a call can leave the box; `publish` exists so the reviewer's
/// categories judge the push instead of a lexical refusal that answers for nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushes {
    Refused,
    Answered,
}

/// Commands that write to the paths they are given.
const WRITERS: [&str; 15] = [
    "rm", "mv", "cp", "tee", "touch", "mkdir", "rmdir", "ln", "chmod", "chown", "dd", "install",
    "truncate", "rsync", "unlink",
];

/// Path arguments of the file tools.
const PATH_KEYS: [&str; 4] = ["path", "file_path", "filePath", "target"];

/// `Ok` when nothing on the denylist is in the call; `Err` names the first thing that is.
pub fn allow_passes(call: &Call, pushes: Pushes) -> Result<(), String> {
    let command = call.args.get("command").and_then(|c| c.as_str());
    let text = command.map_or_else(|| call.args.to_string(), str::to_string);
    if text.contains(".config/letibot/keys") {
        return Err("it touches ~/.config/letibot/keys".into());
    }
    for key in PATH_KEYS {
        if let Some(p) = call.args.get(key).and_then(|p| p.as_str())
            && is_write_tool(&call.tool)
            && !inside(&resolve(p, call), call)
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
            && pushes == Pushes::Refused
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
            if !inside(&resolve(t, call), call) {
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

/// The key store is the ONE store layer A cannot place: it is not a `secrets::STORES` entry, so a
/// command naming it needs this screen wherever it sits. This asks that and only that, which is what
/// a RESOLVED reading gets - its placed operands are layer A's business, and asking every word of
/// one refuses a project's own `keys/` directory under the home.
pub fn key_store_pass(command: &str) -> Result<(), String> {
    if command.contains(".config/letibot/keys") {
        return Err("it touches ~/.config/letibot/keys".into());
    }
    Ok(())
}

/// `Ok` when no word of a command layer A could NOT read names a secret store, a `keys`
/// directory or a credential file. A resolved reading screens the OPERANDS it placed ([`key_store_pass`]
/// is the one store it cannot); one that did not resolve has not shown which of its words are places
/// at all, so each word is asked the question a resolved target is asked. No model is involved and
/// nothing on the ladder can overrule it, which is what keeps `X=~/.ssh/id_ed25519; cat "$X"` a
/// refusal rather than a reviewer's allow.
///
/// Only the DISCLOSURE half of [`read_passes`]: its review list refuses a place a granted READ must
/// not reach - the session's own data directory among them - and an inspection of that is ordinary
/// work, not a secret.
pub fn words_pass(command: &str, call: &Call) -> Result<(), String> {
    key_store_pass(command)?;
    for word in words_of(command) {
        // `$HOME` is the home the way `~` is; a variable this build cannot expand would be placed
        // at `/`, which names no store and would read as harmless.
        let spelled = match word
            .strip_prefix("${HOME}")
            .or_else(|| word.strip_prefix("$HOME"))
        {
            Some(rest) => format!("~{rest}"),
            None => word.to_string(),
        };
        let place = resolve(&spelled, call);
        if let Some(store) = letibot_tools::intent::Surroundings::secret_store_of(
            &place.to_string_lossy(),
            call.home.to_str(),
        ) {
            return Err(format!("`{word}` is in the secret store `{store}`"));
        }
        // Only under the home: a project's own `keys/` directory is its source, and the store this
        // rule is about is the operator's.
        if place.starts_with(&call.home)
            && place
                .components()
                .any(|c| c.as_os_str().to_string_lossy() == "keys")
        {
            return Err(format!("`{word}` names a keys directory"));
        }
    }
    Ok(())
}

/// The words of a command that could be a place: split on shell punctuation, with an assignment's
/// `NAME=` and an option's leading dashes taken off. `$` is not a separator, or `$HOME/.ssh` would
/// arrive as `HOME` and a path at `/`, which names no store.
fn words_of(command: &str) -> impl Iterator<Item = &str> {
    command
        .split(|c: char| c.is_whitespace() || "'\"=:;|&()<>`".contains(c))
        .map(|w| w.trim_start_matches('-'))
        .filter(|w| !w.is_empty())
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

/// Whether a write may land here: the workspace, a folder the operator granted writable, or temp.
/// Judged at the path the call spelled and at the key it really names, since a symlink in temp
/// carries the write wherever it points.
fn inside(p: &Path, call: &Call) -> bool {
    let spelled: Vec<PathBuf> = std::iter::once(call.workspace.clone())
        .chain(call.granted.iter().cloned())
        .chain(call.temp.iter().cloned())
        .collect();
    let keyed: Vec<PathBuf> = spelled
        .iter()
        .map(|r| letibot_tools::view::key(r))
        .collect();
    holds(p, &spelled, &call.home) && holds(&letibot_tools::view::key(p), &keyed, &call.home)
}

/// One half of [`inside`]: under a root, or in temp space. Temp is the session's own scratch, so a
/// secret store is never it - a symlink in `/tmp` that points into one is that store's write - and
/// neither is the operator's home.
fn holds(p: &Path, roots: &[PathBuf], home: &Path) -> bool {
    if roots.iter().any(|r| p.starts_with(r)) {
        return true;
    }
    if (!home.as_os_str().is_empty() && p.starts_with(home))
        || letibot_tools::intent::Surroundings::secret_store_of(&p.to_string_lossy(), home.to_str())
            .is_some()
    {
        return false;
    }
    letibot_tools::intent::TEMP_PREFIXES
        .iter()
        .any(|(t, written)| *written && p.starts_with(t))
}

/// The temp roots a session's own shell has that layer A's table does not name: `$TMPDIR`, which a
/// box or a seat may point anywhere. A directory that is the operator's home (or holds it) or a
/// secret store is not scratch, and is dropped rather than trusted.
pub fn temp_roots(home: &Path) -> Vec<PathBuf> {
    let Some(dir) = std::env::var_os("TMPDIR").map(PathBuf::from) else {
        return Vec::new();
    };
    if !dir.is_absolute() || home.starts_with(&dir) {
        return Vec::new();
    }
    if letibot_tools::intent::Surroundings::secret_store_of(&dir.to_string_lossy(), home.to_str())
        .is_some()
    {
        return Vec::new();
    }
    vec![dir]
}

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
    // A directory above a secret would put it in the view too: home itself, `/`, `/etc`. The
    // table's review entries add the daemon's own files, agent homes and kernel state.
    let below: Vec<PathBuf> = letibot_tools::secrets::review_denied()
        .into_iter()
        .map(|d| home.join(d))
        .collect();
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
