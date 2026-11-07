//! The pane contract (`docs/plugins-slice1.md`): what a `pane_<name>` hook answers. The daemon
//! validates a plugin's answer into these types (`View::validate`) and the head draws them.
//! Unknown fields are ignored; an unknown block kind is kept ([`Block::Unknown`]).

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

// The three words a row shares with the plugin that answered it - a cell, a fold and the act a
// key performs. They live in the SDK because the guest compiles that crate too; re-exported here
// so a reader of the pane contract has one import for its whole vocabulary.
pub use letibot_plugin::pane::{Act, Cell, Fold, Role};

/// The keys the host owns on every pane, whatever a plugin says - C3's list. A view that
/// binds one is refused (see [`View::validate`]): a plugin that could take `q` would own a key
/// the operator expects to leave with.
pub const HOST_KEYS: [char; 5] = ['q', 'j', 'k', 'f', 'l'];

/// The chords the host owns on every surface. A view key spelled as one of them is refused for the
/// same reason [`HOST_KEYS`] is: it is how the person leaves, sends, or enters the cursor.
/// The arrows, paging and the wheel are not words here - a view key is a letter or [`NAMED_KEYS`].
pub const HOST_CHORDS: [&str; 4] = ["esc", "enter", "tab", "ctrl-c"];

/// The chords a view may bind by name, beside a letter of either case. `ctrl-k` is the chat's own
/// (kill to the end of the line) and `home`/`end` move in the stream - none of them is the host's.
pub const NAMED_KEYS: [&str; 3] = ["ctrl-k", "end", "home"];

/// The narrowest and widest a docked pane may ask for, in cells. A column narrower than this holds
/// no message, and a wider one is the conversation with a stripe left over.
pub const MIN_DOCK_W: u32 = 20;
pub const MAX_DOCK_W: u32 = 200;

/// The shortest the host will re-run a look for - `every_s` is 0 (never) or at least this
/// many seconds. Anything faster is a pane that spends more time asking than drawing.
pub const MIN_EVERY_S: u64 = 5;

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The same, for a row's depth - absent is the top level.
pub(crate) fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

/// The view: everything a pane draws for one moment of its life.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// The pane's title, drawn first. The pane's own name is the wire's
    /// ([`crate::SessionEvent::PluginPane::pane`]) and is not repeated here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The pane's counter - a few words for the status row, as a turn's progress is counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<Badge>,
    /// The overview, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<Panel>,
    /// Everything that tripped, is missing, or passed - the `f` view, and the summary line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    /// What every label means: the `l` view, and the hover text.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legend: BTreeMap<String, String>,
    /// What the pane wants run before it is asked again, and how often.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<LookSpec>,
    /// 0 is never; otherwise the seconds between two looks.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub every_s: u64,
    /// What this pane returned last time, handed back to it on the next call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    /// The pane's own text input (`X5`). While the pane is open the host's composer is the
    /// pane's: the host edits the text and keeps history and drafts, the layer is handed a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<PaneInput>,
    /// The keys the view binds itself (`X8`). With an input they are live only in cursor mode -
    /// Tab on an empty input enters it, Esc leaves it - because letters belong to the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<ViewKey>,
    /// Where the host lays the pane out (`X10`). Absent is the screen it has always been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dock: Option<Dock>,
    /// How many cells wide a docked pane asks for. The host owns the divider and the width keys,
    /// and honours this within [`MIN_DOCK_W`]..=[`MAX_DOCK_W`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

/// The pane's own input surface (`X5`): one composer, keyed so two channels keep two drafts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneInput {
    /// The history and draft key - the channel, so each room remembers its own line. Required,
    /// because the host cannot invent the name it keys a person's drafts under.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Whether Shift+Enter and Alt+Enter make a second line. Absent is a one-line field.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiline: bool,
    /// The hint drawn under the field, in the layer's own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footer: Option<String>,
}

/// One key the view binds itself (`X8`) - a shortcut on the pane, not on a stop.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewKey {
    /// A letter of either case, or one of [`NAMED_KEYS`].
    pub key: String,
    /// What the hint bar says about it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Call the pane again with the entry under the cursor as `item`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// One of the person's verbs, on the entry under the cursor - gated like a row's `enter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
}

impl ViewKey {
    /// The key as the host matches it: lowercased, so a shift and a plain letter cannot be two
    /// keys the head's `Key::Char` cannot tell apart.
    pub fn word(&self) -> String {
        self.key.trim().to_ascii_lowercase()
    }

    /// What this key asks for, or nothing. Exactly one of `event` and `verb`, like [`Action`].
    pub fn kind(&self) -> Option<ActionKind> {
        match (&self.event, &self.verb) {
            (Some(e), None) => Some(ActionKind::Event(e.clone())),
            (None, Some(v)) => Some(ActionKind::Verb(v.clone())),
            _ => None,
        }
    }
}

/// Where the host lays a pane out (`X10`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dock {
    /// The screen it has always been: the pane replaces the conversation.
    #[default]
    Screen,
    /// A column beside the conversation, whose width the host owns.
    Right,
}

/// The pane's counter in the status row: `3 running`, in one of the palette's roles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Badge {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

/// One panel of the overview: a title, a denominator, some summary blocks, and a full view
/// behind its own key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    /// The panel's id - what a finding's `panel` names, and what an action's event says.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The key that toggles this panel's view. One lowercase letter, not one of
    /// [`HOST_KEYS`], unique among the panels.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// What every bar in this panel is a share of - `rows in the tier`, `hosts in the
    /// fleet`. Drawn beside the title, dim, because a bar with no denominator is a number
    /// with no meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<String>,
    /// Drawn on the overview: a few lines, read at a glance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary: Vec<Block>,
    /// Drawn when the panel's key is pressed. Absent means the summary is the view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub view: Vec<Block>,
    /// Why this panel has nothing to draw, and how to fix it. A panel that cannot get its
    /// data says so instead of drawing an empty result: an empty list and a broken look look
    /// identical on a screen and mean opposite things.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<Unavailable>,
}

impl Panel {
    /// What the panel draws when it is opened: its own view, or the summary when it has none.
    pub fn shown(&self) -> &[Block] {
        if self.view.is_empty() {
            &self.summary
        } else {
            &self.view
        }
    }
}

/// A panel that could not get its data: the reason, and the command that fixes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Unavailable {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
}

/// One block of a panel. Six kinds the host draws, and every other kind kept by name.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// `{"lines":[…]}` - the one grid.
    Lines(Vec<Line>),
    /// `{"table":{"columns":[…],"rows":[…]}}`.
    Table(Table),
    /// `{"tree":{"rows":[…]}}` - the same rows, drawn indented by their `children`.
    Tree(Tree),
    /// `{"markdown":"…"}` - itemized: its headings and its list items are stops.
    Markdown(String),
    /// `{"kv":{"rows":[["label","value"],…]}}` - rows that carry no id, so no stop either.
    Kv(Vec<(String, String)>),
    /// `{"stream":{…}}` - messages anchored at the bottom, each entry a stop (`X2`). The host
    /// draws a body through rano's markdown, sticks to the tail and pages older entries at the top.
    Stream(Stream),
    /// A kind this host does not draw, by the name the plugin gave it. Drawn as one muted
    /// line naming the kind, which is the honest thing: the plugin's answer is not wrong, this
    /// host is old.
    Unknown(String),
}

