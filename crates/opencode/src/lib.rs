//! Reading an opencode conversation into this head's rows.
//! Opens opencode's SQLite database read-only and turns its stored messages into `TranscriptItem`s. Rows say they
//! are imported, the ledger never claims the spend, and an unknown tag becomes a counted scrap.
//!
//! # The two layouts
//!
//! opencode 1.x wrote `session`, `message` and `part` - `[v1]` below. Every 2.x writes `session_v2`,
//! `session_message` and `session_part` - `[v2]` - and freezes the v1 rows at the upgrade while a background job
//! migrates them across (`v1-migration.bun.ts`). So a fresh install has no v1 tables at all, an upgraded one has
//! both with the v1 rows stopping at the moment it was upgraded, and the operator's own 1.18.31 has v1 only.
//!
//! A database is read as v2 when it has `session_v2` at all. Within such a database each session id is read from
//! v2 where v2 has it and from the frozen v1 rows where it does not - a session the migration job has not reached
//! yet, which is the one case where v1 rows are still the whole history. v1 rows are never preferred over v2 ones
//! for the same id: they are frozen, so reading them would cut a continued session off at the upgrade point.
//! A database with no `session_v2` is read exactly as it was before v2 existed: one query over `part`, ordered by
//! `time_created`.
//!
//! The unit of a v2 read is the stored message (`session_message`), and a v1 read's is the `part` row; a tree's
//! [`Tree::unit`] says which, so the progress line counts what it is actually walking.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use letibot_transcript::{ReasoningField, ToolCall, ToolOutcome, TranscriptItem, UserPart};
use rusqlite::{Connection, OpenFlags};

// opencode 2.x's layout: `session_v2`, `session_message` and `session_part`, in its own file.
mod v2;

/// Everything that can stop a read.
#[derive(Debug)]
pub enum ImportError {
    /// The database is not there, or is not a database. Reported as *this* and
    /// not "no such session", because the two are different facts and the operator
    /// asked for them to be told apart.
    NoDatabase { path: PathBuf, err: String },
    /// The database opened, the schema is there, and no session has that id.
    NoSuchSession { id: String },
    /// The database opened and holds neither layout: no `session` (v1) and no `session_v2` (v2).
    /// A different fact from a failed query, and said as such rather than as "no such table".
    NeitherSchema { path: PathBuf, tables: Vec<String> },
    /// A query that the schema should answer did not.
    Sql(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::NoDatabase { path, err } => write!(
                f,
                "no opencode database at {} ({err}). There is nothing to import from — \
                 this is the file being absent or unreadable, not the session being absent.",
                path.display()
            ),
            ImportError::NoSuchSession { id } => write!(
                f,
                "the database has no session `{id}`. Check the id: opencode ids look \
                 like `ses_…`, and the namespace mark this head strips (`oc-`) is not \
                 part of the id."
            ),
            ImportError::NeitherSchema { path, tables } => write!(
                f,
                "the database at {} holds neither of opencode's layouts: no `session` table (what 1.x \
                 writes) and no `session_v2` (what every 2.x writes). This is not a session id problem. \
                 Its tables are: {}.",
                path.display(),
                if tables.is_empty() {
                    "none".to_string()
                } else {
                    tables.join(", ")
                }
            ),
            ImportError::Sql(e) => write!(f, "reading opencode's database: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

pub type Result<T> = std::result::Result<T, ImportError>;

/// Token counts, in whatever provider's units they were recorded - opencode's here.
/// Four numbers and a cache pair, matching opencode's `tokens` object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    /// The total a header shows. opencode's `total` is exactly the sum, but it is
    /// computed here rather than read so a row missing `total` still has one.
    pub fn total(&self) -> u64 {
        [
            self.output,
            self.reasoning,
            self.cache_read,
            self.cache_write,
        ]
        .into_iter()
        .fold(self.input, u64::saturating_add)
    }
}

/// One opencode session's own metadata - what rule 1 needs and what the header shows.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    /// opencode's `directory`: where the session ran. Part of the origin.
    pub directory: String,
    /// The project's `worktree`, when the project row is present.
    pub worktree: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub cost: f64,
    pub tokens: Tokens,
    pub time_created: u64,
    pub time_updated: u64,
}

impl SessionMeta {
    /// Rule 1, in one line. What every imported row is, and whose it was, so a
    /// reader who cannot tell whose sentence it is can weigh it - the same defect
    /// Named for a card that does not label a borrowed sentence.
    pub fn origin(&self) -> String {
        let who = match (&self.provider, &self.model) {
            (Some(p), Some(m)) => format!("{p}/{m}"),
            (None, Some(m)) => m.clone(),
            _ => "another provider".to_string(),
        };
        format!(
            "imported from opencode — `{}` in {}, run under {who}",
            self.id, self.directory
        )
    }
}

/// A whole session tree: the root and its sub-sessions, with the count the progress
/// line needs known before the first row (the `WITH RECURSIVE … count(*)`).
#[derive(Debug, Clone)]
pub struct Tree {
    pub root: SessionMeta,
    pub children: Vec<SessionMeta>,
    /// What the read walks: parts in a v1 database, stored messages in a v2 one. Not the
    /// root's, per the ruling - it is the whole tree's.
    pub parts: u64,
    /// The word [`Tree::parts`] is counted in, so a progress line says `message(s)` where it
    /// is counting messages. Not a field a caller sets: [`Source::tree`] fills it.
    pub unit: &'static str,
}

impl Tree {
    pub fn sessions(&self) -> usize {
        1 + self.children.len()
    }

    /// The word a progress line puts after the counts, which the head draws as `12/30 parts`. The
    /// reader counts in the singular ([`Tree::unit`]); this is the same fact in the head's spelling.
    pub fn unit_plural(&self) -> &'static str {
        if self.unit == "message" {
            "messages"
        } else {
            "parts"
        }
    }
}

