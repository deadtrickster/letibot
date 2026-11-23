//! Where a composer's candidates come from: one trait and the room's implementation. The host hands
//! over a line and a caret, exactly what a plugin's `complete_` hook gets, so one row serves both.

use letibot_plugin::complete::{Candidate, Completion};
use letibot_plugin::pane::HUES;

/// How many candidates the row offers. Fewer than the host's `MAX_CANDIDATES`: six is as many as
/// one row of the composer's edge is read at a glance.
pub(crate) const TOP: usize = 6;

/// The note a candidate carries when its member is in another room.
pub(crate) const ELSEWHERE: &str = "elsewhere";

/// What an input asks for its candidates: the line as typed and the caret in it, as bytes.
pub(crate) trait Complete {
    fn complete(&self, text: &str, cursor: usize) -> Completion;
}

/// The verbs a room's composer offers before the head's own: the room's four, the chat's two, and
/// `/me`, which the server already reads.
pub(crate) const ROOM_VERBS: &[(&str, &str)] = &[
    ("dm", "NAME TEXT — a direct message"),
    ("invite", "WHO — a guest in this room"),
    ("leave", "ROOM — stop being a member"),
    ("reply", "N TEXT — answer a message"),
    ("me", "TEXT — say it as yourself"),
    ("search", "TEXT — find it in this room"),
    ("jump", "N — a message by seq"),
];

/// One member a completion can offer, flattened out of the roster: the handle the server
/// resolves, the name a person reads, and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Person {
    pub handle: String,
    pub name: String,
    /// In the channel being typed to. The others are offered too, marked [`ELSEWHERE`].
    pub here: bool,
    pub connected: bool,
}

/// The room's completion, built from the view each time it is asked: the roster, the verbs and
/// the aliases all move with the coordinator.
#[derive(Debug, Clone, Default)]
pub(crate) struct RoomComplete {
    pub people: Vec<Person>,
    /// The `/` verbs, the room's first, as `(name, hint)`.
    pub verbs: Vec<(String, String)>,
    /// The bridge's aliases as `(insert, note)`: `@cc` hears every folder.
    pub aliases: Vec<(String, String)>,
    /// Where `@path` completion walks; empty is this process's directory.
    pub workspace: String,
}

/// The most entries one `@path` walk visits: a Tab must answer at once in a large tree.
const FILE_WALK_LIMIT: usize = 20_000;

/// The completion a Tab opened: the answer, which candidate is in the line, and the line that
/// candidate wrote. A character typed on makes the line differ, and the next Tab asks fresh.
#[derive(Debug, Clone)]
pub(crate) struct Open {
    pub line: String,
    pub answer: Completion,
    pub idx: usize,
    /// How many bytes of the line the candidate showing put there, so the next one replaces the
    /// candidate and not the fragment again, which is no longer at the span it named.
    pub written: usize,
}

impl Complete for RoomComplete {
    fn complete(&self, text: &str, cursor: usize) -> Completion {
        let cursor = boundary(text, cursor);
        if let Some((from, frag)) = verb_fragment(text, cursor) {
            return Completion {
                from,
                to: cursor,
                candidates: self.verbs_for(frag),
            };
        }
        if let Some((from, frag)) = at_fragment(text, cursor) {
            // A path-like fragment is a file and anything else a handle. Files only for paths,
            // so the chat layer's roster answer stays the native one and a `@cc` stays a handle.
            let candidates = match pathish(frag) {
                true => self.files_for(frag),
                false => self.people_for(frag),
            };
            return Completion {
                from,
                to: cursor,
                candidates,
            };
        }
        Completion::default()
    }
}

impl RoomComplete {
    /// The `/` verbs: prefix before substring, and the list's own order within a rank, which is
    /// what puts the room's verbs ahead of the head's.
    fn verbs_for(&self, frag: &str) -> Vec<Candidate> {
        let needle = frag.to_lowercase();
        let mut found: Vec<(u8, Candidate)> = self
            .verbs
            .iter()
            .filter_map(|(name, hint)| {
                let rank = hit(&needle, name)?;
                Some((
                    rank,
                    Candidate {
                        // A trailing space, so the argument is typed on from where the verb
                        // left the caret.
                        insert: format!("/{name} "),
                        note: (!hint.is_empty()).then(|| hint.clone()),
                        ..Candidate::default()
                    },
                ))
            })
            .collect();
        found.sort_by_key(|(rank, _)| *rank);
        found
            .into_iter()
            .take(TOP)
            .map(|(_, candidate)| candidate)
            .collect()
    }

    /// The `@` names: the roster's people and the bridge's aliases, ranked by how they match and
    /// then by where they are. The handle is what gets inserted, because the server resolves
    /// handles and a display name would post as nobody.
    fn people_for(&self, frag: &str) -> Vec<Candidate> {
        let needle = frag.to_lowercase();
        let mut found: Vec<(u8, u8, usize, Candidate)> = Vec::new();
        for (i, p) in self.people.iter().enumerate() {
            let Some(rank) = best(&needle, &p.handle, &p.name) else {
                continue;
            };
            let group = match (p.here, p.connected) {
                (true, true) => 0,
                (true, false) => 1,
                _ => 2,
            };
            found.push((
                rank,
                group,
                i,
                Candidate {
                    insert: format!("@{}", p.handle),
                    label: (p.name != p.handle).then(|| p.name.clone()),
                    note: match (p.here, p.connected) {
                        (true, true) => None,
                        (true, false) => Some("away".to_string()),
                        _ => Some(ELSEWHERE.to_string()),
                    },
                    hue: Some(hue(&p.handle)),
                },
            ));
        }
        for (i, (insert, note)) in self.aliases.iter().enumerate() {
            let Some(rank) = hit(&needle, insert.trim_start_matches('@')) else {
                continue;
            };
            found.push((
                rank,
                3,
                self.people.len() + i,
                Candidate {
                    insert: insert.clone(),
                    note: Some(note.clone()),
                    ..Candidate::default()
                },
            ));
        }
        found.sort_by_key(|(rank, group, order, _)| (*rank, *group, *order));
        found
            .into_iter()
            .take(TOP)
            .map(|(_, _, _, candidate)| candidate)
            .collect()
    }
}

