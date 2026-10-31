//! **The room's pane** — one host's agents' queues, per tag, and the four keys that move them.
//!
//! The operator's design (`docs/room.md` § *Still owed* 1): *"the queues per tag, the tie-breaker's
//! raw session, and the manual controls — claim on a session's behalf, release, mark abandoned, rule
//! on a dispute"*. This layer draws the queues and the questions; the four controls are the HOST's
//! verbs, performed by the daemon over the session's own connection to the coordinator. **Nothing
//! here decides what a claim does** — it decides which row says which verb.
//!
//! # What it reads
//!
//! One feed, `room` (`plugin.toml`'s `feeds = ["room"]`): every item with its tag, its state, its
//! claimant and the age of that claim's beat, its abandonment count; the open questions; the arbiter
//! and who is connected; and the room's own **identity** — *the room on this socket, started T ago
//! and holding N item(s)*, and the sentence saying it keeps no store.
//!
//! **That last one is not decoration.** An in-memory room holds nothing at every start, so an empty
//! queue is the same reading for a room that has been up for an hour and one that has just
//! restarted — unless the age is in front of the person. *"Restarted clean" and "restarted empty"
//! must never look the same*, and only the sentence tells them apart.
//!
//! # What it draws
//!
//! * **`room`** (`r`) — the identity, the arbiter and whether it is up, who is connected, and the
//!   counts. The first thing a person reads, because it says whether what follows is the truth.
//! * **`queues`** — one tag, then its items, first come first served. A row says the item, its
//!   state, its claimant, how long the claim has been held and how long since the holder's last
//!   beat, and how many claimants it has lost.
//! * **`questions`** (`a`) — every open question, with one row per decision the person can take.
//!
//! # The four keys, and where each one is offered
//!
//! | row | Enter | `x` |
//! |---|---|---|
//! | an item this session holds | `release` | `abandon` |
//! | an item anybody may claim | `claim` | — |
//! | an item somebody else holds, or a finished one | (nothing) | — |
//! | `q3:retry`, `q3:drop`, `q3:hand:<session>` | `rule` | — |
//!
//! **A key is only offered where it would do something.** A row held by somebody else carries no
//! act, because `claim` would be refused `Busy` and `release` would be refused `NotHeld` — and a row
//! that offers a key the room will refuse is a row that teaches a person not to press keys. The room
//! still refuses when it must, and the host says so (`room_act_refused`).
//!
//! **`release` and `abandon` are two intentions on one room verb.** The room counts a voluntary
//! release as an abandonment (`docs/room.md` § *A claim is a lease with a beat*), so the difference
//! the person gets is the sentence that lands on the item's history — and that sentence is the
//! host's to write, not this layer's.
//!
//! # No clock, and no folds
//!
//! `std::time::SystemTime::now()` is a panic on `wasm32-unknown-unknown` and a panic in a guest is a
//! trap, so every age arrives in milliseconds against the host's clock and this module only
//! subtracts and rounds (`.letibot/notes/the-pane-contracts-two-look-keys.md`).
//!
//! There are no folds: a fold is the HOST's map (`Input::folded`), the head keeps one for the panes
//! it draws itself, and a pane the daemon answers is handed none — so a row here that offered a
//! fold would be a glyph and a key that does nothing.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Input, Role};
use serde::Deserialize;
use serde_json::{Value, json};

/// **How long a claim may be silent before the room takes it back** — the feed carries the number
/// (`lease_ms`), so this is only the fallback for a feed that did not say.
const DEFAULT_LEASE_MS: u64 = 90_000;

/// **The feed, as this layer reads it.**
///
/// Every age in it is already milliseconds against the host's clock — a guest has none — so the
/// `now_ms` the feed also carries is not read here: this layer never needs an age it was not handed.
#[derive(Debug, Default, Deserialize)]
struct Feed {
    /// `Server::identity()` — the sentence a person reads before believing the queues.
    #[serde(default)]
    identity: String,
    #[serde(default)]
    arbiter: Option<Arbiter>,
    #[serde(default)]
    connected: Vec<String>,
    #[serde(default)]
    lease_ms: u64,
    #[serde(default)]
    threshold: u32,
    /// Why the room could not be read, when it could not.
    #[serde(default)]
    unreadable: Option<String>,
    #[serde(default)]
    tags: Vec<Tag>,
    #[serde(default)]
    questions: Vec<Question>,
    #[serde(default)]
    counts: Counts,
}

