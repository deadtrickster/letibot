//! What the conversation shows: the verbosity profiles, the per-kind switches and levels, and the
//! visibility a frame is drawn under (`/verbosity`).

use super::*;

/// How much of the stream reaches the transcript: four registers ordered by loudness, plus
/// `Conversation` at the bottom, which is a scope and not a register - the conversation and nothing
/// the head did to produce it - there because it shows the least and is cycled to first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// The conversation alone: the operator's messages and the model's answers.
    /// Warnings, decision cards and the operator's own act are never hidden - hiding one would
    /// revise what was already read or make a gate card unanswerable - and nothing is dropped.
    Conversation,
    /// Assistant text and tool outcomes only.
    Terse,
    /// Plus reasoning.
    Normal,
    /// Plus head arrivals and who issued which command. A warning is not on this ladder: the daemon
    /// chose to interrupt with it, so a level that hid it would make the head the thing deciding the
    /// operator should not have seen it.
    Loud,
}

impl Verbosity {
    /// Every rung, in the order the ladder climbs: the one list the card's seeding, the typed name,
    /// the cycle and `next()` all read. A second list is a second answer, and the two would drift.
    pub const ALL: [Verbosity; 4] = [
        Verbosity::Conversation,
        Verbosity::Terse,
        Verbosity::Normal,
        Verbosity::Loud,
    ];

    pub fn next(self) -> Verbosity {
        let at = Self::ALL.iter().position(|r| *r == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }

    /// The rung a typed word names, if it names one.
    pub fn parse(typed: &str) -> Option<Verbosity> {
        let t = typed.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|r| r.as_str() == t)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Verbosity::Conversation => "conversation",
            Verbosity::Terse => "terse",
            Verbosity::Normal => "normal",
            Verbosity::Loud => "loud",
        }
    }

    /// Whether the rows the head made are drawn at all. A predicate rather than a comparison at every
    /// call site: the question asked is whether this rung is a scope, not whether it is that value,
    /// so a second scope-shaped rung is one edit here.
    pub fn hides_the_working(self) -> bool {
        matches!(self, Verbosity::Conversation)
    }

    /// What a row of the head's own making needs to be drawn under this rung: the conversation is
    /// `User` and `Assistant`, and everything else is evidence about it. The exception is a row the
    /// person at the keyboard acted on, which is the conversation too.
    pub fn keeps(self, item: &letibot_transcript::TranscriptItem) -> bool {
        use letibot_transcript::TranscriptItem as T;
        if !self.hides_the_working() {
            return true;
        }
        // An act of the person at the keyboard is never hidden by verbosity: this ladder governs how
        // much of the MODEL's working you see, and it must not hide what YOU did. The row is still
        // drawn as a tool row - its header, its `+N lines` - so the fold stays the operator's.
        if operator_act(item) {
            return true;
        }
        // A delivery is the conversation too: the session being told what happened between turns is
        // not the model's working, so no rung may hide it.
        matches!(
            item,
            T::User { .. } | T::Assistant { .. } | T::Notice { .. }
        )
    }
}

/// One of the things that can be shown or not. `Show::ALL` is the only place one is added, since the
/// row filter, the card, `/verbosity`, the parse and the key dispatch all walk it. Warnings, decision
/// cards, liveness and the operator's own act are not on it - they are not a reader's to turn off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    /// The cards that say what the head changed: an `edit`/`write` call and its diff. Off at every
    /// rung of the ladder and on at `read-edits`, which is a set and not a rung.
    Edits,
    /// Tool calls, their outcomes and their payloads.
    Tools,
    /// The model's reasoning.
    Thinking,
    /// `system` rows, who attached, and who issued which command.
    System,
    /// The model's raw `<function=…>` markup.
    RawCalls,
}

impl Show {
    /// **Every switch, in the order the card and the refusal list them** - R38's one list, one
    /// rung down. The switch names are the words `head.toml` and `/verbosity` take, so a reader
    /// who read the name once can spell it everywhere.
    pub const ALL: [Show; 5] = [
        Show::Edits,
        Show::Tools,
        Show::Thinking,
        Show::System,
        Show::RawCalls,
    ];

