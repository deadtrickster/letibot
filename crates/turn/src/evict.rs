//! Structural eviction: a whole unit goes before any prose is summarised.
//!
//! A compaction that replaces a span with a record pays for the record's input as well as its
//! output - a summary turn is a real turn over the whole span. Most of a long conversation is not
//! prose: it is tool calls with their printed bodies, and a body is a fact the CALL already
//! carries in its arguments. So the span handed to the summariser drops the bodies it cannot need
//! (a superseded read, a settled subagent's report, a finished call nobody repeats) and keeps the
//! calls, their outcomes and their facts. The record is then asked only about what is left.
//!
//! Nothing here is an edit of the conversation: the reduced span is a value, used to build the
//! summary turn's own prompt. The session, the ledger and the old transcript are untouched.

use letibot_transcript::{ToolOutcome, TranscriptItem};

/// The least a body must be before a stub is worth its own line: under this the stub costs what
/// the body did.
pub const BODY_FLOOR: usize = 400;

/// How much of a call's arguments a stub quotes, so a reader can still tell which read or which
/// command the body belonged to.
pub const STUB_ARGS: usize = 120;

/// Why a unit's body could go, in the order the plan spends them: the first two lose nothing that
/// is not somewhere else, the third is the finished call whose facts are in its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The same subject was read or run again later in the span: this body cannot be the current
    /// state of anything.
    Superseded { by: String },
    /// A subagent that has answered: its transcript is a session of its own, named by the handle
    /// the stub carries.
    Settled { child: String },
    /// A finished call whose printed output the record does not need: the call, its outcome and
    /// its arguments stay, so every path and command it named is still in the span.
    Finished,
}

impl Why {
    /// The class, for the ordering and for the sentence a stub carries.
    pub fn class(&self) -> u8 {
        match self {
            Why::Superseded { .. } => 0,
            Why::Settled { .. } => 1,
            Why::Finished => 2,
        }
    }

    /// The sentence a stub carries: what a reader of the reduced span is told about the body.
    pub fn said(&self) -> String {
        match self {
            Why::Superseded { by } => format!("superseded by the later call `{by}`"),
            Why::Settled { child } => {
                format!("its subagent `{child}` had answered, and its transcript is that session")
            }
            Why::Finished => "a finished call, whose arguments keep what the record needs".into(),
        }
    }
}

/// Where a body goes when its unit leaves the prompt, and where `recall` reads it back from.
///
/// `None` is a store that would not take it, and then the unit is not evicted at all: an
/// eviction nobody can undo is the hole a summary was supposed to prevent.
pub trait BodyStore {
    /// Keep one body and answer the id the map and `recall` spell it by.
    fn keep(&self, tool: &str, body: &[u8]) -> Option<String>;
}

/// The heading the new base carries above the map, so a reader knows what the lines are.
pub const MAP_HEADER: &str = "## What was evicted before this record was written\n\n\
    The bodies of the units below are not in this transcript, and the record above was never \
    shown them. Each line is one unit; `recall` with its id returns that body exactly.";

/// One unit whose body went: which call, what tool, what it was about, why, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Where the body is: the id the map prints and `recall` takes.
    pub id: String,
    pub call_id: String,
    pub tool: String,
    /// What the call was about, in the words it used: the file and its window, the command, or the
    /// child session. This is the half of a unit a reader can still see, so the map carries it.
    pub named: String,
    pub why: Why,
    /// Bytes of body the span no longer carries.
    pub gone: usize,
    /// What the body itself cost, which is what a reader weighing a `recall` wants to know.
    pub bytes: usize,
}

/// The reduced span and what left it.
#[derive(Debug, Clone, PartialEq)]
pub struct Eviction {
    pub items: Vec<TranscriptItem>,
    pub units: Vec<Unit>,
}

impl Eviction {
    /// The bytes of body the summary turn is no longer shown.
    pub fn gone(&self) -> usize {
        self.units.iter().map(|u| u.gone).sum()
    }

    /// The map the new base carries, see [`map_text`].
    pub fn map(&self) -> String {
        map_text(&self.units)
    }
}

/// The map the new base carries: one line per unit, so a reader can tell what the record was not
/// shown and ask for any of it back. Empty when nothing was evicted, because a map with no lines
/// under its heading reads as a compaction that lost something and is not saying what.
pub fn map_text(units: &[Unit]) -> String {
    if units.is_empty() {
        return String::new();
    }
    let mut out = String::from(MAP_HEADER);
    for u in units {
        // Subject first, then why, then size: a reader scanning for a file finds it at the same
        // place on every line, and the two numbers it does not need first are last.
        out.push_str(&format!(
            "\n- {} `{}` {}: {}; {} bytes",
            u.id,
            u.tool,
            u.named,
            u.why.said(),
            u.bytes
        ));
    }
    out
}