impl Block {
    /// One block out of the JSON a plugin answered. A known kind whose body does not parse
    /// is kept as an unknown of that name - a block this host cannot read is drawn as a line
    /// saying which kind it was, never silently dropped.
    pub fn of(v: Value) -> Block {
        let Value::Object(map) = v else {
            return Block::Unknown(String::new());
        };
        let Some((kind, body)) = map.iter().next() else {
            return Block::Unknown(String::new());
        };
        let parsed = match kind.as_str() {
            "lines" => serde_json::from_value(body.clone()).ok().map(Block::Lines),
            "table" => serde_json::from_value(body.clone()).ok().map(Block::Table),
            "tree" => serde_json::from_value(body.clone()).ok().map(Block::Tree),
            "markdown" => body.as_str().map(|s| Block::Markdown(s.to_string())),
            "kv" => serde_json::from_value(body.clone())
                .ok()
                .map(|k: KvBody| Block::Kv(k.rows)),
            "stream" => serde_json::from_value(body.clone()).ok().map(Block::Stream),
            other => return Block::Unknown(other.to_string()),
        };
        parsed.unwrap_or_else(|| Block::Unknown(kind.clone()))
    }

    /// The block's name as the plugin wrote it - `lines`, `table`, or the kind this host does
    /// not draw.
    pub fn kind(&self) -> &str {
        match self {
            Block::Lines(_) => "lines",
            Block::Table(_) => "table",
            Block::Tree(_) => "tree",
            Block::Markdown(_) => "markdown",
            Block::Kv(_) => "kv",
            Block::Stream(_) => "stream",
            Block::Unknown(k) if k.is_empty() => "unknown",
            Block::Unknown(k) => k,
        }
    }
}

impl Serialize for Block {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        /// `{"kv":{"rows":[[label, value], …]}}` - the shape C3 gives it, which is an array of
        /// pairs rather than an object, because a label may repeat and is not a key.
        #[derive(Serialize)]
        struct KvRef<'a> {
            rows: &'a [(String, String)],
        }
        let mut m = s.serialize_map(Some(1))?;
        match self {
            Block::Lines(l) => m.serialize_entry("lines", l)?,
            Block::Table(t) => m.serialize_entry("table", t)?,
            Block::Tree(t) => m.serialize_entry("tree", t)?,
            Block::Markdown(text) => m.serialize_entry("markdown", text)?,
            Block::Kv(rows) => m.serialize_entry("kv", &KvRef { rows })?,
            Block::Stream(s) => m.serialize_entry("stream", s)?,
            // The key alone: this host kept the kind and never had a body to keep with it, and
            // inventing one would be a fact the plugin did not say.
            Block::Unknown(kind) => {
                m.serialize_entry(if kind.is_empty() { "unknown" } else { kind }, &Value::Null)?
            }
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Block::of(Value::deserialize(d)?))
    }
}

/// `{"table":{"columns":[…],"rows":[…]}}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Row>,
}

/// `{"tree":{"rows":[…]}}` - rows nested by `children`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tree {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Row>,
    /// What to draw when there are no rows - the plugin's own sentence, because *the daemon
    /// has not answered yet* and *this project keeps no such rows* are two different facts and
    /// only the layer knows which one its list is. Empty draws nothing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub empty: String,
}

/// The body of `{"kv":{"rows":[…]}}`, deserialized before it is flattened to pairs.
#[derive(Debug, Clone, Default, Deserialize)]
struct KvBody {
    #[serde(default)]
    rows: Vec<(String, String)>,
}

/// `{"stream":{"entries":[…],"highlight":"…","mentions":{…}}}` - a chat-shaped list anchored
/// at the bottom. It is the one block the host scrolls itself: the tail is where the new entries
/// are, older pages arrive above without moving the reader, and an entry is a stop, so the cursor
/// moves by message rather than by screen row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<StreamEntry>,
    /// What to draw when there are no entries - the layer's own sentence, because *this channel is
    /// empty* and *the window has not arrived* are two different facts and only the layer knows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub empty: String,
    /// The entry the layer wants selected - the message a reply or a thread came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<String>,
    /// Tokens in a body, and how the host draws them: the plugin never edits the markdown it is
    /// handed, it says how a token in it reads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mentions: BTreeMap<String, Mention>,
}

/// One `@token` of a body: the name to show instead of it, and the colour to show it in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    /// What the token reads as - `@s-1793…` drawn as `@fix-room-push`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A hue (`hue:0` to `hue:7`), by index: the theme picks the colour that reads on both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<u8>,
    /// True for the person reading it, which the host draws the one way it draws `you`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub me: bool,
}

/// One entry of a stream: a message, or a divider drawn between two of them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamEntry {
    /// The id - an entry is a stop, and an act on it names this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The header: the speaker in its hue, the title behind the name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub head: Vec<Cell>,
    /// The time as the LAYER prints it (`14:20`, `Mon D`): a guest has no clock, the input has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// The line this one answers, as the layer quotes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    /// The body, markdown. The host renders it with the transcript's own renderer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Blob ids the body draws, by reference: the host holds or fetches the bytes, never the layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pictures: Vec<String>,
    /// The facts on the right, drawn faint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<Cell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// What Enter does on the entry, performed by the host - gated by [`View::validate_for`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter: Option<Act>,
    /// The divider this entry is, when it is one. A divider carries no body: the host draws its
    /// `text` between the message above it and the one below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divider: Option<Divider>,
    /// A divider's word - `today`, `3 new messages`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

impl StreamEntry {
    /// The entry's id, or `""` - the field is absent far more often than not.
    pub fn id(&self) -> &str {
        self.id.as_deref().unwrap_or("")
    }

    /// Whether this entry is a divider rather than a message.
    pub fn is_divider(&self) -> bool {
        self.divider.is_some()
    }
}

/// Which divider an entry draws: the day the messages under it begin on, or the first one the
/// person had not seen when they opened the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Divider {
    Today,
    Unread,
}

/// One row of the grid - the one shape every number goes through, so the columns line up
/// down the frame and across panels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// A share of the panel's denominator, drawn as a bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar: Option<Bar>,
    /// History, each value a share of the same denominator, drawn as a sparkline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spark: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
    /// One of the palette's roles. Absent is `plain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// An id makes the line a stop: the cursor can rest on it, Enter reads its `detail`,
    /// and its `actions` name it. A line with no id is a number and nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

impl Line {
    /// The line's id, or `""` - the field is absent far more often than not, and every reader
    /// here asks the same question of it.
    pub fn id(&self) -> &str {
        self.id.as_deref().unwrap_or("")
    }
}

/// The filled part of a bar, as a share of its panel's denominator.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub frac: f64,
}

/// One row of a table or a tree. A row with an `id` is a stop, like a line.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The cells on the left. A plain string takes the row's role; `{"text", "role"}` says
    /// what one run is regardless of it (`plugin_sdk::pane::Cell`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<Cell>,
    /// The facts on the right, drawn faint and flush right - `generating · 412 rows · 2 heads`.
    /// C3's first draft had none, and a list without a right column is a list whose numbers sit in
    /// the name's way. A cell here may carry a role like any other.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<Cell>,
    /// The faint line(s) UNDER the row - an id, a path, a legend sentence. Not `detail`: that
    /// is prose read whole on Enter, and a row whose whole detail is drawn beneath it is a row
    /// that reads itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub under: Vec<String>,
    /// The number drawn in the list's left field, or nothing for a row that cannot be typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<usize>,
    /// How deep in a FLAT list this row sits - a plugin that answers the rows already walked
    /// (the session list does) says so here, because there are no `children` to walk. A nested
    /// tree leaves it at 0 and the host counts the nesting instead.
    #[serde(default, skip_serializing_if = "crate::pane::is_zero_usize")]
    pub depth: usize,
    /// Whether this row folds, and which way (`plugin_sdk::pane::Fold`). Absent is a row that
    /// said nothing about folding; the host's own fold state (keyed by `id`) wins over whatever
    /// the plugin said, so a fold is instant and survives the next answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Fold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// What Enter does on this row, performed by the host - `switch`, `fold`, `event`. An act
    /// is declared in the layer's `plugin.toml`, so a view that reaches a verb nobody reviewed is
    /// refused by [`View::validate_for`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter: Option<Act>,
    /// A tree's nesting. Empty for a table.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Row>,
}