    /// The word `/verbosity` takes and a refusal names.
    pub fn name(self) -> &'static str {
        match self {
            Show::Edits => "edits",
            Show::Tools => "tools",
            Show::Thinking => "thinking",
            Show::System => "system",
            Show::RawCalls => "raw-calls",
        }
    }

    /// What it covers, in the reader's terms. `tools`, `thinking` and `system` are the LADDER's
    /// switches, drawn by the rung; `edits` and `raw-calls` are this list's own and honoured switch by
    /// switch, which is why `read-edits` is a drawing and not a fifth row filter.
    pub fn covers(self) -> &'static str {
        match self {
            Show::Edits => {
                "the cards that say what the head changed — an edit or write call, \
                            with its diff"
            }
            Show::Tools => "tool calls, their outcomes and their payloads",
            Show::Thinking => "the model's reasoning",
            Show::System => "who attached, and who issued which command",
            Show::RawCalls => "the model's raw <function=…> markup",
        }
    }

    /// The chord as the pair that cannot drift: the name a seam and the hint bar spell, and the key
    /// the terminal sends. One entry for both, because a name in one table and a key in another is how
    /// a chord is advertised and does nothing. `ctrl-x` for the raw markup is mnemonic and unbound.
    pub fn chord(self) -> Option<(&'static str, Key)> {
        match self {
            Show::Thinking => Some(("ctrl-r", Key::CtrlR)),
            Show::RawCalls => Some(("ctrl-x", Key::CtrlX)),
            Show::Edits | Show::Tools | Show::System => None,
        }
    }

    /// The levels this switch holds - what `/verbosity SWITCH=LEVEL` takes for it. `edits`, `system`
    /// and `raw-calls` have no body to fold, so they are drawn or not; `tools` and `thinking` have
    /// bodies, which is what `folded` is.
    pub fn levels(self) -> &'static [Level] {
        match self {
            Show::Tools | Show::Thinking => &[Level::Hidden, Level::Folded, Level::Open],
            Show::Edits | Show::System | Show::RawCalls => &[Level::Hidden, Level::Open],
        }
    }

    /// Where one press of this switch's chord takes it, `None` for a switch no key reaches. The chord
    /// never lands on `hidden`: the ladder hides a whole rung at a time, so `thinking: hidden` on the
    /// status row would name a state the screen does not carry.
    pub fn by_chord(self, now: Level) -> Option<Level> {
        self.chord()?;
        Some(if now == Level::Open {
            if self.levels().contains(&Level::Folded) {
                Level::Folded
            } else {
                Level::Hidden
            }
        } else {
            Level::Open
        })
    }
}

/// How much of one kind of row reaches the screen. Three levels and not two, and the third is what
/// makes one mechanism out of two: `folded` and `hidden` are the two answers a reader already had -
/// fold this body, hide this row - spelled one way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The row is not drawn at all.
    Hidden,
    /// The row is drawn and its body is not.
    Folded,
    /// The row and its body.
    Open,
}

impl Level {
    /// Three words and no synonyms: the operator's words for a switch's two ends are on and off, and
    /// the middle has no name in them, so `hidden`, `folded` and `open` are what a refusal names.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Hidden => "hidden",
            Level::Folded => "folded",
            Level::Open => "open",
        }
    }

    pub fn parse(word: &str) -> Option<Level> {
        match word.trim().to_ascii_lowercase().as_str() {
            "hidden" => Some(Level::Hidden),
            "folded" => Some(Level::Folded),
            "open" => Some(Level::Open),
            _ => None,
        }
    }
}

/// A profile IS its set: one table, one row per profile, and the row's list is the profile. Not an
/// enum with a `set()` beside it, which would be a second definition that drifts when a switch is
/// added. Absence from the list is `hidden`, which makes `conversation`'s empty list a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// The word `/verbosity` takes, the card lists and `head.toml` keeps.
    pub name: &'static str,
    /// **The whole definition**: what is on, at which level. Nothing else is.
    pub set: &'static [(Show, Level)],
    /// What it gives you, as the card says it - *what will be on the screen*, never *what is
    /// filtered*: a reader choosing a profile is not reasoning about the event stream.
    pub why: &'static str,
}