#[derive(Debug, Default, Deserialize)]
struct Arbiter {
    #[serde(default)]
    session: String,
    #[serde(default)]
    up: bool,
}

/// **One tag's queue** — a tag is a LOCK, so this is the unit a person reads in.
#[derive(Debug, Default, Deserialize)]
struct Tag {
    #[serde(default)]
    tag: String,
    #[serde(default)]
    counts: Counts,
    #[serde(default)]
    rows: Vec<Row>,
}

#[derive(Debug, Default, Deserialize)]
struct Counts {
    #[serde(default)]
    items: usize,
    #[serde(default)]
    live: usize,
    #[serde(default)]
    claimable: usize,
    #[serde(default)]
    claimed: usize,
    #[serde(default)]
    frozen: usize,
    #[serde(default)]
    done: usize,
    #[serde(default)]
    dropped: usize,
}

/// **One item**, as the daemon read it off the room.
#[derive(Debug, Default, Deserialize)]
struct Row {
    #[serde(default)]
    id: u64,
    /// The entry the host's verbs name — `i12`.
    #[serde(default)]
    entry: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    posted_by: String,
    #[serde(default)]
    posted_age_ms: u64,
    #[serde(default)]
    claimant: Option<String>,
    #[serde(default)]
    claim_age_ms: Option<u64>,
    #[serde(default)]
    beat_age_ms: Option<u64>,
    #[serde(default)]
    stale: bool,
    #[serde(default)]
    waits_on: Option<String>,
    #[serde(default)]
    abandonments: u32,
    #[serde(default)]
    last_why: Option<String>,
    #[serde(default)]
    claimable: bool,
    #[serde(default)]
    frozen: bool,
    #[serde(default)]
    mine: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Question {
    #[serde(default)]
    id: u64,
    /// `q3` — the prefix a decision is appended to.
    #[serde(default)]
    entry: String,
    #[serde(default)]
    item: u64,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    about: String,
    #[serde(default)]
    asked_age_ms: u64,
    #[serde(default)]
    text: String,
    #[serde(default)]
    lost: usize,
    #[serde(default)]
    behind: usize,
}

/// The pane's answer — `None` is *the layer passed*, and a stack below would be asked.
fn answer(bytes: &[u8]) -> Option<String> {
    let input: Input = serde_json::from_slice(bytes).ok()?;
    // **A feed the host did not hand is a screen that says so.** Drawing nothing would be a pane
    // somebody thinks they closed, and an empty room is a different fact.
    let Some(value) = input.feeds.get("room") else {
        return serde_json::to_string(&no_feed()).ok();
    };
    let feed: Feed = serde_json::from_value(value.clone()).unwrap_or_default();
    serde_json::to_string(&view(&feed)).ok()
}

/// **The pane a host hands no `room` feed**: one panel naming the missing half.
fn no_feed() -> Value {
    json!({
        "title": "the room",
        "panels": [{
            "id": "room",
            "summary": [{"tree": {
                "rows": [],
                "empty": "this host hands no `room` feed, so there is nothing to draw — the pane \
                          needs a daemon holding a connection to the box's coordinator \
                          (`harnessd --room`), and a session that may speak in it.",
            }}],
        }],
    })
}

/// **The whole view.**
fn view(feed: &Feed) -> Value {
    // **A room that could not be read says so instead of drawing an empty queue.** *Nothing is
    // queued* and *nobody answered* are two facts, and a screen that shows one for the other is a
    // screen a person cannot act on.
    if let Some(why) = &feed.unreadable {
        return json!({
            "title": "the room",
            "panels": [{
                "id": "room",
                "title": "the room",
                "summary": [{"lines": [{
                    "label": "unreadable",
                    "value": why,
                    "role": "failure",
                }]}],
                "unavailable": {
                    "why": why,
                    "fix": "the room is a daemon of its own (`harnessd --room`); a session whose \
                            coordinator is not running has no queue to draw",
                },
            }],
        });
    }
    let mut panels = vec![header(feed)];
    panels.push(queues(feed));
    if !feed.questions.is_empty() {
        panels.push(questions(feed));
    }
    json!({
        "title": "the room",
        "badge": {"text": badge(feed), "role": badge_role(feed)},
        "panels": panels,
        "legend": legend(feed),
        // **Five seconds**, C3's floor: the room moves on claims and beats, and a picture older
        // than a handful of seconds is a picture of a queue somebody else has already taken.
        "every_s": 5,
    })
}

/// The status row's few words — what a person glances at without opening the pane.
fn badge(feed: &Feed) -> String {
    let c = &feed.counts;
    let mut parts = vec![format!("{} live", c.live)];
    if c.claimed > 0 {
        parts.push(format!("{} held", c.claimed));
    }
    if c.frozen > 0 {
        parts.push(format!("{} frozen", c.frozen));
    }
    if !feed.questions.is_empty() {
        parts.push(format!("{} to rule", feed.questions.len()));
    }
    parts.join(" · ")
}

fn badge_role(feed: &Feed) -> Role {
    if !feed.questions.is_empty() || feed.counts.frozen > 0 {
        Role::Attention
    } else if feed.counts.claimed > 0 {
        Role::Pending
    } else {
        Role::Faint
    }
}

/// **The room itself** — the identity sentence, the arbiter, who is connected, and the counts.
fn header(feed: &Feed) -> Value {
    let mut lines: Vec<Value> = Vec::new();
    if let Some(a) = &feed.arbiter {
        lines.push(json!({
            "label": "arbiter",
            "value": a.session,
            // **Up or not, in the role that makes a person look.** The socket is the daemon's
            // liveness, so an arbiter that is not connected is a room whose questions wait for
            // whoever opens this pane.
            "role": if a.up { "success" } else { "attention" },
            "tail": if a.up { "connected" } else { "NOT connected — a question waits" },
        }));
    } else {
        lines.push(json!({
            "label": "arbiter",
            "value": "nobody",
            "role": "attention",
            "tail": "a question waits for whoever rules",
        }));
    }
    lines.push(json!({
        "label": "here",
        "value": feed.connected.join(", "),
        "role": "faint",
        "tail": format!("{} session(s) connected", feed.connected.len()),
    }));
    lines.push(json!({
        "label": "items",
        "value": format!(
            "{} live · {} claimable · {} held · {} frozen · {} finished",
            feed.counts.live,
            feed.counts.claimable,
            feed.counts.claimed,
            feed.counts.frozen,
            feed.counts.done + feed.counts.dropped,
        ),
        "role": "plain",
    }));
    lines.push(json!({
        "label": "lease",
        "value": format!(
            "a claim is taken back after {}s of silence, at {} claimant(s) it freezes",
            lease_of(feed) / 1_000,
            if feed.threshold == 0 { 3 } else { feed.threshold },
        ),
        "role": "faint",
    }));
    json!({
        "id": "room",
        "key": "r",
        "title": "the room",
        // **The identity is a SENTENCE**, so it is markdown: a `lines` value is cut to the grid's
        // value column, and a sentence cut in the middle is a sentence nobody finishes.
        "summary": [
            {"markdown": identity(feed)},
            {"lines": lines},
        ],
    })
}

/// **The identity, in the room's own words** — the field the feed carries verbatim, and the one
/// that keeps *restarted clean* and *restarted empty* apart.
fn identity(feed: &Feed) -> String {
    if feed.identity.trim().is_empty() {
        "the room did not say what it is — which is itself a fact: a coordinator that cannot name \
         itself is one a reader should not trust the queue of."
            .to_string()
    } else {
        feed.identity.clone()
    }
}

fn lease_of(feed: &Feed) -> u64 {
    if feed.lease_ms == 0 {
        DEFAULT_LEASE_MS
    } else {
        feed.lease_ms
    }
}

/// **The queues, per tag** — a tag row, then its items in the room's own order.
fn queues(feed: &Feed) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    for tag in &feed.tags {
        rows.push(json!({
            "id": format!("t:{}", tag.tag),
            "cells": [Cell::role(format!("`{}`", tag.tag), Role::Strong)],
            "facts": [
                Cell::role(format!("{} item(s)", tag.counts.items), Role::Faint),
                Cell::role(
                    if tag.counts.claimed > 0 {
                        format!("{} held", tag.counts.claimed)
                    } else {
                        format!("{} claimable", tag.counts.claimable)
                    },
                    if tag.counts.claimed > 0 { Role::Pending } else { Role::Faint },
                ),
                Cell::role(
                    if tag.counts.frozen > 0 {
                        format!("{} frozen", tag.counts.frozen)
                    } else {
                        String::new()
                    },
                    Role::Attention,
                ),
            ],
            "children": tag.rows.iter().map(item_row).collect::<Vec<Value>>(),
        }));
    }
    json!({
        "id": "queues",
        "title": "queues",
        "summary": [{"tree": {
            "rows": rows,
            "empty": "the room holds nothing. That is not the same as *nothing was posted*: this \
                      room keeps no store, so an empty queue is what a restart looks like — read \
                      the line above for how long it has been up.",
        }}],
    })
}