impl RoomComplete {
    /// The `@path` candidates: files under the workspace, `.gitignore` honoured as `grep` honours
    /// it, ranked by name prefix, path prefix, substring, then in-order letters. A fragment with
    /// a directory part walks that directory, so a named ignored tree is still offered.
    fn files_for(&self, frag: &str) -> Vec<Candidate> {
        let ws = match self.workspace.is_empty() {
            true => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            false => std::path::PathBuf::from(&self.workspace),
        };
        let (dir_part, needle) = match frag.rfind('/') {
            Some(i) => (&frag[..=i], &frag[i + 1..]),
            None => ("", frag),
        };
        let dir = match dir_part.strip_prefix("~/") {
            Some(rest) => {
                std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)
            }
            None => ws.join(dir_part),
        };
        // A directory part that is not a directory is a fuzzy needle over the whole tree.
        let (dir, prefix, needle) = if dir.is_dir() {
            (dir, dir_part, needle.to_lowercase())
        } else {
            (ws, "", frag.to_lowercase())
        };
        let walk = ignore::WalkBuilder::new(&dir)
            .hidden(false)
            .require_git(false)
            // `.gitignore` files only, the set the `grep` and `glob` tools read.
            .git_global(false)
            .ignore(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        let mut found: Vec<(u8, String)> = Vec::new();
        for e in walk.flatten().take(FILE_WALK_LIMIT) {
            if !e.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(rel) = e.path().strip_prefix(&dir) else {
                continue;
            };
            let rel = rel.to_string_lossy().to_string();
            if let Some(rank) = file_rank(&needle, &rel) {
                found.push((rank, rel));
            }
        }
        found.sort_by(|(ra, a), (rb, b)| (ra, a.len(), a).cmp(&(rb, b.len(), b)));
        found
            .into_iter()
            .take(TOP)
            .map(|(_, rel)| Candidate {
                insert: format!("@{prefix}{rel}"),
                ..Candidate::default()
            })
            .collect()
    }
}

/// A fragment that names a path rather than a handle, by the shell completion's own test.
pub(crate) fn pathish(frag: &str) -> bool {
    frag.starts_with(['.', '~']) || frag.contains('/')
}

/// How well a needle matches a relative path: `0` the file name starts with it, `1` the path
/// does, `2` it occurs, `3` its letters occur in order, `None` not at all.
fn file_rank(needle: &str, rel: &str) -> Option<u8> {
    let hay = rel.to_lowercase();
    let name = hay.rsplit('/').next().unwrap_or(&hay);
    if name.starts_with(needle) {
        return Some(0);
    }
    if hay.starts_with(needle) {
        return Some(1);
    }
    if hay.contains(needle) {
        return Some(2);
    }
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c)).then_some(3)
}

/// How well a needle matches: `0` at the front, `1` anywhere, `None` not at all. Case-blind,
/// because a person types `@Fix` at a handle called `fix-room-push`.
fn hit(needle: &str, hay: &str) -> Option<u8> {
    let hay = hay.to_lowercase();
    if hay.starts_with(needle) {
        Some(0)
    } else if hay.contains(needle) {
        Some(1)
    } else {
        None
    }
}

/// The better of the handle's and the name's match: one fragment can name a person by either.
fn best(needle: &str, handle: &str, name: &str) -> Option<u8> {
    match (hit(needle, handle), hit(needle, name)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The hue a handle speaks in: FNV-1a of the lowercased handle mod the palette, so one speaker
/// keeps one colour and a rename keeps it too.
pub(crate) fn hue(handle: &str) -> u8 {
    let mut h: u32 = 2_166_136_261;
    for b in handle.to_lowercase().bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16_777_619);
    }
    (h % u32::from(HUES)) as u8
}

/// The caret as a byte offset the line can be split at. The hooks promise a boundary; a caller
/// that hands back one inside a character must not panic the split.
fn boundary(text: &str, cursor: usize) -> usize {
    let mut c = cursor.min(text.len());
    while c > 0 && !text.is_char_boundary(c) {
        c -= 1;
    }
    c
}

/// The `/` word being typed: a slash at the start of the line, and no space after it yet.
fn verb_fragment(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let word = text.get(..cursor)?.strip_prefix('/')?;
    if word.contains(char::is_whitespace) {
        return None;
    }
    Some((0, word))
}

/// The `@` word being typed: the last word before the caret, when it starts with `@`. The span
/// starts at the `@`, so accepting a candidate replaces the fragment and not the `@` with it.
pub(crate) fn at_fragment(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let head = text.get(..cursor)?;
    let start = match head.rfind(char::is_whitespace) {
        Some(i) => i + head[i..].chars().next().map_or(1, char::len_utf8),
        None => 0,
    };
    let frag = head[start..].strip_prefix('@')?;
    Some((start, frag))
}
