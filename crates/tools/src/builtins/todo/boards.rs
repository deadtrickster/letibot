//! A `todo` wait that names ANOTHER board: an external seat's, kept in `seat_todo`, or another
//! session's of this project, kept in `todo`. One reader for the tool's refusal, the daemon's wait
//! sweep and the bridge's, so the three cannot disagree about what a name means or about when the
//! wait it carries has ended.

use std::collections::BTreeSet;
use std::path::Path;

use letibot_tokencore::store::{Store, TodoItem, TodoStatus};

/// How many rows a refusal names as the board it read: enough to pick from, not the whole plan.
const NAMED: usize = 6;

/// Another board, as the store answered a name.
#[derive(Debug, Clone)]
pub enum Board {
    /// The board as it stands, rows or none.
    Rows(Vec<TodoItem>),
    /// Nothing here answers to that name, and the sentence says what the store does hold: the
    /// refusal at write time, and *the board is gone* to the sweep.
    Unknown(String),
}

/// The board `name` names, read for the FOLDER the waiter is in: a seat's (`cc@<folder>[:,name]`)
/// or a session's, by its id or by the title exactly one of them has. `Err` is a board of ANOTHER
/// project, which is refused by name - a wait that crosses projects is not watched yet.
pub fn board_of(store: &Store, mine: &[String], name: &str) -> Result<Board, String> {
    let name = name.trim();
    if name.starts_with("cc@") {
        if crosses(mine, &handle_words(name)) {
            return Err(other_project(name, mine));
        }
        return match store.seat_board(name).map_err(|e| e.to_string())? {
            Some(rows) => Ok(Board::Rows(rows)),
            None => Ok(Board::Unknown(format!(
                "`{name}` has written no board in this store{}. A seat's board is written by the \
                 bridge serving it, and its handle is the `cc@<folder>[:name]` that seat's own \
                 `todos` answers with",
                held(store, mine)
            ))),
        };
    }
    let Some(session) = store.session_named(name).map_err(|e| e.to_string())? else {
        return Ok(Board::Unknown(format!(
            "no session and no seat is called `{name}`{}. A `todo` wait names an external seat \
             (`cc@<folder>`) or another session of this project by its id or by the name \
             `letibot --continue` takes",
            held(store, mine)
        )));
    };
    if crosses(mine, &folder_words(Path::new(&session.workspace_root))) {
        return Err(other_project(name, mine));
    }
    Ok(Board::Rows(
        store.todos(&session.id).map_err(|e| e.to_string())?,
    ))
}

/// What a board says about the row a wait names: `None` while that row is still open, and the note's
/// words once it is not - finished, struck off, or gone from a board that is replaced whole.
pub fn wait_ended(rows: &[TodoItem], name: &str, content: &str) -> Option<String> {
    let want = content.trim();
    let hits: Vec<&TodoItem> = rows.iter().filter(|r| r.content.trim() == want).collect();
    if hits
        .iter()
        .any(|r| !matches!(r.status, TodoStatus::Completed | TodoStatus::Cancelled))
    {
        return None;
    }
    match hits.first() {
        Some(row) if row.status == TodoStatus::Completed => {
            Some(format!("{name} finished \"{want}\""))
        }
        Some(_) => Some(format!("{name} struck \"{want}\" off")),
        None => Some(format!("`{want}` is no longer on {name}'s board")),
    }
}

/// A wait checked against the board it names, the moment it is written: a row that is not there
/// would never end, and one already finished has nothing left to wait for. The sentences are a
/// refusal's *why*, so they start from the board and not from the row being written.
pub fn check_wait(rows: &[TodoItem], content: &str) -> Result<(), String> {
    let want = content.trim();
    let Some(row) = rows.iter().find(|r| r.content.trim() == want) else {
        let open: Vec<String> = rows
            .iter()
            .filter(|r| !matches!(r.status, TodoStatus::Completed | TodoStatus::Cancelled))
            .take(NAMED)
            .map(|r| format!("\"{}\"", r.content.trim()))
            .collect();
        return Err(match open.is_empty() {
            true => "it has no such row, and no open row at all to wait on".to_string(),
            false => format!("it has no such row — its open rows are {}", open.join(", ")),
        });
    };
    match row.status {
        TodoStatus::Completed => Err("that row is already completed there".into()),
        TodoStatus::Cancelled => Err("that row was struck off there, so it will never finish".into()),
        _ => Ok(()),
    }
}

/// The word a FOLDER is known by: its own name, cleaned the way `bridge::rooms::base_name` cleans a
/// handle, so the two spellings meet. One word and not two: a handle's folder part is the folder
/// around the repository, and a session's workspace is the repository itself - which is why a
/// `cc@etcd:etcd-a36` handle carries BOTH and a project is compared on the words the two sides
/// share.
pub fn folder_words(path: &Path) -> Vec<String> {
    path.file_name()
        .map(|name| clean(&name.to_string_lossy()))
        .filter(|word| !word.is_empty())
        .into_iter()
        .collect()
}

/// The words a seat's HANDLE carries: `cc@<folder>` is one word, `cc@<folder>:<name>` is two.
pub fn handle_words(handle: &str) -> Vec<String> {
    let rest = handle.trim().strip_prefix("cc@").unwrap_or(handle.trim());
    rest.split(':')
        .map(clean)
        .filter(|w| !w.is_empty())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// A bridge's `base_name` cleaning, and nothing more: lowercase, everything but a letter, a digit or
/// an `@` becomes `-`, and the ends are trimmed of `-` and `@`.
fn clean(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '@' { c } else { '-' })
        .collect::<String>()
        .trim_matches(|c| c == '-' || c == '@')
        .to_string()
}

/// Whether two sets of project words are about different things: empty on either side is no answer,
/// which is a runtime that cannot tell two projects apart (a fixture, a standalone tool).
fn crosses(mine: &[String], theirs: &[String]) -> bool {
    !mine.is_empty() && !theirs.is_empty() && !mine.iter().any(|w| theirs.contains(w))
}

fn other_project(name: &str, mine: &[String]) -> String {
    format!(
        "`{name}` is a board of another project, and this one is {}. A wait across projects is not \
         watched yet: name a row of your own project, or write what you wait on in your own words — \
         `{{\"kind\": \"external\", \"what\": …}}`",
        mine.iter()
            .map(|w| format!("`{w}`"))
            .collect::<Vec<_>>()
            .join(" or ")
    )
}

/// What this store does hold, for a refusal that has to be repairable: the seat handles written, and
/// the sessions of this project by name.
fn held(store: &Store, mine: &[String]) -> String {
    let mut seats: Vec<String> = store
        .seat_boards()
        .map(|b| b.into_iter().map(|(seat, _)| seat).collect::<Vec<String>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|seat| !crosses(mine, &handle_words(seat)))
        .map(|seat| format!("`{seat}`"))
        .collect();
    let mut sessions: Vec<String> = store
        .list_sessions_archived(letibot_tokencore::store::Archived::Any)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !crosses(mine, &folder_words(Path::new(&s.workspace_root))))
        .filter_map(|s| s.title.map(|t| format!("`{t}`")))
        .collect();
    seats.sort();
    sessions.sort();
    seats.dedup();
    sessions.dedup();
    let mut said: Vec<String> = Vec::new();
    if !seats.is_empty() {
        said.push(format!("its seats are {}", seats.join(", ")));
    }
    if !sessions.is_empty() {
        said.push(format!("this project's sessions are {}", sessions.join(", ")));
    }
    match said.is_empty() {
        true => String::new(),
        false => format!(": {}", said.join("; ")),
    }
}
