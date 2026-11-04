//! The rooms' items and leases, kept the way chat is: one append-only file beside the socket,
//! one record per change, replayed from the top at start. No cursors and no tombstones: a record
//! is the row as it then stood, so the last version of each id is the truth.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::item::Item;
use crate::room::Question;

/// Rows kept per project, items and questions each, oldest id first out. The store is replayed
/// into memory at every start, so this bounds disk and the coordinator's heap together: a row
/// nobody finished, dropped or answered is history, and a queue deeper than this has a problem
/// the cap only makes visible.
pub const RING: usize = 500;

/// One line of the store. Appending the whole item on every change - a claim, a beat, a ruling -
/// is what keeps replay one pass with no cursor to maintain, at the cost of superseded versions
/// that [`Store::compact`] rewrites away.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Record {
    Item { project: String, item: Item },
    Question { project: String, question: Question },
}

/// One store per host, shared by every project's room so all append to one file and a compaction
/// cannot drop a sibling project's rows. The shadow - the last version of each id - is what the
/// file is rewritten from, so it is kept true on every put and a failed write is healed by the
/// next compaction rather than by a rebuild.
pub struct Store {
    path: PathBuf,
    bytes: u64,
    items: BTreeMap<String, BTreeMap<u64, Item>>,
    questions: BTreeMap<String, BTreeMap<u64, Question>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not the rows: a Debug that dumps the shadow would make every panic print the queue.
        f.debug_struct("Store")
            .field("path", &self.path)
            .field("bytes", &self.bytes)
            .field("projects", &self.projects().len())
            .finish()
    }
}

fn trim<T>(rows: &mut BTreeMap<u64, T>) {
    while rows.len() > RING {
        rows.pop_first();
    }
}

impl Store {
    /// Read `path` into the shadow. An unreadable line is skipped and said - a torn last line is
    /// the ordinary wreckage of a crash - and a missing file is an empty store, not an error:
    /// the first start has nothing to read.
    pub fn open(path: &Path) -> Store {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut store = Store {
            path: path.to_path_buf(),
            bytes: text.len() as u64,
            items: BTreeMap::new(),
            questions: BTreeMap::new(),
        };
        for (n, line) in text.lines().enumerate() {
            match serde_json::from_str::<Record>(line) {
                Ok(Record::Item { project, item }) => {
                    store
                        .items
                        .entry(project)
                        .or_default()
                        .insert(item.id, item);
                }
                Ok(Record::Question { project, question }) => {
                    store
                        .questions
                        .entry(project)
                        .or_default()
                        .insert(question.id, question);
                }
                Err(e) => eprintln!(
                    "room: items store {} line {} is not a record ({e}); skipped",
                    path.display(),
                    n + 1
                ),
            }
        }
        for rows in store.items.values_mut() {
            trim(rows);
        }
        for rows in store.questions.values_mut() {
            trim(rows);
        }
        store
    }

