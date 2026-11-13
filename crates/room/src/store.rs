//! The rooms' items and leases, kept the way chat is: one append-only file beside the socket,
//! one record per change, replayed from the top at start. No cursors and no tombstones: a record
//! is the row as it then stood, so the last version of each id is the truth.
//!
//! Two files: the live one, rewritten from memory when it outgrows what is held - [`RING`] is the
//! memory, not the store - and the closed one, appended to when a row leaves that window and never
//! rewritten. A settled item, an answered question and a released claim are still the record of
//! what happened, so they are filed rather than dropped.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::item::Item;
use crate::room::Question;

/// Rows kept in MEMORY per project, items and questions each, oldest id first out - what a queue
/// read, a pane and a verb can see without a file read. A row past this is closed into the store's
/// second file rather than dropped, so the bound is heap and nothing else.
pub const RING: usize = 500;

/// One line of the store. Appending the whole item on every change - a claim, a beat, a ruling -
/// is what keeps replay one pass with no cursor to maintain, at the cost of superseded versions
/// that [`Store::compact`] rewrites away.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Record {
    Item {
        project: String,
        item: Item,
    },
    Question {
        project: String,
        question: Question,
    },
    Claim {
        project: String,
        claim: crate::own::Claim,
    },
}

/// A row on its way into the closed segment: the three the ring drops, and the one place they are
/// named together.
enum Out {
    Item(Item),
    Question(Question),
    Claim(crate::own::Claim),
}

