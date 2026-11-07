//! The `pane` hook's input, and the vocabulary a host and a plugin share.
//!
//! A pane never runs anything to get its data: the host hands it read-only facts, and the layer
//! declares which ones in its manifest, so a feed it did not declare is absent rather than empty.
//!
//! ```toml
//! feeds = ["sessions"]
//! ```

/// The contract version every pane input carries. It moves when a field changes MEANING rather
/// than appears: both session shapes are rows, so the wire cannot tell them apart and a layer has
/// to say which one it draws (`letibot_plugins::MOVED`, the manifest's `[contracts]`).
pub const V: u32 = 3;

/// The most session rows a host puts in the `sessions` feed. A guest pays fuel per byte it parses
/// and the hook's budget is fixed, so a store that has held thousands of sessions would
/// otherwise trap every pane that reads the feed.
pub const SESSION_FEED_ROWS: usize = 400;

/// Cut `rows` to [`SESSION_FEED_ROWS`], keeping the ones a person is in, then the ones that moved
/// last. `key` is `(wanted regardless, last moved ms)`; the order of what is kept is unchanged.
pub fn cap_session_rows<T>(rows: Vec<T>, key: impl Fn(&T) -> (bool, u64)) -> Vec<T> {
    if rows.len() <= SESSION_FEED_ROWS {
        return rows;
    }
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(key(&rows[i])));
    let mut keep = vec![false; rows.len()];
    for &i in order.iter().take(SESSION_FEED_ROWS) {
        keep[i] = true;
    }
    rows.into_iter()
        .zip(keep)
        .filter_map(|(row, kept)| kept.then_some(row))
        .collect()
}

/// The export name a pane answers under: `pane_<name>`.
pub fn hook(name: &str) -> String {
    format!("pane_{name}")
}

/// A fact the host already has and can hand a pane, by the name the manifest spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feed {
    /// The daemon's sessions: what the session picker is drawn from. Fed today.
    Sessions,
    /// This session's monitors - armed, waiting, fired. Planned.
    Monitors,
    /// The session's background jobs, with their handles and their state. Planned.
    Jobs,
    /// The session's todo rows. Planned.
    Todos,
    /// The repository the session stands in - branch, ahead/behind, what changed. Planned.
    Git,
    /// The merge queue in the words a person reads it in: reviews and landings with their state,
    /// reason and age, the targets' health, the leases with their heartbeats, the last moves, and
    /// the repositories that are no queue's. Fed by `plugins/builtin/queue`.
    Queue,
    /// The room: one host's agents' queues per tag - every item with its state, its claimant and
    /// the age of that claim's beat, its abandonment count, the open questions the arbiter is being
    /// asked, and the room's own identity. Fed by `plugins/builtin/room`.
    Room,
    /// The GitHub reviewer seat: the scopes it is polling, each pull request's review state, what
    /// it last did, and whether anything is posted or kept as a draft. Fed by
    /// `plugins/builtin/github`.
    Github,
    /// The operator's own conversations: the channels with their unread counts, the roster per
    /// room, the selected channel and the window of its messages (`Chat`). The one feed that is
    /// not a view of the session's work, so only a built-in or a PINNED layer may declare it.
    Chat,
}

impl Feed {
    /// Every feed this contract names, in the order a document lists them.
    pub const ALL: [Feed; 9] = [
        Feed::Sessions,
        Feed::Monitors,
        Feed::Jobs,
        Feed::Todos,
        Feed::Git,
        Feed::Queue,
        Feed::Room,
        Feed::Github,
        Feed::Chat,
    ];

    /// The wire's spelling - the same word in `plugin.toml` and in `Input::feeds`.
    pub fn name(self) -> &'static str {
        match self {
            Feed::Sessions => "sessions",
            Feed::Monitors => "monitors",
            Feed::Jobs => "jobs",
            Feed::Todos => "todos",
            Feed::Git => "git",
            Feed::Queue => "queue",
            Feed::Room => "room",
            Feed::Github => "github",
            Feed::Chat => "chat",
        }
    }

    /// A feed the manifest spells. A word this build does not know is refused by name rather
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