impl Profile {
    pub const CONVERSATION: Profile = Profile {
        name: "conversation",
        set: &[],
        why: "your messages and the model's answers — nothing the head did to produce the \
              words",
    };
    pub const READ_EDITS: Profile = Profile {
        name: "read-edits",
        set: &[(Show::Edits, Level::Open)],
        why: "the above, plus every edit and write — what the head CHANGED, with its diff",
    };
    pub const TERSE: Profile = Profile {
        name: "terse",
        set: &[(Show::Edits, Level::Open), (Show::Tools, Level::Folded)],
        why: "read-edits, plus the head's other rows: one row per tool call and how it ended, \
              and the thinking, folded",
    };
    pub const NORMAL: Profile = Profile {
        name: "normal",
        set: &[
            (Show::Edits, Level::Open),
            (Show::Tools, Level::Folded),
            (Show::Thinking, Level::Folded),
        ],
        why: "terse, and this head's own start — the model's thinking is folded rather than \
              absent",
    };
    pub const LOUD: Profile = Profile {
        name: "loud",
        set: &[
            (Show::Edits, Level::Open),
            (Show::Tools, Level::Folded),
            (Show::Thinking, Level::Folded),
            (Show::System, Level::Open),
        ],
        why: "normal, plus who attached and who issued which command",
    };

    /// The table, in the order the card lists them. `read-edits` sits next to `conversation` rather
    /// than at the end because it IS `conversation` with one switch up, and the ring climbs from
    /// there.
    pub const ALL: [Profile; 5] = [
        Self::CONVERSATION,
        Self::READ_EDITS,
        Self::TERSE,
        Self::NORMAL,
        Self::LOUD,
    ];

    /// **The profile whose set is exactly this set** - the table's own answer, walked rather
    /// than written down a second time.
    pub fn of(vis: Visibility) -> Option<Profile> {
        Self::ALL
            .into_iter()
            .find(|p| Show::ALL.iter().all(|s| vis.level(*s) == p.level(*s)))
    }

    /// The level this profile's set puts one switch at - `hidden` for a switch the row does not
    /// name, which is what makes the empty set a sentence.
    pub fn level(self, s: Show) -> Level {
        self.set
            .iter()
            .find(|(sw, _)| *sw == s)
            .map(|(_, l)| *l)
            .unwrap_or(Level::Hidden)
    }

    /// The profile a typed word names, if it names one.
    pub fn parse(word: &str) -> Option<Profile> {
        let t = word.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|p| p.name == t)
    }
}

/// The set of switches in force. The set is the state and the profile is a question asked of it, so
/// a reader who takes `normal` and turns one switch up has a set that is no profile and the status
/// row says `custom` rather than pretending otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Visibility {
    pub(crate) edits: Level,
    pub(crate) tools: Level,
    pub(crate) thinking: Level,
    pub(crate) system: Level,
    pub(crate) raw_calls: Level,
}

/// What a typed word means. Two shapes and not one, because a switch named alone is a question about
/// the set in force and a profile is a whole set: read as one, `edits=open` would mean `read-edits`
/// rather than what a reader who already had `loud` typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A whole set: a profile's, or the set a `custom …` name spells out.
    Set(Visibility),
    /// One switch, applied to the set in force.
    Switch(Show, Level),
}

impl Visibility {
    /// The set a profile IS.
    pub fn of(p: Profile) -> Visibility {
        let mut v = Visibility::empty();
        for (s, l) in p.set {
            v = v.with(*s, *l);
        }
        v
    }

    /// The empty set - *"the conversation and nothing the head made"* - which is
    /// `conversation`, and where a head that has read nothing but an old `head.toml` starts.
    pub const fn empty() -> Visibility {
        Visibility {
            edits: Level::Hidden,
            tools: Level::Hidden,
            thinking: Level::Hidden,
            system: Level::Hidden,
            raw_calls: Level::Hidden,
        }
    }

    /// Where this head starts: `normal`'s set, and the one place that is said.
    pub fn starting() -> Visibility {
        Visibility::of(Profile::NORMAL)
    }

    /// The set with everything the ladder draws turned on: what an OPEN run is drawn at. It is
    /// `loud`'s set because that is the profile showing everything the ladder can, and it carries
    /// `edits` with it so an open run draws its edit cards too.
    pub fn lifted() -> Visibility {
        Visibility::of(Profile::LOUD)
    }

    pub fn level(self, s: Show) -> Level {
        match s {
            Show::Edits => self.edits,
            Show::Tools => self.tools,
            Show::Thinking => self.thinking,
            Show::System => self.system,
            Show::RawCalls => self.raw_calls,
        }
    }

    /// The set with one switch moved - **the only way a set is built**, so a switch that is in
    /// [`Show::ALL`] cannot be missing from it.
    pub fn with(self, s: Show, l: Level) -> Visibility {
        let mut v = self;
        match s {
            Show::Edits => v.edits = l,
            Show::Tools => v.tools = l,
            Show::Thinking => v.thinking = l,
            Show::System => v.system = l,
            Show::RawCalls => v.raw_calls = l,
        }
        v
    }

