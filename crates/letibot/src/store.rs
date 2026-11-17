//! `letibot store forget-import SESSION` - the operator's repair of a half-finished move.
//!
//! An import writes its marker in a commit of its own before the rows, and clears it after, so a
//! run that dies between a set's two files' commits leaves one behind. Every retry of that move
//! then refuses by name until somebody has looked at what landed, and this is the door that says
//! "looked, forget it".

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
letibot store forget-import SESSION [--store DIR|FILE]
  Clear the marker a dead import left for SESSION, so the move can be retried.

  SESSION   the session id the import was moving
  --store   the store the daemon reads (the launcher hands it this; naming one asks a
            store no daemon is serving)

  It applies or refuses: a session with no marker is said to have none, and its file is not
  touched. Check what landed first - `letibot session show SESSION`, or the rows themselves.

  example:
    letibot store forget-import s-1789919514688401228
";

/// One verb line, split from the `--store` the launcher appends: `--help` needs no store, and a
/// typo is refused by name before anything is opened.
fn parse(args: &[String]) -> Result<(PathBuf, String), String> {
    let mut words: Vec<&String> = Vec::new();
    let mut store: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--store" => store = Some(PathBuf::from(it.next().ok_or("--store needs a value")?)),
            _ => words.push(a),
        }
    }
    let (verb, rest) = words
        .split_first()
        .ok_or("store needs a verb: forget-import")?;
    if verb.as_str() != "forget-import" {
        return Err(format!("store has no verb {verb}: forget-import"));
    }
    let session = rest
        .first()
        .ok_or("store forget-import needs a SESSION")?
        .to_string();
    if let Some(bad) = rest.get(1) {
        return Err(format!("store forget-import takes one session, not {bad}"));
    }
    let store = store.ok_or("no --store: this verb acts on a store, and none was named")?;
    Ok((store, session))
}

/// Forget `session`'s half-import marker, and say what that means for the move. The store is
/// opened at the session's own project file, which is where an import of it writes.
fn forget_import(store: &Path, session: &str) -> Result<String, String> {
    let opened = crate::migrate::open_store(store, Some(session))
        .map_err(|e| format!("opening {}: {e}", store.display()))?;
    match opened.forget_import(session).map_err(|e| e.to_string())? {
        true => Ok(format!(
            "{session}: the half-import marker is gone. Check what landed before you move it \
             again (`letibot session show {session}`), then retry the move."
        )),
        false => Ok(format!(
            "{session}: no half-import was pending, so nothing was forgotten and nothing \
             changed. A retry of the move was never blocked by a marker."
        )),
    }
}

pub fn run(args: &[String]) -> ExitCode {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let out = parse(args).and_then(|(store, session)| forget_import(&store, &session));
    match out {
        Ok(said) => {
            println!("{said}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("letibot: {why}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{SessionRecord, Store};

    /// A store path only this test uses.
    fn fixture(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "letibot-store-verb-{tag}-{}-{}.db",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        path
    }

    /// The act, and the no-op: a marker a dead import left is cleared, and a session with none is
    /// said to have none rather than quietly reported as repaired.
    #[test]
    fn a_half_import_marker_is_cleared_and_its_absence_is_said() {
        let path = fixture("marker");
        let store = Store::open(&path).expect("a store");
        store
            .put_session(&SessionRecord {
                id: "s-1".into(),
                title: None,
                model_id: "m".into(),
                dialect_sha: "d".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                approvers: vec![],
                role: None,
                parent_session_id: None,
            })
            .expect("the session row");
        drop(store);
        rusqlite::Connection::open(&path)
            .expect("a connection of the test's own")
            .execute(
                "INSERT INTO import_pending (session_id, at_ms) VALUES ('s-1', 1)",
                [],
            )
            .expect("the marker a killed import leaves");

        let said = forget_import(&path, "s-1").expect("the verb applies");
        assert!(said.contains("is gone"), "{said}");
        let store = Store::open(&path).expect("a store");
        assert_eq!(
            store.import_pending("s-1").expect("the read"),
            None,
            "the marker was not cleared, so every retry of the move still refuses"
        );
        drop(store);

        let again = forget_import(&path, "s-1").expect("nothing pending is not a failure");
        assert!(again.contains("no half-import was pending"), "{again}");
        let _ = std::fs::remove_file(&path);
    }

    /// The line: one verb, one session, and a `--store` that must be named.
    #[test]
    fn the_line_is_one_verb_one_session_and_a_named_store() {
        let line = |words: &[&str]| parse(&words.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let (store, session) = line(&["forget-import", "s-1", "--store", "/s"]).unwrap();
        assert_eq!(store, PathBuf::from("/s"));
        assert_eq!(session, "s-1");
        assert!(
            line(&["forget-import", "s-1"])
                .unwrap_err()
                .contains("no --store")
        );
        assert!(
            line(&["bogus", "--store", "/s"])
                .unwrap_err()
                .contains("no verb bogus")
        );
        assert!(line(&["--store", "/s"]).is_err());
        assert!(
            line(&["forget-import", "s-1", "s-2", "--store", "/s"])
                .unwrap_err()
                .contains("one session")
        );
    }
}