/// One row to append: a transcript item, the session it belongs to, and the moment
/// it happened. `ts_ms` is opencode's `time_created`, taken and never invented.
#[derive(Debug, Clone)]
pub struct Row {
    pub source_session: String,
    pub ts_ms: u64,
    pub part_id: String,
    pub item: TranscriptItem,
}

/// A part that is real but is not a transcript row - an unknown tag, a `patch`,
/// a `compaction`. Counted and said, never dropped.
#[derive(Debug, Clone)]
pub struct Scrap {
    pub source_session: String,
    pub ts_ms: u64,
    pub part_id: String,
    pub tag: String,
    pub said: String,
}

/// What [`Source::read_tree`] hands its sink, in order.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // taken once and dropped, so a boxed row buys nothing
pub enum Event {
    /// The importer's own counter - parts read, against the count from
    /// `count(*)`. Not derived from what has not arrived: *an indicator must be the
    /// fact, not a rendering of the fact.*
    Progress {
        done: u64,
        total: u64,
    },
    Row(Row),
    Scrap(Scrap),
}

/// The spend rule 2 is about: opencode's real cost and tokens, summed from
/// `step-finish`, in another provider's units.
#[derive(Debug, Clone, Default)]
pub struct Spend {
    pub cost: f64,
    pub tokens: Tokens,
}

/// What a whole read produced - the numbers the finish line and `/status` keep.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Items examined (the denominator): parts in a v1 database, stored messages in a v2 one.
    pub parts: u64,
    /// The word `parts` is counted in. Empty means `part`, which is what a v1 database has
    /// always counted, so a summary from one reads exactly as it did.
    pub unit: &'static str,
    /// Transcript rows emitted.
    pub rows: u64,
    /// Parts that became a sentence rather than a row.
    pub scraps: u64,
    /// Parts per tag, as seen.
    pub by_tag: BTreeMap<String, u64>,
    /// Tags this build does not know, counted individually. Non-empty is a fact
    /// to report, not a silence.
    pub unknown_tags: BTreeMap<String, u64>,
    /// `patch` parts seen, and how many files they named in total.
    pub patches: u64,
    pub patch_files: u64,
    /// opencode's spend for the session, for the label (rule 2).
    pub spend: Spend,
    /// `(sessions, parts)` across the tree, set by a tree read so the summary can
    /// name the tree. `None` for a caller that read one session.
    tree: Option<(u64, u64)>,
}

impl Report {
    /// The sentence the finish line and `/status` carry.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "imported {} row(s) from {} {}(s)",
            self.rows,
            self.parts,
            self.unit_word()
        );
        if let Some((d, t)) = self.child_parts() {
            s.push_str(&format!(" (of {t} across {d} session(s))"));
        }
        if self.spend.cost > 0.0 || self.spend.tokens.total() > 0 {
            s.push_str(&format!(
                "; opencode spent ${:.4} and {} token(s) on it, in another provider's units \
                 — history, not this session's budget",
                self.spend.cost,
                self.spend.tokens.total()
            ));
        }
        if self.patches > 0 {
            s.push_str(&format!(
                "; {} patch row(s) name {} changed file(s) and are shown as a count, not a diff",
                self.patches, self.patch_files
            ));
        }
        if !self.unknown_tags.is_empty() {
            let named: Vec<String> = self
                .unknown_tags
                .iter()
                .map(|(t, n)| format!("`{t}` × {n}"))
                .collect();
            s.push_str(&format!(
                "; {} part type(s) this build does not read were counted and said, not dropped: {}",
                self.unknown_tags.len(),
                named.join(", ")
            ));
        }
        s
    }

    /// Sessions and parts, for a tree read; `None` when the caller only read one.
    fn child_parts(&self) -> Option<(u64, u64)> {
        self.tree
    }

    /// The unit noun, defaulted so a report built before the field existed still says `part`.
    fn unit_word(&self) -> &'static str {
        if self.unit.is_empty() {
            "part"
        } else {
            self.unit
        }
    }

    /// Set by [`Source::read_tree`] so [`Report::summary`] can name the tree.
    pub fn set_tree(&mut self, sessions: u64, parts: u64) {
        self.tree = Some((sessions, parts));
    }
}

/// Which of opencode's two storage layouts one session id is read with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schema {
    /// `session`/`message`/`part`: what opencode 1.x wrote, and what an upgraded database still
    /// holds - frozen - for a session the background migration has not reached.
    V1,
    /// `session_v2`/`session_message`/`session_part`: what every 2.x writes.
    V2,
}

/// One session id in a tree, and the layout it is read with. A tree can hold both: a v2 root's
/// child may be a session created before the upgrade and not yet migrated.
#[derive(Debug, Clone)]
struct Id {
    id: String,
    schema: Schema,
}

/// Both layouts as one table of ids, v2 winning: a v1 row for an id v2 also has is dropped, so a
/// frozen row can never be the parent of a tree walk over an upgraded database.
const SRC_BOTH: &str = "(SELECT id, parent_id, 1 AS v2 FROM session_v2 \
                        UNION ALL \
                        SELECT id, parent_id, 0 AS v2 FROM session \
                         WHERE id NOT IN (SELECT id FROM session_v2))";
/// A fresh 2.x database has no v1 tables at all, so the walk is over `session_v2` alone.
const SRC_V2: &str = "(SELECT id, parent_id, 1 AS v2 FROM session_v2)";

/// A read-only handle on opencode's database.
pub struct Source {
    conn: Connection,
    path: PathBuf,
    /// The database has opencode 1.x's tables: every upgraded 2.x one does, a fresh 2.x one does not.
    has_v1: bool,
    /// The database has opencode 2.x's tables, so it is read as v2 - per session id.
    has_v2: bool,
}

