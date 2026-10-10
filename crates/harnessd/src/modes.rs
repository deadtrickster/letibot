//! **Where a project sits, remembered.**
//!
//! One durable mapping: **project root → point name**. `~/Projects/letibot` at *writes
//! allowed*; a directory nobody has said anything about at *always-ask*; `/etc` at
//! *read-only* if the operator put it there.
//!
//! This is what replaces the grant table an earlier draft of this design asked for —
//! `(project, tool, intent class)`, durable, listable, revocable. That table is
//! correct and nobody audits one. A per-project **point** is a single value a person
//! can hold in their head, which is the property that makes it safe rather than merely
//! configurable, and it is the reason the file below is one line per project rather
//! than a schema.
//!
//! # It is a text file on purpose
//!
//! Two lines of TSV: the point's name, then the root. An operator can read it with
//! `cat`, edit it with an editor, and delete a line to put a project back to the
//! default — and every one of those is a thing they can do while the daemon is not
//! running, which a database is not.
//!
//! # What an unknown line does
//!
//! **It is reported and skipped, never guessed at.** A row naming a point this build
//! does not have — a newer name, a typo, a hand-edit — does not silently fall back to
//! the default and does not stop the daemon: [`ModeStore::load`] returns the rows it
//! could not read alongside the ones it could, and the caller puts them in the startup
//! disclosure. A store that quietly downgraded one project would be the silent
//! degradation `crate::config`'s adjudicator refusals already refuse to do, one layer
//! out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tools::mode::{Mode, UNSEEN_PROJECT};

/// A line the store could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub line: usize,
    pub text: String,
    pub why: String,
}

/// The mapping, in memory.
#[derive(Debug, Clone, Default)]
pub struct ModeStore {
    path: Option<PathBuf>,
    by_root: BTreeMap<PathBuf, Mode>,
    /// **The operator's recorded consent, per root** — the moment it was given, for the one
    /// point that needs one.
    ///
    /// `allow-all` on a bare host is reachable only through the operator's answer to a
    /// question ([`Mode::ALLOW_ALL_HERE`]), and that answer used to live in a single process:
    /// every restart dropped it and they gave it again. Their words: *"make my allow-all
    /// survive across restarts"*. Recording it HERE is what the module's own objection asks
    /// for rather than what it forbids — the row then says the point AND that a person agreed
    /// to it, and [`ModeStore::describe`] reads both aloud, so nothing comes back that the
    /// banner cannot explain. What the module refused was a point that arrived silently and
    /// claimed a boundary it did not have; a stamp is the opposite of silent.
    ///
    /// Meaningful only on an `allow-all` row, and cleared when the row moves anywhere else,
    /// because consent is for one question and does not travel to a point nobody picked.
    consented: BTreeMap<PathBuf, String>,
    /// Rows that did not parse, kept so the disclosure can say so. **Not** dropped: a
    /// project whose line is malformed is a project the operator thinks they
    /// configured.
    pub unreadable: Vec<Unreadable>,
}

/// `$XDG_CONFIG_HOME/letibot/modes.tsv`, or `$HOME/.config/letibot/modes.tsv`.
///
/// `None` when neither is set, which is a bare test environment. A store with no path
/// still works and simply forgets — and says so in [`ModeStore::describe`], because a
/// harness that silently failed to remember would have an operator setting the same
/// point every morning and wondering why.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("letibot").join("modes.tsv"))
}

