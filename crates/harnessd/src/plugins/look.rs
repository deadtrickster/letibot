//! Looks, slice 1: `{"read": path}` and nothing else. A plugin never does I/O.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_plugins::Capability;
use letibot_sessionlog::pane::LookSpec;
use letibot_tools::backend::ExecBackend;
use letibot_tools::runtime::GateDecision;
use letibot_tools::schema::Access;
use serde_json::{Value, json};

use crate::plugins::tool::Decides;

/// How much of a file one look may hand a plugin. A megabyte is a status file, a plan, a log tail -
/// and a pane that read a gigabyte would spend a session's memory on a screen nobody asked for.
pub const CAP: usize = 1 << 20;

/// The word a refused `exec` look carries. One constant, because it is in the answer a plugin
/// may echo into a panel.
pub const EXEC_REFUSED: &str = "exec looks are not in slice 1";

/// The trees a look reads in, the checkout first: the repository the session's tree belongs to, which
/// for a worktree is the checkout it was cut from - where a project's `.letibot` files live. The tree
/// the session stands in is read too, so a worktree's own file is still an answer.
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

/// What one look may reach: the trees it resolves in, the capabilities the answering layer
/// declared, the session's view where there is one, and whether that layer's bytes are pinned.
#[derive(Clone, Copy)]
pub struct Reach<'a> {
    pub trees: Trees<'a>,
    pub caps: &'a [Capability],
    /// The session's read door. `None` is a host nothing handed one to - a test, or a session the
    /// daemon has not opened yet - and then a read is this process's, bounded by the capabilities.
    pub view: Option<&'a dyn Decides>,
    /// A layer whose bytes a person pinned, which is the only layer a `read:<path>` tree is
    /// honoured for: the pin is the vetted thing, and a tree outside the workspace is reach.
    pub trusted: bool,
}

impl<'a> Reach<'a> {
    /// A look fenced by its capabilities alone: no view, nothing pinned. What a caller that has
    /// neither a session nor a trust book can honestly say.
    pub fn caps(trees: Trees<'a>, caps: &'a [Capability]) -> Reach<'a> {
        Reach {
            trees,
            caps,
            view: None,
            trusted: false,
        }
    }
}

/// The session's read door, for the looks that run under a host rather than a tool call: a pane's
/// look and a condition's, which have no `InvokeCtx` to ask. It is the backend the model's file
/// tools read through, so a session seated on another machine or in a VM is read there.
pub struct SessionView {
    backend: Arc<dyn ExecBackend>,
}

impl SessionView {
    pub fn new(backend: Arc<dyn ExecBackend>) -> SessionView {
        SessionView { backend }
    }
}

impl Decides for SessionView {
    /// No gate: an `exec` look asks the one its own host holds (`conditions::Unasked`), and a
    /// pane's is refused outright. `None` here is *this door did not judge it*, not *admit*.
    fn admit_unasked(&self, _name: &str, _access: Access, _args: &Value) -> Option<GateDecision> {
        None
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        self.backend.read(path).map_err(|e| e.to_string())
    }

    fn list(&self, path: &str) -> Result<Vec<letibot_tools::backend::DirEntry>, String> {
        self.backend.list(path).map_err(|e| e.to_string())
    }

    fn run(&self, argv: &[String]) -> Result<letibot_tools::backend::Output, String> {
        self.backend
            .run(&letibot_tools::backend::Command {
                argv: argv.to_vec(),
                cwd: ".".to_string(),
                env: Vec::new(),
            })
            .map_err(|e| e.to_string())
    }
}

/// Run one look the pane asked for, within what the answering layer reaches.
pub fn run(spec: &LookSpec, reach: Reach) -> Value {
    match spec {
        LookSpec::Read { read } => read_file(read, reach),
        // 126 is the shell's own *found but not executable*, so a plugin written for slice 2 draws
        // an unavailable panel instead of an empty one that looks like a finished read.
        LookSpec::Exec { .. } => {
            json!({"exit": 126, "stdout": "", "stderr": EXEC_REFUSED})
        }
    }
}

/// `{"exit":0,"stdout":<the file>,"stderr":""}`, or `exit 1` with the reason in `stderr`.
fn read_file(path: &str, reach: Reach) -> Value {
    let trees = reach.trees;
    let caps = reach.caps;
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
    // A `read:<path>` tree is honoured only for a layer whose bytes a person pinned. The pin is
    // what vetted the declaration; an unpinned layer arrives with the checkout, so its declared
    // tree is kept aside and the refusal below names it rather than silently widening.
    let mut ignored: Vec<(String, PathBuf)> = Vec::new();
    for cap in caps {
        let Capability::ReadPath(declared) = cap else {
            continue;
        };
        if let Some(tree) = crate::plugins::tool::expand(declared) {
            let real = tree.canonicalize().unwrap_or(tree);
            let what = format!("`read:{declared}`");
            if reach.trusted {
                roots.push((what, real));
            } else {
                ignored.push((what, real));
            }
        }
    }
    if roots.is_empty() && ignored.is_empty() {
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
        // A file inside a tree the layer declared and nobody vetted says so, with the way out:
        // *outside the workspace* is true and useless about a reach that was declared and dropped.
        if let Some((what, _)) = ignored.iter().find(|(_, tree)| real.starts_with(tree)) {
            return refused(format!(
                "`{path}` resolves to {}, inside the tree this layer declared as {what}, and an \
                 unpinned layer's declared tree is not honoured: pin the layer's bytes and load it \
                 again if that reach was meant",
                real.display()
            ));
        }
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
    // Through the session's view when there is one: the fence a command obeys, and the substrate
    // the session stands on, so a pane in a VM or on another host is not answered off this disk.
    let mut bytes = match reach.view {
        Some(view) => match view.read(&real.display().to_string()) {
            Ok(bytes) => bytes,
            Err(why) => return refused(why),
        },
        None => {
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
            // One byte over the cap is read so a file of exactly `CAP` is not reported as
            // truncated, and the text is cut back afterwards.
            if let Err(e) = file.take(CAP as u64 + 1).read_to_end(&mut bytes) {
                return refused(format!("{}: {e}", real.display()));
            }
            bytes
        }
    };
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