impl Row {
    /// The row's id, or `""`. See [`Line::id`].
    pub fn id(&self) -> &str {
        self.id.as_deref().unwrap_or("")
    }
}

/// One finding: what tripped (or is missing, or passed), the numbers behind it, what to do
/// and how to check that it worked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The one line the findings list shows.
    #[serde(rename = "what", default, skip_serializing_if = "String::is_empty")]
    pub what: String,
    /// The panel this belongs to, by the panel's id - so a reader knows where to look.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub panel: String,
    /// Required, and deliberately not defaulted: a finding that did not say how bad it is
    /// must not be counted as one that passed.
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The numbers the finding rests on, drawn as label/value rows on Enter. Free-form
    /// JSON on purpose: a finding about an age carries an age, one about a port carries a
    /// port, and the host renders what it is given.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evidence: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
    /// A command or a query the operator can run to check it themselves.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub verify: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

/// How bad a finding is. `Passed` is a finding too: a check that ran and found nothing is a
/// fact worth one line, and a list that holds only failures cannot say what was looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Tripped,
    Missing,
    Passed,
}

impl Severity {
    /// The word the summary line and the findings list use.
    pub fn word(self) -> &'static str {
        match self {
            Severity::Tripped => "tripped",
            Severity::Missing => "missing",
            Severity::Passed => "passed",
        }
    }
}

/// What a key of a stop does. Four, and no more: `look` and `event` call back into the
/// plugin, `say` hands the operator's line to the model, and `verb` asks the HOST to perform one
/// of its own verbs - which is what keeps a pane's actions inside the plugin's capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    /// A line to the session's model, as if the operator had typed it.
    Say(String),
    /// Re-run the pane's look now.
    Look,
    /// Call the pane again with `{"item": <the stop's id>, "action": <this word>}`.
    Event(String),
    /// One of the host's own verbs, on the entry this row names - the same word
    /// [`crate::pane::Act::Verb`] carries on Enter, on a letter instead. The manifest gates it
    /// exactly as it gates `enter` ([`View::validate_for`]).
    Verb(String),
}

/// One key shown in the hint bar for the selected stop.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// One lowercase letter. The host refuses a key that collides (see [`View::validate`]).
    pub key: String,
    /// What the hint bar says about it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub say: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub look: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// The host's verb, on the entry the row names - the letter a row binds for a verb that is
    /// not the row's `enter`, so one row can offer two of the person's verbs. Gated by the manifest
    /// like `enter` is: [`View::validate_for`] refuses a verb the layer did not declare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
}

impl Action {
    /// What this key does, or nothing. Exactly one of `say`, `look`, `event` and `verb` is a
    /// question with one answer; two of them is a key whose meaning nobody can say, and
    /// [`View::validate`] refuses it rather than letting the host pick.
    pub fn kind(&self) -> Option<ActionKind> {
        let mut found = Vec::new();
        if let Some(s) = &self.say {
            found.push(ActionKind::Say(s.clone()));
        }
        if self.look {
            found.push(ActionKind::Look);
        }
        if let Some(e) = &self.event {
            found.push(ActionKind::Event(e.clone()));
        }
        if let Some(v) = &self.verb {
            found.push(ActionKind::Verb(v.clone()));
        }
        match found.len() {
            1 => found.pop(),
            _ => None,
        }
    }

    /// The act as the WIRE spells it - C4's `PaneAction.action`: the key's `event` word, the
    /// host's `verb` word, or `look`. `Say` is not a pane action (the line goes to the session's
    /// model), so it has none.
    pub fn word(&self) -> Option<&str> {
        match (&self.event, &self.verb, self.look, &self.say) {
            (Some(e), None, false, None) => Some(e.as_str()),
            (None, Some(v), false, None) => Some(v.as_str()),
            (None, None, true, None) => Some("look"),
            _ => None,
        }
    }

    /// The letter, when it is one lowercase letter - what the host matches a keypress against.
    pub fn letter(&self) -> Option<char> {
        one_letter(&self.key)
    }
}

/// One lowercase letter, or nothing - the shape every key a pane binds must have.
pub fn one_letter(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if c.is_ascii_lowercase() => Some(c),
        _ => None,
    }
}

/// One letter of EITHER case, lowercased - what a view key (`X8`) may be: with an input the plain
/// letters type and these are live only in cursor mode, so a capital is a key the layer can keep.
fn letter_of_either_case(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

/// The verbs one stop can reach: the act its Enter performs, and the ones its letters carry. Both
/// are doors to the host's own verbs, and [`View::validate_for`] gates them by the same list.
fn asked<'a>(enter: Option<&'a Act>, actions: &'a [Action]) -> impl Iterator<Item = &'a str> + 'a {
    enter
        .map(|a| a.verb())
        .into_iter()
        .chain(actions.iter().filter_map(|a| a.verb.as_deref()))
}

/// Refuse a verb the layer's manifest did not declare, naming where it was asked for and what WAS
/// declared - the sentence a model reads before it edits the manifest.
fn gate_verbs<'a>(
    declared: &[String],
    at: &str,
    asked: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    for verb in asked {
        if declared.iter().any(|d| d == verb) {
            continue;
        }
        return Err(format!(
            "{at} asks the host to `{verb}`, and the layer's `plugin.toml` does not declare that \
             action — its `actions` are [{}]",
            declared.join(", ")
        ));
    }
    Ok(())
}

/// An answer carrying acts (`X7`): what a `command_` hook answers when the line means something the
/// HOST performs - `{"acts":[{"verb":"room_say","args":{…}}]}`. The host performs them in order,
/// and each verb is one the layer's `plugin.toml` declares.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acts: Vec<Acted>,
    /// The sentence the host shows when the layer will not do it - `that channel is not mine`.
    /// A refusal is an answer, not an error: the person is told, and nothing is performed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// One act an answer carries: the host's verb, and the arguments the host performs it with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Acted {
    /// The word `plugin.toml`'s `actions` declares - `room_say`, `room_dm`, `copy`, `open`, `pick`.
    pub verb: String,
    /// The verb's own arguments, free-form: the host knows its verbs' fields, and typing them here
    /// would put every future verb in one enum this crate has to grow with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

impl Answer {
    /// The answer a layer wrote, or the sentence saying why it is not one.
    pub fn parse(v: Value) -> Result<Answer, String> {
        serde_json::from_value(v).map_err(|e| format!("not an answer: {e}"))
    }

    /// Refuse an answer whose act the manifest did not declare. The same list a row's `enter` is
    /// gated by ([`View::validate_for`]): an act in an answer is a second door to the same verbs.
    pub fn validate_for(&self, declared: &[String]) -> Result<(), String> {
        gate_verbs(
            declared,
            "this answer",
            self.acts.iter().map(|a| a.verb.as_str()),
        )
    }
}

/// What a pane asks the host to run before it is asked again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LookSpec {
    /// `{"exec":["prog","arg",…]}` - a program, run by the daemon.
    Exec { exec: Vec<String> },
    /// `{"read":"path"}` - a file's text.
    Read { read: String },
}

