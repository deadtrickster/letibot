//! A directory's own `AGENTS.md`, handed to a SUBAGENT the first time a tool touches it.
//!
//! The operator's ruling: the tree's instructions are gold for subagents. A root session already
//! reads the workspace's `AGENTS.md` into its standing notes at open, and its prompt is frozen - but
//! a child is opened to do one thing in one directory, and a file that arrived with message zero
//! would be a file about a directory it may never enter.
//!
//! So: at the round boundary after a call named a path, every directory between the workspace root
//! and that path that carries an `AGENTS.md` and has not been handed over yet is injected, once, as
//! a `[guidance]` notice. Nothing is appended between a tool call and its result - the sentence goes
//! through the mid-turn queue, which the loop drains at its own step boundary - and nothing about a
//! MAIN session changes: `on` is false for a root.

use std::collections::BTreeSet;
use std::path::Path;

use letibot_tools::backend::ExecBackend;

/// The three spellings the merge gate accepts, in the order a reader looks for them: on a
/// case-sensitive host `agents.md` is not `AGENTS.md`.
pub const NAMES: [&str; 3] = ["AGENTS.md", "agents.md", "Agents.md"];

/// What one session has been told, and whether it is a session this applies to at all.
#[derive(Debug, Clone, Default)]
pub struct Guidance {
    on: bool,
    /// The directories whose file this session already has, relative to its root, `/`-joined. The
    /// root itself is in here for a session whose standing notes carried it.
    seen: BTreeSet<String>,
}

impl Guidance {
    /// A session's book, with the directories it was handed EAGERLY - the workspace root, whose file
    /// is in its prompt, and the ones its role profile names in `guidance:`.
    pub fn new(on: bool, already: impl IntoIterator<Item = String>) -> Guidance {
        Guidance {
            on,
            seen: already.into_iter().collect(),
        }
    }

    /// The files this round's calls touched and nobody has handed over yet, as (path as the model
    /// spells it, the file's text). Marks them seen, so a second call in the same directory says
    /// nothing: an instruction file read twice is prompt bytes for nothing.
    ///
    /// `root` is the workspace as the BACKEND spells it, because a placed session's tree is on that
    /// host and the daemon's own path is a tree the session cannot see.
    pub fn take(
        &mut self,
        backend: &dyn ExecBackend,
        root: &Path,
        calls: &[letibot_transcript::ToolCall],
    ) -> Vec<(String, String)> {
        if !self.on {
            return Vec::new();
        }
        let mut out: Vec<(String, String)> = Vec::new();
        for call in calls {
            let args: serde_json::Value = match serde_json::from_str(&call.arguments) {
                Ok(v) => v,
                // A call whose arguments are not JSON produced a refusal, and a refusal is not a
                // touch: nothing to read and nothing to say.
                Err(_) => continue,
            };
            for dir in letibot_tools::profiles::touched(&args) {
                if self.seen.contains(&dir) {
                    continue;
                }
                let Some((name, text)) = read(backend, root, &dir) else {
                    // No file here. Not marked seen: a later call could be looking at a directory
                    // whose file arrives with a checkout.
                    continue;
                };
                self.seen.insert(dir.clone());
                out.push((format!("{dir}/{name}"), text));
            }
        }
        out
    }
}

/// One directory's file, read through the session's own backend: the spelling that is there and
/// its text. `None` is *there is no such file*, which is the ordinary case - most directories have
/// none - and a read that fails is not news.
fn read(backend: &dyn ExecBackend, root: &Path, dir: &str) -> Option<(&'static str, String)> {
    for name in NAMES {
        let path = root.join(dir).join(name);
        let rel = path
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        if let Ok(bytes) = backend.read(&rel)
            && let Ok(text) = String::from_utf8(bytes)
            && !text.trim().is_empty()
        {
            return Some((name, text));
        }
    }
    None
}

/// The sentence the model reads. The file's text is verbatim and fenced: a header line and its
/// instructions, so a child can tell an instruction file from its own parent's words.
pub fn notice(path: &str, text: &str) -> String {
    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    format!(
        "[guidance] `{path}` — instructions the operator wrote for that directory, which a tool \
         call just touched ({lines} lines). Read them before you work further in it.\n\n{text}"
    )
}

