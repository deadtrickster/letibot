//! **Looks, slice 1: `{"read": path}` and nothing else.**
//!
//! A plugin never does I/O (`docs/plugins-plan.md` §1, principle 1). When a pane needs the
//! world, it *declares* a look in its answer and the host runs it — so the wasm call never blocks
//! on a filesystem, the same input always gives the same output, and the capability a manifest
//! declared is checked at the one moment it means something: here, by the host, against the layer
//! that answered.
//!
//! # Why the answer is a look RESULT and not an error
//!
//! Everything that can go wrong — no `read` capability, a path outside the workspace, a file that
//! is not there — is answered `{"exit":1,"stderr":"…"}` rather than raised, because that is what
//! the contract says a look produces and because the plugin is the half that knows how to draw a
//! refusal: the example dashboard turns a non-zero exit into a panel saying what it could not read
//! and how to fix it (C3's *"a panel that cannot get its data says why and how to fix it"*). An
//! `Err` raised here would reach the pane as a failure with no plugin involved, which is the same
//! screen with less said on it.
//!
//! # Confinement
//!
//! `docs/plugins-plan.md` §3: `read` is *"the same confinement `read` has"* — the workspace root
//! and nothing above it. Both paths are canonicalised before the comparison, so a symlink or a
//! `..` cannot spell its way out: the check is on what the path *resolves to*, not on what it
//! says. An absolute path is not refused by name, because canonicalising it is the same check and
//! a better one — `/etc/passwd` simply resolves outside the root.
//!
//! # `exec` is refused, and says which slice it is missing from
//!
//! Slice 1 runs no process (C3 lists `exec` in the vocabulary; W4's brief says *do not run
//! anything*), and the answer is `exit 126` — the shell's own *"found but not executable"* —
//! rather than a false success, so a plugin written for slice 2 draws an unavailable panel instead
//! of an empty one.

use std::io::Read;
use std::path::Path;

use letibot_plugins::Capability;
use letibot_sessionlog::pane::LookSpec;
use serde_json::{Value, json};

/// **How much of a file one look may hand a plugin.** A megabyte is a status file, a plan, a log
/// tail — C3's looks are for dashboards, and a pane that read a gigabyte would spend a session's
/// memory on a screen nobody asked to be that big.
pub const CAP: usize = 1 << 20;

/// The word a refused `exec` look carries. One constant, because it is in the answer a plugin
/// may echo into a panel.
pub const EXEC_REFUSED: &str = "exec looks are not in slice 1";

/// Run one look the pane asked for, against the workspace `root`, with the capabilities the
/// answering layer declared.
pub fn run(spec: &LookSpec, root: &Path, caps: &[Capability]) -> Value {
    match spec {
        LookSpec::Read { read } => read_file(read, root, caps),
        // 126 is the shell's own *found but not executable*, so a plugin written for slice 2
        // draws an unavailable panel instead of an empty one that looks like a finished read.
        LookSpec::Exec { .. } => {
            json!({"exit": 126, "stdout": "", "stderr": EXEC_REFUSED})
        }
    }
}

/// `{"exit":0,"stdout":<the file>,"stderr":""}`, or `exit 1` with the reason in `stderr`.
fn read_file(path: &str, root: &Path, caps: &[Capability]) -> Value {
    if !caps.contains(&Capability::Read) {
        return refused(format!(
            "the layer did not declare the `read` capability, so `{path}` is not read for it"
        ));
    }
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(e) => {
            return refused(format!(
                "the session's workspace {} cannot be resolved: {e}",
                root.display()
            ));
        }
    };
    // A path that is absolute replaces the root in `join`, and that is fine: the containment
    // check below is on what the path resolves to, so an absolute path outside the workspace is
    // refused for being outside it rather than for being absolute.
    let wanted = root.join(path);
    let real = match wanted.canonicalize() {
        Ok(real) => real,
        Err(e) => return refused(format!("{}: {e}", wanted.display())),
    };
    if !real.starts_with(&root) {
        return refused(format!(
            "`{path}` resolves to {}, which is outside the session's workspace {}",
            real.display(),
            root.display()
        ));
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
    // the text is cut back to `CAP` afterwards. A lossy decode because a plugin asked for a text
    // and a plan with a stray byte in it is still a plan.
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

/// The one shape every refusal has: a non-zero exit and the sentence in `stderr`, which is where
/// a shell and C3's look result both put it.
fn refused(why: impl AsRef<str>) -> Value {
    json!({"exit": 1, "stdout": "", "stderr": why.as_ref()})
}
