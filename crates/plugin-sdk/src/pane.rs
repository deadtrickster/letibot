//! **The `pane` hook's input, and the vocabulary a host and a plugin share** — C3 of
//! `docs/plugins-slice1.md`, §4.9 of `docs/plugins-plan.md`.
//!
//! The pane's *answer* (a `View`) lives in `letibot_sessionlog::pane`, because the host that
//! validates and draws it is the head and that crate is where the panes' document belongs. What
//! lives here is the other direction and the words both directions use: the input a host builds,
//! the **feeds** it carries, and the small vocabulary a row is made of — a cell, a fold, and the
//! act a key performs.
//!
//! # Feeds
//!
//! A pane never runs anything to get its data (§1, principle 1). What it needs, the host already
//! has: the daemon's session list, this session's monitors, its jobs, its todos, the repository's
//! state — read-only facts, nothing adjudicated, nothing to grant. A layer **declares** which of
//! them it reads in its `plugin.toml`:
//!
//! ```toml
//! feeds = ["sessions"]
//! ```
//!
//! and the host hands it exactly those, under their names, in `Input::feeds`. A layer that reads
//! a feed it did not declare is not handed one — the field is absent, because the host built the
//! map from the manifest and not from the answer. That is the whole of the sandbox on this side:
//! the declarations are what a reviewer reads, and what a host can honestly promise.
//!
//! `sessions` and `queue` are fed today (`plugins/builtin/sessions` and `plugins/builtin/queue`
//! are their readers). `monitors`, `jobs`, `todos` and `git` are named here — one more feed each,
//! no format change — because the next panes want them and a vocabulary that grows one word at a
//! time grows faster than one that has to be designed twice.
//!
//! # Acts
//!
//! A row may carry an **act** the person performs with Enter: `switch` to that session, `fold`
//! it, or call the pane again with an `event`. The act is performed by the **host**, never by the
//! plugin, for the same reason a look is: a plugin answers data, and data that said *switch to
//! s-3* is a request the head carries out where the head knows what switching costs.
//!
//! An act is **declared too** (`actions = ["switch"]`), so a layer cannot ask the host for a verb
//! nobody reviewed — and so that the next verb (`disarm` a monitor, `fire` one now) is one more
//! word rather than a new mechanism.

/// The contract version every pane input carries. **Version 2** added `feeds`, `folded` and the
/// row's `enter`: three things a version-1 pane never saw and cannot want, so a plugin that reads
/// `v` and refuses anything but 1 keeps working — it simply never asks.
///
/// The manifest's `contract` (in `crates/plugins`) is a different number and stays 1: an added
/// *field* is not a version there (`docs/plugins-plan.md` §1, principle 6).
pub const V: u32 = 2;

/// The export name a pane answers under: `pane_<name>`.
pub fn hook(name: &str) -> String {
    format!("pane_{name}")
}

/// **A fact the host already has and can hand a pane**, by the name the manifest spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feed {
    /// The daemon's sessions: what the session picker is drawn from. Fed today.
    Sessions,
    /// This session's monitors — armed, waiting, fired. Planned.
    Monitors,
    /// The session's background jobs, with their handles and their state. Planned.
    Jobs,
    /// The session's todo rows. Planned.
    Todos,
    /// The repository the session stands in — branch, ahead/behind, what changed. Planned.
    Git,
    /// **The merge queue, in the words a person reads it in** — the reviews and the landings
    /// with their state, their reason and their age, the target branches' health, the leases
    /// with their heartbeats, the last moves, and the repositories that are no queue's. Fed
    /// today, by `plugins/builtin/queue`.
    Queue,
}

impl Feed {
    /// Every feed this contract names, in the order a document lists them.
    pub const ALL: [Feed; 6] = [
        Feed::Sessions,
        Feed::Monitors,
        Feed::Jobs,
        Feed::Todos,
        Feed::Git,
        Feed::Queue,
    ];