    /// **Is this switch showing at all** - `folded` is showing: the row is drawn, and its body
    /// is the fold's.
    pub fn shows(self, s: Show) -> bool {
        self.level(s) != Level::Hidden
    }

    /// **Which profile this set is, if any** - `None` is `custom …`.
    pub fn profile(self) -> Option<Profile> {
        Profile::of(self)
    }

    /// The name of this state, for the status row, `/status` and `head.toml`: the profile's name when
    /// the set is one, and `custom` with the switches that differ from the head's start when it is
    /// not. It round-trips through `parse`, which is what makes it a name and not a caption.
    pub fn as_str(self) -> String {
        if let Some(p) = self.profile() {
            return p.name.to_string();
        }
        let base = Visibility::starting();
        let mut words = vec!["custom".to_string()];
        for s in Show::ALL {
            if self.level(s) != base.level(s) {
                words.push(format!("{}={}", s.name(), self.level(s).as_str()));
            }
        }
        words.join(" ")
    }

    /// The one reader of a typed word: a profile name, a switch name, or `SWITCH=LEVEL` - the same
    /// function `head.toml` is read with. A bare switch name turns it on, on and off being the
    /// operator's words for the two ends. The refusal is a sentence because the caller repeats it.
    pub fn parse(typed: &str) -> Result<Change, String> {
        let t = typed.trim().to_ascii_lowercase();
        if t.is_empty() {
            return Err(VISIBILITY_REFUSAL.to_string());
        }
        if let Some(p) = Profile::parse(&t) {
            return Ok(Change::Set(Visibility::of(p)));
        }
        // **`custom edits=hidden thinking=open …`** - the name `as_str` writes for a set that is
        // no profile, read back against the head's own start. Without this a stored preference
        // would be a caption: the status row would name a set no file could restore.
        let mut words: Vec<&str> = t.split_whitespace().collect();
        let whole = words.first() == Some(&"custom");
        if whole {
            words.remove(0);
            if words.is_empty() {
                return Err(format!(
                    "`custom` needs a switch after it — {VISIBILITY_REFUSAL}"
                ));
            }
        } else if words.len() > 1 {
            return Err(format!("`{typed}` is not one word — {VISIBILITY_REFUSAL}"));
        }
        let mut named: Vec<(Show, Level)> = Vec::new();
        for word in &words {
            let (name, level) = match word.split_once('=') {
                Some((n, l)) => (n, Some(l)),
                None if !whole => (*word, None),
                None => {
                    return Err(format!(
                        "`{word}` names no level — a set is written `SWITCH=LEVEL …`"
                    ));
                }
            };
            let Some(show) = Show::ALL.into_iter().find(|s| s.name() == name) else {
                return Err(format!(
                    "`{typed}` is not a profile and `{name}` is not a switch — {VISIBILITY_REFUSAL}"
                ));
            };
            let level = match level {
                // Turned ON. `open` and not `folded`: a bare name is the switch's own word for
                // itself, and every switch has an `open` end.
                None => Level::Open,
                Some(l) => match Level::parse(l) {
                    Some(l) if show.levels().contains(&l) => l,
                    Some(l) => {
                        return Err(format!(
                            "`{}` is not a level of `{}` — that switch holds {}",
                            l.as_str(),
                            show.name(),
                            names(
                                &show
                                    .levels()
                                    .iter()
                                    .map(|l| l.as_str().to_string())
                                    .collect::<Vec<_>>()
                            ),
                        ));
                    }
                    None => {
                        return Err(format!(
                            "`{l}` is not a level — the three are hidden, folded and open"
                        ));
                    }
                },
            };
            named.push((show, level));
        }
        if whole {
            // A whole set, read against a BASE - the head's own start, which is what `as_str` writes a
            // custom name as. One word is `SWITCH=LEVEL`, applied to the set IN FORCE; `custom` replaces
            // it, which is the distinction `Change` carries.
            let mut back = Visibility::starting();
            for (s, l) in named {
                back = back.with(s, l);
            }
            return Ok(Change::Set(back));
        }
        let (s, l) = named[0];
        Ok(Change::Switch(s, l))
    }

    /// Is this row drawn: the ladder's answer plus the one clause the ladder cannot say - an
    /// `edit`/`write` card is kept when `edits` is showing, whatever the rung says. This is the one
    /// place it is decided. The operator's own act is asked first, the switches being verbosity too.
    pub fn keeps(self, item: &letibot_transcript::TranscriptItem) -> bool {
        if operator_act(item) {
            return true;
        }
        if is_edit_card(item) {
            return self.shows(Show::Edits);
        }
        self.rung().keeps(item)
    }

