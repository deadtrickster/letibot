//! A session's name where a tool takes a session: the `/rename` title (or its slug) of a
//! conversation in the bridge's folder, resolved to the id every other part of the bridge keys by.

use std::path::{Path, PathBuf};

use letibot_tokencore::store::Store;

/// Does `raw` already read as a handle the bridge can use as it is: an id, or the review queue's
/// name. Those skip the store.
fn is_id(raw: &str) -> bool {
    raw == "gatekeeper" || raw.starts_with("cc-") || raw.starts_with("s-")
}

/// The session `raw` names in any of `folders`, read from `store`. An id, an unknown name and a
/// store that does not exist come back unchanged for the caller's own refusal. A name that fits
/// several sessions is refused with each one's id and folder.
pub fn resolve(store: &Path, folders: &[PathBuf], raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() || is_id(raw) || !store.exists() {
        return Ok(raw.to_string());
    }
    let store = Store::open(store).map_err(|e| format!("reading session names: {e}"))?;
    let mut found: Vec<(String, String)> = Vec::new();
    for folder in folders {
        let rows = store
            .named_in(&folder.display().to_string(), raw)
            .map_err(|e| format!("reading session names: {e}"))?;
        for s in rows {
            if !found.iter().any(|(id, _)| *id == s.id) {
                found.push((s.id, folder.display().to_string()));
            }
        }
    }
    match found.as_slice() {
        [] => Ok(raw.to_string()),
        [(id, _)] => Ok(id.clone()),
        many => Err(format!(
            "the name `{raw}` fits {} sessions: {}. Use the id.",
            many.len(),
            many.iter()
                .map(|(id, folder)| format!("`{id}` in {folder}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::SessionRecord;

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
    /// name nobody has comes back for the caller's own refusal.
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
        assert_eq!(
            resolve(&path, &here, "stroppy").unwrap(),
            "stroppy",
            "no prefix guess"
        );
        assert_eq!(
            resolve(&dir.join("missing.db"), &here, "stroppy-conformity").unwrap(),
            "stroppy-conformity"
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
