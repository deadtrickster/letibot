//! Looks, slice 1: `{"read": path}` and nothing else. A plugin never does I/O.

use std::io::Read;
use std::path::{Path, PathBuf};

use letibot_plugins::Capability;
use letibot_sessionlog::pane::LookSpec;
use serde_json::{Value, json};

/// How much of a file one look may hand a plugin. A megabyte is a status file, a plan, a log tail -
/// and a pane that read a gigabyte would spend a session's memory on a screen nobody asked for.
pub const CAP: usize = 1 << 20;

/// The word a refused `exec` look carries. One constant, because it is in the answer a plugin
/// may echo into a panel.
pub const EXEC_REFUSED: &str = "exec looks are not in slice 1";

/// The trees a look reads in, the checkout first: the repository the session's tree belongs to,
/// which for a worktree is the checkout it was cut from - where a project's `.letibot` files live.
/// The tree the session stands in is read too, so a worktree's own file is still an answer (R151).
#[derive(Debug, Clone, Copy)]
pub struct Trees<'a> {
    /// The session's checkout: where a relative path is looked for first.
    pub checkout: &'a Path,
    /// The tree the session stands in: read as well, so a worktree's own files are not lost.
    pub workspace: &'a Path,
}

impl<'a> Trees<'a> {
    pub fn new(checkout: &'a Path, workspace: &'a Path) -> Trees<'a> {
        Trees {
            checkout,
            workspace,
        }
    }

    /// One tree, for a caller that has no second one: a test, a session whose tree is a checkout.
    pub fn one(dir: &'a Path) -> Trees<'a> {
        Trees {
            checkout: dir,
            workspace: dir,
        }
    }

    /// The trees a relative path is looked for in, in order, without repeats.
    fn candidates(&self) -> Vec<PathBuf> {
        let mut out = vec![self.checkout.to_path_buf()];
        if self.workspace != self.checkout {
            out.push(self.workspace.to_path_buf());
        }
        out
    }
}

/// Run one look the pane asked for, against the session's trees, with the capabilities the
/// answering layer declared.
pub fn run(spec: &LookSpec, trees: Trees, caps: &[Capability]) -> Value {
    match spec {
        LookSpec::Read { read } => read_file(read, trees, caps),
        // 126 is the shell's own *found but not executable*, so a plugin written for slice 2 draws
        // an unavailable panel instead of an empty one that looks like a finished read.
        LookSpec::Exec { .. } => {
            json!({"exit": 126, "stdout": "", "stderr": EXEC_REFUSED})
        }
    }
}

/// `{"exit":0,"stdout":<the file>,"stderr":""}`, or `exit 1` with the reason in `stderr`.
fn read_file(path: &str, trees: Trees, caps: &[Capability]) -> Value {
    // Every tree this layer may read: the session's checkout and its own tree if it declared `read`,
    // and each tree it named as `read:<path>`. Nothing else - the declaration is the approval.
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    if caps.contains(&Capability::Read) {
        for root in [trees.checkout, trees.workspace] {
            if roots.iter().any(|(_, seen)| seen == root) {
                continue;
            }
            match root.canonicalize() {
                Ok(real) => roots.push((root.display().to_string(), real)),
                Err(e) => {
                    return refused(format!(
                        "the session's checkout {} cannot be resolved: {e}",
                        root.display()
                    ));
                }
            }
        }
    }
    for cap in caps {
        let Capability::ReadPath(declared) = cap else {
            continue;
        };
        if let Some(tree) = crate::plugins::tool::expand(declared) {
            let real = tree.canonicalize().unwrap_or(tree);
            roots.push((format!("`read:{declared}`"), real));
        }
    }
    if roots.is_empty() {
        return refused(format!(
            "the layer did not declare the `read` capability (and names no `read:<path>` tree), \
             so `{path}` is not read for it"
        ));
    }
    // A relative path is the checkout's first and the session's own tree after it (see [`Trees`]);
    // an absolute one, or `~/`, replaces both.
    let tried: Vec<PathBuf> = if path.starts_with("~/") {
        match crate::plugins::tool::expand(path) {
            Some(p) => vec![p],
            None => return refused(format!("`{path}` needs $HOME, and this daemon has none")),
        }
    } else if path.starts_with('/') {
        vec![PathBuf::from(path)]
    } else {
        trees
            .candidates()
            .into_iter()
            .map(|root| root.join(path))
            .collect()
    };
    let mut missing: Vec<String> = Vec::new();
    let mut found: Option<(PathBuf, PathBuf)> = None;
    for wanted in &tried {
        match wanted.canonicalize() {
            Ok(real) => {
                found = Some((wanted.clone(), real));
                break;
            }
            Err(e) => missing.push(format!("{}: {e}", wanted.display())),
        }
    }
    let Some((_, real)) = found else {
        // The paths it tried, named: a refusal that says only "not found" sends the reader back to
        // guess which tree the host looked in.
        return refused(format!(
            "`{path}` is not there: {} (the session's checkout is {})",
            missing.join(", "),
            trees.checkout.display()
        ));
    };
    let inside = roots.iter().any(|(_, tree)| real.starts_with(tree));
    if !inside {
        let named: Vec<String> = roots.iter().map(|(what, _)| what.clone()).collect();
        // The one-tree refusal keeps its own words: it is the sentence a project's own layer reads
        // most often, and a test asserts it.
        let why = if named.len() == 1 && named[0] == trees.checkout.display().to_string() {
            format!(
                "`{path}` resolves to {}, which is outside the session's workspace {}",
                real.display(),
                trees.checkout.display()
            )
        } else {
            format!(
                "`{path}` resolves to {}, which is outside every tree this layer may read ({})",
                real.display(),
                named.join(", ")
            )
        };
        return refused(why);
    }
    match real.metadata() {
        Ok(meta) if meta.is_dir() => {
            return refused(format!("`{path}` is a directory, and a look reads a file"));
        }
        Ok(_) => {}
        Err(e) => return refused(format!("{}: {e}", real.display())),
    }
    let file = match std::fs::File::open(&real) {
        Ok(file) => file,
        Err(e) => return refused(format!("{}: {e}", real.display())),
    };
    let mut bytes = Vec::new();
    // One byte over the cap is read so a file of exactly `CAP` is not reported as truncated, and
    // the text is cut back afterwards. Lossy, because a plan with a stray byte is still a plan.
    if let Err(e) = file.take(CAP as u64 + 1).read_to_end(&mut bytes) {
        return refused(format!("{}: {e}", real.display()));
    }
    bytes.truncate(CAP);
    json!({
        "exit": 0,
        "stdout": String::from_utf8_lossy(&bytes),
        "stderr": "",
    })
}

/// The one shape every refusal has: a non-zero exit and the sentence in `stderr`, which is where a
/// shell and a look result both put it.
fn refused(why: impl AsRef<str>) -> Value {
    json!({"exit": 1, "stdout": "", "stderr": why.as_ref()})
}