    /// Is anything of the head's hidden - the question the run markers and the counts are
    /// counted for.
    pub fn hides_the_working(self) -> bool {
        self.rung().hides_the_working()
    }

    /// The rung the LADDER's own rows are drawn at: `tools`, `thinking` and `system` are the three
    /// switches of `terse`, `normal` and `loud`, so a set that turns them on in the ladder's order
    /// names a rung and one that turns none on is the bottom - which makes `read-edits` a rung.
    pub fn rung(self) -> Verbosity {
        if self.shows(Show::System) {
            Verbosity::Loud
        } else if self.shows(Show::Thinking) {
            Verbosity::Normal
        } else if self.shows(Show::Tools) {
            Verbosity::Terse
        } else {
            Verbosity::Conversation
        }
    }

    /// Can the ladder draw this set, switch by switch - and which switch it cannot. The ladder turns
    /// its three on in one order, so a set that hides a switch BELOW one it shows is a set no rung can
    /// be, and the verb refuses it rather than storing a word the screen would not carry.
    pub fn undrawable(self) -> Option<Show> {
        if self.shows(Show::Tools) || !self.shows(Show::Thinking) {
            if self.shows(Show::Thinking) || !self.shows(Show::System) {
                return None;
            }
            return Some(Show::System);
        }
        Some(Show::Thinking)
    }
}

/// Is this row the act of the person at the keyboard - the one fact verbosity may not hide.
/// `CallOrigin::Operator` is what makes it knowable and is why the field is on the row: a head that
/// inferred it from the absence of a proposing row above would be guessing.
pub(crate) fn operator_act(item: &letibot_transcript::TranscriptItem) -> bool {
    matches!(
        item,
        letibot_transcript::TranscriptItem::ToolResult {
            origin: Some(letibot_transcript::CallOrigin::Operator { .. }),
            ..
        }
    )
}

/// Is this row one that says what the head CHANGED: the one predicate `read-edits` differs by. Two
/// signals, both the tree's own - the excerpt on a finished call, and the call's name through
/// `card::Verb::of`. An unknown name or a refused call is not an edit: this errs toward hiding.
pub(crate) fn is_edit_card(item: &letibot_transcript::TranscriptItem) -> bool {
    let letibot_transcript::TranscriptItem::ToolResult {
        name,
        outcome,
        edit,
        ..
    } = item
    else {
        return false;
    };
    if !matches!(outcome, letibot_transcript::ToolOutcome::Ok) {
        return false;
    }
    edit.is_some() || matches!(card::Verb::of(name), card::Verb::Edit | card::Verb::Write)
}

/// **What a refused word is told**, and the whole of it: the two vocabularies are named by the
/// tables themselves in the longer refusals ([`Visibility::parse`]), and this is the half a
/// sentence can carry on one line of a status row.
pub(crate) const VISIBILITY_REFUSAL: &str = "name a profile or a switch, or SWITCH=LEVEL — `/verbosity` with nothing after it shows the \
     profiles and what each one gives you";

/// `a, b and c` - the refusal and the card name a list, and a join written per call site is a
/// join that disagrees with the one beside it about the last comma.
pub(crate) fn names(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// How much of a foldable thing is on the screen: two states and a key that flips them, rather than
/// a per-item toggle. The pointer here is a wheel that selects nothing, so a per-item affordance
/// would need a cursor mode and a second keymap. The fold's header names its key instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// A title and a count. The default for reasoning, because the reasoning is
    /// working-out and the answer is the answer.
    Folded,
    /// Everything, bounded only by the render budget.
    Open,
}

impl Fold {
    pub(crate) fn flip(self) -> Fold {
        match self {
            Fold::Folded => Fold::Open,
            Fold::Open => Fold::Folded,
        }
    }

    pub(crate) fn is_open(self) -> bool {
        self == Fold::Open
    }
}

/// The same fact in rano's words: its widgets take `rano::agent::Fold`, and this is the one
/// place the two are reconciled.
impl From<Fold> for rano::agent::Fold {
    fn from(f: Fold) -> rano::agent::Fold {
        match f {
            Fold::Folded => rano::agent::Fold::Folded,
            Fold::Open => rano::agent::Fold::Open,
        }
    }
}
