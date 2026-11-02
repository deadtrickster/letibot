//! Looks, slice 1: `{"read": path}` and nothing else. A plugin never does I/O. When a pane
//! needs the world it declares a look in its answer and the host runs it, so the wasm call
//! never blocks on a filesystem and the capability a manifest declared is checked where it
//! means something: here, against the layer that answered. Everything that can go wrong is
//! a look RESULT (`exit 1` with the reason in `stderr`) and not an `Err`: the plugin is the
//! half that knows how to draw a refusal, and a raised error would reach the pane as a
//! failure with no plugin involved - the same screen with less said on it. Confinement:

use std::io::Read;
use std::path::Path;

use letibot_plugins::Capability;
use letibot_sessionlog::pane::LookSpec;
use serde_json::{Value, json};

/// How much of a file one look may hand a plugin. A megabyte is a status file, a plan, a log tail -
/// and a pane that read a gigabyte would spend a session's memory on a screen nobody asked for.
pub const CAP: usize = 1 << 20;

/// The word a refused `exec` look carries. One constant, because it is in the answer a plugin
/// may echo into a panel.
pub const EXEC_REFUSED: &str = "exec looks are not in slice 1";

/// Run one look the pane asked for, against the workspace `root`, with the capabilities the
/// answering layer declared.
pub fn run(spec: &LookSpec, root: &Path, caps: &[Capability]) -> Value {
    match spec {
        LookSpec::Read { read } => read_file(read, root, caps),
        // 126 is the shell's own *found but not executable*, so a plugin written for slice 2 draws
        // an unavailable panel instead of an empty one that looks like a finished read.
        LookSpec::Exec { .. } => {
            json!({"exit": 126, "stdout": "", "stderr": EXEC_REFUSED})
        }
    }
}

/// `{"exit":0,"stdout":<the file>,"stderr":""}`, or `exit 1` with the reason in `stderr`.
fn read_file(path: &str, root: &Path, caps: &[Capability]) -> Value {
    // Every tree this layer may read: the workspace if it declared `read`, and each tree it named
    // as `read:<path>`. Nothing else - the declaration is the approval.
    let mut roots: Vec<(String, std::path::PathBuf)> = Vec::new();
    if caps.contains(&Capability::Read) {
        match root.canonicalize() {
            Ok(real) => roots.push((root.display().to_string(), real)),
            Err(e) => {
                return refused(format!(
                    "the session's workspace {} cannot be resolved: {e}",
                    root.display()
                ));
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
    // An absolute path replaces the root in `join`, and that is fine: the check below is on what
    // the path resolves to, so one outside every tree is refused for being outside it.
    let wanted = if path.starts_with("~/") {
        match crate::plugins::tool::expand(path) {
            Some(p) => p,
            None => return refused(format!("`{path}` needs $HOME, and this daemon has none")),
        }
    } else {
        root.join(path)
    };
    let real = match wanted.canonicalize() {
        Ok(real) => real,
        Err(e) => return refused(format!("{}: {e}", wanted.display())),
    };
    let inside = roots.iter().any(|(_, tree)| real.starts_with(tree));
    if !inside {
        let named: Vec<String> = roots.iter().map(|(what, _)| what.clone()).collect();
        // The workspace's refusal keeps its own words: it is the sentence a project's own layer
        // reads most often, and a test asserts it.
        let why = if named.len() == 1 && named[0] == root.display().to_string() {
            format!(
                "`{path}` resolves to {}, which is outside the session's workspace {}",
                real.display(),
                root.display()
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