/// **One item, one row** — and the one verb that would do something on it.
fn item_row(r: &Row) -> Value {
    let mut facts: Vec<Cell> = Vec::new();
    if let Some(who) = &r.claimant {
        facts.push(Cell::role(
            format!("held by {who}"),
            if r.mine { Role::Strong } else { Role::Plain },
        ));
        if let Some(held) = r.claim_age_ms {
            facts.push(Cell::role(age(held), Role::Faint));
        }
        if let Some(beat) = r.beat_age_ms {
            // **The age of the beat is the whole question** *is that claim about to be taken
            // back?* — so it is a fact of its own, in the role that makes it one.
            facts.push(Cell::role(
                format!("beat {}", age(beat)),
                if r.stale { Role::Failure } else { Role::Faint },
            ));
        }
    }
    if r.abandonments > 0 {
        facts.push(Cell::role(
            format!("{} lost", r.abandonments),
            if r.frozen {
                Role::Attention
            } else {
                Role::Faint
            },
        ));
    }
    if let Some(waits) = &r.waits_on {
        facts.push(Cell::role(format!("waits: {waits}"), Role::Faint));
    }
    let mut under: Vec<String> = Vec::new();
    under.push(format!(
        "#{} posted by `{}` {} ago",
        r.id,
        r.posted_by,
        age(r.posted_age_ms)
    ));
    if let Some(why) = &r.last_why {
        under.push(format!("last: {why}"));
    }
    if r.frozen {
        under.push(
            "frozen until a ruling — the tag's queue has moved past it, and the question is in the \
             `a` panel"
                .to_string(),
        );
    }
    json!({
        "id": r.entry,
        "cells": [
            Cell::role(format!("#{}", r.id), Role::Faint),
            Cell::role(r.body.clone(), Role::Strong),
            Cell::role(r.state.clone(), state_role(&r.state)),
        ],
        "facts": facts,
        "under": under,
        // **The one verb that would move this row**, and none where none would.
        "enter": enter_of(r),
        "actions": actions_of(r),
    })
}