/// The directories a session is told about EAGERLY, from its role profile: `guidance:` in the
/// file, `/`-normalised, empty entries dropped.
pub fn named(dirs: &[String]) -> Vec<String> {
    dirs.iter()
        .map(|d| d.trim().trim_matches('/').replace('\\', "/"))
        .filter(|d| !d.is_empty() && d != ".")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_transcript::ToolCall;

    /// A backend over a real directory: the fixture's tree is the test's own.
    fn backend(root: &std::path::Path) -> letibot_tools::backend::HostBackend {
        letibot_tools::backend::HostBackend::new(root).expect("a fixture root")
    }

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    fn tree() -> (letibot_tools::backend::tempdir::TempDir, std::path::PathBuf) {
        let d = letibot_tools::backend::tempdir::TempDir::new();
        let root = d.path().to_path_buf();
        std::fs::create_dir_all(root.join("crates/tools/src")).expect("dirs");
        std::fs::write(root.join("crates/AGENTS.md"), "the crates rules\n").expect("write");
        std::fs::write(root.join("crates/tools/AGENTS.md"), "the tools rules\n").expect("write");
        (d, root)
    }

    #[test]
    fn a_touched_directory_s_file_is_handed_over_once() {
        let (_d, root) = tree();
        let b = backend(&root);
        let mut g = Guidance::new(true, std::iter::empty());
        let got = g.take(
            &b,
            &root,
            &[call(
                "read",
                serde_json::json!({"path": "crates/tools/src/lib.rs"}),
            )],
        );
        // A file read in crates/tools/src touches crates and crates/tools: both carry a file, in
        // the order a reader walks down, and the third directory has none.
        let paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["crates/AGENTS.md", "crates/tools/AGENTS.md"]);
        assert_eq!(got[1].1, "the tools rules\n");
        // Said once: a second call in the same directory is prompt bytes for nothing.
        assert!(
            g.take(
                &b,
                &root,
                &[call(
                    "write",
                    serde_json::json!({"path": "crates/tools/x.rs"})
                )]
            )
            .is_empty()
        );
    }

    /// A MAIN session reads none of this: the operator's ruling is *gold for subagents*.
    #[test]
    fn a_root_session_is_told_nothing() {
        let (_d, root) = tree();
        let b = backend(&root);
        let mut g = Guidance::new(false, std::iter::empty());
        assert!(
            g.take(
                &b,
                &root,
                &[call(
                    "read",
                    serde_json::json!({"path": "crates/tools/src/lib.rs"})
                )]
            )
            .is_empty()
        );
    }

    #[test]
    fn a_directory_already_handed_over_eagerly_says_nothing_again() {
        let (_d, root) = tree();
        let b = backend(&root);
        let mut g = Guidance::new(true, vec!["crates/tools".to_string()]);
        let got = g.take(
            &b,
            &root,
            &[call(
                "read",
                serde_json::json!({"path": "crates/tools/src/lib.rs"}),
            )],
        );
        let paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["crates/AGENTS.md"]);
    }

    #[test]
    fn a_call_that_names_no_path_touches_nothing() {
        let (_d, root) = tree();
        let b = backend(&root);
        let mut g = Guidance::new(true, std::iter::empty());
        assert!(
            g.take(
                &b,
                &root,
                &[call("bash", serde_json::json!({"command": "ls crates"}))]
            )
            .is_empty()
        );
        // And arguments that are not JSON at all - the refusal that produced them is the row.
        assert!(
            g.take(
                &b,
                &root,
                &[ToolCall {
                    id: "c2".into(),
                    name: "read".into(),
                    arguments: "{not json".into(),
                }]
            )
            .is_empty()
        );
    }

    #[test]
    fn the_sentence_names_the_file_and_carries_it_whole() {
        let line = notice("crates/tools/AGENTS.md", "one\ntwo\n");
        assert!(
            line.starts_with("[guidance] `crates/tools/AGENTS.md`"),
            "{line}"
        );
        assert!(line.contains("one\ntwo"), "{line}");
        assert!(line.contains("2 lines"), "{line}");
    }
}