impl Source {
    /// Open the database read-only, and not `immutable=1`: opencode's WAL is live, and an
    /// immutable open ignores it and reads a torn snapshot, while read-only still sees it.
    /// The open is proven here with the cheapest query, because SQLite opens lazily. Which layout
    /// the file speaks is decided once, here, so every later read knows without asking again.
    pub fn open(path: &Path) -> Result<Self> {
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
                ImportError::NoDatabase {
                    path: path.to_path_buf(),
                    err: e.to_string(),
                }
            })?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(|e| ImportError::NoDatabase {
                path: path.to_path_buf(),
                err: e.to_string(),
            })?;
        let mut tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()
            })
            .map_err(sql_err)?;
        let has_v1 = tables.iter().any(|t| t == "session");
        let has_v2 = tables.iter().any(|t| t == "session_v2");
        if !has_v1 && !has_v2 {
            // Neither layout is a different fact from a query that failed, and the operator asked
            // for the two to be told apart: this file is not an opencode database at all.
            tables.retain(|t| !t.starts_with("sqlite_"));
            return Err(ImportError::NeitherSchema {
                path: path.to_path_buf(),
                tables,
            });
        }
        Ok(Source {
            conn,
            path: path.to_path_buf(),
            has_v1,
            has_v2,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The layout this database is read with: v2 when it has `session_v2` at all, because a v2 id
    /// must be read from v2 even where frozen v1 rows exist for it. A v1-only database - the
    /// operator's own opencode 1.18.31 - answers `V1` and is read exactly as it always was.
    pub fn schema(&self) -> Schema {
        if self.has_v2 { Schema::V2 } else { Schema::V1 }
    }

    /// The whole tree of `root`: root first in `children` order, plus the count. `NoSuchSession`
    /// when `root` names nothing, which is a different sentence from `NoDatabase` and the operator
    /// asked for the two to be told apart.
    pub fn tree(&self, root: &str) -> Result<Tree> {
        let ids = self.tree_ids(root)?;
        self.tree_of(ids, root)
    }

    fn tree_of(&self, ids: Vec<Id>, root: &str) -> Result<Tree> {
        if ids.is_empty() {
            return Err(ImportError::NoSuchSession {
                id: root.to_string(),
            });
        }
        let mut metas = Vec::new();
        for id in &ids {
            metas.push(match id.schema {
                Schema::V2 => v2::session_meta(&self.conn, &id.id)?,
                Schema::V1 => self.session_meta(&id.id)?,
            });
        }
        let root_meta = metas
            .iter()
            .find(|m| m.id == root)
            .cloned()
            .ok_or_else(|| ImportError::NoSuchSession {
                id: root.to_string(),
            })?;
        let children = metas.into_iter().filter(|m| m.id != root).collect();
        let (parts, unit) = self.count(&ids)?;
        Ok(Tree {
            root: root_meta,
            children,
            parts,
            unit,
        })
    }

    /// Read the whole tree, oldest first, one callback per event so the caller can publish as it
    /// goes and keep the screen live. A function returning a `Vec` would have read everything before
    /// drawing anything.
    pub fn read_tree(&self, root: &str, sink: &mut dyn FnMut(Event)) -> Result<Report> {
        let ids = self.tree_ids(root)?;
        let tree = self.tree_of(ids.clone(), root)?;
        let total = tree.parts;

        let mut report = Report {
            parts: total,
            unit: tree.unit,
            ..Default::default()
        };
        report.set_tree(tree.sessions() as u64, total);

        if self.has_v2 {
            v2::read_tree(&self.conn, &ids, &mut report, total, sink)?;
            return Ok(report);
        }
        // A v1-only database: unchanged, one query over `part` ordered by `time_created` across the
        // whole tree, because that is the order this importer has always read.
        let names: Vec<&str> = ids.iter().map(|i| i.id.as_str()).collect();
        // `IN (?1, ?2, …)`, built from the ids - never interpolated, always bound.
        let placeholders = vec!["?"; names.len()].join(",");
        let sql = format!(
            "SELECT p.session_id, m.data, p.id, p.time_created, p.data \
             FROM part p JOIN message m ON p.message_id = m.id \
             WHERE p.session_id IN ({placeholders}) \
             ORDER BY p.time_created, p.id"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(sql_err)?;
        // `params_from_iter` needs the iterator inline; the borrow lives for the query.
        let mut rows = stmt
            .query(rusqlite::params_from_iter(names.iter()))
            .map_err(sql_err)?;

        let mut done = 0u64;
        while let Some(r) = rows.next().map_err(sql_err)? {
            let source_session: String = r.get(0).map_err(sql_err)?;
            let msg_data: String = r.get(1).map_err(sql_err)?;
            let part_id: String = r.get(2).map_err(sql_err)?;
            let ts: i64 = r.get(3).map_err(sql_err)?;
            let data: String = r.get(4).map_err(sql_err)?;
            done += 1;
            sink(Event::Progress { done, total });
            interpret(
                &source_session,
                &part_id,
                ts.max(0) as u64,
                &msg_data,
                &data,
                &mut report,
                sink,
            );
        }
        Ok(report)
    }

    /// Just the session's `directory`, so the imported session's tools are confined
    /// to the tree the conversation was had in rather than the daemon's start directory.
    /// One query, read before the backend is built because the root is baked into it.
    pub fn directory(&self, id: &str) -> Result<String> {
        if self.has_v2 {
            match v2::directory(&self.conn, id) {
                Ok(dir) => return Ok(dir),
                // A session the background migration has not reached is in the frozen v1 rows only.
                Err(ImportError::NoSuchSession { .. }) if self.has_v1 => {}
                Err(e) => return Err(e),
            }
        }
        if !self.has_v1 {
            return Err(ImportError::NoSuchSession { id: id.to_string() });
        }
        self.conn
            .query_row("SELECT directory FROM session WHERE id = ?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ImportError::NoSuchSession { id: id.to_string() }
                }
                other => ImportError::Sql(other.to_string()),
            })
    }

    /// The recursive tree of session ids, root first, in a stable order, each with the layout it is
    /// read with. On an upgraded database the walk unions both layouts, so a child that only exists
    /// in the frozen v1 rows is still part of its v2 parent's tree.
    fn tree_ids(&self, root: &str) -> Result<Vec<Id>> {
        if !self.has_v2 {
            let sql = "WITH RECURSIVE t(id, depth) AS ( \
                           SELECT id, 0 FROM session WHERE id = ?1 \
                           UNION ALL \
                           SELECT s.id, t.depth + 1 FROM session s JOIN t ON s.parent_id = t.id \
                       ) \
                       SELECT id FROM t ORDER BY depth, id";
            let mut stmt = self.conn.prepare(sql).map_err(sql_err)?;
            let ids = stmt
                .query_map([root], |r| r.get::<_, String>(0))
                .map_err(sql_err)?
                .collect::<rusqlite::Result<Vec<String>>>()
                .map_err(sql_err)?;
            return Ok(ids
                .into_iter()
                .map(|id| Id {
                    id,
                    schema: Schema::V1,
                })
                .collect());
        }
        let src = if self.has_v1 { SRC_BOTH } else { SRC_V2 };
        let sql = format!(
            "WITH RECURSIVE t(id, depth, v2) AS ( \
                 SELECT id, 0, v2 FROM {src} WHERE id = ?1 \
                 UNION ALL \
                 SELECT s.id, t.depth + 1, s.v2 FROM {src} s JOIN t ON s.parent_id = t.id \
             ) \
             SELECT id, v2 FROM t ORDER BY depth, id"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(sql_err)?;
        let ids = stmt
            .query_map([root], |r| {
                Ok(Id {
                    id: r.get::<_, String>(0)?,
                    schema: if r.get::<_, i64>(1)? == 1 {
                        Schema::V2
                    } else {
                        Schema::V1
                    },
                })
            })
            .map_err(sql_err)?
            .collect::<rusqlite::Result<Vec<Id>>>()
            .map_err(sql_err)?;
        Ok(ids)
    }

    /// The denominator, before the first row is read, and the word it is counted in: `part` rows in
    /// a v1-only database (as it has always been), stored messages once the database is v2.
    fn count(&self, ids: &[Id]) -> Result<(u64, &'static str)> {
        if !self.has_v2 {
            let names: Vec<&str> = ids.iter().map(|i| i.id.as_str()).collect();
            let placeholders = vec!["?"; names.len()].join(",");
            let sql = format!("SELECT count(*) FROM part WHERE session_id IN ({placeholders})");
            let n: i64 = self
                .conn
                .query_row(&sql, rusqlite::params_from_iter(names.iter()), |r| r.get(0))
                .map_err(sql_err)?;
            return Ok((n.max(0) as u64, "part"));
        }
        let mut total = 0u64;
        for (schema, table) in [(Schema::V2, "session_message"), (Schema::V1, "message")] {
            let names: Vec<&str> = ids
                .iter()
                .filter(|i| i.schema == schema)
                .map(|i| i.id.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }
            let placeholders = vec!["?"; names.len()].join(",");
            let sql = format!("SELECT count(*) FROM {table} WHERE session_id IN ({placeholders})");
            let n: i64 = self
                .conn
                .query_row(&sql, rusqlite::params_from_iter(names.iter()), |r| r.get(0))
                .map_err(sql_err)?;
            total = total.saturating_add(n.max(0) as u64);
        }
        Ok((total, "message"))
    }

    fn session_meta(&self, id: &str) -> Result<SessionMeta> {
        let row = self
            .conn
            .query_row(
                "SELECT s.id, s.parent_id, s.title, s.directory, s.agent, s.model, \
                        s.cost, s.tokens_input, s.tokens_output, s.tokens_reasoning, \
                        s.tokens_cache_read, s.tokens_cache_write, s.time_created, \
                        s.time_updated, p.worktree \
                   FROM session s LEFT JOIN project p ON p.id = s.project_id \
                  WHERE s.id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, f64>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, i64>(9)?,
                        r.get::<_, i64>(10)?,
                        r.get::<_, i64>(11)?,
                        r.get::<_, i64>(12)?,
                        r.get::<_, i64>(13)?,
                        r.get::<_, Option<String>>(14)?,
                    ))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ImportError::NoSuchSession { id: id.to_string() }
                }
                other => ImportError::Sql(other.to_string()),
            })?;

        // opencode stores `model` two ways across versions: a bare string, or an
        // object `{providerID, modelID}`. Both are read; a bare string has no provider.
        let (provider, model) = parse_model(row.5.as_deref());
        Ok(SessionMeta {
            id: row.0,
            parent_id: row.1,
            title: row.2,
            directory: row.3,
            worktree: row.14,
            provider,
            model,
            agent: row.4,
            cost: row.6,
            tokens: Tokens {
                input: row.7.max(0) as u64,
                output: row.8.max(0) as u64,
                reasoning: row.9.max(0) as u64,
                cache_read: row.10.max(0) as u64,
                cache_write: row.11.max(0) as u64,
            },
            time_created: row.12.max(0) as u64,
            time_updated: row.13.max(0) as u64,
        })
    }
}

