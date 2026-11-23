//! Where a gate's session grants are kept so they outlive the process: view widenings,
//! `allow_session` answers and the memory ceiling. The gate holds a copy and reloads it when
//! anything kept or revoked one, in this process or through the store, so a revoke reaches a
//! live session.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::adjudicate::ActionClass;
use crate::grant::{Coverage, Grant, Written};
use crate::intent::Intent;

/// What a card says when its session grant lands on the queue rather than the asker.
pub const QUEUE_SCOPE_NOTE: &str =
    "covers this project's review queue (all reviewers) until you revoke it";

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Bumped by every keep and revoke, so each gate in this process knows to reload.
pub fn bump() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// The review queue `session_id` belongs to: its gatekeeper's id, when the session is the
/// gatekeeper or a session under it.
pub fn queue_of(session_id: &str) -> Option<&str> {
    let root = session_id.split("-sub-").next()?;
    root.starts_with("s-gatekeeper").then_some(root)
}

/// The scope an answer names for the grant it gives: the asking session, or the queue it
/// belongs to. The gate turns it into a [`Scope`] with the ids it knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantAt {
    Session,
    Queue,
}

/// Who a grant belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// One session, for its life.
    Session(String),
    /// Every session of the review queue under this gatekeeper, until revoked.
    Queue(String),
}

impl Scope {
    pub fn owner(&self) -> &str {
        match self {
            Scope::Session(s) | Scope::Queue(s) => s,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Session(_) => "session",
            Scope::Queue(_) => "queue",
        }
    }

    pub fn parse(scope: &str, owner: &str) -> Option<Scope> {
        match scope {
            "session" => Some(Scope::Session(owner.to_string())),
            "queue" => Some(Scope::Queue(owner.to_string())),
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Scope::Session(s) => format!("session `{s}`"),
            Scope::Queue(g) => format!("the review queue under `{g}` (all reviewers)"),
        }
    }
}

/// One grant, as kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    View {
        path: PathBuf,
        writable: bool,
    },
    Allow(Grant),
    /// `None` is uncapped.
    Memory(Option<u64>),
    /// An action the operator DENIED: the shape and the target they refused, refused without
    /// another card for this session's tree until the row is revoked. Not a grant - it narrows
    /// where the others widen - and it lives in the same row, the same scopes and under the same
    /// revoke that lifts it.
    Deny {
        shape: String,
        target: String,
    },
}

