//! A Claude Code seat's todo board (`docs/seat-todo.md`). `todo`'s class - mutable metadata about a
//! conversation, replaced whole, riding no chain - with a `seat` where that table has a
//! `session_id`: the seat is not a session, so this table has no foreign key and no cascade.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store, TodoItem, now_ms};

pub const SEAT_TODO_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS seat_todo (
    seat        TEXT PRIMARY KEY,
    todos_json  TEXT NOT NULL,   -- JSON array of {content, status, by, needs}
    updated_ms  INTEGER NOT NULL
);
"#;

impl Store {
    /// The seat's board as it was last written - empty for a seat that never wrote one, which is
    /// the same state as a board it left empty.
    pub fn seat_todos(&self, seat: &str) -> Result<Vec<TodoItem>> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT todos_json FROM seat_todo WHERE seat = ?1",
                params![seat],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json
            .map(|j| serde_json::from_str(&j))
            .transpose()?
            .unwrap_or_default())
    }

    /// Replace the seat's board whole, for `put_todos`' reason: a board is what it is now, not a
    /// sequence of edits.
    pub fn put_seat_todos(&self, seat: &str, todos: &[TodoItem]) -> Result<()> {
        self.exec(
            "INSERT INTO seat_todo (seat, todos_json, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(seat) DO UPDATE SET todos_json = ?2, updated_ms = ?3",
            params![seat, serde_json::to_string(todos)?, now_ms()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{TodoBy, TodoNeed, TodoStatus};

    fn row(content: &str) -> TodoItem {
        TodoItem {
            content: content.into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: vec![TodoNeed::Child {
                id: "s-package".into(),
            }],
        }
    }

    /// A seat's rows go in and come back with their edges, replaced whole: the wait on a package is
    /// half of what a board IS, and a restart that lost it would silent the row.
    #[test]
    fn a_seats_rows_are_written_replaced_and_read_back() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.seat_todos("cc@proj").unwrap().is_empty());
        let row = row("collect the package");
        s.put_seat_todos("cc@proj", std::slice::from_ref(&row))
            .unwrap();
        assert_eq!(s.seat_todos("cc@proj").unwrap(), vec![row.clone()]);
        assert!(
            s.seat_todos("cc@other").unwrap().is_empty(),
            "a seat's board is its own"
        );
        // Two seats, two boards: the primary key is the handle and not one shared row.
        s.put_seat_todos("cc@other", std::slice::from_ref(&row))
            .unwrap();
        assert_eq!(s.seat_todos("cc@proj").unwrap().len(), 1);
        s.put_seat_todos("cc@proj", &[]).unwrap();
        assert!(s.seat_todos("cc@proj").unwrap().is_empty());
        assert_eq!(
            s.seat_todos("cc@other").unwrap().len(),
            1,
            "one seat's write never reaches another's board"
        );
    }

    /// A store from before v41 gains the table on its next open, and a seat's rows written into it
    /// are read back by another connection: the restart a bridge takes between two MCP runs.
    #[test]
    fn a_v40_store_gains_the_seat_table_and_keeps_a_seats_rows() {
        let path = std::env::temp_dir().join(format!(
            "letibot-seat-todo-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());
        {
            let s = Store::open(&path).unwrap();
            s.put_seat_todos("cc@proj", &[row("keep me")]).unwrap();
        }
        {
            // Back to a store that predates the table, the way one on disk from an older bridge is.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("DROP TABLE seat_todo", []).unwrap();
            c.execute("UPDATE schema_version SET version = 40", [])
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert!(
            s.seat_todos("cc@proj").unwrap().is_empty(),
            "the table came back, empty"
        );
        s.put_seat_todos("cc@proj", &[row("keep me")]).unwrap();
        let next_bridge = Store::open_private(&path).unwrap();
        assert_eq!(
            next_bridge.seat_todos("cc@proj").unwrap(),
            vec![row("keep me")],
            "a connection of its own - the next bridge - reads what was written"
        );
    }
}