/// A key on a row, performed by the host.
/// One of the four and never two: an act that says two things is a key nobody can state, and the
/// host refuses it rather than picking (`Row::refusals`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Act {
    /// Go to that session - what Enter means on a row of the session list.
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
    /// One of the person's verbs, on ONE row - `{"verb":"approve","id":"r-1f2…"}`. The keys act
    /// on the entry the row NAMES, not on whatever the host's own cursor is over. The word is what
    /// the manifest gates: `View::validate_for` refuses a verb the answering layer did not declare.
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

    /// A verb on one entry - see [`Act::Verb`].
    pub fn verb_on(verb: impl Into<String>, id: impl Into<String>) -> Act {
        Act::Verb {
            verb: verb.into(),
            id: id.into(),
        }
    }

    /// Which verb this is, as a manifest declares it: `switch`, `fold`, `event`, or the layer's own
    /// word. A borrowed word because a queue verb's word arrives in the answer and has no table to
    /// intern against - the alternative would be a second door, or a word `validate_for` never saw.
    pub fn verb(&self) -> &str {
        match self {
            Act::Switch { .. } => "switch",
            Act::Fold { .. } => "fold",
            Act::Event { .. } => "event",
            Act::Verb { verb, .. } => verb,
        }
    }

    /// The entry this act names, when it names one - what the queue pane's verb keys act on.
    pub fn entry(&self) -> Option<&str> {
        match self {
            Act::Verb { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// How a row folds, on the wire: `true` open, `false` closed, and `"fixed"` for a row that cannot
/// fold at all. Two states were not enough: a header that is always open would have to be written
/// `true` or `false`, and either is a claim that some key folds it - so a glyph and a hint that lie.
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

/// One run of text in a row, and what it means: the string it is when it takes the row's own role,
/// an object when it does not. The one-string spelling is the common case, which is why the wire is
/// untagged - a cell that says *this one is where I am* has to say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    /// `None` takes the row's role, and then the palette's plain.
    pub role: Option<Role>,
}

/// How many hues a role can name: `hue:0` to `hue:7`. A theme maps each to a colour that reads
/// on dark and light, so the host never takes an RGB from a plugin - the whole point of a hue.
pub const HUES: u8 = 8;

/// The palette's roles, as C3 names them, plus the eight hues a speaker list needed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// A colour by INDEX rather than by meaning: `hue:3` is the third of [`HUES`], and the theme
    /// decides which colour that is. It exists because *this speaker is blue* is a decision a
    /// chat makes and a palette of meanings cannot hold.
    Hue(u8),
}

impl Role {
    /// The role a wire word names. A word this build does not know and a hue outside the palette
    /// are both refused by name: a role is drawn, and a role guessed at is a colour nobody chose.
    pub fn parse(word: &str) -> Result<Role, String> {
        Ok(match word {
            "muted" => Role::Muted,
            "working" => Role::Working,
            "attention" => Role::Attention,
            "success" => Role::Success,
            "failure" => Role::Failure,
            "plain" => Role::Plain,
            "strong" => Role::Strong,
            "faint" => Role::Faint,
            "pending" => Role::Pending,
            other => match other.strip_prefix("hue:") {
                Some(n) => {
                    let n: u8 = n.parse().map_err(|_| {
                        format!(
                            "`{other}` is not a role: a hue is `hue:0` to `hue:{}",
                            HUES - 1
                        )
                    })?;
                    if n >= HUES {
                        return Err(format!(
                            "`{other}` is outside the palette: the hues are `hue:0` to `hue:{}",
                            HUES - 1
                        ));
                    }
                    Role::Hue(n)
                }
                None => {
                    return Err(format!(
                        "`{other}` is not a role: the palette is muted, working, attention, \
                         success, failure, plain, strong, faint, pending, and `hue:0` to `hue:{}",
                        HUES - 1
                    ));
                }
            },
        })
    }

    /// The hue this role is, when it is one.
    pub fn hue(self) -> Option<u8> {
        match self {
            Role::Hue(n) => Some(n),
            _ => None,
        }
    }
}

impl serde::Serialize for Role {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Role::Muted => s.serialize_str("muted"),
            Role::Working => s.serialize_str("working"),
            Role::Attention => s.serialize_str("attention"),
            Role::Success => s.serialize_str("success"),
            Role::Failure => s.serialize_str("failure"),
            Role::Plain => s.serialize_str("plain"),
            Role::Strong => s.serialize_str("strong"),
            Role::Faint => s.serialize_str("faint"),
            Role::Pending => s.serialize_str("pending"),
            Role::Hue(n) => s.collect_str(&format_args!("hue:{n}")),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Role {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Role, D::Error> {
        let word = String::deserialize(d)?;
        Role::parse(&word).map_err(serde::de::Error::custom)
    }
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

/// A string IS a cell - the untagged spelling, in Rust as on the wire: a caller writing a
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

/// What a pane is asked with, once per call.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// The contract version this host speaks - [`V`].
    pub v: u32,
    /// What the pane returned last time, handed back to it: state belongs to the host
    ///.
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
    /// The feeds this layer declared, and only those, by name (`Feed::name`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub feeds: std::collections::BTreeMap<String, serde_json::Value>,
    /// The host's fold state, by row id: `true` for a row the person has OPENED, which is what a
    /// row's `fold` act sets. The host keeps it so a fold is instant and survives the next answer,
    /// and a layer answers `Fold::Open`/`Fold::Closed` from it - not the other way round.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub folded: std::collections::BTreeMap<String, bool>,
    /// The answer of the layer below, when this layer asked for it - absent otherwise, and
    /// inserted by the stack rather than by a host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<serde_json::Value>,
    /// The host's clock, unix millis. A guest cannot read one, and a chat needs today versus
    /// yesterday; absent from a host that predates it, which is a host with no clock rather than
    /// one at 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now_ms: Option<u64>,
    /// The person's local offset from UTC, in seconds east of it - `3600` in Berlin's winter. The
    /// host's `now_ms` is one instant; the day and the hour a pane prints are the local reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset_s: Option<i32>,
}

/// The `chat` feed's shape (X1): the head's own room view, cut down to what a pane draws.
///
/// It is the operator's conversations, so it is the one feed whose declaration is checked against
/// the LAYER and not only against the manifest: a built-in or a pinned layer may read it, and a
/// session's own unpinned layer may not (`letibot_plugins::Manifest::admit_for`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Chat {
    /// Every channel the person can see, in the host's order - the switcher's list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChatChannel>,
    /// The channel whose messages are in `messages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// Who is in each room, by channel name - what `@` completes from.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub roster: std::collections::BTreeMap<String, Vec<String>>,
    /// The newest `seq` the person has been shown.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub seen: u64,
    /// The newest `seq` that was on screen when the channel was opened: the unread divider sits
    /// between the two, and it survives the next answer.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub opened_seen: u64,
    /// Sends that have not landed, so a layer draws them as unsent rather than as sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<ChatFailed>,
    /// The selected channel's window: the viewport plus one page each way, at most 200 messages.
    /// A window and not the history, because the host owns paging (an act asks for an older page).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ChatMessage>,
}

/// One room in the switcher: its wire name, the label the host shows, and how much is unseen.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatChannel {
    /// `host`, `project/<key>` or `dm` - the name an act spells.
    pub room: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub unread: u64,
}