    /// The wire's spelling — the same word in `plugin.toml` and in `Input::feeds`.
    pub fn name(self) -> &'static str {
        match self {
            Feed::Sessions => "sessions",
            Feed::Monitors => "monitors",
            Feed::Jobs => "jobs",
            Feed::Todos => "todos",
            Feed::Git => "git",
            Feed::Queue => "queue",
        }
    }

    /// A feed the manifest spells. A word this build does not know is refused **by name** rather
    /// than ignored: a pane asked for a feed nobody hands over would draw an empty panel and look
    /// like a host that lost its data.
    pub fn parse(s: &str) -> Result<Feed, String> {
        Feed::ALL
            .into_iter()
            .find(|f| f.name() == s.trim())
            .ok_or_else(|| {
                format!(
                    "unknown feed `{s}` — this host knows {}",
                    Feed::ALL.map(Feed::name).join(", ")
                )
            })
    }
}

impl serde::Serialize for Feed {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> serde::Deserialize<'de> for Feed {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Feed, D::Error> {
        let word = String::deserialize(d)?;
        Feed::parse(&word).map_err(serde::de::Error::custom)
    }
}

/// **A key on a row, performed by the host.**
///
/// One of the four and never two: an act that says two things is a key whose meaning nobody can
/// state, and the host refuses it rather than picking (`Row::refusals`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Act {
    /// Go to that session — what Enter means on a row of the session list.
    Switch {
        #[serde(rename = "switch")]
        session: String,
    },
    /// Open or close this row: the host keeps the fold state per row id, so a fold is instant and
    /// survives the next answer.
    Fold {
        #[serde(rename = "fold")]
        fold: bool,
    },
    /// Call the pane again with `{"item": <the row's id>, "action": <this word>}`.
    Event { event: String },
    /// **One of the person's verbs, on ONE row** — `{"verb":"approve","id":"r-1f2…"}`.
    ///
    /// The queue pane's keys (`a` approve, `v` veto, `r` restart, `d` rm) act on the entry the
    /// row NAMES, not on whatever the host's own cursor happened to be over. That is the whole
    /// point: before this variant a row could draw an entry and the keys would act on another,
    /// so a plugin row could not say *which* entry it meant.
    ///
    /// **The verb word is what the manifest gates.** A layer that answers a row carrying
    /// `approve` must declare `actions = ["approve", …]`, and `View::validate_for` refuses the
    /// view otherwise; the head also refuses to perform a verb the answering layer did not
    /// declare, so a letter whose word is absent from the manifest sends nothing.
    Verb { verb: String, id: String },
}

impl Act {
    pub fn switch(session: impl Into<String>) -> Act {
        Act::Switch {
            session: session.into(),
        }
    }

    pub fn fold() -> Act {
        Act::Fold { fold: true }
    }

    pub fn event(word: impl Into<String>) -> Act {
        Act::Event { event: word.into() }
    }

    /// **A verb on one entry** — see [`Act::Verb`].
    pub fn verb_on(verb: impl Into<String>, id: impl Into<String>) -> Act {
        Act::Verb {
            verb: verb.into(),
            id: id.into(),
        }
    }

    /// Which verb this is, as a manifest declares it — `switch`, `fold`, `event`, or the queue
    /// verb's own word (`approve`, `veto`, `restart`, `rm`, …).
    ///
    /// The last arm is why this answers a borrowed word rather than a `'static` one: a queue
    /// verb's name is the plugin's, and the allowlist a reviewer reads is the list of those names.
    pub fn verb(&self) -> &str {
        match self {
            Act::Switch { .. } => "switch",
            Act::Fold { .. } => "fold",
            Act::Event { .. } => "event",
            Act::Verb { verb, .. } => verb,
        }
    }