impl Kept {
    pub fn kind(&self) -> &'static str {
        match self {
            Kept::View { .. } => "view",
            Kept::Allow(_) => "allow",
            Kept::Memory(_) => "memory",
            Kept::Deny { .. } => "deny",
        }
    }

    /// What makes two grants the same one: a later write of the same key replaces the first.
    pub fn key(&self) -> String {
        match self {
            Kept::View { path, .. } => path.display().to_string(),
            Kept::Allow(g) => g
                .coverage
                .iter()
                .map(|c| {
                    let intents: Vec<&str> = c.intents.iter().map(|i| i.as_str()).collect();
                    format!("{} {} [{}]", c.program, c.class, intents.join(","))
                })
                .collect::<Vec<_>>()
                .join("; "),
            Kept::Memory(_) => "memory".to_string(),
            // One denial per (shape, target): the same refusal twice is one row, so the operator
            // is told once and lifts once.
            Kept::Deny { shape, target } => format!("deny {shape}\u{1}{target}"),
        }
    }

    pub fn body(&self) -> String {
        let v = match self {
            Kept::View { path, writable } => serde_json::json!({
                "path": path.display().to_string(),
                "writable": writable,
            }),
            Kept::Allow(g) => serde_json::json!({
                "written": match &g.written {
                    Written::Enumerated { patterns } => serde_json::json!({ "enumerated": patterns }),
                    Written::Glob { pattern } => serde_json::json!({ "glob": pattern }),
                },
                "coverage": g.coverage.iter().map(|c| serde_json::json!({
                    "program": c.program,
                    "class": c.class.to_string(),
                    "intents": c.intents.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "why": g.why,
            }),
            Kept::Memory(bytes) => serde_json::json!({ "bytes": bytes }),
            Kept::Deny { shape, target } => {
                serde_json::json!({ "shape": shape, "target": target })
            }
        };
        v.to_string()
    }

    /// The inverse of [`Kept::kind`] and [`Kept::body`]. `None` for a row this build cannot
    /// read whole, which is then not applied: a grant read in part would be a different grant.
    pub fn parse(kind: &str, body: &str) -> Option<Kept> {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        match kind {
            "view" => Some(Kept::View {
                path: PathBuf::from(v.get("path")?.as_str()?),
                writable: v.get("writable")?.as_bool()?,
            }),
            "memory" => match v.get("bytes")? {
                serde_json::Value::Null => Some(Kept::Memory(None)),
                n => Some(Kept::Memory(Some(n.as_u64()?))),
            },
            "deny" => Some(Kept::Deny {
                shape: v.get("shape")?.as_str()?.to_string(),
                target: v.get("target")?.as_str()?.to_string(),
            }),
            "allow" => {
                let w = v.get("written")?;
                let written = if let Some(p) = w.get("glob") {
                    Written::Glob {
                        pattern: p.as_str()?.to_string(),
                    }
                } else {
                    Written::Enumerated {
                        patterns: w
                            .get("enumerated")?
                            .as_array()?
                            .iter()
                            .map(|p| p.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()?,
                    }
                };
                let coverage = v
                    .get("coverage")?
                    .as_array()?
                    .iter()
                    .map(|c| {
                        Some(Coverage {
                            program: c.get("program")?.as_str()?.to_string(),
                            class: ActionClass::parse(c.get("class")?.as_str()?)?,
                            intents: c
                                .get("intents")?
                                .as_array()?
                                .iter()
                                .map(|i| Intent::parse(i.as_str()?))
                                .collect::<Option<BTreeSet<_>>>()?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(Kept::Allow(Grant {
                    written,
                    coverage,
                    why: v.get("why")?.as_str()?.to_string(),
                }))
            }
            _ => None,
        }
    }

    /// One line for a listing.
    pub fn describe(&self) -> String {
        match self {
            Kept::View { path, writable } => format!(
                "{} `{}` in the view",
                if *writable { "read and write" } else { "read" },
                path.display()
            ),
            Kept::Allow(g) => format!(
                "allow {}",
                g.coverage
                    .iter()
                    .map(Coverage::describe)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Kept::Memory(Some(b)) => format!("memory up to {} MiB a command", b >> 20),
            Kept::Memory(None) => "memory uncapped".to_string(),
            Kept::Deny { shape, target } => match shape.trim() {
                "" => format!("refuse `{target}`"),
                s => format!("refuse `{target}` (or anything shaped `{s}`)"),
            },
        }
    }
}

/// A kept grant with its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub id: i64,
    pub scope: Scope,
    pub kept: Kept,
    pub why: String,
    /// Who answered it: `human:<name>`, `shadow:<judge>`. Empty when nobody recorded it.
    pub by: String,
}

impl Stored {
    /// Given by the main session's shadow rather than a person.
    pub fn by_shadow(&self) -> bool {
        self.by.starts_with(SHADOW_BY)
    }
}

/// How a grant the shadow answered names its answerer.
pub const SHADOW_BY: &str = "shadow:";

/// The store behind a gate's grants. Errors are sentences: a grant that was not kept still
/// holds for this process, and the gate says so.
pub trait GrantLedger: Send + Sync {
    /// Keep `kept` under `scope`, answered by `by`.
    fn keep(&self, scope: &Scope, kept: &Kept, why: &str, by: &str) -> Result<(), String>;
    fn load(&self, scope: &Scope) -> Result<Vec<Stored>, String>;
    /// Drop every grant of `kind` under `scope`.
    fn clear(&self, scope: &Scope, kind: &str) -> Result<usize, String>;
    /// A value that moves when any process keeps, changes or revokes a grant, so a gate reloads
    /// on a revoke another daemon made. The default is this process's [`generation`] alone.
    fn generation(&self) -> Result<String, String> {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjudicate::{Cost, EffectScope, Reversibility};
    use crate::schema::Access;

    #[test]
    fn the_queue_is_the_gatekeepers_tree_and_nothing_else() {
        assert_eq!(
            queue_of("s-gatekeeper-c74d1fd17483-sub-7"),
            Some("s-gatekeeper-c74d1fd17483")
        );
        assert_eq!(
            queue_of("s-gatekeeper-c74d1fd17483"),
            Some("s-gatekeeper-c74d1fd17483")
        );
        assert_eq!(queue_of("s-1794057745451974498"), None);
        assert_eq!(queue_of("s-1794057745451974498-sub-3"), None);
    }

    #[test]
    fn every_kind_reads_back_as_written() {
        let grant = Grant {
            written: Written::Enumerated {
                patterns: vec!["git status".into()],
            },
            coverage: vec![Coverage {
                program: "git".into(),
                class: ActionClass {
                    access: Access::Read,
                    scope: EffectScope::HostProject,
                    reversibility: Reversibility::Reversible,
                    cost: Cost::Free,
                },
                intents: [Intent::Inspect].into_iter().collect(),
            }],
            why: "answered".into(),
        };
        for kept in [
            Kept::View {
                path: "/build".into(),
                writable: true,
            },
            Kept::Allow(grant),
            Kept::Memory(Some(8 << 30)),
            Kept::Memory(None),
        ] {
            assert_eq!(Kept::parse(kept.kind(), &kept.body()).as_ref(), Some(&kept));
        }
        assert_eq!(Kept::parse("view", "{\"path\":\"/x\"}"), None);
    }
}