/// One send the room did not take. `text` is what the person typed, so it can be offered again.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatFailed {
    pub room: String,
    pub text: String,
    /// The coordinator's own words for it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// One message, as the room's wire has it. Nothing is reformatted: a body is markdown the host
/// renders, and `ts` is unix millis against the input's `now_ms`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub seq: u64,
    pub ts: u64,
    /// The room it was said in.
    pub room: String,
    pub author: String,
    /// The seat it came from: `operator`, `agent`, `external` or `system`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// The one principal it is addressed to - a DM's only reader besides the author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The `seq` it answers, in the same room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<ChatRef>,
    /// Pictures and files it carries, by blob id - the bytes never reach the plugin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChatAttachment>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// A pointer from a message to a row another part of letibot owns - a todo, a queue row.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatRef {
    pub kind: String,
    pub id: String,
}

/// One picture or file a message carries, by the id a body's `![alt](blob:ID)` names.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatAttachment {
    /// The sha256 hex of the bytes.
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub bytes: u64,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The cap keeps what a person is in first, then the newest, in the original order.
    #[test]
    fn the_session_cap_keeps_the_wanted_and_the_newest_in_order() {
        let rows: Vec<(u32, bool, u64)> = (0..(SESSION_FEED_ROWS as u32 + 50))
            .map(|i| (i, i == 0, u64::from(i)))
            .collect();
        let kept = cap_session_rows(rows, |r| (r.1, r.2));
        assert_eq!(kept.len(), SESSION_FEED_ROWS);
        assert_eq!(kept[0].0, 0, "the wanted row survives though it is oldest");
        assert!(kept.windows(2).all(|w| w[0].0 < w[1].0), "order unchanged");
        assert_eq!(
            kept.last().map(|r| r.0),
            Some(SESSION_FEED_ROWS as u32 + 49)
        );
        assert!(cap_session_rows(vec![1, 2], |_| (false, 0)).len() == 2);
    }

    #[test]
    fn a_feed_is_the_word_its_manifest_spells() {
        assert_eq!(Feed::parse("sessions"), Ok(Feed::Sessions));
        assert_eq!(Feed::Sessions.name(), "sessions");
        // A word this build does not know is refused BY NAME, with the list.
        let why = Feed::parse("watchers").expect_err("no such feed");
        assert!(why.contains("watchers"), "{why}");
        assert!(why.contains("monitors"), "and the ones it does know: {why}");
        // Every feed the plan names is here, so a later pane adds no mechanism.
        for name in [
            "sessions", "monitors", "jobs", "todos", "git", "queue", "room", "github", "chat",
        ] {
            assert!(Feed::parse(name).is_ok(), "{name}");
        }
        assert_eq!(Feed::parse("queue"), Ok(Feed::Queue));
        assert_eq!(Feed::Queue.name(), "queue");
        assert_eq!(Feed::parse("room"), Ok(Feed::Room));
        assert_eq!(Feed::Room.name(), "room");
        // The reviewer seat's feed, which `plugins/builtin/github` declares.
        assert_eq!(Feed::parse("github"), Ok(Feed::Github));
        assert_eq!(Feed::Github.name(), "github");
        // The operator's conversations, which only a built-in or a pinned layer may declare.
        assert_eq!(Feed::parse("chat"), Ok(Feed::Chat));
        assert_eq!(Feed::Chat.name(), "chat");
        // `ALL` is the list the refusal prints, so a feed missing from it is a feed nobody can use.
        assert_eq!(Feed::ALL.len(), 9);
        for feed in Feed::ALL {
            assert_eq!(Feed::parse(feed.name()), Ok(feed), "{}", feed.name());
        }
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

    /// A verb act names the entry it is about, and the word the manifest gates is the verb's.
    /// The id is the whole reason the variant exists: a row draws ONE entry and the person's
    /// `a`/`v`/`r`/`d` must act on that one, whatever the host's own cursor is over.
    #[test]
    fn a_verb_act_carries_its_entry_and_its_own_word() {
        let one: Act =
            serde_json::from_value(json!({"verb": "approve", "id": "r-1f2"})).expect("a verb act");
        assert_eq!(one, Act::verb_on("approve", "r-1f2"));
        assert_eq!(one.verb(), "approve", "the manifest's word is the verb's");
        assert_eq!(one.entry(), Some("r-1f2"), "and the entry is the id");
        assert_eq!(
            serde_json::to_value(&one).unwrap(),
            json!({"verb": "approve", "id": "r-1f2"})
        );
        // An act that names no entry answers none - `fold` and `switch` are not verbs on a row.
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

    /// A field an input gained - the clock and the offset - is absent from a host that predates
    /// it and round-trips when it is there: an added field is not a version.
    #[test]
    fn the_clock_is_in_the_input_and_absent_from_an_old_host() {
        let clocked = Input {
            v: V,
            now_ms: Some(1_762_000_000_000),
            utc_offset_s: Some(3600),
            ..Input::default()
        };
        let wire = serde_json::to_value(&clocked).expect("serialises");
        assert_eq!(wire["now_ms"], json!(1_762_000_000_000u64));
        assert_eq!(wire["utc_offset_s"], json!(3600));
        assert_eq!(serde_json::from_value::<Input>(wire).unwrap(), clocked);

        // An input written before the clock has neither key, and is not a clock at 1970.
        let old = serde_json::from_value::<Input>(json!({"v": V, "size": {"w": 80, "h": 24}}))
            .expect("an old input parses");
        assert_eq!(old.now_ms, None);
        assert_eq!(old.utc_offset_s, None);
        let wire = serde_json::to_value(&old).unwrap();
        assert!(
            wire.get("now_ms").is_none(),
            "and is written back as it came"
        );
        assert!(wire.get("utc_offset_s").is_none());
    }

    /// A hue is a colour by index, and the palette is eight wide: `hue:8` is a role no theme can
    /// map, so it is refused at the wire rather than drawn as the plain role nobody chose.
    #[test]
    fn a_hue_outside_the_palette_does_not_parse() {
        for n in 0..HUES {
            let role = Role::parse(&format!("hue:{n}")).expect("inside the palette");
            assert_eq!(role, Role::Hue(n));
            assert_eq!(role.hue(), Some(n));
            assert_eq!(
                serde_json::to_value(role).unwrap(),
                json!(format!("hue:{n}")),
                "and it is written as the word it was read from"
            );
        }
        for word in ["hue:8", "hue:200", "hue:", "hue:three", "hue:-1"] {
            let why = Role::parse(word).expect_err("outside the palette is not a role");
            assert!(why.contains(word), "the word is named: {why}");
            assert!(why.contains("hue:0"), "and the palette: {why}");
            assert!(
                serde_json::from_value::<Role>(json!(word)).is_err(),
                "{word} must not parse off the wire either"
            );
        }
        // The meanings are unchanged, and a cell carries a hue like any other role.
        assert_eq!(Role::parse("attention"), Ok(Role::Attention));
        assert_eq!(Role::parse("strong").unwrap().hue(), None);
        let cell = Cell::role("@fix-room-push", Role::Hue(3));
        assert_eq!(
            serde_json::to_value(&cell).unwrap(),
            json!({"text": "@fix-room-push", "role": "hue:3"})
        );
        assert_eq!(
            serde_json::from_value::<Cell>(json!({"text": "x", "role": "hue:3"})).unwrap(),
            Cell::role("x", Role::Hue(3))
        );
    }

    /// The `chat` feed's shape: the wire's own fields, a window of messages, and the two marks the
    /// unread divider sits between. Round-tripped as a whole, because a field that only appears
    /// inside one message is a field a layer cannot ask for.
    #[test]
    fn the_chat_feed_carries_what_the_wire_has() {
        let doc = json!({
            "channels": [{"room": "project/letibot", "label": "letibot", "unread": 3}],
            "selected": "project/letibot",
            "roster": {"project/letibot": ["operator", "fix-room-push"]},
            "seen": 41,
            "opened_seen": 39,
            "failed": [{"room": "project/letibot", "text": "hi", "why": "refused"}],
            "messages": [{
                "seq": 41, "ts": 1_762_000_000_000u64, "room": "project/letibot",
                "author": "operator", "kind": "operator", "to": "fix-room-push",
                "thread": 39, "refs": [{"kind": "todo", "id": "T7"}],
                "attachments": [{"id": "ab12", "mime": "image/png", "name": "shot.png", "bytes": 900}],
                "text": "ping @fix-room-push"
            }]
        });
        let feed: Chat = serde_json::from_value(doc.clone()).expect("the feed parses");
        assert_eq!(feed.channels[0].unread, 3);
        assert_eq!(feed.selected.as_deref(), Some("project/letibot"));
        assert_eq!(feed.roster["project/letibot"].len(), 2);
        assert_eq!((feed.seen, feed.opened_seen), (41, 39));
        assert_eq!(feed.failed[0].why, "refused");
        let m = &feed.messages[0];
        assert_eq!((m.seq, m.ts), (41, 1_762_000_000_000));
        assert_eq!(m.to.as_deref(), Some("fix-room-push"));
        assert_eq!(m.thread, Some(39));
        assert_eq!(m.refs[0].id, "T7");
        assert_eq!(m.attachments[0].id, "ab12");
        assert_eq!(m.text, "ping @fix-room-push");
        assert_eq!(serde_json::to_value(&feed).unwrap(), doc);
        // An empty feed is a room with nothing in it, and it says so by saying nothing.
        assert_eq!(serde_json::to_value(Chat::default()).unwrap(), json!({}));
    }
}