/// One store per host, shared by every project's room so all append to one file and a compaction
/// cannot drop a sibling project's rows. The shadow - the last version of each id - is kept true on
/// every put, so a failed write is healed by the next compaction rather than by a rebuild.
pub struct Store {
    path: PathBuf,
    bytes: u64,
    items: BTreeMap<String, BTreeMap<u64, Item>>,
    questions: BTreeMap<String, BTreeMap<u64, Question>>,
    claims: BTreeMap<String, BTreeMap<u64, crate::own::Claim>>,
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

/// Drop the oldest rows past [`RING`] and hand them back, so the caller can file them in the
/// closed segment: what leaves here leaves the memory, never the store.
fn trim<T>(rows: &mut BTreeMap<u64, T>) -> Vec<T> {
    let mut out = Vec::new();
    while rows.len() > RING {
        match rows.pop_first() {
            Some((_, row)) => out.push(row),
            None => break,
        }
    }
    out
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
            claims: BTreeMap::new(),
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
                Ok(Record::Claim { project, claim }) => {
                    store
                        .claims
                        .entry(project)
                        .or_default()
                        .insert(claim.id, claim);
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
            // Nothing is filed from here: a row is closed once, the moment a newer one pushes it
            // out of the live ring, so a replay that re-trims would only write what the file has.
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
        let gone = {
            let rows = self.items.entry(project.to_string()).or_default();
            rows.insert(item.id, item.clone());
            trim(rows)
        };
        self.close(project, gone.into_iter().map(Out::Item).collect());
        self.write(&[Record::Item {
            project: project.to_string(),
            item,
        }]);
    }

    /// Every project's ownership claims as the file last had them, ended ones included.
    pub fn claims(&self) -> BTreeMap<String, Vec<crate::own::Claim>> {
        self.claims
            .iter()
            .map(|(p, rows)| (p.clone(), rows.values().cloned().collect()))
            .collect()
    }

    /// Put an ownership claim as it now stands.
    pub fn put_claim(&mut self, project: &str, claim: crate::own::Claim) {
        let gone = {
            let rows = self.claims.entry(project.to_string()).or_default();
            rows.insert(claim.id, claim.clone());
            let mut ended: Vec<crate::own::Claim> = rows
                .values()
                .filter(|c| c.ended.is_some())
                .cloned()
                .collect();
            let past = ended.len().saturating_sub(crate::own::ENDED_KEEP);
            ended.truncate(past);
            for old in &ended {
                rows.remove(&old.id);
            }
            ended
        };
        self.close(project, gone.into_iter().map(Out::Claim).collect());
        self.write(&[Record::Claim {
            project: project.to_string(),
            claim,
        }]);
    }

    /// Put the question as it now stands - asked, or answered.
    pub fn put_question(&mut self, project: &str, question: Question) {
        let gone = {
            let rows = self.questions.entry(project.to_string()).or_default();
            rows.insert(question.id, question.clone());
            trim(rows)
        };
        self.close(project, gone.into_iter().map(Out::Question).collect());
        self.write(&[Record::Question {
            project: project.to_string(),
            question,
        }]);
    }

    /// One item by its id: the live window first, then the closed segment. A row the room has
    /// already put out of memory is still a fact a reader may name, which is what keeps a queue
    /// deeper than [`RING`] accountable.
    pub fn item(&self, project: &str, id: u64) -> Option<Item> {
        if let Some(item) = self.items.get(project).and_then(|rows| rows.get(&id)) {
            return Some(item.clone());
        }
        self.closed(project, |out| match out {
            Out::Item(item) if item.id == id => Some(item),
            _ => None,
        })
    }

    /// One question by its id, answered or not - the pair dsh keeps: a question and its
    /// settlement are both the record of a ruling.
    pub fn question(&self, project: &str, id: u64) -> Option<Question> {
        if let Some(question) = self.questions.get(project).and_then(|rows| rows.get(&id)) {
            return Some(question.clone());
        }
        self.closed(project, |out| match out {
            Out::Question(question) if question.id == id => Some(question),
            _ => None,
        })
    }

    /// One ownership claim by its id, ended or not.
    pub fn claim(&self, project: &str, id: u64) -> Option<crate::own::Claim> {
        if let Some(claim) = self.claims.get(project).and_then(|rows| rows.get(&id)) {
            return Some(claim.clone());
        }
        self.closed(project, |out| match out {
            Out::Claim(claim) if claim.id == id => Some(claim),
            _ => None,
        })
    }

    /// The closed segment's file: beside the live one, appended to when a row leaves the window and
    /// never rewritten. Nothing compacts it, because a closed row is what happened.
    fn closed_path(&self) -> PathBuf {
        self.path.with_extension("items-closed")
    }

    /// File the rows the ring just dropped. A row is written here once, so the last line naming an
    /// id is the state it closed in.
    fn close(&self, project: &str, rows: Vec<Out>) {
        if rows.is_empty() {
            return;
        }
        let records: Vec<Record> = rows
            .into_iter()
            .map(|out| match out {
                Out::Item(item) => Record::Item {
                    project: project.to_string(),
                    item,
                },
                Out::Question(question) => Record::Question {
                    project: project.to_string(),
                    question,
                },
                Out::Claim(claim) => Record::Claim {
                    project: project.to_string(),
                    claim,
                },
            })
            .collect();
        self.append(&self.closed_path(), &records);
    }

    /// Look in the closed segment, newest line first: the segments are append-only, so the last
    /// line that answers is the whole truth about the row. A scan of the file is what one by-id
    /// read costs, and a by-id read is rare - which is why no index of the history is kept in
    /// memory, where the bound is the whole point.
    fn closed<T>(&self, project: &str, want: impl Fn(Out) -> Option<T>) -> Option<T> {
        let bytes = std::fs::read(self.closed_path()).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines().rev() {
            match serde_json::from_str::<Record>(line) {
                Ok(Record::Item { project: p, item }) if p == project => {
                    if let Some(found) = want(Out::Item(item)) {
                        return Some(found);
                    }
                }
                Ok(Record::Question {
                    project: p,
                    question,
                }) if p == project => {
                    if let Some(found) = want(Out::Question(question)) {
                        return Some(found);
                    }
                }
                Ok(Record::Claim { project: p, claim }) if p == project => {
                    if let Some(found) = want(Out::Claim(claim)) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn write(&mut self, records: &[Record]) {
        let wrote = self.append(&self.path, records);
        if let Some(bytes) = wrote {
            self.bytes += bytes;
        }
        self.compact();
    }

    /// Append lines to one of the store's files, answering how many bytes they came to. A store
    /// that will not take a line says so once and the rooms continue: the shadow is the truth this
    /// process keeps, and the next compaction writes the file from it.
    fn append(&self, path: &Path, records: &[Record]) -> Option<u64> {
        let mut buf = String::new();
        for record in records {
            if let Ok(line) = serde_json::to_string(record) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(buf.as_bytes()))
        {
            Ok(()) => Some(buf.len() as u64),
            Err(e) => {
                eprintln!(
                    "room: items store {} not written ({e}); the rooms continue in memory",
                    path.display()
                );
                None
            }
        }
    }

    /// Rewrite the file from the shadow once it holds several times the live rows, so superseded
    /// versions and trimmed history do not grow it without bound. The 512 bytes a row is a floor, not
    /// a measure: it only keeps a small room from compacting on every beat.
    fn compact(&mut self) {
        let rows = self.items.values().map(BTreeMap::len).sum::<usize>()
            + self.questions.values().map(BTreeMap::len).sum::<usize>()
            + self.claims.values().map(BTreeMap::len).sum::<usize>();
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
        for (project, rows) in &self.claims {
            for claim in rows.values() {
                if let Ok(line) = serde_json::to_string(&Record::Claim {
                    project: project.clone(),
                    claim: claim.clone(),
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
        let _ = std::fs::remove_file(p.with_extension("items-closed"));
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
        assert_eq!(again.all()[0].id, 2, "item 1 left the window");
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

    fn question(id: u64) -> Question {
        Question {
            id,
            asked_ms: id,
            about: crate::room::About::ItemRaised {
                item: id,
                tag: "ram".into(),
            },
            text: format!("question {id}"),
            facts: crate::room::Facts {
                item: id,
                tag: "ram".into(),
                body: "8 GB".into(),
                posted_by: "s-a".into(),
                posted_ms: 1,
                claimed_by: None,
                claim_ms: None,
                beat_ms: None,
                abandonments: Vec::new(),
                waiting: Vec::new(),
            },
            project: String::new(),
            answer: None,
        }
    }

    /// The bar: six hundred items posted and settled, a restart, and the FIRST of them read by its
    /// id. The window bounds the memory; it is no longer the end of the record.
    #[test]
    fn a_settled_item_out_of_the_window_is_read_by_its_id_after_a_restart() {
        let p = path("closed-items");
        let mut room = room_with_store(&p, "", 10_000);
        let a = Speaker::top_level("s-a");
        for i in 0..600u64 {
            room.post(&a, &format!("p{i}"), "ram", "8 GB", 1_000 + i)
                .unwrap();
            room.claim(&a, &format!("c{i}"), "ram", 2_000 + i).unwrap();
            room.done(&a, &format!("d{i}"), i + 1, 3_000 + i).unwrap();
        }
        let again = room_with_store(&p, "", 10_000);
        assert_eq!(again.all().len(), RING, "the window is what memory holds");
        let first = again.read_item(1).expect("item 1 outlives the window");
        assert_eq!(first.state_name(), "done", "the state it closed in");
        assert_eq!(first.body, "8 GB");
        assert_eq!(
            again.read_item(600).map(|i| i.state_name()),
            Some("done"),
            "a row still in the window answers from it"
        );
        assert!(again.read_item(601).is_none(), "a number nothing minted");
    }

    /// An answered question outlives the window too, with its answer: the question and its
    /// settlement are one record, which is the rule this keeps.
    #[test]
    fn an_answered_question_out_of_the_window_is_read_by_its_id_with_its_answer() {
        let p = path("closed-questions");
        let mut store = Store::open(&p);
        for id in 1..=(RING as u64 + 100) {
            let mut q = question(id);
            q.answer = Some(crate::room::Ruling {
                decision: crate::room::Decision::Retry,
                by: "s-host".into(),
                ms: id + 1,
            });
            store.put_question("", q);
        }
        let live = store.questions_in("").unwrap_or_default();
        assert_eq!(live.len(), RING, "memory holds the window");
        let read = store.question("", 1).expect("question 1 is kept");
        assert_eq!(read.answer.expect("answered").by, "s-host");
        assert!(store.question("", 0).is_none(), "a number nothing asked");
    }

    /// A released claim past [`crate::own::ENDED_KEEP`] is the record of who held a scope, so it is
    /// filed rather than dropped when the room stops keeping ended claims in memory.
    #[test]
    fn a_released_claim_past_the_keep_is_read_by_its_id() {
        let p = path("closed-claims");
        let mut store = Store::open(&p);
        let total = crate::own::ENDED_KEEP as u64 + 20;
        for id in 1..=total {
            store.put_claim(
                "",
                crate::own::Claim {
                    id,
                    scope: format!("crates/room/**/{id}"),
                    owner: "s-a".into(),
                    since_ms: 1,
                    renewed_ms: 1,
                    ttl_ms: 60_000,
                    hand: None,
                    ended: Some(crate::own::Ended::Released {
                        by: "s-a".into(),
                        at_ms: 2,
                    }),
                    flagged: false,
                },
            );
        }
        let live = store.claims().remove("").unwrap_or_default();
        assert_eq!(
            live.len(),
            crate::own::ENDED_KEEP,
            "the room keeps the newest"
        );
        let first = store
            .claim("", 1)
            .expect("claim 1 is kept in the closed segment");
        assert!(matches!(
            first.ended,
            Some(crate::own::Ended::Released { .. })
        ));
        assert!(store.claim("", total + 1).is_none());
    }

    /// Nothing rewrites or removes the closed segment: the live file is compacted, and a closed row
    /// is what happened - the same rule the room's chat log is kept by.
    #[test]
    fn a_compaction_never_rewrites_the_closed_segment() {
        let p = path("closed-append-only");
        let mut room = room_with_store(&p, "", 10_000);
        let a = Speaker::top_level("s-a");
        for i in 0..(RING as u64 + 20) {
            room.post(&a, &format!("p{i}"), "ram", "8 GB", 1_000 + i)
                .unwrap();
        }
        let closed = p.with_extension("items-closed");
        let was = std::fs::read(&closed).expect("the closed segment");
        assert!(!was.is_empty(), "twenty rows were closed");
        let held = room.claim(&a, "c1", "ram", 2_000).unwrap().item.id;
        for i in 0..4_000u64 {
            room.beat(&a, &format!("b{i}"), held, 3_000 + i).unwrap();
        }
        let now = std::fs::read(&closed).expect("the closed segment");
        assert!(
            now.starts_with(&was),
            "the closed segment grew or stayed; it was not rewritten"
        );
    }
}
