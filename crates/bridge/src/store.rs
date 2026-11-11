//! Which file of the store the bridge reads (`docs/store-split.md`).
//!
//! A daemon is handed a DIRECTORY once the store is split and one `sessions.db` while it is not,
//! and a bridge may be handed either, or a single file by `$LETIBOT_STORE` or a test. This module
//! is the one place that turns that into a file per table, and it asks tokencore rather than
//! keeping a second copy of which table lives where.

use std::path::{Path, PathBuf};

use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::Store;
use letibot_tokencore::store::split::{self, Place};

/// The key a set is opened under when no folder says whose project file to attach: the index's
/// query is the same whatever the key, and a project file nobody named stays out of the set.
const NO_FOLDER_KEY: &str = "000000000000";

/// Is there a store here at all? A bridge never makes one - it is the daemon's file, and one made
/// here would be a second writer in a folder with no daemon - so every reader asks this first.
pub fn exists(store: &Path) -> bool {
    if !store.is_dir() {
        return store.is_file();
    }
    store.join(split::SHARED_FILE).is_file() || store.join(split::LEGACY_FILE).is_file()
}

/// The file `table`'s rows are in, under a store the bridge was handed: the file itself when that
/// is what it got, a directory's `sessions.db` while the store is unsplit, and a split
/// directory's `shared.db` or `projects/<key>.db` by [`split::place_of`].
///
/// `folder` names the project a per-project table belongs to, and a caller that serves one passes
/// it. No table this bridge reads is per-project, so a table the assignment has no row for refuses
/// rather than answering the wrong file.
pub fn file_for(store: &Path, table: &str, folder: Option<&Path>) -> Result<PathBuf, String> {
    let store = &current(store);
    if !store.is_dir() {
        return Ok(store.to_path_buf());
    }
    if !store.join(split::SHARED_FILE).is_file() {
        return Ok(store.join(split::LEGACY_FILE));
    }
    match split::place_of(table) {
        Some(Place::Project) => {
            let folder = folder.ok_or_else(|| {
                format!(
                    "`{table}` is kept per project, and no folder was named to say which of {} it is in",
                    store.join(split::PROJECTS_DIR).display()
                )
            })?;
            let key = project_key(&folder.to_string_lossy());
            Ok(store.join(split::PROJECTS_DIR).join(format!("{key}.db")))
        }
        Some(_) => Ok(store.join(split::SHARED_FILE)),
        // A table the split has never been told about could be in either file, and answering
        // `shared.db` would read an empty table rather than say the assignment is missing a row.
        None => Err(format!(
            "`{table}` is in neither file of a split store: the table assignment has no row for it"
        )),
    }
}

/// A bridge resolves its store once, so a `sessions.db` it was handed before the split names a file
/// that is gone; opening it would create an empty store nobody reads. Its directory is the store now.
fn current(store: &Path) -> PathBuf {
    match store.parent() {
        Some(dir)
            if store.file_name().is_some_and(|n| n == split::LEGACY_FILE)
                && dir.join(split::SHARED_FILE).is_file() =>
        {
            dir.to_path_buf()
        }
        _ => store.to_path_buf(),
    }
}

/// [`file_for`]'s store, opened: the file itself, or - for a split directory - the SET of the two
/// files, because the session index's query must not read `transcript` through the shared file and
/// one `Store` over a set is what knows not to.
pub fn open(store: &Path, table: &str, folder: Option<&Path>) -> Result<Store, String> {
    let store = &current(store);
    let file = file_for(store, table, folder)?;
    if !(store.is_dir() && store.join(split::SHARED_FILE).is_file()) {
        return Store::open(&file).map_err(|e| format!("{}: {e}", file.display()));
    }
    let key = match folder {
        Some(folder) => project_key(&folder.to_string_lossy()),
        None => NO_FOLDER_KEY.to_string(),
    };
    Store::open_set_existing(store, &key).map_err(|e| format!("{}: {e}", store.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own per test, removed when it ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let p = std::env::temp_dir()
                .join(format!("letibot-bridge-store-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn split_dir(d: &TempDir) -> PathBuf {
        let key = project_key("/w/here");
        Store::open_set(&d.0, &key).expect("a split set");
        d.0.clone()
    }

    #[test]
    fn a_split_directory_opens_as_a_set_and_a_file_does_not() {
        let d = TempDir::new("set");
        let dir = split_dir(&d);
        assert!(
            open(&dir, "session", None).expect("the set").is_set(),
            "the index's query must not read a project's transcript through the shared file"
        );
        assert!(
            !open(&d.0.join("one.db"), "session", None)
                .expect("the file")
                .is_set(),
            "one file is one file"
        );
    }

    #[test]
    fn a_sessions_db_handed_before_the_split_opens_the_split_store() {
        let d = TempDir::new("stale");
        let dir = split_dir(&d);
        let stale = dir.join(split::LEGACY_FILE);
        assert!(open(&stale, "seat_todo", None).expect("the set").is_set());
        assert_eq!(
            file_for(&stale, "seat_todo", None).unwrap(),
            dir.join(split::SHARED_FILE)
        );
        assert!(
            !stale.exists(),
            "no empty sessions.db is created beside the split store"
        );
    }

    #[test]
    fn one_file_is_the_file_it_was_handed() {
        let d = TempDir::new("file");
        Store::open(&d.db()).unwrap();
        assert_eq!(file_for(&d.db(), "session", None).unwrap(), d.db());
        assert!(exists(&d.db()));
    }

    #[test]
    fn an_unsplit_directory_is_its_sessions_db() {
        let d = TempDir::new("unsplit");
        Store::open(&d.db()).unwrap();
        assert_eq!(
            file_for(&d.0, "transcript", None).unwrap(),
            d.0.join("sessions.db")
        );
        assert!(exists(&d.0));
    }

    #[test]
    fn a_split_directory_answers_the_file_place_of_names() {
        let d = TempDir::new("split");
        let dir = split_dir(&d);
        assert_eq!(
            file_for(&dir, "session", None).unwrap(),
            dir.join("shared.db")
        );
        assert_eq!(
            file_for(&dir, "seat_todo", None).unwrap(),
            dir.join("shared.db")
        );
        let folder = Path::new("/w/here");
        assert_eq!(
            file_for(&dir, "transcript", Some(folder)).unwrap(),
            dir.join("projects")
                .join(format!("{}.db", project_key("/w/here")))
        );
        assert!(exists(&dir));
    }

    #[test]
    fn a_per_project_table_with_no_folder_refuses_rather_than_guessing() {
        let d = TempDir::new("no-folder");
        let dir = split_dir(&d);
        let why = file_for(&dir, "adjudication", None).expect_err("which project?");
        assert!(why.contains("per project"), "{why}");
    }

    #[test]
    fn a_table_the_assignment_has_never_heard_of_is_a_refusal_on_a_split_store() {
        let d = TempDir::new("unknown");
        let dir = split_dir(&d);
        let why = file_for(&dir, "no_such_table", None).expect_err("no row for it");
        assert!(why.contains("no_such_table"), "{why}");
    }

    #[test]
    fn a_directory_with_no_store_is_not_one() {
        let d = TempDir::new("empty");
        assert!(!exists(&d.0), "a bridge may not make a store of its own");
    }

    impl TempDir {
        fn db(&self) -> PathBuf {
            self.0.join("sessions.db")
        }
    }
}
