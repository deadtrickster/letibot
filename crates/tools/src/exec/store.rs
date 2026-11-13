//! The letibot data directory (`~/.local/share/letibot`) as a seat sees it: its data is
//! readable and never writable, its secrets and live control files are neither. One rule set,
//! read by the macOS profile, the bwrap view and the file tools, so they cannot drift.
//!
//! Data: `sessions.db*`, `metrics.db*`, `embeddings.db*`, `logs/**`, `panes/**`, `room-*.chat`.
//! Everything else in the directory stays fenced (`sessions/<id>/` is writable only by its
//! own session, through the file tools), and `~/.config/letibot` is not in this directory.

use std::path::{Path, PathBuf};

/// The store, relative to the operator's home.
pub const STORE_REL: &str = ".local/share/letibot";

/// SQLite files: the database and its `-wal`, `-shm` and `-journal` companions.
pub const DATA_DBS: &[&str] = &["sessions.db", "metrics.db", "embeddings.db"];
const DB_COMPANIONS: &[&str] = &["-wal", "-shm", "-journal"];
/// Directories readable as a whole.
pub const DATA_DIRS: &[&str] = &["logs", "panes"];
/// A top-level `room-<key>.chat` is a transcript: data.
const CHAT: (&str, &str) = ("room-", ".chat");

/// Names that are secrets or live control files, wherever they sit under the store.
pub const SECRET_SUFFIXES: &[&str] = &[
    ".token",
    ".lease",
    ".lease.lock",
    ".lock",
    ".sock",
    ".placement",
];
/// `.restart` and `.restart-<pid>`.
pub const SECRET_INFIX: &str = ".restart";
/// A directory of keys.
pub const SECRET_DIRS: &[&str] = &["keys"];

fn name_is_secret(name: &str) -> bool {
    SECRET_SUFFIXES.iter().any(|s| name.ends_with(s))
        || name.contains(SECRET_INFIX)
        || SECRET_DIRS.contains(&name)
}

/// Whether `rel` (relative to the store) is a secret or control file, or sits under one.
pub fn is_secret(rel: &Path) -> bool {
    rel.components()
        .any(|c| name_is_secret(&c.as_os_str().to_string_lossy()))
}

/// Whether a seat may read `rel` (relative to the store). Never true for a write.
pub fn is_readable(rel: &Path) -> bool {
    if is_secret(rel) {
        return false;
    }
    let mut parts = rel.components();
    let Some(first) = parts.next() else {
        return false;
    };
    let first = first.as_os_str().to_string_lossy();
    let top_level = parts.next().is_none();
    DATA_DIRS.contains(&first.as_ref())
        || (top_level
            && (DATA_DBS.iter().any(|db| {
                first == *db
                    || DB_COMPANIONS
                        .iter()
                        .any(|c| first.strip_prefix(db).is_some_and(|r| r == *c))
            }) || (first.starts_with(CHAT.0) && first.ends_with(CHAT.1))))
}

