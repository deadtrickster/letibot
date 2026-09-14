//! Spool, then ack.
//!
//! flowy's contract: *the mark is not moved on handover. A waiter that is handed
//! messages and dies before it has written them out has lost them permanently
//! if the server counted the handover as delivery.* Its CLI writes every
//! delivered page to a local file before acking, so a crash costs a duplicate
//! rather than a silence, and `flowy inbox replay` reads the file back.
//!
//! The seat keeps that contract with the same shape: one JSON line per event,
//! appended and flushed **before** `POST /api/inbox/ack`. A daemon that dies
//! between the two comes back to a page it has already spooled, which is the
//! duplicate. A daemon that dies after the ack but before a session read the
//! firing still has the line here, and `flowy replay` hands it over.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::client::Event;

#[derive(Debug, Clone)]
pub struct Spool {
    path: PathBuf,
}

impl Spool {
    /// `~/.local/state/letibot/flowy/<reader>.jsonl` unless a directory is given.
    pub fn for_reader(dir: Option<&Path>, reader: &str) -> Result<Spool, std::io::Error> {
        let dir = match dir {
            Some(d) => d.to_path_buf(),
            None => {
                let base = std::env::var_os("XDG_STATE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME")
                            .map(|h| PathBuf::from(h).join(".local").join("state"))
                    })
                    .ok_or_else(|| {
                        std::io::Error::other("neither XDG_STATE_HOME nor HOME is set")
                    })?;
                base.join("letibot").join("flowy")
            }
        };
        std::fs::create_dir_all(&dir)?;
        let safe: String = reader
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-".contains(c) {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        Ok(Spool {
            path: dir.join(format!("{safe}.jsonl")),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append and flush. An error here is returned, and the caller must NOT ack:
    /// a page that could not be written out has not been delivered.
    pub fn append(&self, events: &[Event]) -> Result<(), std::io::Error> {
        if events.is_empty() {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        let mut buf = Vec::new();
        for e in events {
            serde_json::to_writer(&mut buf, e)?;
            buf.push(b'\n');
        }
        f.write_all(&buf)?;
        f.flush()?;
        f.sync_data()
    }

    /// The last `n` spooled events, oldest first. Lines that do not parse are
    /// skipped and counted, not silently dropped.
    pub fn replay(&self, n: usize) -> Result<(Vec<Event>, usize), std::io::Error> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
            Err(e) => return Err(e),
        };
        let mut bad = 0;
        let mut all: Vec<Event> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| match serde_json::from_str(l) {
                Ok(e) => Some(e),
                Err(_) => {
                    bad += 1;
                    None
                }
            })
            .collect();
        let keep = all.len().saturating_sub(n);
        all.drain(..keep);
        Ok((all, bad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_one_line_per_event_and_replays_the_tail() {
        let d = std::env::temp_dir().join(format!("letibot-spool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let s = Spool::for_reader(Some(&d), "a/b").unwrap();
        assert!(s.path().ends_with("a-b.jsonl"));
        let ev = |i: i64| -> Event {
            serde_json::from_value(
                serde_json::json!({"id": format!("e{i}"), "type": "chat", "seq_hlc": i}),
            )
            .unwrap()
        };
        s.append(&[ev(1), ev(2)]).unwrap();
        s.append(&[ev(3)]).unwrap();
        let (tail, bad) = s.replay(2).unwrap();
        assert_eq!(bad, 0);
        assert_eq!(
            tail.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["e2", "e3"]
        );
        assert_eq!(s.replay(10).unwrap().0.len(), 3);
    }
}