/// **What Enter does on an item row** — `release` on this session's own claim, `claim` where
/// anybody may take it, and nothing at all otherwise.
fn enter_of(r: &Row) -> Value {
    if r.mine {
        return json!(Act::verb_on("release", r.entry.clone()));
    }
    if r.claimable {
        return json!(Act::verb_on("claim", r.entry.clone()));
    }
    Value::Null
}

/// **The other key a row offers** — `x` marks this session's own claim abandoned, which is the same
/// room verb as a release with the harder sentence on it.
fn actions_of(r: &Row) -> Value {
    if !r.mine {
        return json!([]);
    }
    json!([{"key": "x", "label": "mark abandoned", "verb": "abandon"}])
}

/// The palette's role for a state — the room's own words, in the colours this head already uses.
fn state_role(state: &str) -> Role {
    match state {
        "open" => Role::Pending,
        "claimed" => Role::Working,
        "abandoned" => Role::Attention,
        "done" => Role::Success,
        "dropped" => Role::Muted,
        _ => Role::Faint,
    }
}

/// **The open questions, one row per decision** — a question is answered by picking the answer, so
/// the answers are the rows.
fn questions(feed: &Feed) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    for q in &feed.questions {
        rows.push(json!({
            "id": q.entry,
            "cells": [
                Cell::role(format!("q{}", q.id), Role::Attention),
                Cell::role(format!("#{} under `{}`", q.item, q.tag), Role::Strong),
                Cell::role(q.body.clone(), Role::Plain),
            ],
            "facts": [
                Cell::role(format!("asked {} ago", age(q.asked_age_ms)), Role::Faint),
                Cell::role(format!("{} claimant(s) lost", q.lost), Role::Faint),
                Cell::role(format!("{} behind it", q.behind), Role::Faint),
            ],
            "detail": q.text.clone(),
            "under": [if q.about == "claim_disputed" {
                "a claimant says it is still working on it — rule which of the two has it"
            } else {
                "it has lost its claimants — offer it again, give it to somebody, or drop it"
            }],
        }));
        rows.push(decision_row(q, "retry", "retry — offer it again, the count kept"));
        rows.push(decision_row(
            q,
            "drop",
            "drop — out of the queue for good, with the reason on it",
        ));
        // **One row per session the room has met**, because `hand` names one and a row that named
        // nobody would be refused by the room (`UnknownSession`) — the person picks who.
        for session in &feed.connected {
            rows.push(decision_row(
                q,
                &format!("hand:{session}"),
                &format!("hand it to {session}"),
            ));
        }
    }
    json!({
        "id": "questions",
        "key": "a",
        "title": "questions",
        "summary": [{"tree": {"rows": rows}}],
    })
}

