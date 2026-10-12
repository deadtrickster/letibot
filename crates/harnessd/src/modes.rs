//! **Where a project sits, and where a session does, remembered.**
//!
//! One durable mapping: **project root → point name**. `~/Projects/letibot` at *writes
//! allowed*; a directory nobody has said anything about at *always-ask*; `/etc` at
//! *read-only* if the operator put it there. And beside it the point each SESSION was put at,
//! which is the session's own fact: a restart or a reseat brings it back there.
//!
//! This is what replaces the grant table an earlier draft of this design asked for —
//! `(project, tool, intent class)`, durable, listable, revocable. That table is
//! correct and nobody audits one. A per-project **point** is a single value a person
//! can hold in their head, which is the property that makes it safe rather than merely
//! configurable.
//!
//! # In the store, not a file
//!
//! It was `modes.tsv`, a text file each daemon read once at start and rewrote whole from its
//! own copy — and there is a daemon per workspace, so one daemon's write undid another's. The
//! operator: *"and again peerdb allow-all lost after restart/reseat"*, then *"lets switch it to
//! sqlite db"*. The rows are now `project_mode` and `session_mode` in the session store every
//! daemon already shares (`letibot_tokencore::store::modes`): a write is one row, and a read is
//! the table as it is now ([`ModeStore::refresh`]). A `modes.tsv` from before is imported once.
//!
//! # What an unknown row does
//!
//! **It is reported and skipped, never guessed at.** A row naming a point this build
//! does not have — a newer name, a typo — does not silently fall back to the default and
//! does not stop the daemon: it is kept in [`ModeStore::unreadable`] and the caller puts it
//! in the startup disclosure. A store that quietly downgraded one project would be the silent
//! degradation `crate::config`'s adjudicator refusals already refuse to do, one layer out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tokencore::store::Store;
use letibot_tools::mode::{Mode, UNSEEN_PROJECT};

/// A row the store could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub line: usize,
    pub text: String,
    pub why: String,
}

/// The mapping, as last read from the store.
#[derive(Debug, Clone, Default)]
pub struct ModeStore {
    /// The session store the rows live in. `None` is a store that forgets, and says so.
    db: Option<PathBuf>,
    /// The legacy `modes.tsv` still to import, taken at the first read.
    legacy: Option<PathBuf>,
    by_root: BTreeMap<PathBuf, Mode>,
    /// **The operator's recorded consent, per root** — the moment it was given, for the one
    /// point that needs one.
    ///
    /// `allow-all` on a bare host is reachable only through the operator's answer to a
    /// question ([`Mode::ALLOW_ALL_HERE`]); recorded, the row says the point AND that a person
    /// agreed to it, and [`ModeStore::describe`] reads both aloud. Meaningful only on an
    /// `allow-all` row, and cleared when the row moves anywhere else, because consent is for
    /// one question and does not travel to a point nobody picked.
    consented: BTreeMap<PathBuf, String>,
    /// Rows that did not parse, kept so the disclosure can say so. **Not** dropped: a
    /// project whose row is malformed is a project the operator thinks they configured.
    pub unreadable: Vec<Unreadable>,
}

/// `$XDG_CONFIG_HOME/letibot/modes.tsv`, or `$HOME/.config/letibot/modes.tsv` — where the
/// rows were kept before the store held them, read once to import them.
pub fn legacy_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("letibot").join("modes.tsv"))
}

impl ModeStore {
    /// The rows in the store at `db`. A store that cannot be opened is an empty one that says
    /// why in [`ModeStore::unreadable`] rather than stopping the daemon.
    pub fn load(db: &Path) -> ModeStore {
        let mut s = ModeStore {
            db: Some(db.to_path_buf()),
            ..Default::default()
        };
        s.refresh();
        s
    }

    /// **The store at `db`, not yet read** — or a forgetful store when the daemon has no
    /// session store. Nothing is opened here: the daemon builds this before any session exists,
    /// and the first [`ModeStore::refresh`] (a session opening) imports the legacy file once
    /// and reads the rows.
    pub fn open(db: Option<&Path>) -> ModeStore {
        ModeStore {
            db: db.map(Path::to_path_buf),
            legacy: db.and(legacy_path()),
            ..Default::default()
        }
    }