impl View {
    /// Refuse a view the host's keys cannot drive. The pane's keys are the host's and are
    /// templated, so a panel's key, an action's key, an action that does two things, an `every_s`
    /// under [`MIN_EVERY_S`] and a finding nobody can name are all refused here.
    pub fn validate_for(&self, declared: &[String]) -> Result<(), String> {
        self.validate()?;
        for p in &self.panels {
            for block in p.summary.iter().chain(p.view.iter()) {
                match block {
                    Block::Tree(t) => {
                        for (_, row) in rows_deep(&t.rows) {
                            gate_verbs(
                                declared,
                                &format!("row `{}`", row.id()),
                                asked(row.enter.as_ref(), &row.actions),
                            )?;
                        }
                    }
                    // A stream entry is a stop with acts like a row, and a letter on it is not a
                    // lesser door than Enter.
                    Block::Stream(s) => {
                        for (i, e) in s.entries.iter().enumerate() {
                            gate_verbs(
                                declared,
                                &format!("stream entry {i}"),
                                asked(e.enter.as_ref(), &e.actions),
                            )?;
                        }
                    }
                    _ => {}
                }
            }
        }
        // The view's own keys reach the same verbs, so they are gated by the same list.
        for k in &self.keys {
            if let Some(verb) = &k.verb {
                gate_verbs(
                    declared,
                    &format!("view key `{}`", k.word()),
                    std::iter::once(verb.as_str()),
                )?;
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut panel_keys: BTreeMap<char, String> = BTreeMap::new();
        for p in &self.panels {
            if p.key.is_empty() && p.view.is_empty() {
                // Nothing behind a key: `Panel::shown` answers the summary, so there is no view
                // for a key to open and none is asked for (C3 keys open views).
                continue;
            }
            let c = one_letter(&p.key).ok_or_else(|| {
                format!(
                    "panel `{}` takes key {:?}: a panel that has a view needs one lowercase letter \
                     to open it",
                    p.id, p.key
                )
            })?;
            if HOST_KEYS.contains(&c) {
                return Err(format!(
                    "panel `{}` takes key `{c}`: {HOST_KEYS:?} are the host's keys",
                    p.id
                ));
            }
            if let Some(other) = panel_keys.insert(c, p.id.clone()) {
                return Err(format!(
                    "panels `{other}` and `{}` both take key `{c}`",
                    p.id
                ));
            }
        }
        // The view's own keys (`X8`): a letter of either case or one of the named chords, never a
        // host chord, never twice, and one meaning each.
        let mut view_words: BTreeSet<String> = BTreeSet::new();
        let mut view_letters: BTreeSet<char> = BTreeSet::new();
        for k in &self.keys {
            let word = k.word();
            if HOST_CHORDS.contains(&word.as_str()) {
                return Err(format!(
                    "view key `{word}` is one of the host's {HOST_CHORDS:?}: it is how the person \
                     leaves, sends or enters the cursor"
                ));
            }
            let letter = letter_of_either_case(&word);
            if letter.is_none() && !NAMED_KEYS.contains(&word.as_str()) {
                return Err(format!(
                    "view key {:?} is not a key: one letter of either case, or one of \
                     {NAMED_KEYS:?}",
                    k.key
                ));
            }
            if !view_words.insert(word.clone()) {
                return Err(format!(
                    "two view keys take `{word}`: the host matches one, and the other never fires"
                ));
            }
            if let Some(c) = letter
                && HOST_KEYS.contains(&c)
            {
                return Err(format!(
                    "view key `{word}` is one of the host's {HOST_KEYS:?}"
                ));
            }
            if let Some(c) = letter {
                if let Some(panel) = panel_keys.get(&c) {
                    return Err(format!("view key `{word}` is panel `{panel}`'s"));
                }
                view_letters.insert(c);
            }
            if k.kind().is_none() {
                return Err(format!(
                    "view key `{word}` must be exactly one of event, verb"
                ));
            }
        }
        if let Some(input) = &self.input
            && input.key.trim().is_empty()
        {
            return Err(
                "input.key is empty: the host keys this pane's history and drafts by it, and has \
                 none to invent"
                    .to_string(),
            );
        }
        if let Some(w) = self.width {
            if self.dock != Some(Dock::Right) {
                return Err(format!(
                    "width is {w} and `dock` is not \"right\": only a docked column has a width \
                     the host honours"
                ));
            }
            if !(MIN_DOCK_W..=MAX_DOCK_W).contains(&w) {
                return Err(format!(
                    "width is {w}: a docked column is {MIN_DOCK_W} to {MAX_DOCK_W} cells"
                ));
            }
        }
        for (pi, p) in self.panels.iter().enumerate() {
            for block in p.summary.iter().chain(p.view.iter()) {
                let Block::Stream(s) = block else { continue };
                for (i, e) in s.entries.iter().enumerate() {
                    let acted = e.enter.is_some() || e.actions.iter().any(|a| a.verb.is_some());
                    if acted && e.id().is_empty() {
                        return Err(format!(
                            "panel {pi}: stream entry {i} asks the host for a verb and has no id \
                             - a verb acts on the entry it names"
                        ));
                    }
                }
                for (token, m) in &s.mentions {
                    if m.hue.is_some_and(|h| h >= letibot_plugin::pane::HUES) {
                        return Err(format!(
                            "panel {pi}: mention `{token}` asks for hue {}: the palette is \
                             `hue:0` to `hue:{}`",
                            m.hue.unwrap_or_default(),
                            letibot_plugin::pane::HUES - 1
                        ));
                    }
                }
            }
        }
        for (what, actions) in self.action_sets() {
            let mut seen: BTreeSet<char> = BTreeSet::new();
            for a in actions {
                let c = a.letter().ok_or_else(|| {
                    format!("{what}: action key {:?} is not one lowercase letter", a.key)
                })?;
                if HOST_KEYS.contains(&c) {
                    return Err(format!(
                        "{what}: action key `{c}` is one of the host's {HOST_KEYS:?}"
                    ));
                }
                if let Some(panel) = panel_keys.get(&c) {
                    return Err(format!("{what}: action key `{c}` is panel `{panel}`'s"));
                }
                if view_letters.contains(&c) {
                    return Err(format!(
                        "{what}: action key `{c}` is a view key's, and one key cannot be both"
                    ));
                }
                if !seen.insert(c) {
                    return Err(format!("{what}: two actions take key `{c}`"));
                }
                if a.kind().is_none() {
                    return Err(format!(
                        "{what}: action `{c}` must be exactly one of say, look, event"
                    ));
                }
            }
        }
        if self.every_s != 0 && self.every_s < MIN_EVERY_S {
            return Err(format!(
                "every_s is {}: 0 (never) or at least {MIN_EVERY_S} seconds",
                self.every_s
            ));
        }
        for (i, f) in self.findings.iter().enumerate() {
            if f.id.is_empty() && f.what.is_empty() {
                return Err(format!("finding {i} has neither an id nor a `what`"));
            }
        }
        Ok(())
    }

    /// Every set of actions one stop carries, with the stop named the way a refusal has to say
    /// it - the panel and the block, or the finding. This is the walk [`View::validate`] reads.
    pub fn action_sets(&self) -> Vec<(String, &[Action])> {
        let mut out = Vec::new();
        for (pi, p) in self.panels.iter().enumerate() {
            let name = if p.id.is_empty() {
                format!("panel {pi}")
            } else {
                format!("panel `{}`", p.id)
            };
            for (bi, block) in p.summary.iter().chain(p.view.iter()).enumerate() {
                let what = format!("{name}, block {bi} ({})", block.kind());
                match block {
                    Block::Lines(lines) => {
                        for (li, l) in lines.iter().enumerate() {
                            out.push((format!("{what}, line {li}"), l.actions.as_slice()));
                        }
                    }
                    Block::Table(t) => {
                        for (ri, r) in t.rows.iter().enumerate() {
                            out.push((format!("{what}, row {ri}"), r.actions.as_slice()));
                        }
                    }
                    Block::Tree(t) => {
                        for (ri, (_, r)) in rows_deep(&t.rows).into_iter().enumerate() {
                            out.push((format!("{what}, row {ri}"), r.actions.as_slice()));
                        }
                    }
                    Block::Stream(s) => {
                        for (ei, e) in s.entries.iter().enumerate() {
                            out.push((format!("{what}, entry {ei}"), e.actions.as_slice()));
                        }
                    }
                    Block::Markdown(_) | Block::Kv(_) | Block::Unknown(_) => {}
                }
            }
        }
        for (fi, f) in self.findings.iter().enumerate() {
            let name = if f.id.is_empty() {
                format!("finding {fi}")
            } else {
                format!("finding `{}`", f.id)
            };
            out.push((name, f.actions.as_slice()));
        }
        out
    }

    /// How many findings there are at each severity - the summary line under the title.
    pub fn counted(&self) -> (usize, usize, usize) {
        let mut out = (0, 0, 0);
        for f in &self.findings {
            match f.severity {
                Severity::Tripped => out.0 += 1,
                Severity::Missing => out.1 += 1,
                Severity::Passed => out.2 += 1,
            }
        }
        out
    }

    /// The panel whose key is `c`, if any.
    pub fn panel_of_key(&self, c: char) -> Option<usize> {
        self.panels
            .iter()
            .position(|p| one_letter(&p.key) == Some(c))
    }
}

/// A tree's rows in the order they are drawn: each row, then its children, depth first  -
/// with its depth. One walk, so the drawing and the stop the cursor lands on cannot disagree
/// about which row is the fifth.
pub fn rows_deep(rows: &[Row]) -> Vec<(usize, &Row)> {
    fn walk<'a>(rows: &'a [Row], depth: usize, out: &mut Vec<(usize, &'a Row)>) {
        for r in rows {
            out.push((depth, r));
            walk(&r.children, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(rows, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(json: &str) -> View {
        serde_json::from_str(json).expect("the fixture parses")
    }

    /// The whole document, field for field - every piece of C3 in one view, parsed and
    /// written back out to the same JSON. A round trip is the cheapest proof that the types
    /// carry what the contract says and nothing else.
    #[test]
    fn a_whole_view_round_trips() {
        let json = r##"{
            "title": "stroppy-conformity · v0.2.0",
            "badge": {"text": "3 running", "role": "attention"},
            "panels": [
                {"id":"tiers","key":"t","title":"tiers","denominator":"rows in the tier",
                 "summary":[{"lines":[
                    {"label":"tier 1","value":"14/22","bar":{"frac":0.64},
                     "spark":[0.1,0.4,0.9],"tail":"3 in progress","role":"working",
                     "id":"tier1","detail":"the tier",
                     "actions":[{"key":"s","label":"start","say":"start tier 1"}]}
                 ]}],
                 "view":[{"table":{"columns":["item","status"],"rows":[
                    {"id":"tier1/jstests","cells":["Mongo jstests","not started"],"role":"muted",
                     "detail":"…","actions":[{"key":"e","label":"expand","event":"expand"}],
                     "children":[{"id":"tier1/jstests/a","cells":["a","done"]}]}
                 ]}}]},
                {"id":"hosts","key":"h","title":"hosts",
                 "summary":[{"kv":{"rows":[["lab2x1","busy"],["lab2x2","idle"]]}},
                            {"markdown":"# hosts\n- lab2x1 is busy"},
                            {"tree":{"rows":[{"id":"fleet","cells":["fleet"]}]}}],
                 "unavailable":{"why":"cannot connect to lab2x1","fix":"add the lab2x1 key to the agent"}}
            ],
            "findings":[{"id":"stale-status","what":"status file is 3 days old","panel":"tiers",
             "severity":"tripped","detail":"…prose…","evidence":{"updated":"2026-10-22T…","age_h":74},
             "fix":"write the status file","verify":"cat .letibot/status.json",
             "actions":[{"key":"r","label":"refresh","look":true}]}],
            "legend":{"tier":"a row of the plan"},
            "look":{"exec":["cat","status.json"]},
            "every_s": 30,
            "state": {"seen": 1}
        }"##;
        let v = parsed(json);
        assert_eq!(v.title, "stroppy-conformity · v0.2.0");
        assert_eq!(v.badge.as_ref().unwrap().role, Some(Role::Attention));
        assert_eq!(v.panels.len(), 2);
        assert_eq!(v.panels[0].shown()[0].kind(), "table");
        assert_eq!(v.panels[1].shown()[0].kind(), "kv");
        assert_eq!(
            v.findings[0].actions[0].kind(),
            Some(ActionKind::Look),
            "look is a kind of its own"
        );
        assert_eq!(v.findings[0].evidence["age_h"], serde_json::json!(74));
        assert_eq!(v.counted(), (1, 0, 0));
        assert_eq!(v.panel_of_key('t'), Some(0));
        assert!(v.validate().is_ok(), "{:?}", v.validate());
        // Out and back: the document is the same JSON the plugin wrote.
        let out = serde_json::to_value(&v).unwrap();
        let want: Value = serde_json::from_str(json).unwrap();
        assert_eq!(out, want);
        assert_eq!(serde_json::from_value::<View>(out).unwrap(), v);
    }

    /// Each piece on its own, from the smallest JSON that carries it - so a field that
    /// only round-trips inside a whole panel is caught.
    #[test]
    fn each_piece_round_trips_from_c3s_json() {
        let line_json = r#"{"label":"tier 1","value":"14/22","bar":{"frac":0.64},
            "spark":[0.1,0.4],"tail":"3 in progress","role":"success",
            "id":"tier1","detail":"…","actions":[{"key":"s","label":"start","say":"go"}]}"#;
        let line: Line = serde_json::from_str(line_json).unwrap();
        assert_eq!(line.bar.as_ref().unwrap().frac, 0.64);
        assert_eq!(line.role, Some(Role::Success));
        assert_eq!(line.id(), "tier1");
        assert_eq!(
            serde_json::to_value(&line).unwrap(),
            serde_json::from_str::<Value>(line_json).unwrap()
        );

        let row_json = r#"{"id":"tier1/a","cells":["a","done"],"role":"failure","detail":"…",
            "actions":[{"key":"x","label":"expand","event":"expand"}],
            "children":[{"id":"tier1/a/b","cells":["b"]}]}"#;
        let row: Row = serde_json::from_str(row_json).unwrap();
        let deep = rows_deep(std::slice::from_ref(&row));
        assert_eq!(deep.len(), 2, "a tree walk sees the child");
        assert_eq!(deep[1].0, 1, "and its depth");
        assert_eq!(
            serde_json::from_value::<Row>(serde_json::to_value(&row).unwrap()).unwrap(),
            row
        );

        let finding_json = r#"{"id":"f","what":"x","panel":"p","severity":"missing","detail":"d",
            "evidence":{"n":1},"fix":"fx","verify":"v",
            "actions":[{"key":"s","label":"start","say":"go"}]}"#;
        let f: Finding = serde_json::from_str(finding_json).unwrap();
        assert_eq!(f.severity, Severity::Missing);
        assert_eq!(
            serde_json::from_value::<Finding>(serde_json::to_value(&f).unwrap()).unwrap(),
            f
        );

        for (json, kind) in [
            (r#"{"lines":[]}"#, "lines"),
            (
                r#"{"table":{"columns":["a"],"rows":[{"cells":["b"]}]}}"#,
                "table",
            ),
            (r#"{"tree":{"rows":[{"cells":["b"]}]}}"#, "tree"),
            (r##"{"markdown":"# a"}"##, "markdown"),
            (r#"{"kv":{"rows":[["a","b"]]}}"#, "kv"),
        ] {
            let b: Block = serde_json::from_str(json).unwrap();
            assert_eq!(b.kind(), kind);
            assert_eq!(
                serde_json::to_value(&b).unwrap(),
                serde_json::from_str::<Value>(json).unwrap()
            );
        }

        let look: LookSpec = serde_json::from_str(r#"{"read":"status.json"}"#).unwrap();
        assert_eq!(
            look,
            LookSpec::Read {
                read: "status.json".into()
            }
        );
        assert_eq!(
            serde_json::to_value(&look).unwrap(),
            serde_json::json!({"read":"status.json"})
        );
    }

    /// A block kind this host does not draw is kept by name - the plugin's answer is not
    /// wrong and the pane is not empty: the line it draws says which kind is missing. And an
    /// unknown FIELD anywhere is ignored, by every type.
    #[test]
    fn an_unknown_block_kind_keeps_its_name() {
        let b: Block = serde_json::from_str(r#"{"progress":{"done":41,"of":96}}"#).unwrap();
        assert_eq!(b, Block::Unknown("progress".into()));
        assert_eq!(b.kind(), "progress");
        let v = parsed(r#"{"panels":[{"id":"p","key":"p","summary":[{"progress":{}}]}]}"#);
        assert!(v.validate().is_ok(), "an old host is not a bad plugin");
        let v =
            parsed(r#"{"title":"t","nonsense":1,"panels":[{"id":"p","key":"p","extra":true}]}"#);
        assert_eq!(v.title, "t");
    }

    /// Every refusal, one test each - these are the host's keys and the operator's
    /// expectations, and a view that breaks one is a pane whose keys do something nobody chose.
    #[test]
    fn validate_refuses_the_keys_the_host_owns() {
        let say = |key: &str| format!(r#"{{"key":"{key}","label":"l","say":"s"}}"#);
        let one_line = |actions: String| {
            format!(
                r#"{{"panels":[{{"id":"p","key":"p","summary":[{{"lines":[
                    {{"label":"a","id":"a","actions":[{actions}]}}]}}]}}]}}"#
            )
        };
        // A host key on an action.
        for k in HOST_KEYS {
            let v = parsed(&one_line(say(&k.to_string())));
            let why = v.validate().unwrap_err();
            assert!(why.contains("host"), "{k}: {why}");
        }
        // A panel key that is one of the host's.
        let v = parsed(r#"{"panels":[{"id":"p","key":"f"}]}"#);
        assert!(v.validate().unwrap_err().contains("host"));
        // A panel key that is not one letter.
        let v = parsed(r#"{"panels":[{"id":"p","key":"esc"}]}"#);
        assert!(v.validate().unwrap_err().contains("one lowercase letter"));
        // Two panels on one key.
        let v = parsed(r#"{"panels":[{"id":"a","key":"t"},{"id":"b","key":"t"}]}"#);
        assert!(v.validate().unwrap_err().contains("both take key `t`"));
        // An action that takes a panel's key.
        let v = parsed(
            r#"{"panels":[{"id":"p","key":"t","summary":[{"lines":[
                {"label":"a","id":"a","actions":[{"key":"t","say":"go"}]}]}]}]}"#,
        );
        assert!(v.validate().unwrap_err().contains("is panel `p`'s"));
        // Two actions of one stop on one key.
        let v = parsed(&one_line(format!("{},{}", say("s"), say("s"))));
        assert!(
            v.validate()
                .unwrap_err()
                .contains("two actions take key `s`")
        );
        // The same key on two DIFFERENT stops is fine: one stop is selected at a time.
        let v = parsed(&format!(
            r#"{{"panels":[{{"id":"p","key":"p","summary":[{{"lines":[
                {{"label":"a","id":"a","actions":[{}]}},
                {{"label":"b","id":"b","actions":[{}]}}]}}]}}]}}"#,
            say("s"),
            say("s")
        ));
        assert!(v.validate().is_ok(), "{:?}", v.validate());
        // An action that does two things, or none.
        let v = parsed(
            r#"{"panels":[{"id":"p","key":"p","summary":[{"lines":[
                {"label":"a","id":"a","actions":[{"key":"s","say":"go","look":true}]}]}]}]}"#,
        );
        assert!(v.validate().unwrap_err().contains("exactly one"));
        let v = parsed(
            r#"{"panels":[{"id":"p","key":"p","summary":[{"lines":[
                {"label":"a","id":"a","actions":[{"key":"s","label":"nothing"}]}]}]}]}"#,
        );
        assert!(v.validate().unwrap_err().contains("exactly one"));
        // A finding's action need not collide with a line's: different stops.
        let v = parsed(
            r#"{"panels":[{"id":"p","key":"p","summary":[{"lines":[
                {"label":"a","id":"a","actions":[{"key":"s","say":"go"}]}]}]}],
                "findings":[{"id":"f","what":"w","severity":"passed",
                "actions":[{"key":"s","say":"go"}]}]}"#,
        );
        assert!(v.validate().is_ok(), "{:?}", v.validate());
    }

    /// The manifest gates both doors to the host's verbs - the row's `enter` and its letters.
    /// A layer could otherwise reach a verb no reviewer read by writing it one key further along.
    #[test]
    fn validate_for_gates_the_rows_enter_and_its_letters_alike() {
        let row = |enter: Value, actions: Value| -> View {
            let mut r = serde_json::json!({"id":"i12","cells":["#12"],"actions":actions});
            if !enter.is_null() {
                r["enter"] = enter;
            }
            let view = serde_json::json!({"panels":[{
                "id":"p","key":"p",
                "summary":[{"tree":{"rows":[r]}}]
            }]});
            serde_json::from_value(view).expect("the fixture is a view")
        };
        let declared = vec!["claim".to_string(), "abandon".to_string()];

        // Both declared: the view stands.
        let both = row(
            serde_json::json!({"verb":"claim","id":"i12"}),
            serde_json::json!([{"key":"x","label":"mark abandoned","verb":"abandon"}]),
        );
        assert!(both.validate().is_ok(), "{:?}", both.validate());
        assert!(both.validate_for(&declared).is_ok());
        assert_eq!(
            both.panels[0].summary.len(),
            1,
            "one block, so the row is the fixture's subject"
        );

        // The letter reaches a verb nobody declared - refused, and the sentence names it.
        let undeclared_action = row(
            serde_json::json!({"verb":"claim","id":"i12"}),
            serde_json::json!([{"key":"x","label":"rule","verb":"rule"}]),
        );
        let why = undeclared_action.validate_for(&declared).unwrap_err();
        assert!(why.contains("`rule`"), "{why}");
        assert!(why.contains("i12"), "the row it is on: {why}");
        assert!(
            why.contains("claim, abandon"),
            "and what was declared: {why}"
        );

        // And the row's own `enter`, as before.
        let undeclared_enter = row(
            serde_json::json!({"verb":"veto","id":"i12"}),
            serde_json::json!([{"key":"x","label":"mark abandoned","verb":"abandon"}]),
        );
        assert!(undeclared_enter.validate_for(&declared).is_err());

        // An `event` letter is a call back into the LAYER, not the host's verb, so it needs no
        // declaration - the rule `plugins/builtin/irc` writes down.
        let event = row(
            Value::Null,
            serde_json::json!([{"key":"w","label":"wider","event":"wider"}]),
        );
        assert!(event.validate_for(&[]).is_ok());
    }

    /// A look faster than [`MIN_EVERY_S`] is refused, and 0 is the ordinary "never".
    #[test]
    fn validate_refuses_a_look_that_never_stops_asking() {
        assert!(parsed(r#"{"every_s":0}"#).validate().is_ok());
        assert!(parsed(r#"{"every_s":5}"#).validate().is_ok());
        assert!(parsed(r#"{"every_s":1}"#).validate().is_err());
    }

    /// The summary line's counts, and the words beside them.
    #[test]
    fn findings_are_counted_by_severity() {
        let v = parsed(
            r#"{"findings":[
                {"id":"a","severity":"tripped"},{"id":"b","severity":"tripped"},
                {"id":"c","severity":"missing"},{"id":"d","severity":"passed"}]}"#,
        );
        assert_eq!(v.counted(), (2, 1, 1));
        assert_eq!(
            [
                Severity::Tripped.word(),
                Severity::Missing.word(),
                Severity::Passed.word()
            ],
            ["tripped", "missing", "passed"]
        );
    }

    /// A stream block as a chat layer writes it: two dividers, messages with every field, and the
    /// mentions map that colours a token without the layer editing its own markdown.
    #[test]
    fn a_stream_block_round_trips() {
        let json = r##"{"stream":{
            "entries":[
                {"divider":"today","text":"Today"},
                {"id":"m-41","head":[{"text":"operator","role":"hue:2"},"op"],
                 "time":"14:20","quote":"the line above","body":"**ping** @s-1793…",
                 "pictures":["ab12"],"facts":["edited"],"role":"plain",
                 "actions":[{"key":"r","label":"reply","event":"reply"}],
                 "enter":{"event":"thread"}},
                {"divider":"unread","text":"3 new messages"},
                {"id":"m-42","head":["fix-room-push"],"body":"on it"}
            ],
            "highlight":"m-42",
            "mentions":{"@s-1793…":{"text":"@fix-room-push","hue":3},"@operator":{"me":true}}
        }}"##;
        let block: Block = serde_json::from_str(json).expect("a stream parses");
        assert_eq!(block.kind(), "stream");
        let Block::Stream(s) = &block else {
            panic!("a stream block, not {:?}", block.kind())
        };
        assert_eq!(s.entries.len(), 4);
        assert!(s.entries[0].is_divider());
        assert_eq!(s.entries[0].divider, Some(Divider::Today));
        assert_eq!(s.entries[0].text, "Today");
        assert_eq!(s.entries[2].divider, Some(Divider::Unread));
        let m = &s.entries[1];
        assert_eq!(m.id(), "m-41");
        assert_eq!(m.head[0].role, Some(Role::Hue(2)), "a speaker in its hue");
        assert_eq!(m.time.as_deref(), Some("14:20"));
        assert_eq!(m.pictures, vec!["ab12".to_string()]);
        assert_eq!(m.actions[0].kind(), Some(ActionKind::Event("reply".into())));
        assert_eq!(m.enter.as_ref().map(Act::verb), Some("event"));
        assert_eq!(s.highlight.as_deref(), Some("m-42"));
        assert_eq!(s.mentions["@s-1793…"].hue, Some(3));
        assert!(s.mentions["@operator"].me);
        // Out and back: the block is the JSON the plugin wrote.
        assert_eq!(
            serde_json::to_value(&block).unwrap(),
            serde_json::from_str::<Value>(json).unwrap()
        );
        // An empty stream says why it is empty, and is written the same way round.
        let empty: Block = serde_json::from_str(r#"{"stream":{"empty":"nothing said yet"}}"#)
            .expect("an empty stream parses");
        assert_eq!(empty.kind(), "stream");
        assert_eq!(
            serde_json::to_value(&empty).unwrap(),
            serde_json::json!({"stream": {"empty": "nothing said yet"}})
        );
    }

    /// `stream` is additive: a head that predates it knows the five kinds and keeps every other one
    /// by name, so the document still parses, still validates, and draws one muted line naming the
    /// kind instead of an empty panel.
    #[test]
    fn an_old_head_draws_a_stream_as_its_kind() {
        /// The old head's table, spelled out because that build cannot be linked in here: the five
        /// kinds it drew, and every other kind kept by name.
        fn as_an_old_head_draws_it(v: &Value) -> Block {
            let kind = v
                .as_object()
                .and_then(|m| m.keys().next().cloned())
                .unwrap_or_default();
            match Block::of(v.clone()) {
                b @ (Block::Lines(_)
                | Block::Table(_)
                | Block::Tree(_)
                | Block::Markdown(_)
                | Block::Kv(_)) => b,
                _ => Block::Unknown(kind),
            }
        }
        let doc = serde_json::json!({"panels":[{"id":"chat","key":"c","summary":[
            {"stream":{"entries":[{"id":"m-1","body":"hello"}]}}
        ]}]});
        // The old head keeps the kind by name and draws its one muted line.
        let old = as_an_old_head_draws_it(&doc["panels"][0]["summary"][0]);
        assert_eq!(old, Block::Unknown("stream".into()));
        assert_eq!(old.kind(), "stream", "and the line names it");

        // This head parses it, and both heads read the document as valid: an old host is not a bad
        // plugin, and the block's own kind is the same word either way.
        let v: View = serde_json::from_value(doc).expect("the document parses");
        let Block::Stream(s) = &v.panels[0].summary[0] else {
            panic!("this head draws the stream")
        };
        assert_eq!(s.entries[0].id(), "m-1");
        assert_eq!(v.panels[0].summary[0].kind(), "stream");
        assert!(v.validate().is_ok(), "{:?}", v.validate());
        assert!(v.validate_for(&[]).is_ok(), "it performs nothing by itself");
    }

    /// A view key (`X8`) is bound once: two keys spelled the same are one key the host matches and
    /// one that never fires.
    #[test]
    fn a_key_bound_twice_is_refused() {
        let two = |first: &str, second: &str| {
            parsed(&format!(
                r#"{{"input":{{"key":"project/letibot"}},"keys":[
                    {{"key":"{first}","label":"reply","event":"reply"}},
                    {{"key":"{second}","label":"again","event":"reply"}}]}}"#
            ))
        };
        let why = two("R", "r").validate().unwrap_err();
        assert!(why.contains("two view keys take `r`"), "{why}");
        assert!(two("ctrl-k", "ctrl-k").validate().is_err());

        // Two different keys stand, of either case and from the named set.
        let v = two("R", "ctrl-k");
        assert!(v.validate().is_ok(), "{:?}", v.validate());
        assert_eq!(v.keys[0].kind(), Some(ActionKind::Event("reply".into())));

        // And a key with two meanings, or none, is refused for the same reason an action is.
        let both = parsed(r#"{"keys":[{"key":"R","event":"reply","verb":"room_say"}]}"#);
        assert!(both.validate().unwrap_err().contains("exactly one"));
        let none = parsed(r#"{"keys":[{"key":"R","label":"nothing"}]}"#);
        assert!(none.validate().unwrap_err().contains("exactly one"));
        // A key that is not a key at all - the arrows are not words in this vocabulary.
        let word = parsed(r#"{"keys":[{"key":"pgup","event":"up"}]}"#);
        assert!(word.validate().unwrap_err().contains("not a key"));
    }

    /// The chords the host owns are the host's: a view cannot take the key the person leaves, sends
    /// or enters the cursor with.
    #[test]
    fn a_view_key_on_a_host_chord_is_refused() {
        for chord in HOST_CHORDS {
            let v = parsed(&format!(
                r#"{{"keys":[{{"key":"{chord}","label":"x","event":"x"}}]}}"#
            ));
            let why = v.validate().unwrap_err();
            assert!(why.contains("host"), "{chord}: {why}");
            assert!(why.contains(chord), "{chord}: {why}");
        }
        // `f` and `l` stay the host's too, as panel keys always were.
        let v = parsed(r#"{"keys":[{"key":"f","event":"x"}]}"#);
        assert!(v.validate().is_err());
        // The named set is the layer's, and a panel's letter is not.
        assert!(
            parsed(r#"{"keys":[{"key":"home","event":"top"}]}"#)
                .validate()
                .is_ok()
        );
        let panel = parsed(
            r#"{"panels":[{"id":"p","key":"t","summary":[{"lines":[]}]}],
                "keys":[{"key":"t","event":"x"}]}"#,
        );
        assert!(panel.validate().unwrap_err().contains("panel `p`'s"));
    }

    /// An answer carries acts (`X7`), and each is one the manifest declares - the same list a row's
    /// `enter` is gated by, because it is a second door to the same verbs.
    #[test]
    fn an_undeclared_act_in_an_answer_is_refused() {
        let answer = Answer::parse(serde_json::json!({
            "acts": [
                {"verb": "room_say", "args": {"room": "host", "text": "ping"}},
                {"verb": "shell", "args": {"cmd": "rm -rf /"}}
            ]
        }))
        .expect("an answer parses");
        let declared = vec!["room_say".to_string(), "copy".to_string()];
        let why = answer.validate_for(&declared).unwrap_err();
        assert!(why.contains("`shell`"), "the act is named: {why}");
        assert!(
            why.contains("room_say, copy"),
            "and what was declared: {why}"
        );

        // The declared ones stand, arguments and all.
        let ok = Answer::parse(serde_json::json!(
            {"acts":[{"verb":"room_say","args":{"room":"host","text":"ping"}}]}
        ))
        .expect("an answer parses");
        assert!(ok.validate_for(&declared).is_ok());
        assert_eq!(ok.acts[0].args.as_ref().unwrap()["text"], "ping");

        // A refusal is an answer too: it performs nothing, so it declares nothing.
        let refusal = Answer::parse(serde_json::json!(
            {"refusal":"that channel is not mine"}
        ))
        .expect("a refusal parses");
        assert!(refusal.acts.is_empty());
        assert!(refusal.validate_for(&[]).is_ok());
    }

    /// A document written before H0 is written back out BYTE for byte: the fields it had, in the
    /// order it had them, and none of the new ones. That is the whole promise of an additive
    /// package - every pane document, and every layer that wrote one, parses and re-serialises
    /// unchanged.
    #[test]
    fn an_old_pane_document_round_trips_byte_identical() {
        let doc = concat!(
            r##"{"title":"tiers","badge":{"text":"2 waiting","role":"attention"},"##,
            r##""panels":[{"id":"p","key":"t","title":"tiers","denominator":"rows","##,
            r##""summary":[{"lines":[{"label":"one","value":"1/2","bar":{"frac":0.5},"##,
            r##""tail":"up","role":"success","id":"one","detail":"d","##,
            r##""actions":[{"key":"s","label":"start","say":"go"}]}]}],"##,
            r##""view":[{"table":{"columns":["a"],"rows":[{"id":"r1","cells":["x"],"##,
            r##""facts":["1"],"under":["u"],"number":1,"open":true,"role":"muted","##,
            r##""detail":"d","actions":[{"key":"e","label":"go","event":"go"}],"##,
            r##""enter":{"switch":"s-1"}}]}}],"##,
            r##""unavailable":{"why":"w","fix":"f"}}],"##,
            r##""findings":[{"id":"f","what":"w","panel":"p","severity":"tripped","##,
            r##""detail":"d","evidence":{"n":1},"fix":"x","verify":"y","##,
            r##""actions":[{"key":"r","look":true}]}],"##,
            r##""legend":{"a":"b"},"look":{"read":"s.json"},"every_s":30,"state":{"seen":1}}"##
        );
        let v: View = serde_json::from_str(doc).expect("an old document parses");
        assert_eq!(v.title, "tiers");
        assert!(v.input.is_none() && v.keys.is_empty() && v.dock.is_none() && v.width.is_none());
        assert!(v.validate().is_ok(), "{:?}", v.validate());
        assert_eq!(
            serde_json::to_string(&v).expect("it serialises"),
            doc,
            "the document is the same bytes"
        );
    }

    /// One document using every field H0 added, accepted as a whole - and each of them refused
    /// where the host cannot honour it.
    #[test]
    fn a_document_using_each_new_field_validates() {
        let doc = serde_json::json!({
            "title": "project/letibot",
            "input": {"key": "project/letibot", "placeholder": "say something",
                      "multiline": true, "footer": "enter sends, alt-enter breaks the line"},
            "keys": [{"key": "T", "label": "reply", "event": "reply"},
                     {"key": "ctrl-k", "label": "copy", "verb": "copy"}],
            "dock": "right",
            "width": 60,
            "panels": [{"id": "chat", "key": "c", "summary": [{"stream": {
                "empty": "nothing said yet",
                "highlight": "m-42",
                "mentions": {"@s-1793…": {"text": "@fix-room-push", "hue": 3}},
                "entries": [
                    {"divider": "unread", "text": "3 new"},
                    {"id": "m-42", "head": [{"text": "operator", "role": "hue:2"}],
                     "time": "14:20", "body": "hi", "pictures": ["ab12"],
                     "facts": ["edited"],
                     "actions": [{"key": "r", "label": "reply", "verb": "room_say"}],
                     "enter": {"verb": "room_read", "id": "m-42"}}
                ]}}]}]
        });
        let v: View = serde_json::from_value(doc.clone()).expect("every new field parses");
        let input = v.input.as_ref().expect("an input");
        assert_eq!(input.key, "project/letibot");
        assert!(input.multiline);
        assert_eq!(v.dock, Some(Dock::Right));
        assert_eq!(v.width, Some(60));
        assert_eq!(v.keys.len(), 2);
        assert_eq!(
            v.keys[1].kind(),
            Some(ActionKind::Verb("copy".into())),
            "a view key reaches a host verb"
        );
        assert_eq!(v.panels[0].summary[0].kind(), "stream");
        // Written back as it came, and accepted for the acts its manifest declares.
        assert_eq!(serde_json::to_value(&v).unwrap(), doc);
        let declared = vec![
            "room_say".to_string(),
            "room_read".to_string(),
            "copy".to_string(),
        ];
        assert!(v.validate().is_ok(), "{:?}", v.validate());
        assert!(
            v.validate_for(&declared).is_ok(),
            "{:?}",
            v.validate_for(&declared)
        );

        // The same stream entry with an undeclared verb is refused - by the entry, not the row walk.
        let why = v.validate_for(&["copy".to_string()]).unwrap_err();
        assert!(why.contains("stream entry 1"), "{why}");
        assert!(why.contains("`room_read`"), "{why}");
        // And a view key's verb is gated too.
        let keys_only = parsed(r#"{"keys":[{"key":"R","verb":"room_say"}]}"#);
        let why = keys_only.validate_for(&[]).unwrap_err();
        assert!(why.contains("view key `r`"), "{why}");

        // A panel's letter in a view key, a dock width the host would not honour, a mention in a hue
        // the palette has not got, and a verb on an entry with no id: each refused, in its own words.
        let wide = parsed(r#"{"dock":"right","width":400}"#);
        assert!(wide.validate().unwrap_err().contains("20 to 200"));
        let undocked = parsed(r#"{"width":60}"#);
        assert!(undocked.validate().unwrap_err().contains("not \"right\""));
        let hue = parsed(
            r##"{"panels":[{"id":"c","key":"c","summary":[{"stream":{
                "mentions":{"@x":{"hue":9}}}}]}]}"##,
        );
        assert!(hue.validate().unwrap_err().contains("hue:0"));
        let anonymous = parsed(
            r#"{"panels":[{"id":"c","key":"c","summary":[{"stream":{
                "entries":[{"body":"hi","enter":{"verb":"room_say","id":"m-1"}}]}}]}]}"#,
        );
        assert!(anonymous.validate().unwrap_err().contains("no id"));
    }
}