/// `model` is a bare id in some rows and `{providerID, modelID}` in others.
fn parse_model(raw: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return (None, None);
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(obj) = v.as_object()
    {
        let provider = obj
            .get("providerID")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let model = obj
            .get("modelID")
            .or_else(|| obj.get("id"))
            .and_then(|x| x.as_str())
            .map(str::to_string);
        return (provider, model);
    }
    (None, Some(raw.to_string()))
}

fn sql_err(e: rusqlite::Error) -> ImportError {
    ImportError::Sql(e.to_string())
}

/// Turn one part into whatever it becomes, calling `sink` for each row or scrap. The v2 reader calls
/// this too, for a session an upgraded database has only in its frozen v1 rows.
pub(crate) fn interpret(
    source_session: &str,
    part_id: &str,
    ts_ms: u64,
    _msg_data: &str,
    data: &str,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    let v: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            // A `part.data` that will not parse is real and is said, not dropped  -
            // the same rule as an unknown tag.
            report.scraps += 1;
            *report
                .by_tag
                .entry("<unparseable>".to_string())
                .or_default() += 1;
            sink(Event::Scrap(Scrap {
                source_session: source_session.to_string(),
                ts_ms,
                part_id: part_id.to_string(),
                tag: "<unparseable>".to_string(),
                said: format!("a part whose JSON this build could not read ({e})"),
            }));
            return;
        }
    };
    let tag = v
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("<no type>")
        .to_string();
    *report.by_tag.entry(tag.clone()).or_default() += 1;

    match tag.as_str() {
        "text" => {
            let text = str_field(&v, "text");
            if text.is_empty() {
                return;
            }
            let row = match role_of(_msg_data).as_deref() {
                Some("user") => TranscriptItem::User {
                    // An imported conversation's `user` role is the person's. Nothing this
                    // importer reads is the harness's own voice, so the other value has no
                    // site here - and saying so is cheaper than a reader wondering.
                    speaker: letibot_transcript::Speaker::Operator,
                    parts: vec![UserPart::Text { text }],
                },
                _ => TranscriptItem::Assistant {
                    text,
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            };
            emit_row(source_session, part_id, ts_ms, row, report, sink);
        }
        "reasoning" => {
            let text = str_field(&v, "text");
            if text.is_empty() {
                return;
            }
            emit_row(
                source_session,
                part_id,
                ts_ms,
                TranscriptItem::Reasoning {
                    text,
                    field: ReasoningField::ReasoningContent,
                    truncated: false,
                },
                report,
                sink,
            );
        }
        "tool" => {
            // Two rows: the call, then the result. The head labels a tool card
            // from the nearest preceding `Assistant { tool_calls }`; a lone result
            // would show the outcome with no command. See the module docs.
            let name = str_field(&v, "tool");
            let call_id = str_field(&v, "callID");
            let input = v
                .pointer("/state/input")
                .map(|i| i.to_string())
                .unwrap_or_else(|| "{}".to_string());
            let status = v
                .pointer("/state/status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let output = v
                .pointer("/state/output")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let error = v
                .pointer("/state/error")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            let call_id = if call_id.is_empty() {
                part_id.to_string()
            } else {
                call_id
            };

            emit_row(
                source_session,
                part_id,
                ts_ms,
                TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        arguments: input,
                    }],
                    truncated: false,
                },
                report,
                sink,
            );

            let outcome = match status.as_str() {
                "completed" => ToolOutcome::Ok,
                "error" => ToolOutcome::Failed {
                    reason: if error.is_empty() {
                        "opencode recorded this call as an error".to_string()
                    } else {
                        error.clone()
                    },
                },
                other => ToolOutcome::NotRun {
                    why: format!("opencode recorded this call as `{other}`"),
                },
            };
            let payload = if !output.is_empty() { output } else { error };
            emit_row(
                source_session,
                part_id,
                ts_ms,
                TranscriptItem::ToolResult {
                    call_id,
                    name,
                    outcome,
                    payload,
                    edits: Vec::new(),
                    origin: None,
                    // An imported row carries no media: opencode stores an attachment as a
                    // file part whose content is on disk, and this importer reads the message
                    // log, so the conversation shows the sentence and not the picture.
                    media: None,
                },
                report,
                sink,
            );
        }
        "patch" => {
            // A hash and a file list, not a diff. The content is in opencode's
            // `snapshot/` object store, which is not opened. Reported as a count so
            // it can never read as an empty diff.
            let files: Vec<String> = v
                .get("files")
                .and_then(|f| f.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let hash = str_field(&v, "hash");
            report.patches += 1;
            report.patch_files = report.patch_files.saturating_add(files.len() as u64);
            let short = if hash.len() > 12 { &hash[..12] } else { &hash };
            let said = if files.is_empty() {
                format!("a patch (hash {short}) naming no files")
            } else {
                format!(
                    "a patch (hash {short}) changing {} file(s): {}",
                    files.len(),
                    files.join(", ")
                )
            };
            report.scraps += 1;
            sink(Event::Scrap(Scrap {
                source_session: source_session.to_string(),
                ts_ms,
                part_id: part_id.to_string(),
                tag,
                said,
            }));
        }
        "compaction" => {
            let auto = v.get("auto").and_then(|b| b.as_bool()).unwrap_or(false);
            let overflow = v.get("overflow").and_then(|b| b.as_bool()).unwrap_or(false);
            report.scraps += 1;
            sink(Event::Scrap(Scrap {
                source_session: source_session.to_string(),
                ts_ms,
                part_id: part_id.to_string(),
                tag,
                said: format!(
                    "opencode compacted this conversation here (auto: {auto}, overflow: {overflow})"
                ),
            }));
        }
        "step-start" => {}
        "step-finish" => {
            if let Some(t) = v.get("tokens") {
                let t = tokens_from(t);
                report.spend.tokens.input = report.spend.tokens.input.saturating_add(t.input);
                report.spend.tokens.output = report.spend.tokens.output.saturating_add(t.output);
                report.spend.tokens.reasoning =
                    report.spend.tokens.reasoning.saturating_add(t.reasoning);
                report.spend.tokens.cache_read =
                    report.spend.tokens.cache_read.saturating_add(t.cache_read);
                report.spend.tokens.cache_write = report
                    .spend
                    .tokens
                    .cache_write
                    .saturating_add(t.cache_write);
            }
            if let Some(c) = v.get("cost").and_then(|c| c.as_f64()) {
                report.spend.cost += c;
            }
        }
        other => {
            // Counted and said, never dropped. R3's rule, inside the transcript.
            *report.unknown_tags.entry(other.to_string()).or_default() += 1;
            report.scraps += 1;
            sink(Event::Scrap(Scrap {
                source_session: source_session.to_string(),
                ts_ms,
                part_id: part_id.to_string(),
                tag: other.to_string(),
                said: format!(
                    "a `{other}` part this build does not read — counted and left here rather \
                     than silently dropped"
                ),
            }));
        }
    }
}