    /// **The entry this act names**, when it names one — what the queue pane's verb keys act on.
    pub fn entry(&self) -> Option<&str> {
        match self {
            Act::Verb { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// **How a row folds**, on the wire: `true` open, `false` closed, and `"fixed"` for a row that
/// cannot fold at all.
///
/// Two states were not enough. A group header that is always open — the session list's `mine` —
/// has to be written `true` or `false` when only a bool exists, and both are a claim that some key
/// folds it: a list then draws the glyph and offers *enter folds it away* on a row where Enter
/// does nothing. `Fixed` is that row, and it is the third state a bool cannot say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fold {
    Open,
    /// What a group a host has not opened yet is.
    #[default]
    Closed,
    /// Not foldable at all: no glyph, no key, and a hint that names none.
    Fixed,
}

impl serde::Serialize for Fold {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Fold::Open => s.serialize_bool(true),
            Fold::Closed => s.serialize_bool(false),
            Fold::Fixed => s.serialize_str("fixed"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Fold {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Fold, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Bool(bool),
            Word(String),
        }
        match Wire::deserialize(d)? {
            Wire::Bool(true) => Ok(Fold::Open),
            Wire::Bool(false) => Ok(Fold::Closed),
            Wire::Word(w) if w == "fixed" => Ok(Fold::Fixed),
            // A word this list does not know is refused rather than guessed: a fold read as
            // `Closed` on a string meant for something else draws a glyph and offers a key that
            // does nothing.
            Wire::Word(w) => Err(serde::de::Error::custom(format!(
                "`open` is true, false or \"fixed\", not {w:?}"
            ))),
        }
    }
}

/// **One run of text in a row, and what it means** — the string it is when it takes the row's own
/// role, an object when it does not.
///
/// `"the habr article"` is a cell; `{"text": "the habr article", "role": "strong"}` is a cell that
/// says *this one is where I am*. The one-string spelling is the common case and is why this is
/// untagged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    /// `None` takes the row's role, and then the palette's plain.
    pub role: Option<Role>,
}

/// The palette's roles, as C3 names them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Muted,
    Working,
    Attention,
    Success,
    Failure,
    #[default]
    Plain,
    /// Where the head is standing (`here` on a session row).
    Strong,
    /// A fact that is true rather than good (`generating`, `on disk`).
    Faint,
    /// A turn is running in it.
    Pending,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            role: None,
        }
    }

    pub fn role(text: impl Into<String>, role: Role) -> Cell {
        Cell {
            text: text.into(),
            role: Some(role),
        }
    }
}