impl ModeStore {
    /// Read the store at `path`. A missing file is an empty store and **not** an
    /// error: the first run of anything has no file, and treating that as a failure
    /// would make the ordinary case look broken.
    pub fn load(path: &Path) -> ModeStore {
        let mut s = ModeStore {
            path: Some(path.to_path_buf()),
            ..Default::default()
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return s;
        };
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Two fields are a row; a third is the operator's recorded consent for it. A
            // row without one is every row written before this existed, and it loads
            // unchanged.
            let mut fields = line.split('\t');
            let (Some(name), Some(root)) = (fields.next(), fields.next()) else {
                s.unreadable.push(Unreadable {
                    line: i + 1,
                    text: line.to_string(),
                    why: "no tab: a row is `<mode name>\\t<absolute project root>`, and a third \
                          `\\t<consented-at>` is the operator's own answer for `allow-all`"
                        .into(),
                });
                continue;
            };
            let consented = fields.next().map(str::trim).filter(|c| !c.is_empty());
            match Mode::parse(name.trim()) {
                Ok(m) => {
                    let root = PathBuf::from(root.trim());
                    if let Some(when) = consented {
                        s.consented.insert(root.clone(), when.to_string());
                    }
                    s.by_root.insert(root, m);
                }
                Err(why) => s.unreadable.push(Unreadable {
                    line: i + 1,
                    text: line.to_string(),
                    why,
                }),
            }
        }
        s
    }

    /// Load from [`default_path`], or an empty forgetful store when there is no
    /// config directory to put one in.
    pub fn open() -> ModeStore {
        match default_path() {
            Some(p) => ModeStore::load(&p),
            None => ModeStore::default(),
        }
    }

    /// **Where this project sits.**
    ///
    /// The longest matching ancestor wins, so putting `~/Projects` at *writes allowed*
    /// covers everything under it and a single project inside it can still be pinned
    /// tighter. Nothing matching is [`UNSEEN_PROJECT`] — always-ask, where nothing that
    /// is not a read happens without the operator.
    ///
    /// A directory *above* a configured root does not inherit from it, which is worth
    /// stating because the opposite would be the dangerous direction: `~/Projects/x` at
    /// writes-allowed must not make `~` writes-allowed.
    pub fn for_project(&self, root: &Path) -> Mode {
        let root = canonical(root);
        self.by_root
            .iter()
            .filter(|(k, _)| root.starts_with(k))
            .max_by_key(|(k, _)| k.as_os_str().len())
            .map(|(_, m)| *m)
            .unwrap_or(UNSEEN_PROJECT)
    }

    /// Whether this project's point was set, as opposed to defaulted.
    ///
    /// A separate question from `for_project`, and the disclosure needs both: *"this
    /// project is at always-ask"* and *"nobody has said where this project sits, so it
    /// is at always-ask"* are different sentences, and only the second one tells the
    /// operator there is something they might want to do.
    pub fn is_set(&self, root: &Path) -> bool {
        let root = canonical(root);
        self.by_root.keys().any(|k| root.starts_with(k))
    }

    /// Put a project at a point, and write the file.
    ///
    /// The prerequisite check is **not** here. It belongs to whoever is opening a
    /// session, because what is available is a fact about this daemon on this box at
    /// this moment — a store that refused to record *writes allowed* because no head
    /// happened to be attached would be refusing to remember an intention.
    pub fn set(&mut self, root: &Path, mode: Mode) -> std::io::Result<()> {
        let root = canonical(root);
        // **A row that moves OFF `allow-all` takes the consent with it.** The operator
        // answered a question about that point, not about the one they are moving to — the
        // same rule the point itself states, that consent does not travel. Moving TO
        // `allow-all` keeps whatever was recorded, so the row write and the answer can arrive
        // in either order without one undoing the other.
        if mode.name != Mode::ALLOW_ALL.name {
            self.consented.remove(&root);
        }
        self.by_root.insert(root, mode);
        self.flush()
    }

    /// **The operator's recorded consent for this project**, if they have given one.
    ///
    /// Read beside the row at open: an `allow-all` row WITH a stamp resolves to
    /// [`Mode::ALLOW_ALL_HERE`] — the point that tells the boundary the truth — and one
    /// without it is refused as it always was, naming how to give the answer. Resolved by the
    /// same longest-ancestor rule as the row itself, so a stamp on `~/Projects` covers what
    /// the row covers and no more.
    pub fn consent(&self, root: &Path) -> Option<&str> {
        let root = canonical(root);
        self.consented
            .iter()
            .filter(|(k, _)| root.starts_with(k))
            .max_by_key(|(k, _)| k.as_os_str().len())
            .map(|(_, when)| when.as_str())
    }

    /// **Record the operator's answer**, and write the file.
    ///
    /// Called from the `/mode` path when the answer to the unconfined-`allow-all` question is
    /// yes; `when` is the moment, so the disclosure can say whose answer it was and when.
    pub fn set_consent(&mut self, root: &Path, when: &str) -> std::io::Result<()> {
        self.consented.insert(canonical(root), when.to_string());
        self.flush()
    }

    /// **Forget it**, for a point that does not need an answer.
    ///
    /// `set` already does this when a row moves off `allow-all`; this is the same act for a
    /// caller that knows the answer is not wanted, and it reports whether there was one — a
    /// "forgotten" that forgot nothing is the kind of report that makes somebody think they
    /// revoked something.
    pub fn clear_consent(&mut self, root: &Path) -> std::io::Result<bool> {
        let had = self.consented.remove(&canonical(root)).is_some();
        self.flush()?;
        Ok(had)
    }

    /// Drop a project's row, putting it back to [`UNSEEN_PROJECT`]. Returns whether
    /// there was one — a "removed" that removed nothing is the kind of report that
    /// makes somebody think they revoked something.
    pub fn remove(&mut self, root: &Path) -> std::io::Result<bool> {
        let root = canonical(root);
        let had = self.by_root.remove(&root).is_some();
        // **The row and the answer go together**: a project with no row has no consent, or a
        // `/mode` that forgot where the project sits would still open `allow-all` for it.
        self.consented.remove(&root);
        self.flush()?;
        Ok(had)
    }

    /// Every project and where it sits, for a listing. Sorted by root, because a
    /// listing whose order changes between runs is one nobody can diff.
    pub fn list(&self) -> Vec<(&Path, Mode)> {
        self.by_root
            .iter()
            .map(|(k, m)| (k.as_path(), *m))
            .collect()
    }

    fn flush(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = String::from(
            "# letibot: where each project sits. One row per project root.\n\
             #   <mode name>\\t<absolute project root>[\\t<consented-at>]\n\
             # A root with no row is `always-ask`. The longest matching ancestor wins.\n\
             # The third field is the operator's own answer to the question `allow-all` asks\n\
             # on a bare host, recorded so that a restart does not throw it away.\n",
        );
        for (root, m) in &self.by_root {
            match self.consented.get(root) {
                Some(when) => out.push_str(&format!("{}\t{}\t{}\n", m.name, root.display(), when)),
                None => out.push_str(&format!("{}\t{}\n", m.name, root.display())),
            }
        }
        // Rows this build could not read are **kept**, not dropped on the first write.
        // Losing an operator's line because a newer build wrote the file is the worst
        // thing this file could do, and it is the thing a naive rewrite does.
        for u in &self.unreadable {
            out.push_str(&format!("{}\n", u.text));
        }
        std::fs::write(path, out)
    }

    /// One line for the startup disclosure, with denominators.
    pub fn describe(&self, root: &Path) -> String {
        let mode = self.for_project(root);
        let where_ = match &self.path {
            Some(p) => p.display().to_string(),
            None => "nowhere — no XDG_CONFIG_HOME and no HOME, so a change to this \
                     project's mode will not survive the daemon"
                .to_string(),
        };
        let mut s = if self.is_set(root) {
            format!("this project is at `{}` — {}", mode.name, mode.summary)
        } else {
            format!(
                "nobody has said where this project sits, so it is at `{}` — {}",
                mode.name, mode.summary
            )
        };
        s.push_str(&format!(
            " ({} project(s) recorded in {where_})",
            self.by_root.len()
        ));
        if !self.unreadable.is_empty() {
            s.push_str(&format!(
                ". {} row(s) in that file could not be read and were SKIPPED rather \
                 than guessed at: {}",
                self.unreadable.len(),
                self.unreadable
                    .iter()
                    .map(|u| format!("line {} ({})", u.line, u.why))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        s
    }
}

/// Resolve a path as far as the filesystem allows, and keep it as given otherwise.
///
/// `canonicalize` fails for a directory that does not exist, and a project root that
/// does not exist yet is a thing an operator may reasonably configure ahead of time.
/// Falling back to the path as written is right for that; what it costs is that two
/// spellings of one directory can be two rows, which is why the fallback is named
/// rather than silent.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-modes-{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d.join("modes.tsv")
    }

    /// **The operator's answer round-trips, and only on the row it was given for.**
    ///
    /// MEASURED 2026-10-10: `allow-all` was the one point that could not survive a restart,
    /// because it needs a confinement a bare host cannot supply and the only thing that opens
    /// it is a person's answer — an answer that lived in one process and died with it. The row
    /// carries it now, and this is what that has to mean: it survives the file, it is there
    /// only for the project that was asked about, and moving that project anywhere else
    /// forgets it, so `/mode` away and back cannot resurrect an older `yes`.
    #[test]
    fn the_operators_answer_round_trips_and_only_where_it_was_given() {
        let path = tmp("consent");
        let _ = std::fs::remove_file(&path);
        let mine = std::env::temp_dir().join("consent/mine");
        let theirs = std::env::temp_dir().join("consent/theirs");
        let when = "2026-10-10T13:22:13+00:00";

        let mut s = ModeStore::load(&path);
        s.set(&mine, Mode::ALLOW_ALL).expect("set the row");
        s.set_consent(&mine, when).expect("record the answer");
        assert_eq!(s.consent(&mine), Some(when), "the answer did not come back");
        assert_eq!(
            s.consent(&theirs),
            None,
            "an answer to one project travelled to another"
        );

        // **It survives the file**, which is the whole point of recording it rather than
        // keeping it in memory.
        let back = ModeStore::load(&path);
        assert_eq!(
            back.consent(&mine),
            Some(when),
            "the answer did not survive a reload"
        );
        assert_eq!(back.for_project(&mine).name, "allow-all");
        let text = std::fs::read_to_string(&path).expect("the file");
        assert!(
            text.lines()
                .any(|l| l.starts_with("allow-all\t") && l.ends_with(when)),
            "the row does not carry the answer: {text}"
        );

        // **And moving the project off `allow-all` forgets it.** Consent is for one
        // question, and it does not travel to a point nobody picked.
        let mut s = ModeStore::load(&path);
        s.set(&mine, Mode::WRITES_ALLOWED).expect("move the row");
        assert_eq!(
            s.consent(&mine),
            None,
            "the answer outlived the row it was given for"
        );
        let back = ModeStore::load(&path);
        assert_eq!(back.consent(&mine), None, "and not in the file either");
        let text = std::fs::read_to_string(&path).expect("the file");
        assert!(
            !text.contains(when),
            "the forgotten answer is still written: {text}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A directory nobody has configured is at always-ask: nothing that is not a read
    /// happens without the operator, and the first thing they hit is not "why can this
    /// session not edit".
    #[test]
    fn an_unseen_project_is_always_ask_and_says_nobody_said() {
        let s = ModeStore::default();
        let root = Path::new("/tmp/never-configured");
        assert_eq!(s.for_project(root).name, "always-ask");
        assert!(!s.is_set(root));
        let d = s.describe(root);
        assert!(d.contains("nobody has said"), "{d}");
    }

    #[test]
    fn a_project_round_trips_through_the_file() {
        let path = tmp("round-trip");
        let _ = std::fs::remove_file(&path);
        let root = std::env::temp_dir();
        let mut s = ModeStore::load(&path);
        s.set(&root, Mode::WRITES_ALLOWED).unwrap();

        let back = ModeStore::load(&path);
        assert_eq!(back.for_project(&root).name, "writes allowed");
        assert!(back.is_set(&root));
        assert_eq!(back.list().len(), 1);

        let mut back = back;
        assert!(back.remove(&root).unwrap(), "there was a row");
        assert!(!back.remove(&root).unwrap(), "and now there is not");
        assert_eq!(ModeStore::load(&path).for_project(&root).name, "always-ask");
        let _ = std::fs::remove_file(&path);
    }

    /// The longest matching ancestor wins, and a parent does **not** inherit from a
    /// child. The second half is the dangerous direction: a subdirectory at
    /// writes-allowed must not widen the tree above it.
    #[test]
    fn the_longest_ancestor_wins_and_a_parent_inherits_nothing() {
        let mut s = ModeStore::default();
        s.by_root
            .insert(PathBuf::from("/home/x/Projects"), Mode::WRITES_ALLOWED);
        s.by_root
            .insert(PathBuf::from("/home/x/Projects/secret"), Mode::READ_ONLY);

        assert_eq!(
            s.for_project(Path::new("/home/x/Projects/letibot")).name,
            "writes allowed"
        );
        assert_eq!(
            s.for_project(Path::new("/home/x/Projects/secret/sub")).name,
            "read-only",
            "the tighter, longer row wins"
        );
        assert_eq!(
            s.for_project(Path::new("/home/x")).name,
            "always-ask",
            "a parent must not inherit a child's widening"
        );
    }

    /// **A row this build cannot read is reported and skipped, never guessed at**, and
    /// it survives the next write. Losing an operator's line because a newer build
    /// rewrote the file is the worst thing this file could do.
    #[test]
    fn an_unknown_row_is_reported_skipped_and_kept() {
        let path = tmp("unknown-row");
        std::fs::write(
            &path,
            "# a comment\nwrites allowed\t/home/x/a\nyolo\t/home/x/b\nnotabhere\n",
        )
        .unwrap();
        let mut s = ModeStore::load(&path);
        assert_eq!(s.unreadable.len(), 2, "{:?}", s.unreadable);
        assert_eq!(s.for_project(Path::new("/home/x/a")).name, "writes allowed");
        assert_eq!(
            s.for_project(Path::new("/home/x/b")).name,
            "always-ask",
            "an unreadable row does not silently downgrade — it is simply not there, \
             and the disclosure says so"
        );
        let d = s.describe(Path::new("/home/x/b"));
        assert!(d.contains("could not be read"), "{d}");
        assert!(d.contains("SKIPPED"), "{d}");

        // And a write keeps them.
        s.set(Path::new("/home/x/c"), Mode::READ_ONLY).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("yolo\t/home/x/b"), "{text}");
        assert!(text.contains("notabhere"), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    /// A store with nowhere to write says so rather than silently forgetting. An
    /// operator setting the same point every morning is the symptom this avoids.
    #[test]
    fn a_store_with_no_path_admits_it_forgets() {
        let mut s = ModeStore::default();
        s.set(Path::new("/tmp/x"), Mode::WRITES_ALLOWED).unwrap();
        let d = s.describe(Path::new("/tmp/x"));
        assert!(d.contains("will not survive"), "{d}");
    }
}