pub(crate) fn emit_row(
    source_session: &str,
    part_id: &str,
    ts_ms: u64,
    item: TranscriptItem,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    report.rows += 1;
    sink(Event::Row(Row {
        source_session: source_session.to_string(),
        ts_ms,
        part_id: part_id.to_string(),
        item,
    }));
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn role_of(msg_data: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(msg_data)
        .ok()?
        .get("role")?
        .as_str()
        .map(str::to_string)
}

fn tokens_from(t: &serde_json::Value) -> Tokens {
    let n = |k: &str| t.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Tokens {
        input: n("input"),
        output: n("output"),
        reasoning: n("reasoning"),
        cache_read: t
            .pointer("/cache/read")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        cache_write: t
            .pointer("/cache/write")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
    }
}

/// Where opencode's database is on this box, if it is anywhere. Absence is reported by
/// `Source::open` as `NoDatabase`, with this path in the sentence.
pub fn default_db_path() -> Option<PathBuf> {
    db_path_from(|k| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()))
}

/// opencode's own rule (`packages/cli/src/database-path.ts`, `packages/util/src/global-roots.ts`):
/// `$OPENCODE_DB` - resolved against the DATA directory and not the working one - else
/// `opencode-<channel>.db` when a channel is named, else `opencode.db`. The data directory is
/// `$XDG_DATA_HOME/opencode`, defaulting to `~/.local/share/opencode`.
///
/// opencode's channel is baked in at build time, so `$OPENCODE_CHANNEL` is the only way a person
/// can name it here; a release channel and `$OPENCODE_DISABLE_CHANNEL_DB` both mean `opencode.db`,
/// as they do there. Taking the lookup as an argument keeps this testable without touching the
/// process environment, which every test in one binary shares.
fn db_path_from(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let data = match env("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("opencode"),
        None => PathBuf::from(env("HOME")?).join(".local/share/opencode"),
    };
    let name = match env("OPENCODE_DB").filter(|d| !d.is_empty()) {
        Some(db) => PathBuf::from(db),
        None => match env("OPENCODE_CHANNEL").filter(|c| !c.is_empty()) {
            Some(channel) if !is_release_channel(&channel) => {
                match env("OPENCODE_DISABLE_CHANNEL_DB") {
                    Some(off) if off == "1" || off == "true" => PathBuf::from("opencode.db"),
                    _ => PathBuf::from(format!(
                        "opencode-{}.db",
                        channel
                            .chars()
                            .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) {
                                c
                            } else {
                                '-'
                            })
                            .collect::<String>()
                    )),
                }
            }
            _ => PathBuf::from("opencode.db"),
        },
    };
    // `path.resolve(data, name)` in opencode: a bare filename lands in the data directory, an
    // absolute one stands as written.
    Some(if name.is_absolute() {
        name
    } else {
        data.join(name)
    })
}