/// What a call is ABOUT, which is what decides whether a later call makes it stale: the file it
/// touched, the command it ran, or its arguments when it is neither.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Subject {
    File {
        path: String,
        /// Which part of the file: two windows are two subjects, and a write replaces them all.
        window: String,
        write: bool,
    },
    Command(String),
    Other(String),
}

impl Subject {
    fn of(name: &str, arguments: &str) -> Subject {
        let args: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        match name {
            // A write or an edit makes every earlier body of that file stale, whatever window it
            // was read from.
            "write" | "edit" if !path.is_empty() => Subject::File {
                path,
                window: "write".into(),
                write: true,
            },
            "bash" => Subject::Command(
                args.get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            _ if !path.is_empty() => {
                let window: String = ["offset", "limit", "ranges"]
                    .iter()
                    .filter_map(|k| args.get(*k).map(|v| format!("{k}={v}")))
                    .collect::<Vec<String>>()
                    .join(",");
                Subject::File {
                    path,
                    window,
                    write: false,
                }
            }
            _ => Subject::Other(format!("{name} {arguments}")),
        }
    }

    /// What a map line calls this unit: the file and the window it was read from, the command it
    /// ran, or the child whose transcript holds it. Clipped, because a map line is an index entry.
    fn named(&self, child: Option<&str>) -> String {
        if let Some(child) = child {
            return format!("the subagent `{child}`");
        }
        match self {
            // A write's `window` is the word, not a window: the whole file is what it is about.
            Subject::File { path, write, .. } if *write => clip(path),
            Subject::File { path, window, .. } if window.is_empty() => clip(path),
            Subject::File { path, window, .. } => format!("{path} ({window})"),
            Subject::Command(command) => format!("`{}`", clip(command)),
            Subject::Other(other) => clip(other),
        }
    }

    /// Whether a LATER call of this subject leaves nothing current in `self`.
    fn covers(&self, itself: &Subject) -> bool {
        match (self, itself) {
            (
                Subject::File {
                    path,
                    window,
                    write,
                    ..
                },
                Subject::File {
                    path: was,
                    window: was_window,
                    ..
                },
            ) => path == was && (*write || window == was_window),
            (Subject::Command(a), Subject::Command(b)) => a == b,
            (Subject::Other(a), Subject::Other(b)) => a == b,
            _ => false,
        }
    }

    /// The strings a plan row would name this subject by, for the one that is never evicted.
    fn names(&self, child: Option<&str>) -> Vec<String> {
        let mut out = Vec::new();
        match self {
            Subject::File { path, .. } => {
                out.push(path.clone());
                // A plan row usually spells a file by its own name rather than its whole path.
                if let Some(base) = path.rsplit('/').next()
                    && base.chars().count() >= 6
                    && base != path
                {
                    out.push(base.to_string());
                }
            }
            Subject::Command(command) => {
                if let Some(first) = command.lines().next().map(str::trim)
                    && !first.is_empty()
                {
                    out.push(first.to_string());
                }
            }
            Subject::Other(_) => {}
        }
        if let Some(child) = child {
            out.push(child.to_string());
        }
        out
    }
}

/// How much of what a unit was about a map line quotes: enough to recognise it, not the arguments.
pub const NAME_CLIP: usize = 100;

/// One line's worth of a subject, whatever the subject's own lines were.
fn clip(text: &str) -> String {
    let one = text.lines().next().unwrap_or_default().trim();
    match one.char_indices().nth(NAME_CLIP) {
        Some((at, _)) => format!("{}…", &one[..at]),
        None => one.to_string(),
    }
}

/// The plan rows as one lowercased text, which is what "the plan still references it" is asked of.
fn plan_text(rows: &[String]) -> String {
    rows.iter()
        .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<String>>()
        .join("\n")
        .to_lowercase()
}

/// Reduce a span, evicting whole units in class order and leaving everything else alone.
///
/// The span is the part of the conversation a record is about to replace, so a body inside it is
/// a body that buys a record a longer prompt. What is never touched: the newest exchange (the work
/// in hand), a failure's body (an error string is the evidence), a call that is still running, and
/// anything a plan row still names. A body the store will not keep is not evicted either.
pub fn reduce(items: &[TranscriptItem], rows: &[String], store: &dyn BodyStore) -> Eviction {
    let plan = plan_text(rows);
    // Everything after the last thing the operator said is the exchange the session is inside -
    // and a delivery opens one as a prompt does, so it counts as the last thing said.
    let in_hand = items
        .iter()
        .rposition(|i| {
            matches!(
                i,
                TranscriptItem::User { .. } | TranscriptItem::Notice { .. }
            )
        })
        .map(|at| at + 1)
        .unwrap_or(0);
    let calls = calls_of(items);
    let mut out = items.to_vec();
    let mut units: Vec<(usize, Unit)> = Vec::new();
    for (at, item) in items.iter().enumerate() {
        let TranscriptItem::ToolResult {
            call_id,
            name,
            outcome,
            payload,
            ..
        } = item
        else {
            continue;
        };
        // `at >= in_hand` is the newest exchange: everything before it is old enough to lose a
        // body.
        if at >= in_hand || !matches!(outcome, ToolOutcome::Ok) || payload.len() < BODY_FLOOR {
            continue;
        }
        let Some(call) = calls.get(call_id) else {
            continue;
        };
        let subject = Subject::of(name, &call.arguments);
        let child = (name == "task_result")
            .then(|| {
                serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .ok()
                    .and_then(|v| v.get("task").and_then(|t| t.as_str()).map(String::from))
            })
            .flatten();
        if subject
            .names(child.as_deref())
            .iter()
            .any(|n| plan.contains(&n.to_lowercase()))
        {
            continue;
        }
        let why = match later_twin(&calls, call.at, &subject) {
            Some(by) => Why::Superseded { by },
            None => match &child {
                Some(child) => Why::Settled {
                    child: child.clone(),
                },
                None => Why::Finished,
            },
        };
        // The body goes to the store first, because a unit whose body cannot be got back is not
        // one this may evict: the id the map prints is the store's answer, not a guess.
        let Some(id) = store.keep(name, payload.as_bytes()) else {
            continue;
        };
        let stub = stub_of(name, &call.arguments, outcome, payload.len(), &id, &why);
        let gone = payload.len().saturating_sub(stub.len());
        out[at] = match item.clone() {
            TranscriptItem::ToolResult {
                call_id,
                name,
                outcome,
                edit,
                origin,
                media,
                ..
            } => TranscriptItem::ToolResult {
                call_id,
                name,
                outcome,
                payload: stub,
                edit,
                origin,
                media,
            },
            other => other,
        };
        units.push((
            at,
            Unit {
                id,
                call_id: call_id.clone(),
                tool: name.clone(),
                named: subject.named(child.as_deref()),
                why,
                gone,
                bytes: payload.len(),
            },
        ));
    }
    // The order the plan spends them is the order of the classes: what loses nothing first.
    units.sort_by(|(ia, a), (ib, b)| a.why.class().cmp(&b.why.class()).then_with(|| ia.cmp(ib)));
    Eviction {
        items: out,
        units: units.into_iter().map(|(_, u)| u).collect(),
    }
}

/// One call in the span: where it is, what tool, and the arguments that carry its identity AND its
/// facts.
struct Call {
    at: usize,
    name: String,
    arguments: String,
}

/// Every call in the span, by id.
fn calls_of(items: &[TranscriptItem]) -> std::collections::BTreeMap<String, Call> {
    let mut out = std::collections::BTreeMap::new();
    for (at, item) in items.iter().enumerate() {
        if let TranscriptItem::Assistant { tool_calls, .. } = item {
            for call in tool_calls {
                out.insert(
                    call.id.clone(),
                    Call {
                        at,
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                );
            }
        }
    }
    out
}

/// The first later call that makes this one stale, by id. `at` is the CALL's own position: a call
/// is superseded only by a call after it, never by the result that answers it.
fn later_twin(
    calls: &std::collections::BTreeMap<String, Call>,
    at: usize,
    subject: &Subject,
) -> Option<String> {
    let mut later: Vec<(&String, &Call)> = calls.iter().filter(|(_, call)| call.at > at).collect();
    later.sort_by_key(|(_, call)| call.at);
    later.into_iter().find_map(|(id, call)| {
        Subject::of(&call.name, &call.arguments)
            .covers(subject)
            .then(|| id.clone())
    })
}

/// What stands where a body was: one line, the subject, the outcome, the size, the reason - and the
/// id `recall` takes, because a stub that names where the body is but not how to reach it is a hole.
fn stub_of(
    name: &str,
    arguments: &str,
    outcome: &ToolOutcome,
    bytes: usize,
    id: &str,
    why: &Why,
) -> String {
    let args: String = arguments.chars().take(STUB_ARGS).collect();
    let word = match outcome {
        ToolOutcome::Ok => "ok",
        _ => "settled",
    };
    format!(
        "[evicted before this record was written: `{name}` {args} gave {word} and {bytes} byte(s) \
         of body, kept as `{id}`; {}. The old transcript in the store still holds it.]",
        why.said()
    )
}

#[cfg(test)]
mod tests;