    /// **Read the rows again** — what any daemon wrote since this one last looked. Called at
    /// every session open and after every write, so no copy outlives the moment it is read.
    pub fn refresh(&mut self) {
        let Some(db) = &self.db else {
            return;
        };
        if let Some(tsv) = self.legacy.take() {
            let _ = import_tsv(db, &tsv);
        }
        self.by_root.clear();
        self.consented.clear();
        self.unreadable.clear();
        let rows = match Store::open(db).and_then(|st| st.project_modes()) {
            Ok(rows) => rows,
            Err(e) => {
                self.unreadable.push(Unreadable {
                    line: 0,
                    text: db.display().to_string(),
                    why: format!("the store could not be read: {e}"),
                });
                return;
            }
        };
        for (i, r) in rows.into_iter().enumerate() {
            match Mode::parse(r.mode.trim()) {
                Ok(m) => {
                    let root = PathBuf::from(&r.root);
                    if let Some(when) = r.consented_at.filter(|c| !c.is_empty()) {
                        self.consented.insert(root.clone(), when);
                    }
                    self.by_root.insert(root, m);
                }
                Err(why) => self.unreadable.push(Unreadable {
                    line: i + 1,
                    text: format!("{}\t{}", r.mode, r.root),
                    why,
                }),
            }
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

    /// Put a project at a point, and write its row.
    ///
    /// The prerequisite check is **not** here. It belongs to whoever is opening a
    /// session, because what is available is a fact about this daemon on this box at
    /// this moment — a store that refused to record *writes allowed* because no head
    /// happened to be attached would be refusing to remember an intention.
    ///
    /// **A row that moves OFF `allow-all` takes the consent with it**; moving TO `allow-all`
    /// keeps what was recorded, so the row and the answer can arrive in either order.
    pub fn set(&mut self, root: &Path, mode: Mode) -> std::io::Result<()> {
        let root = canonical(root);
        let keep = (mode.name == Mode::ALLOW_ALL.name)
            .then(|| self.consented.get(&root).cloned())
            .flatten();
        self.write(&root, mode, keep)
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

    /// **Record the operator's answer**, on the project's row.
    pub fn set_consent(&mut self, root: &Path, when: &str) -> std::io::Result<()> {
        let root = canonical(root);
        let mode = self.by_root.get(&root).copied().unwrap_or(Mode::ALLOW_ALL);
        self.write(&root, mode, Some(when.to_string()))
    }

    /// **Forget it**, for a point that does not need an answer. Reports whether there was one
    /// — a "forgotten" that forgot nothing is the kind of report that makes somebody think
    /// they revoked something.
    pub fn clear_consent(&mut self, root: &Path) -> std::io::Result<bool> {
        let root = canonical(root);
        let had = self.consented.contains_key(&root);
        if let Some(mode) = self.by_root.get(&root).copied() {
            self.write(&root, mode, None)?;
        }
        Ok(had)
    }

    /// Drop a project's row, putting it back to [`UNSEEN_PROJECT`]; the answer goes with it.
    /// Returns whether there was one.
    pub fn remove(&mut self, root: &Path) -> std::io::Result<bool> {
        let root = canonical(root);
        let Some(db) = &self.db else {
            self.consented.remove(&root);
            return Ok(self.by_root.remove(&root).is_some());
        };
        let had = Store::open(db)
            .and_then(|st| st.remove_project_mode(&root.display().to_string()))
            .map_err(std::io::Error::other)?;
        self.refresh();
        Ok(had)
    }

    /// Every project and where it sits, for a listing, sorted by root.
    pub fn list(&self) -> Vec<(&Path, Mode)> {
        self.by_root
            .iter()
            .map(|(k, m)| (k.as_path(), *m))
            .collect()
    }

    /// **The point this session was put at**, with the answer that opened it — `None` for a
    /// session nobody moved, which opens at its project's point.
    pub fn session(&self, session_id: &str) -> Option<(Mode, Option<String>)> {
        let db = self.db.as_ref()?;
        let (name, when) = Store::open(db).ok()?.session_mode(session_id).ok()??;
        Some((Mode::parse(&name).ok()?, when))
    }

    /// Record the point this session was put at.
    pub fn set_session(
        &self,
        session_id: &str,
        mode: Mode,
        consented: Option<&str>,
    ) -> std::io::Result<()> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        Store::open(db)
            .and_then(|st| st.set_session_mode(session_id, mode.name, consented))
            .map_err(std::io::Error::other)
    }

    /// One row, written; then the table read back.
    fn write(&mut self, root: &Path, mode: Mode, consent: Option<String>) -> std::io::Result<()> {
        let Some(db) = &self.db else {
            match &consent {
                Some(w) => self.consented.insert(root.to_path_buf(), w.clone()),
                None => self.consented.remove(root),
            };
            self.by_root.insert(root.to_path_buf(), mode);
            return Ok(());
        };
        Store::open(db)
            .and_then(|st| {
                st.set_project_mode(&root.display().to_string(), mode.name, consent.as_deref())
            })
            .map_err(std::io::Error::other)?;
        self.refresh();
        Ok(())
    }

    /// One line for the startup disclosure, with denominators.
    pub fn describe(&self, root: &Path) -> String {
        let mode = self.for_project(root);
        let where_ = match &self.db {
            Some(p) => p.display().to_string(),
            None => "nowhere — this daemon has no session store, so a change to this \
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
                ". {} row(s) there could not be read and were SKIPPED rather \
                 than guessed at: {}",
                self.unreadable.len(),
                self.unreadable
                    .iter()
                    .map(|u| format!("row {} ({})", u.line, u.why))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        s
    }
}

/// **The rows `modes.tsv` held, into the store — once per store.** A row the store already has
/// is not overwritten (the store's is newer), and the file is left exactly where it is: the
/// store records that it was imported, so a row removed later is not brought back by it.
/// Returns how many rows were imported.
pub fn import_tsv(db: &Path, tsv: &Path) -> std::io::Result<usize> {
    let Ok(text) = std::fs::read_to_string(tsv) else {
        return Ok(0);
    };
    let st = Store::open(db).map_err(std::io::Error::other)?;
    let source = tsv.display().to_string();
    if st.mode_imported(&source).map_err(std::io::Error::other)? {
        return Ok(0);
    }
    let have: std::collections::BTreeSet<String> = st
        .project_modes()
        .map_err(std::io::Error::other)?
        .into_iter()
        .map(|r| r.root)
        .collect();
    let mut n = 0;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split('\t');
        let (Some(name), Some(root)) = (f.next(), f.next()) else {
            continue;
        };
        let (name, root) = (name.trim(), root.trim());
        let consent = f.next().map(str::trim).filter(|c| !c.is_empty());
        if have.contains(root) {
            continue;
        }
        st.set_project_mode(root, name, consent)
            .map_err(std::io::Error::other)?;
        n += 1;
    }
    st.mark_mode_imported(&source)
        .map_err(std::io::Error::other)?;
    Ok(n)
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

    /// A store of its own per call, in a directory removed when the test ends.
    struct Db(PathBuf);

    impl Drop for Db {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Db {
        fn path(&self) -> PathBuf {
            self.0.join("sessions.db")
        }
    }

    fn db(name: &str) -> Db {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-modes-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Db(d)
    }

    /// **The operator's answer round-trips, and only on the row it was given for** — and
    /// moving that project anywhere else forgets it, so `/mode` away and back cannot resurrect
    /// an older `yes`.
    #[test]
    fn the_operators_answer_round_trips_and_only_where_it_was_given() {
        let d = db("consent");
        let mine = d.0.join("mine");
        let theirs = d.0.join("theirs");
        let when = "2026-10-10T13:22:13+00:00";

        let mut s = ModeStore::load(&d.path());
        s.set(&mine, Mode::ALLOW_ALL).expect("set the row");
        s.set_consent(&mine, when).expect("record the answer");
        assert_eq!(s.consent(&mine), Some(when), "the answer did not come back");
        assert_eq!(
            s.consent(&theirs),
            None,
            "an answer travelled to another project"
        );

        let back = ModeStore::load(&d.path());
        assert_eq!(
            back.consent(&mine),
            Some(when),
            "the answer did not survive"
        );
        assert_eq!(back.for_project(&mine).name, "allow-all");

        let mut s = ModeStore::load(&d.path());
        s.set(&mine, Mode::WRITES_ALLOWED).expect("move the row");
        assert_eq!(s.consent(&mine), None, "the answer outlived its row");
        assert_eq!(ModeStore::load(&d.path()).consent(&mine), None);

        // And the row and the answer arriving in the other order keep each other.
        let mut s = ModeStore::load(&d.path());
        s.set_consent(&theirs, when).unwrap();
        s.set(&theirs, Mode::ALLOW_ALL).unwrap();
        assert_eq!(ModeStore::load(&d.path()).consent(&theirs), Some(when));
    }

    /// **Two daemons share the store, and neither drops the other's row** — the operator,
    /// 2026-10-12: *"and again peerdb allow-all lost after restart/reseat"*. Each workspace has
    /// its own daemon; each read `modes.tsv` once at its start and rewrote the whole of it from
    /// that copy, so letim's daemon, started before peerdb's row was written, wrote its own
    /// row later and took peerdb's out. A write is one row now, and a fresh read sees the rest.
    #[test]
    fn two_daemons_writing_the_store_keep_each_others_rows() {
        let d = db("two-daemons");
        let peerdb = d.0.join("peerdb");
        let letim = d.0.join("letim");
        let when = "2026-10-12T07:00:00Z";

        let mut a = ModeStore::load(&d.path());
        let mut b = ModeStore::load(&d.path());
        a.set(&peerdb, Mode::ALLOW_ALL).unwrap();
        a.set_consent(&peerdb, when).unwrap();
        b.set(&letim, Mode::ALLOW_ALL).unwrap();

        let back = ModeStore::load(&d.path());
        assert_eq!(
            back.for_project(&peerdb).name,
            "allow-all",
            "peerdb's row went"
        );
        assert_eq!(back.consent(&peerdb), Some(when), "peerdb's answer went");
        assert_eq!(back.for_project(&letim).name, "allow-all");

        // A daemon holding an older read sees the row once it looks again.
        let mut c = ModeStore::load(&d.path());
        a.set(&d.0.join("later"), Mode::READ_ONLY).unwrap();
        assert!(!c.is_set(&d.0.join("later")));
        c.refresh();
        assert!(c.is_set(&d.0.join("later")), "a fresh read sees it");
    }

    /// **A session keeps the point it was put at**, whatever its project's row says later.
    #[test]
    fn a_session_keeps_its_own_point() {
        let d = db("session");
        let s = ModeStore::load(&d.path());
        assert_eq!(s.session("s-1"), None, "nobody moved it");
        s.set_session("s-1", Mode::ALLOW_ALL, Some("t")).unwrap();
        let (m, when) = ModeStore::load(&d.path()).session("s-1").unwrap();
        assert_eq!(m.name, "allow-all");
        assert_eq!(when.as_deref(), Some("t"));
    }

    /// **`modes.tsv` is imported once, and left where it is.** A row the store already has is
    /// the newer and is kept; a row removed after the import does not come back.
    #[test]
    fn the_legacy_file_is_imported_once_and_left_alone() {
        let d = db("import");
        let tsv = d.0.join("modes.tsv");
        let a = d.0.join("a");
        let b = d.0.join("b");
        std::fs::write(
            &tsv,
            format!(
                "# a comment\nallow-all\t{}\t2026-10-12T07:54:18Z\nread-only\t{}\n",
                a.display(),
                b.display()
            ),
        )
        .unwrap();
        let mut s = ModeStore::load(&d.path());
        s.set(&b, Mode::WRITES_ALLOWED).unwrap();

        assert_eq!(
            import_tsv(&d.path(), &tsv).unwrap(),
            1,
            "only the row it lacks"
        );
        assert!(tsv.exists(), "the file is left where it is");
        let mut s = ModeStore::load(&d.path());
        assert_eq!(s.consent(&a), Some("2026-10-12T07:54:18Z"));
        assert_eq!(
            s.for_project(&b).name,
            "writes allowed",
            "the store's row is newer"
        );

        assert!(s.remove(&a).unwrap());
        assert_eq!(import_tsv(&d.path(), &tsv).unwrap(), 0, "once");
        assert!(
            !ModeStore::load(&d.path()).is_set(&a),
            "a removed row stays removed"
        );
    }

    /// A directory nobody has configured is at always-ask, and says nobody said.
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
    fn a_project_round_trips_through_the_store() {
        let d = db("round-trip");
        let root = d.0.join("p");
        let mut s = ModeStore::load(&d.path());
        s.set(&root, Mode::WRITES_ALLOWED).unwrap();

        let mut back = ModeStore::load(&d.path());
        assert_eq!(back.for_project(&root).name, "writes allowed");
        assert!(back.is_set(&root));
        assert_eq!(back.list().len(), 1);
        assert!(back.remove(&root).unwrap(), "there was a row");
        assert!(!back.remove(&root).unwrap(), "and now there is not");
        assert_eq!(
            ModeStore::load(&d.path()).for_project(&root).name,
            "always-ask"
        );
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

    /// **A row this build cannot read is reported and skipped, never guessed at**, and a
    /// write to another project leaves it in the store.
    #[test]
    fn an_unknown_row_is_reported_skipped_and_kept() {
        let d = db("unknown-row");
        let st = Store::open(&d.path()).unwrap();
        st.set_project_mode("/home/x/a", "writes allowed", None)
            .unwrap();
        st.set_project_mode("/home/x/b", "yolo", None).unwrap();
        drop(st);
        let mut s = ModeStore::load(&d.path());
        assert_eq!(s.unreadable.len(), 1, "{:?}", s.unreadable);
        assert_eq!(s.for_project(Path::new("/home/x/a")).name, "writes allowed");
        assert_eq!(s.for_project(Path::new("/home/x/b")).name, "always-ask");
        let desc = s.describe(Path::new("/home/x/b"));
        assert!(desc.contains("could not be read"), "{desc}");
        assert!(desc.contains("SKIPPED"), "{desc}");

        s.set(Path::new("/home/x/c"), Mode::READ_ONLY).unwrap();
        let rows = Store::open(&d.path()).unwrap().project_modes().unwrap();
        assert!(rows.iter().any(|r| r.mode == "yolo"), "{rows:?}");
    }

    /// A store with nowhere to write says so rather than silently forgetting.
    #[test]
    fn a_store_with_no_path_admits_it_forgets() {
        let mut s = ModeStore::default();
        s.set(Path::new("/tmp/x"), Mode::WRITES_ALLOWED).unwrap();
        let d = s.describe(Path::new("/tmp/x"));
        assert!(d.contains("will not survive"), "{d}");
    }
}