/// The top-level entries of `store` a seat may read, for a view that mounts them one by one.
pub fn readable_entries(store: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(store)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_readable(Path::new(&e.file_name())))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Secret files and directories inside the readable `entries`, for a view that mounts the
/// entries whole and has to cover these. Bounded: a store is shallow and a walk must end.
pub fn secrets_within(entries: &[PathBuf]) -> Vec<(PathBuf, bool)> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, bool)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            if name_is_secret(&e.file_name().to_string_lossy()) {
                out.push((path, is_dir));
            } else if is_dir && depth > 0 {
                walk(&path, depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for e in entries.iter().filter(|e| e.is_dir()) {
        walk(e, 8, &mut out);
    }
    out
}

/// The note a failed command earns when what it names is inside the letibot store. A command's
/// view does not hold the store's session directories (`store.rs` is the rule), so an ENOENT
/// there is the fence rather than a missing file, and the note names the tools that do see it.
/// Two spellings are recognised: the absolute store and `~/<store>`, as an output may print either.
pub fn fence_note(body: &str, store: &Path) -> Option<String> {
    let absolute = store.display().to_string();
    let home = format!("~/{STORE_REL}");
    let at = body
        .match_indices(absolute.as_str())
        .map(|(i, s)| (i, s.len()))
        .chain(body.match_indices(home.as_str()).map(|(i, s)| (i, s.len())))
        // The store itself, not a name that merely begins with it: `letibot-copy` is not the store.
        .find(|(i, len)| {
            body[i + len..]
                .chars()
                .next()
                .is_none_or(|c| c == '/' || !(c.is_alphanumeric() || "._-~".contains(c)))
        })?;
    // The path it named, cut where a message stops spelling it.
    let named: String = body[at.0..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !",;:'\"`)]}".contains(*c))
        .take(200)
        .collect();
    Some(format!(
        "`{named}` is inside the letibot store, which a command's view does not hold: this \
         session's own directory there is reached with `read`, `edit` and `write`, never with \
         `bash`. Use a file tool for it — no `[boundary]` key grants the store."
    ))
}

fn re(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if ".+*?()[]{}|^$\\".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// SBPL filter matching the readable data under `store`.
pub fn sbpl_data(store: &Path) -> String {
    let s = re(&store.display().to_string());
    let dbs = DATA_DBS.iter().map(|d| re(d)).collect::<Vec<_>>().join("|");
    let comp = DB_COMPANIONS
        .iter()
        .map(|d| re(d))
        .collect::<Vec<_>>()
        .join("|");
    let mut out = String::from("(require-any\n");
    out.push_str(&format!("    (regex #\"^{s}/({dbs})({comp})?$\")\n"));
    out.push_str(&format!(
        "    (regex #\"^{s}/{}[^/]*{}$\")\n",
        re(CHAT.0),
        re(CHAT.1)
    ));
    for d in DATA_DIRS {
        out.push_str(&format!("    (subpath \"{}/{d}\")\n", store.display()));
    }
    out.push_str("  )");
    out
}

/// SBPL filter matching secrets and control files anywhere under `store`.
pub fn sbpl_secret(store: &Path) -> String {
    let s = re(&store.display().to_string());
    let suff = SECRET_SUFFIXES
        .iter()
        .map(|d| re(d))
        .collect::<Vec<_>>()
        .join("|");
    let dirs = SECRET_DIRS
        .iter()
        .map(|d| re(d))
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "(require-any\n    (regex #\"^{s}/(.*/)?[^/]*({suff})$\")\n    \
         (regex #\"^{s}/(.*/)?[^/]*{}[^/]*$\")\n    \
         (regex #\"^{s}/(.*/)?({dirs})(/.*)?$\")\n  )",
        re(SECRET_INFIX)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_is_readable_and_secrets_are_not() {
        for ok in [
            "sessions.db",
            "sessions.db-wal",
            "metrics.db-shm",
            "embeddings.db",
            "logs/2026/a.jsonl",
            "panes/x/y",
            "room-h.chat",
        ] {
            assert!(is_readable(Path::new(ok)), "{ok}");
        }
        for no in [
            "room-h.token",
            "room-h.lease",
            "room-h.lease.lock",
            "daemon-room-h.lock",
            "room-h.sock",
            "daemon-room-h.placement",
            "room-h.restart",
            "room-h.restart-123",
            "keys",
            "keys/k",
            "logs/x.token",
            "panes/keys/k",
            "sessions/s-1/plugins/x",
            "sessions.dbx",
            "gate.sh",
            "landing/x",
            "notes/x",
            "sessions.db/x",
        ] {
            assert!(!is_readable(Path::new(no)), "{no}");
        }
    }

    /// A command that failed on a path in the store is answered with the tools that do hold it:
    /// an ENOENT under `sessions/` is the fence, and the model cannot tell that from a typo.
    #[test]
    fn a_failed_commands_store_path_earns_the_file_tools_sentence() {
        let store = Path::new("/home/op/.local/share/letibot");
        let said = "cat: /home/op/.local/share/letibot/sessions/s-1/plugins/fuzzlegs/plugin.toml: \
                    No such file or directory";
        let note = fence_note(said, store).expect("the note");
        assert!(
            note.contains(
                "/home/op/.local/share/letibot/sessions/s-1/plugins/fuzzlegs/plugin.toml"
            ),
            "the path it named, cut where the message stops spelling it: {note}"
        );
        for tool in ["read", "edit", "write"] {
            assert!(
                note.contains(tool),
                "{tool} must be named as the way in: {note}"
            );
        }
        assert!(
            note.contains("`bash`") && note.contains("does not hold"),
            "and why a command could not: {note}"
        );
        // The `~/` spelling a seat's own output often prints.
        assert!(fence_note("ls: cannot access '~/.local/share/letibot/x'", store).is_some());
        // A body that names nothing in the store earns nothing.
        assert!(fence_note("cat: src/lib.rs: No such file or directory", store).is_none());
        // And a path that only looks similar is not the store.
        assert!(fence_note("cat: /home/op/.local/share/letibot-copy/x", store).is_none());
    }
}