/// The channels opencode ships as stable builds, and so the ones that use the plain `opencode.db`.
fn is_release_channel(channel: &str) -> bool {
    matches!(channel, "latest" | "dev" | "beta" | "next" | "prod")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_totals_saturate_instead_of_overflowing() {
        let t = Tokens {
            input: u64::MAX,
            output: 1,
            ..Tokens::default()
        };
        assert_eq!(t.total(), u64::MAX);
    }

    /// A throwaway opencode database, populated by the test. A file and not
    /// `:memory:`, because the point is to open it read-only afterwards and an
    /// in-memory database has no second opener.
    fn fixture(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "letibot-opencode-{tag}-{}-{}.db",
            std::process::id(),
            // A per-call counter, so two fixtures in one test never collide.
            {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                N.fetch_add(1, Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL);
             CREATE TABLE session (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
                slug TEXT NOT NULL DEFAULT '', directory TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '',
                cost REAL NOT NULL DEFAULT 0,
                tokens_input INTEGER NOT NULL DEFAULT 0,
                tokens_output INTEGER NOT NULL DEFAULT 0,
                tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                tokens_cache_read INTEGER NOT NULL DEFAULT 0,
                tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                agent TEXT, model TEXT,
                time_created INTEGER NOT NULL DEFAULT 0,
                time_updated INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);
             CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);",
        )
        .unwrap();
    }

    fn add_session(conn: &Connection, id: &str, parent: Option<&str>, dir: &str) {
        conn.execute(
            "INSERT INTO session (id, project_id, parent_id, directory, title, agent, model) \
             VALUES (?1, 'p1', ?2, ?3, ?4, 'build', \
                     '{\"providerID\":\"deepseek\",\"modelID\":\"deepseek-v4-pro\"}')",
            rusqlite::params![id, parent, dir, format!("title {id}")],
        )
        .unwrap();
    }

    fn add_message(conn: &Connection, id: &str, session: &str, role: &str, ts: i64) {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                id,
                session,
                ts,
                format!("{{\"role\":\"{role}\",\"time\":{{\"created\":{ts}}}}}")
            ],
        )
        .unwrap();
    }

    fn add_part(conn: &Connection, id: &str, message: &str, session: &str, ts: i64, data: &str) {
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![id, message, session, ts, data],
        )
        .unwrap();
    }

    /// The whole shape: user text, reasoning, a tool call, an unknown tag, a patch,
    /// a step-finish, and a child session's text.
    fn populated(tag: &str) -> PathBuf {
        let path = fixture(tag);
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        conn.execute(
            "INSERT INTO project (id, worktree) VALUES ('p1', '/home/dead/Projects/x')",
            [],
        )
        .unwrap();
        add_session(&conn, "ses_root", None, "/home/dead");
        add_session(&conn, "ses_child", Some("ses_root"), "/home/dead");

        add_message(&conn, "m1", "ses_root", "user", 10);
        add_message(&conn, "m2", "ses_root", "assistant", 20);
        add_message(&conn, "m3", "ses_child", "user", 30);

        add_part(
            &conn,
            "p1",
            "m1",
            "ses_root",
            10,
            r#"{"type":"text","text":"hello"}"#,
        );
        add_part(
            &conn,
            "p2",
            "m2",
            "ses_root",
            20,
            r#"{"type":"reasoning","text":"thinking"}"#,
        );
        add_part(
            &conn,
            "p3",
            "m2",
            "ses_root",
            21,
            r#"{"type":"tool","tool":"bash","callID":"call_1","state":{"status":"completed","input":{"command":"ls"},"output":"a\nb"}}"#,
        );
        add_part(
            &conn,
            "p4",
            "m2",
            "ses_root",
            22,
            r#"{"type":"widget","whatever":true}"#,
        );
        add_part(
            &conn,
            "p5",
            "m2",
            "ses_root",
            23,
            r#"{"type":"patch","hash":"a38f12e379162decaa94387a7090eb8f2a0327a1","files":["/a.rs","/b.rs"]}"#,
        );
        add_part(
            &conn,
            "p6",
            "m2",
            "ses_root",
            24,
            r#"{"type":"step-finish","reason":"tool-calls","tokens":{"total":135,"input":100,"output":20,"reasoning":5,"cache":{"write":0,"read":30}},"cost":0.5}"#,
        );
        add_part(
            &conn,
            "p7",
            "m3",
            "ses_child",
            30,
            r#"{"type":"text","text":"child says hi"}"#,
        );
        drop(conn);
        path
    }

    /// The database not being there is a different fact from the session being
    /// absent, and the operator asked for the two to be told apart.
    #[test]
    fn a_missing_database_is_named_as_that_and_not_as_a_missing_session() {
        let path = std::env::temp_dir().join("letibot-opencode-definitely-absent.db");
        let _ = std::fs::remove_file(&path);
        let err = match Source::open(&path) {
            Ok(_) => panic!("a file that is not there must not open"),
            Err(e) => e,
        };
        let said = format!("{err}");
        assert!(
            matches!(err, ImportError::NoDatabase { .. }),
            "a missing file is NoDatabase: {said}"
        );
        assert!(said.contains("no opencode database"), "{said}");
    }

    /// The tree, and the count, before the first row - the recursive query the
    /// progress line's denominator comes from, across the whole tree.
    #[test]
    fn the_tree_count_is_the_whole_trees_and_a_stranger_id_is_no_such_session() {
        let path = populated("tree");
        let src = Source::open(&path).unwrap();

        let tree = src.tree("ses_root").unwrap();
        assert_eq!(tree.root.id, "ses_root");
        assert_eq!(tree.children.len(), 1, "one sub-session");
        assert_eq!(tree.children[0].id, "ses_child");
        assert_eq!(tree.parts, 7, "every part in the tree, root and child");
        assert_eq!(tree.sessions(), 2);
        // Rule 1: the origin names the opencode id, the directory and the provider.
        let origin = tree.root.origin();
        assert!(origin.contains("ses_root"), "{origin}");
        assert!(origin.contains("/home/dead"), "{origin}");
        assert!(origin.contains("deepseek/deepseek-v4-pro"), "{origin}");
        assert_eq!(tree.root.worktree.as_deref(), Some("/home/dead/Projects/x"));

        // A database that exists but names no such session is its own sentence.
        let err = src.tree("ses_nope").unwrap_err();
        assert!(matches!(err, ImportError::NoSuchSession { .. }), "{err}");
        assert!(format!("{err}").contains("no session"), "{err}");

        let _ = std::fs::remove_file(&path);
    }

    /// The mapping, part by part - and the progress counter is the importer's
    /// own, counting parts read, not rows still missing a body.
    #[test]
    fn every_tag_maps_and_the_progress_counts_parts_read() {
        let path = populated("map");
        let src = Source::open(&path).unwrap();

        let mut rows: Vec<(String, String)> = Vec::new(); // (kind, a word of it)
        let mut scraps: Vec<String> = Vec::new();
        let mut progress: Vec<(u64, u64)> = Vec::new();
        let report = src
            .read_tree("ses_root", &mut |e| match e {
                Event::Progress { done, total } => progress.push((done, total)),
                Event::Row(r) => {
                    let kind = match &r.item {
                        TranscriptItem::User { .. } => "user",
                        TranscriptItem::Assistant { .. } => "assistant",
                        TranscriptItem::Reasoning { .. } => "reasoning",
                        TranscriptItem::ToolResult { .. } => "tool_result",
                        _ => "other",
                    };
                    rows.push((kind.to_string(), r.source_session.clone()));
                }
                Event::Scrap(s) => scraps.push(s.said),
            })
            .unwrap();

        // Progress is the fact, and it ends at the denominator. Seven parts,
        // seven ticks, total known from the start.
        assert_eq!(progress.first(), Some(&(1, 7)), "{progress:?}");
        assert_eq!(progress.last(), Some(&(7, 7)), "{progress:?}");
        assert_eq!(progress.len(), 7, "one tick per part read: {progress:?}");

        // A user text is a User row; reasoning is its own; a tool is TWO rows (the
        // call then the result); the child's text is a row too.
        let kinds: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["user", "reasoning", "assistant", "tool_result", "user"],
            "one or two rows per part, in order: {rows:?}"
        );
        // The child's row is tagged with the child session, not the root.
        assert_eq!(rows.last().unwrap().1, "ses_child");

        // The unknown tag and the patch are scraps, counted, not dropped.
        assert_eq!(report.unknown_tags.get("widget"), Some(&1));
        assert_eq!(report.patches, 1);
        assert_eq!(report.patch_files, 2);
        assert_eq!(report.rows, 5);
        assert!(scraps.iter().any(|s| s.contains("widget")), "{scraps:?}");
        assert!(
            scraps
                .iter()
                .any(|s| s.contains("2 file(s)") && s.contains("/a.rs")),
            "a patch says how many files, and which: {scraps:?}"
        );

        // Rule 2: the spend is summed from step-finish, in opencode's units.
        assert_eq!(report.spend.cost, 0.5);
        assert_eq!(report.spend.tokens.input, 100);
        assert_eq!(report.spend.tokens.cache_read, 30);
        // opencode's own `total` is input+output+reasoning+cache_read(+write)  -
        // checked against a real row: 6838+175+92+1920 = 9025.
        assert_eq!(report.spend.tokens.total(), 155);

        // Rule 3: the timestamp is the row's own, taken not invented.
        let mut ts_seen = Vec::new();
        src.read_tree("ses_root", &mut |e| {
            if let Event::Row(r) = e {
                ts_seen.push(r.ts_ms);
            }
        })
        .unwrap();
        assert!(
            ts_seen.contains(&10) && ts_seen.contains(&30),
            "{ts_seen:?}"
        );

        let summary = report.summary();
        assert!(
            summary.contains("imported 5 row(s) from 7 part(s)"),
            "{summary}"
        );
        assert!(summary.contains("another provider's units"), "{summary}");
        assert!(summary.contains("not dropped"), "{summary}");

        let _ = std::fs::remove_file(&path);
    }

    /// A patch never renders an empty diff. It is a hash and a file list; the
    /// content is in an object store this crate does not open, so the row is a count.
    #[test]
    fn a_patch_is_a_count_and_never_an_empty_diff() {
        let path = fixture("patch");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        conn.execute("INSERT INTO project (id, worktree) VALUES ('p1', '/w')", [])
            .unwrap();
        add_session(&conn, "ses_root", None, "/home/dead");
        add_message(&conn, "m1", "ses_root", "assistant", 5);
        add_part(
            &conn,
            "p1",
            "m1",
            "ses_root",
            5,
            r#"{"type":"patch","hash":"deadbeefdeadbeefdeadbeef","files":[]}"#,
        );
        drop(conn);

        let src = Source::open(&path).unwrap();
        let mut said = Vec::new();
        let mut rows = 0;
        src.read_tree("ses_root", &mut |e| match e {
            Event::Scrap(s) => said.push(s.said),
            Event::Row(_) => rows += 1,
            Event::Progress { .. } => {}
        })
        .unwrap();
        assert_eq!(rows, 0, "a patch is never a diff row");
        assert!(
            said.iter().any(|s| s.contains("naming no files")),
            "an empty file list is said, not drawn as an empty diff: {said:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// opencode's own rule for where the database is: `$OPENCODE_DB` resolved against the DATA
    /// directory (not the working one, which is what this importer used to do), `$XDG_DATA_HOME`, and
    /// the channel file when a channel is named. The lookup is handed in so no test touches the
    /// process environment, which every test in one binary shares.
    #[test]
    fn the_database_path_follows_opencodes_own_rule() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let home = env(&[("HOME", "/home/someone")]);
        assert_eq!(
            db_path_from(home),
            Some(PathBuf::from(
                "/home/someone/.local/share/opencode/opencode.db"
            )),
            "today's default, unchanged"
        );

        let xdg = env(&[("HOME", "/home/someone"), ("XDG_DATA_HOME", "/xdg")]);
        assert_eq!(
            db_path_from(xdg),
            Some(PathBuf::from("/xdg/opencode/opencode.db")),
            "$XDG_DATA_HOME is where opencode puts its data directory"
        );

        // A bare `$OPENCODE_DB` is resolved against the data directory, never the cwd: opencode's
        // `path.resolve(data, filename)`.
        let relative = env(&[("HOME", "/home/someone"), ("OPENCODE_DB", "other.db")]);
        assert_eq!(
            db_path_from(relative),
            Some(PathBuf::from(
                "/home/someone/.local/share/opencode/other.db"
            ))
        );
        let absolute = env(&[
            ("HOME", "/home/someone"),
            ("OPENCODE_DB", "/elsewhere/x.db"),
        ]);
        assert_eq!(
            db_path_from(absolute),
            Some(PathBuf::from("/elsewhere/x.db"))
        );

        // A channel that is not a release build has its own file; a release channel and an explicit
        // opt-out both mean the plain one.
        let channel = env(&[("HOME", "/h"), ("OPENCODE_CHANNEL", "local")]);
        assert_eq!(
            db_path_from(channel),
            Some(PathBuf::from("/h/.local/share/opencode/opencode-local.db"))
        );
        let release = env(&[("HOME", "/h"), ("OPENCODE_CHANNEL", "latest")]);
        assert_eq!(
            db_path_from(release),
            Some(PathBuf::from("/h/.local/share/opencode/opencode.db"))
        );
        let off = env(&[
            ("HOME", "/h"),
            ("OPENCODE_CHANNEL", "local"),
            ("OPENCODE_DISABLE_CHANNEL_DB", "1"),
        ]);
        assert_eq!(
            db_path_from(off),
            Some(PathBuf::from("/h/.local/share/opencode/opencode.db"))
        );

        assert_eq!(db_path_from(env(&[])), None, "no HOME, no path to name");
    }

    /// The reader runs against the real database without writing to it, when one is
    /// present. Ignored by default so the suite is deterministic; run it with
    /// `cargo test -p letibot-opencode -- --ignored` on the box that has the data.
    #[test]
    #[ignore = "reads the operator's real opencode database"]
    fn the_real_database_reads_and_writes_nothing() {
        let Some(path) = default_db_path() else {
            eprintln!("no HOME; nothing to read");
            return;
        };
        let Ok(src) = Source::open(&path) else {
            eprintln!(
                "no opencode database at {}; nothing to read",
                path.display()
            );
            return;
        };
        // A session id from a real opencode database.
        let root = "ses_f68f5fd80ffe3lS06lxORVNfS9";
        let tree = match src.tree(root) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("that session is not in this database: {e}");
                return;
            }
        };
        eprintln!(
            "tree: {} session(s), {} parts, origin: {}",
            tree.sessions(),
            tree.parts,
            tree.root.origin()
        );
        let mut last = 0;
        let report = src
            .read_tree(root, &mut |e| {
                if let Event::Progress { done, .. } = e {
                    last = done;
                }
            })
            .unwrap();
        assert_eq!(last, report.parts, "read every part");
        eprintln!("{}", report.summary());
    }
}