/// One decision a question offers — the row a person picks.
fn decision_row(q: &Question, decision: &str, label: &str) -> Value {
    json!({
        "id": format!("{}:{}", q.entry, decision),
        "cells": [Cell::role(" ", Role::Faint), Cell::role(label, Role::Plain)],
        // **The verb is `rule` on all of them**, and what differs is the entry: the decision is
        // part of the row's identity because it is what the person picked.
        "enter": Act::verb_on("rule", format!("{}:{}", q.entry, decision)),
    })
}

/// What every mark and word means — the `l` view, and the text that answers on hover.
fn legend(feed: &Feed) -> Value {
    json!({
        "open": "nobody has it — `enter` claims it for this session",
        "claimed": "somebody holds it; while that claim lives the tag is locked",
        "abandoned": "it lost its claimant and went back to the head of its tag; the count is how \
                      many it has lost",
        "done": "the work was done",
        "dropped": "a ruling took it out of the queue — nobody did the work",
        "frozen": format!(
            "it lost {} claimant(s) and waits for a ruling; the tag's queue has moved past it",
            if feed.threshold == 0 { 3 } else { feed.threshold }
        ),
        "beat": "how long since the holder said it was working — a claim goes back to the queue \
                 after the lease of silence, and the age turns red past it",
        "2 lost": "how many claimants this item has lost; at the threshold it freezes",
        "enter": "claim it for this session (`claim`), give up a claim this session holds \
                  (`release`), or rule (`rule`)",
        "x": "mark this session's own claim abandoned — the same room verb as a release, with the \
              harder sentence on the item's history",
        "r": "the room itself: what it is, how long it has been up, and who arbitrates",
        "a": "the questions the room is asking, and the decisions a person can take",
    })
}

/// **A duration a person reads** — `4s`, `2m`, `1h 3m`. The feed hands milliseconds; a row that
/// printed `92000` would be a number nobody converts.
fn age(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let s = ms / 1_000;
    if s < 60 {
        return format!("{s}s");
    }
    let (m, rs) = (s / 60, s % 60);
    if m < 60 {
        return if rs == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {rs}s")
        };
    }
    let (h, rm) = (m / 60, m % 60);
    if rm == 0 {
        format!("{h}h")
    } else {
        format!("{h}h {rm}m")
    }
}

/// The layer's policy, tested natively — see `tests.rs`.
#[cfg(test)]
mod tests;

/// The pane hook — `pane_<name>`, the name a head opens by (`room/room`).
#[unsafe(no_mangle)]
pub extern "C" fn pane_room(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}