    /// The projects the file holds rows for, sorted - the rooms a restart must rebuild before
    /// any of them dials in again.
    pub fn projects(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .items
            .keys()
            .chain(self.questions.keys())
            .cloned()
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// One project's items, oldest id first, or `None` when the file holds none: a room the file
    /// has never heard of keeps what it already holds rather than being emptied by silence.
    pub fn items_in(&self, project: &str) -> Option<Vec<Item>> {
        self.items
            .get(project)
            .map(|rows| rows.values().cloned().collect())
    }

    /// One project's questions, oldest id first, or `None` for the same reason as [`Store::items_in`].
    pub fn questions_in(&self, project: &str) -> Option<Vec<Question>> {
        self.questions
            .get(project)
            .map(|rows| rows.values().cloned().collect())
    }

    /// Put the item as it now stands: shadow first, then the file. The shadow is updated even
    /// when the write fails, so the room and the next compaction agree and the file catches up.
    pub fn put_item(&mut self, project: &str, item: Item) {
        // The record is the item the caller holds even when the trim drops it straight back out:
        // a replay re-inserts and re-trims, so the two can disagree about history, never about
        // the newest version of a row that is live.
        {
            let rows = self.items.entry(project.to_string()).or_default();
            rows.insert(item.id, item.clone());
            trim(rows);
        }
        self.write(&[Record::Item {
            project: project.to_string(),
            item,
        }]);
    }

    /// Put the question as it now stands - asked, or answered.
    pub fn put_question(&mut self, project: &str, question: Question) {
        {
            let rows = self.questions.entry(project.to_string()).or_default();
            rows.insert(question.id, question.clone());
            trim(rows);
        }
        self.write(&[Record::Question {
            project: project.to_string(),
            question,
        }]);
    }

    fn write(&mut self, records: &[Record]) {
        let mut buf = String::new();
        for record in records {
            if let Ok(line) = serde_json::to_string(record) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(buf.as_bytes()));
        match wrote {
            Ok(()) => self.bytes += buf.len() as u64,
            Err(e) => eprintln!(
                "room: items store {} not written ({e}); the rooms continue in memory",
                self.path.display()
            ),
        }
        self.compact();
    }

    /// Rewrite the file from the shadow once it holds several times the live rows, so superseded
    /// versions and trimmed history do not grow it without bound. The 512 bytes a row is a floor,
    /// not a measure: it only decides when to rewrite, and the floor keeps a small room from
    /// compacting on every beat.
    fn compact(&mut self) {
        let rows = self.items.values().map(BTreeMap::len).sum::<usize>()
            + self.questions.values().map(BTreeMap::len).sum::<usize>();
        if self.bytes <= (rows as u64 * 512).max(64 * 1024) {
            return;
        }
        let mut buf = String::new();
        for (project, rows) in &self.items {
            for item in rows.values() {
                if let Ok(line) = serde_json::to_string(&Record::Item {
                    project: project.clone(),
                    item: item.clone(),
                }) {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
        }
        for (project, rows) in &self.questions {
            for question in rows.values() {
                if let Ok(line) = serde_json::to_string(&Record::Question {
                    project: project.clone(),
                    question: question.clone(),
                }) {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
        }
        let tmp = self.path.with_extension("items-tmp");
        let done =
            std::fs::write(&tmp, buf.as_bytes()).and_then(|()| std::fs::rename(&tmp, &self.path));
        match done {
            Ok(()) => self.bytes = buf.len() as u64,
            Err(e) => eprintln!(
                "room: items store {} not compacted ({e}); appends continue",
                self.path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Speaker;

    fn path(what: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("letibot-room-store-{}-{what}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn room_with_store(p: &Path, project: &str, lease_ms: u64) -> crate::room::Room {
        let store = std::sync::Arc::new(std::sync::Mutex::new(Store::open(p)));
        let mut room = crate::room::Room::with_lease_ms(lease_ms);
        room.load(&store, project, 1_000);
        room
    }

    #[test]
    fn the_last_version_of_an_item_is_what_a_reload_finds() {
        let p = path("replay");
        let mut room = room_with_store(&p, "", 10_000);
        let a = Speaker::top_level("s-a");
        let b = Speaker::top_level("s-b");
        room.post(&a, "p1", "ram", "8 GB", 1_000).unwrap();
        room.claim(&a, "c1", "ram", 2_000).unwrap();
        room.done(&a, "d1", 1, 3_000).unwrap();
        room.post(&b, "p2", "ram", "2 GB", 4_000).unwrap();

        let mut again = room_with_store(&p, "", 10_000);
        assert_eq!(
            again.all().len(),
            2,
            "both items, the finished one included"
        );
        assert_eq!(again.item(1).unwrap().state_name(), "done");
        assert_eq!(again.item(2).unwrap().holder(), None);
        // And the ids continue rather than colliding with the restored ones.
        let out = again.post(&b, "p3", "ram", "1 GB", 5_000).unwrap();
        assert_eq!(out.item.id, 3);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn one_file_keeps_projects_apart() {
        let p = path("projects");
        for project in ["aaaaaaaaaaaa", "bbbbbbbbbbbb"] {
            let mut room = room_with_store(&p, project, 10_000);
            let a = Speaker::top_level("s-a");
            room.post(&a, "p1", "ram", "8 GB", 1_000).unwrap();
        }
        let a = room_with_store(&p, "aaaaaaaaaaaa", 10_000);
        let b = room_with_store(&p, "bbbbbbbbbbbb", 10_000);
        assert_eq!(a.all().len(), 1);
        assert_eq!(b.all().len(), 1);
        assert_eq!(a.all()[0].id, b.all()[0].id, "two rooms number from 1");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_cap_keeps_the_newest_rows_and_is_survived_by_the_ids() {
        let p = path("cap");
        let mut room = room_with_store(&p, "", 10_000);
        let a = Speaker::top_level("s-a");
        for i in 0..=RING as u64 {
            room.post(&a, &format!("p{i}"), "ram", "8 GB", 1_000 + i)
                .unwrap();
        }
        let mut again = room_with_store(&p, "", 10_000);
        assert_eq!(again.all().len(), RING, "the oldest went first");
        assert_eq!(again.all()[0].id, 2, "item 1 is history now");
        let out = again.post(&a, "after", "ram", "8 GB", 9_000).unwrap();
        assert_eq!(out.item.id, RING as u64 + 2, "no id is reused");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_file_is_rewritten_once_it_outgrows_what_is_live() {
        let p = path("compact");
        let mut room = room_with_store(&p, "", 10_000);
        let a = Speaker::top_level("s-a");
        room.post(&a, "p1", "ram", "8 GB", 1_000).unwrap();
        room.claim(&a, "c1", "ram", 2_000).unwrap();
        for i in 0..4_000u64 {
            room.beat(&a, &format!("b{i}"), 1, 3_000 + i).unwrap();
        }
        let size = std::fs::metadata(&p).expect("the store").len();
        assert!(size < 64 * 1024, "compacted: {size}");
        let again = room_with_store(&p, "", 10_000);
        assert_eq!(again.item(1).unwrap().claim().unwrap().beat_ms, 6_999);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_unreadable_line_is_skipped_and_the_rest_still_loads() {
        let p = path("torn");
        std::fs::write(&p, b"{\"item\":{\"project\":\"\",\"item\"}\n").expect("a torn line");
        let store = Store::open(&p);
        assert!(store.items_in("").unwrap_or_default().is_empty());
        let _ = std::fs::remove_file(&p);
    }
}
