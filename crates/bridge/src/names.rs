//! A session's name where a tool takes a session: the `/rename` title (or its slug) of a
//! conversation in the bridge's folder, resolved to the id every other part of the bridge keys by.
//! The rule is the store's own (`Store::meaning`), so `-r NAME` and a tool call cannot disagree.

use std::path::{Path, PathBuf};

use letibot_tokencore::store::{Meaning, Named};

/// Does `raw` already read as a handle the bridge can use as it is: an id, or the review queue's
/// name. Those skip the store.
fn is_id(raw: &str) -> bool {
    raw == "gatekeeper" || raw.starts_with("cc-") || raw.starts_with("s-")
}

/// The session `raw` names in any of `folders`, read from `store`. An id, an unknown name and a
/// store that does not exist come back unchanged for the caller's own refusal. A name that means
/// one session is that session's id; a name that means several, only BEGINS like one, or belongs
/// to an ARCHIVED session is refused with the candidates - a tool call has nobody to ask, so a
/// guess here would open a conversation nobody chose.
pub fn resolve(store: &Path, folders: &[PathBuf], raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() || is_id(raw) || !crate::store::exists(store) {
        return Ok(raw.to_string());
    }
    let folder = folders.first().map(|f| f.as_path());
    let store = crate::store::open_read(store, "session", folder)
        .map_err(|e| format!("reading session names: {e}"))?;
    let asked: Vec<String> = folders.iter().map(|f| f.display().to_string()).collect();
    let meaning = store
        .meaning(&asked, raw)
        .map_err(|e| format!("reading session names: {e}"))?;
    match meaning {
        Meaning::One(one) => Ok(one.id),
        // Nothing here is called that: the caller's own refusal is the one with the right words.
        Meaning::Nothing => Ok(raw.to_string()),
        Meaning::Ambiguous(rows) => Err(format!(
            "the name `{raw}` fits {} sessions: {}. Use the id.",
            rows.len(),
            listed(&rows)
        )),
        Meaning::Prefixes(rows) => Err(format!(
            "no session here is named `{raw}`; {} begin{} with it: {}. Name one in full, or use \
             the id.",
            rows.len(),
            if rows.len() == 1 { "s" } else { "" },
            listed(&rows)
        )),
        Meaning::Archived(rows) => Err(format!(
            "`{raw}` is archived: {}. Open it by id and it comes back into the list.",
            listed(&rows)
        )),
    }
}

/// Each candidate as `` `id` "title" in FOLDER``, which is what a refusal from a tool call has room
/// for - the id and the folder are the two facts that make it actionable.
fn listed(rows: &[Named]) -> String {
    rows.iter()
        .map(|r| format!("`{}` {:?} in {}", r.id, r.title, r.folder))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{SessionRecord, Store};

    fn put(s: &Store, id: &str, workspace: &str, title: &str) {
        s.put_session(&SessionRecord {
            id: id.into(),
            title: Some(title.into()),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: workspace.into(),
            owner: "op".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("session");
    }

    fn db(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("names-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("sessions.db"))
    }

    /// A name or its slug resolves to the id inside the folder, an id passes through, and a
    /// name nobody has comes back for the caller's own refusal. A PREFIX is refused with its
    /// candidates: a tool call has nobody to ask, so guessing one is how the wrong conversation
    /// gets opened.
    #[test]
    fn a_name_resolves_to_its_id_within_the_folder() {
        let (dir, path) = db("one");
        {
            let s = Store::open(&path).unwrap();
            put(&s, "s-100", "/w/here", "Stroppy Conformity");
            put(&s, "s-200", "/w/other", "Stroppy Conformity");
        }
        let here = vec![PathBuf::from("/w/here")];
        assert_eq!(
            resolve(&path, &here, "stroppy-conformity").unwrap(),
            "s-100"
        );
        assert_eq!(
            resolve(&path, &here, "Stroppy Conformity").unwrap(),
            "s-100"
        );
        assert_eq!(resolve(&path, &here, "s-200").unwrap(), "s-200");
        assert_eq!(resolve(&path, &here, "nope").unwrap(), "nope");
        let e = resolve(&path, &here, "stroppy").unwrap_err();
        assert!(
            e.contains("s-100") && e.contains("begin") && e.contains("Stroppy Conformity"),
            "the prefix is offered, with its candidate: {e}"
        );
        assert_eq!(
            resolve(&dir.join("missing.db"), &here, "stroppy-conformity").unwrap(),
            "stroppy-conformity"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An ARCHIVED session has freed its name, so it neither answers a tool call nor disappears
    /// into "no such session": the refusal says it is archived and how to bring it back.
    #[test]
    fn an_archived_session_is_refused_as_archived() {
        let (dir, path) = db("away");
        {
            let s = Store::open(&path).unwrap();
            put(&s, "s-100", "/w/here", "the old work");
            s.set_archived("s-100", true, Some("a week ago"))
                .expect("archived");
        }
        let here = vec![PathBuf::from("/w/here")];
        let e = resolve(&path, &here, "the old work").unwrap_err();
        assert!(
            e.contains("archived") && e.contains("s-100") && e.contains("comes back"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One name in two of the bridge's folders is refused with both candidates.
    #[test]
    fn an_ambiguous_name_is_refused_with_the_candidates() {
        let (dir, path) = db("two");
        {
            let s = Store::open(&path).unwrap();
            put(&s, "s-100", "/w/a", "same");
            put(&s, "s-200", "/w/b", "same");
        }
        let both = vec![PathBuf::from("/w/a"), PathBuf::from("/w/b")];
        let e = resolve(&path, &both, "same").unwrap_err();
        assert!(
            e.contains("s-100") && e.contains("s-200") && e.contains("/w/b"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