/// **A string IS a cell** — the untagged spelling, in Rust as on the wire: a caller writing a
/// row of plain text writes `vec!["one".into(), "two".into()]` and says nothing about roles.
impl From<&str> for Cell {
    fn from(text: &str) -> Cell {
        Cell::new(text)
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Cell {
        Cell { text, role: None }
    }
}

impl serde::Serialize for Cell {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.role {
            None => s.serialize_str(&self.text),
            Some(role) => {
                use serde::ser::SerializeStruct;
                let mut out = s.serialize_struct("Cell", 2)?;
                out.serialize_field("text", &self.text)?;
                out.serialize_field("role", &role)?;
                out.end()
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for Cell {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Cell, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Text(String),
            Full { text: String, role: Role },
        }
        Ok(match Wire::deserialize(d)? {
            Wire::Text(text) => Cell { text, role: None },
            Wire::Full { text, role } => Cell {
                text,
                role: Some(role),
            },
        })
    }
}

/// The rectangle the pane is drawn in, which only the head knows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

/// What the person just pressed, when the pane is asked again because of a key.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    /// The id of the row the key was pressed on.
    pub item: String,
    /// The word the row's action carried (`Act::Event`).
    pub action: String,
}

/// **What a pane is asked with**, once per call.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// The contract version this host speaks — [`V`].
    pub v: u32,
    /// What the pane returned last time, handed back to it: state belongs to the host
    /// (§1, principle 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
    /// The answer to the look the pane's last answer asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<serde_json::Value>,
    /// The key that asked for this call, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<Event>,
    #[serde(default)]
    pub size: Size,
    /// **The feeds this layer declared, and only those**, by name (`Feed::name`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub feeds: std::collections::BTreeMap<String, serde_json::Value>,
    /// **The host's fold state, by row id** — `true` for a row the person has folded away. The
    /// host keeps it so a fold is instant and survives the next answer; a pane that has no folds
    /// never sees the field.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub folded: std::collections::BTreeMap<String, bool>,
    /// The answer of the layer below, when this layer asked for it (§2.1) — absent otherwise, and
    /// inserted by the stack rather than by a host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_feed_is_the_word_its_manifest_spells() {
        assert_eq!(Feed::parse("sessions"), Ok(Feed::Sessions));
        assert_eq!(Feed::Sessions.name(), "sessions");
        // A word this build does not know is refused BY NAME, with the list.
        let why = Feed::parse("watchers").expect_err("no such feed");
        assert!(why.contains("watchers"), "{why}");
        assert!(why.contains("monitors"), "and the ones it does know: {why}");
        // Every feed the plan names is here, so a later pane adds no mechanism.
        for name in ["sessions", "monitors", "jobs", "todos", "git", "queue"] {
            assert!(Feed::parse(name).is_ok(), "{name}");
        }
        assert_eq!(Feed::parse("queue"), Ok(Feed::Queue));
        assert_eq!(Feed::Queue.name(), "queue");
    }

    #[test]
    fn a_cell_is_a_string_or_a_string_with_a_role() {
        let plain: Cell = serde_json::from_value(json!("a name")).expect("a plain cell");
        assert_eq!(plain, Cell::new("a name"));
        assert_eq!(serde_json::to_value(&plain).unwrap(), json!("a name"));

        let strong: Cell =
            serde_json::from_value(json!({"text": "a name", "role": "strong"})).expect("a role");
        assert_eq!(strong, Cell::role("a name", Role::Strong));
        assert_eq!(
            serde_json::to_value(&strong).unwrap(),
            json!({"text": "a name", "role": "strong"})
        );
    }

    #[test]
    fn a_fold_is_a_bool_and_fixed_is_the_third_word() {
        let fold = |j: serde_json::Value| serde_json::from_value::<Fold>(j).ok();
        assert_eq!(fold(json!(true)), Some(Fold::Open));
        assert_eq!(fold(json!(false)), Some(Fold::Closed));
        assert_eq!(fold(json!("fixed")), Some(Fold::Fixed));
        assert_eq!(fold(json!("shut")), None, "a word it does not know");
        assert_eq!(serde_json::to_value(Fold::Fixed).unwrap(), json!("fixed"));
        assert_eq!(serde_json::to_value(Fold::Open).unwrap(), json!(true));
    }

    #[test]
    fn an_act_says_one_thing_and_says_which_verb_it_is() {
        let act = |j: serde_json::Value| serde_json::from_value::<Act>(j).ok();
        assert_eq!(act(json!({"switch": "s-1"})), Some(Act::switch("s-1")));
        assert_eq!(act(json!({"fold": true})), Some(Act::fold()));
        assert_eq!(act(json!({"event": "open"})), Some(Act::event("open")));
        // The verb is what a manifest declares.
        assert_eq!(Act::switch("s-1").verb(), "switch");
        assert_eq!(Act::fold().verb(), "fold");
        assert_eq!(Act::event("open").verb(), "event");
    }

    /// **A verb act names the entry it is about, and the word the manifest gates is the verb's.**
    ///
    /// The id is the whole reason the variant exists: a row of a queue pane draws ONE entry and
    /// the person's `a`/`v`/`r`/`d` must act on that one, whatever the host's own cursor is over.
    #[test]
    fn a_verb_act_carries_its_entry_and_its_own_word() {
        let one: Act = serde_json::from_value(json!({"verb": "approve", "id": "r-1f2"}))
            .expect("a verb act");
        assert_eq!(one, Act::verb_on("approve", "r-1f2"));
        assert_eq!(one.verb(), "approve", "the manifest's word is the verb's");
        assert_eq!(one.entry(), Some("r-1f2"), "and the entry is the id");
        assert_eq!(
            serde_json::to_value(&one).unwrap(),
            json!({"verb": "approve", "id": "r-1f2"})
        );
        // An act that names no entry answers none — `fold` and `switch` are not verbs on a row.
        assert_eq!(Act::fold().entry(), None);
        assert_eq!(Act::switch("s-1").entry(), None);
        assert_eq!(Act::event("open").entry(), None);
        // A verb act without an id is not a verb act, and no other variant claims it either.
        assert_eq!(
            serde_json::from_value::<Act>(json!({"verb": "approve"})).ok(),
            None
        );
    }

    #[test]
    fn the_input_round_trips_with_only_the_feeds_it_was_given() {
        let mut input = Input {
            v: V,
            size: Size { w: 120, h: 40 },
            ..Input::default()
        };
        input
            .feeds
            .insert(Feed::Sessions.name().into(), json!([{"id": "s-1"}]));
        let wire = serde_json::to_value(&input).expect("serialises");
        assert_eq!(wire["v"], json!(V));
        assert_eq!(wire["feeds"]["sessions"][0]["id"], json!("s-1"));
        assert!(wire.get("folded").is_none(), "an empty map is not sent");
        assert!(wire.get("state").is_none());
        assert_eq!(serde_json::from_value::<Input>(wire).unwrap(), input);
    }
}
