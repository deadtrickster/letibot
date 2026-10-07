//! **What the conversation shows**: the verbosity profiles, the per-kind switches and levels,
//! and the visibility a frame is drawn under (`/verbosity`).

use super::*;

/// How much of the stream reaches the transcript.
///
/// **Five rungs, and the bottom one is a different kind of thing from the other four.** The
/// four are *registers* — how much of the event stream is shown — and they are ordered by
/// how loud they are. `Conversation` is a *scope*: the conversation, and nothing the head did
/// to produce it. It sits at the bottom because that is the order they are cycled in and
/// because it shows the least, but the row it is about is not a decibel.
///
/// The name is letibot's to choose and **both heads use it** (R37, §11.6): `Verbosity` is this
/// ladder and these levels are its vocabulary, so a reader moving between the two heads must
/// not have to learn two words for one view. The key or verb that reaches it is each head's
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// **The conversation alone** — the operator's messages and the model's answers, and
    /// nothing the head did to produce them.
    ///
    /// Hidden: tool calls, tool outcomes, tool payloads, reasoning, head arrivals, command
    /// attribution. **Not hidden, and this is the ruling rather than a preference:**
    ///
    /// * **Warnings.** [`Verbosity::Loud`]'s docstring settled it and the reasoning transfers
    ///   whole — a warning is a fact the daemon chose to *interrupt* with, and this whole
    ///   ladder is applied to the transcript at once, so a rung that hid one would
    ///   retroactively erase a warning already read. That is not a filter but a revision.
    /// * **Decision cards.** A gate card is not a tool row. Hiding it makes the session
    ///   unanswerable while the call times out against a quiet screen.
    /// * **Liveness.** This rung's own risk, and it is specific to it: with the tool rows
    ///   hidden, **a ten-minute tool-heavy turn draws nothing at all**. The composer's border
    ///   keeps the spinner and the elapsed time, so working and wedged stay distinguishable —
    ///   see `App::turn_status`, which is not gated by any of this.
    /// * **The operator's own act.** A `!` line and the `ToolResult` it produced are the
    ///   conversation, not the head's working: *"an act of the person at the keyboard is never
    ///   hidden by verbosity"* — this ladder governs how much of the MODEL's working you see,
    ///   and it must not hide what YOU did. See [`Verbosity::keeps`], which is where the row's
    ///   `origin` is read and where the ruling is argued — this docstring said *hidden: tool
    ///   outcomes* until a `! ls` on a live head proved the sentence wrong.
    ///
    /// **It is a view.** Nothing is dropped from the transcript, the ledger, the corpus or
    /// what is sent to the model, and the filter is applied to the whole transcript at once,
    /// so switching back restores every row including the span it was on. That is what makes
    /// hiding safe here where an elision would need a placeholder per row — R29's remedy rule
    /// is satisfied by the MODE being named on screen (`App::scroll_state`'s neighbour,
    /// `App::rung_state`), which is the thing the operator asked to be rid of.
    Conversation,
    /// Assistant text and tool outcomes only.
    Terse,
    /// Plus reasoning.
    Normal,
    /// Plus head arrivals and who issued which command.
    ///
    /// **And a warning is not on this ladder.** This doc used to promise that
    /// *Loud* adds warnings, and the code never gated them — so the doc and the
    /// head disagreed, and R10 asked which was wrong. The code was right and the
    /// doc was: a warning is a fact the daemon chose to INTERRUPT with, and a
    /// level that hides it makes the head the thing that decides the operator
    /// should not have seen it. Worse in this shape than in most: `Verbosity` is
    /// applied to the whole transcript at once, so switching to `terse` would
    /// retroactively erase a warning that had already been read — which is not a
    /// filter but a revision. What a reader has against a warning is `/notes
    /// dismiss`, which is per-note, visible, and counted on `/status`.
    Loud,
}

impl Verbosity {
    /// **Every rung, in the order the ladder climbs** — R38's one list.
    ///
    /// The card's seeding, the typed name, the cycle and the `next()` all ask *which of
    /// the values is this*, and a second list is a second answer. This is the same lesson
    /// the completion table and the `mode` settings row each cost this tree once.
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

    /// **Whether the rows the head made are drawn at all** — R37's one question.
    ///
    /// A predicate rather than a comparison at every call site: `== Verbosity::Conversation`
    /// spelled out in five places is five places to update when a second scope-shaped rung
    /// arrives, and the question being asked is *is this rung a scope*, not *is it
    /// that value*.
    pub fn hides_the_working(self) -> bool {
        matches!(self, Verbosity::Conversation)
    }

    /// What a row of the head's own making needs, to be drawn under this rung.
    ///
    /// The conversation is `User` and `Assistant`; everything else in a transcript is
    /// evidence *about* the conversation — a tool call, its outcome and payload, the model's
    /// reasoning, a system update — and this rung is the one that shows the conversation and
    /// not the working.
    ///
    /// **Except when the person at the keyboard is the one who acted**, which is a second
    /// sentence rather than a footnote to the first: the rows that carry such an act are their
    /// own `User` line and the `ToolResult` whose `origin` says they ran it.
    pub fn keeps(self, item: &letibot_transcript::TranscriptItem) -> bool {
        use letibot_transcript::TranscriptItem as T;
        if !self.hides_the_working() {
            return true;
        }
        // **An act of the person at the keyboard is never hidden by verbosity.**
        //
        // MEASURED on a live head, and this is the whole of the report: the operator typed
        // `! ls`, the store held the right two rows — their own `User` line and the `bash`
        // result with `origin: CallOrigin::Operator { … }` — the turn started correctly, and
        // **the operator never saw the result on the screen**. Their own guess was the right
        // one: *"i guess it was eaten by verbosity level"*, and their verbosity is
        // `read-edits`, whose rung is this one.
        //
        // It is the wrong thing for a rung to eat, and the reason is the whole of what this
        // ladder is for. **Verbosity governs how much of the MODEL's working you see; it must
        // not hide what YOU did.** A `ToolResult` the operator ran is therefore the
        // conversation exactly as the `User` row beside it is, and it is drawn at every rung of
        // [`Verbosity::ALL`].
        //
        // **And it is still a tool row.** Being kept is a question about the filter and not
        // about the fold: the row is drawn the way tool output is drawn — its header, its
        // `+N lines`, and `ctrl-v`/`/t` to open it — and it is never expanded against the
        // operator's wishes, because the fold is the operator's own switch and this clause
        // does not touch it.
        //
        // [`operator_act`] reads the one fact that makes this knowable, and `origin` is on the
        // row precisely so no head has to guess it: `None` is a call the MODEL proposed.
        if operator_act(item) {
            return true;
        }
        matches!(item, T::User { .. } | T::Assistant { .. })
    }
}

/// **One of the things that can be shown or not** — the list the operator asked for.
///
/// > *"the verbositiy and visiblity toggles need a rewrite and normalization - some toggled by
/// > shortcuts some by /commands. What I want - a list of things that can be shown and then
/// > verbosity profiles composed by switching them on and off. For example leticl has read-edits
/// > verbosity levels when all is hidden except edits"*
///
/// Five, and **[`Show::ALL`] is the only place one is added**: the row filter, the card,
/// `/verbosity`, [`Visibility::parse`] and the key dispatch all walk that one list, so a switch
/// in it is reachable by every one of them and a switch outside it is reachable by none.
///
/// # The three that are NOT on this list, and why the list must not grow them
///
/// **Warnings**, **decision cards** and **liveness** are not hideable, and there is no switch
/// here to hang them on. [`Verbosity::Conversation`]'s docstring carried those three rulings
/// and they survive: they are not repealed by putting a switch list beside the ladder, and the
/// reasoning transfers whole.
///
/// * **Warnings.** *"a warning is a fact the daemon chose to interrupt with"*, and a filter
///   applied to the whole transcript at once would *"retroactively erase a warning already
///   read. That is not a filter but a revision."* A warning is a [`Note`] and not a row, so
///   the list could not express one even if somebody wanted it to.
/// * **Decision cards.** *"A gate card is not a tool row. Hiding it makes the session
///   unanswerable while the call times out against a quiet screen."*
/// * **Liveness.** [`App::turn_status`] is not gated by anything here, and this list is why it
///   matters: `conversation` hides the tool rows, so a ten-minute tool-heavy turn would draw
///   nothing at all, and the composer's border is what keeps working and wedged apart.
///
/// **And the operator's own act is not on the list either**, though unlike those three it has a
/// switch that appears to cover it: `tools` is *"tool calls, their outcomes and their
/// payloads"*, and the `ToolResult` of a `!` line is one of those rows. It is drawn anyway —
/// *"an act of the person at the keyboard is never hidden by verbosity"* — and the clause lives
/// in [`Verbosity::keeps`] rather than as a sixth switch, because **a switch is something a
/// reader can turn off and this is not.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    /// **The cards that say what the head CHANGED** — an `edit`/`write` call and its diff.
    ///
    /// The operator's own switch, and the one the ladder could not be told: it is off at
    /// every rung of the ladder and on at `read-edits`, which is a set and not a rung.
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
    /// **Every switch, in the order the card and the refusal list them** — R38's one list, one
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

    /// What it covers, in the reader's terms rather than the head's.
    ///
    /// **In this slice `tools`, `thinking` and `system` are the LADDER's switches**, drawn by
    /// the rung [`Visibility::rung`] names: they are what [`Verbosity::Terse`], `Normal` and
    /// `Loud` each turn ON, and their `hidden` end is the bottom of the ladder — which is why
    /// `read-edits`, *"all is hidden except edits"*, is the switch list's one new drawing and
    /// not a fifth kind of row filter. `edits` and `raw-calls` are this list's own and are
    /// honoured switch by switch.
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

    /// **The chord, as the pair that cannot drift**: the name a seam and the hint bar spell,
    /// and the key the terminal actually sends.
    ///
    /// One entry for both, because a name in one table and a key in another is exactly how a
    /// chord comes to be advertised and do nothing — and the key dispatch asks this table rather
    /// than matching chords by hand ([`Key::show`]).
    ///
    /// **Why these two keys, kept from the arms they were written in.** `ctrl-r` for the thinking
    /// is the fold this head has always had. `ctrl-x` for the raw markup is not one of the obvious
    /// letters, and each of the obvious ones is taken: `ctrl-r` is the thinking — and the operator
    /// ruled it out for this one by name, *"I want to save the ability to see raw tool calls but
    /// it should be behind some chord, different to C-r"* — `ctrl-c`, `ctrl-d`, `ctrl-z`, `ctrl-s`
    /// and `ctrl-q` are the terminal's own (two of them flow control that would freeze a pane),
    /// `ctrl-l`, `ctrl-t` and `ctrl-s` are already this head's, and `ctrl-a/e/w/u/y/k/b/f` are the
    /// composer's readline keys, which are muscle memory and not available. `alt-r` would read
    /// better in the hint bar and is not safe: a lone `Esc` followed by a typed `r` arrives in the
    /// same read as `ESC r`, and the composer's interrupt is `Esc` twice. What is left and is
    /// mnemonic is **`x` for the XML-ish markup** — `<function=…><parameter=…>` — which is exactly
    /// what the chord shows; `0x18` is unbound here, is not one of the tty's control characters,
    /// and readline uses it only as a prefix, so nothing is waiting for a second byte.
    pub fn chord(self) -> Option<(&'static str, Key)> {
        match self {
            Show::Thinking => Some(("ctrl-r", Key::CtrlR)),
            Show::RawCalls => Some(("ctrl-x", Key::CtrlX)),
            Show::Edits | Show::Tools | Show::System => None,
        }
    }

    /// The levels this switch holds — what `/verbosity SWITCH=LEVEL` will take for it.
    ///
    /// `edits`, `system` and `raw-calls` have no body to fold: an edit card is drawn with its
    /// excerpt or it is not drawn, a system row is a sentence, and the raw markup is the raw
    /// markup. `tools` and `thinking` have bodies, which is what `folded` is.
    pub fn levels(self) -> &'static [Level] {
        match self {
            Show::Tools | Show::Thinking => &[Level::Hidden, Level::Folded, Level::Open],
            Show::Edits | Show::System | Show::RawCalls => &[Level::Hidden, Level::Open],
        }
    }

    /// **Where one press of this switch's chord takes it** — `None` for a switch no key
    /// reaches.
    ///
    /// **The chord never lands on `hidden`**, and that is a ruling rather than a shortcut: the
    /// chord's switch is the ladder's, the ladder hides a whole rung at a time
    /// ([`Visibility::rung`]), and a press that put `thinking: hidden` on the status row would
    /// name a state the screen does not carry. What a chord does is the body's — fold it and
    /// unfold it — and hiding a row kind whole is what a profile is for.
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

/// **How much of one kind of row reaches the screen** — the value one switch holds.
///
/// Three levels and not two, and the third is what makes ONE mechanism out of two. The
/// operator: *"some toggled by shortcuts some by /commands"*. Measured, those two were `ctrl-r`
/// folding the model's thinking and `/t` unfolding every tool row — both questions about a
/// BODY — while the rung above them hid whole rows, a question about the ROW. `folded` and
/// `hidden` are those two answers, spelled one way.
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
    /// **Three words and no synonyms** — the operator's own words for the two ENDS of a switch
    /// are *on* and *off*, and the middle one has no name in them; so `hidden`, `folded` and
    /// `open` are what `/verbosity SWITCH=LEVEL` takes and what a refusal names, rather than
    /// this head guessing which end somebody meant.
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

/// **A profile IS its set** — one table, one row per profile, and the row's list is the
/// profile.
///
/// **Not a variant of an enum with a `set()` beside it.** The operator asked for *"verbosity
/// profiles composed by switching them on and off"*, and a profile that had to be DECODED into
/// its switches would be a second definition of it — the copy this file keeps refusing to make,
/// and the one that drifts the moment a switch is added.
///
/// # The table
///
/// | profile | the set it IS |
/// |---|---|
/// | `conversation` | the empty list — *"the conversation and nothing the head made"* |
/// | `read-edits` | `{edits: open}` — **the operator's own example**: *"all is hidden except\n///   edits"*, which is `conversation` with ONE switch turned up |
/// | `terse` | `read-edits` plus `{tools: folded}` |
/// | `normal` | `terse` plus `{thinking: folded}` — where this head starts |
/// | `loud` | `normal` plus `{system: open}` |
///
/// **Absence from a row's list is `hidden`**, which is what makes the empty list a sentence:
/// a set is what is turned on, and everything a profile does not name is off.
///
/// # What a profile's levels mean here, and the one remainder
///
/// `edits` and `raw-calls` are honoured switch by switch ([`Visibility::keeps`], and the raw
/// markup's own flag). `tools`, `thinking` and `system` are the LADDER's three, and the ladder
/// turns them on together above its bottom rung, so at `terse`, `normal` and `loud` the rows of
/// all three are drawn — as they are today — whatever level the row names. `conversation` and
/// `read-edits` are exact: they turn none of the three on, and the bottom rung is the rung that
/// hides them.
///
/// **Which is leticl's shape as well as this head's**: there, `reading-hides-p` is asked only at
/// the two READING rungs (`:reading` and `:read-edits`) and the rungs above draw everything;
/// what separates `:terse`, `:normal` and `:loud` there is `verbosity-at-least`'s gates, which
/// is what separates `Terse`, `Normal` and `Loud` here (`src/session/events.lisp:120`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// The word `/verbosity` takes, the card lists and `head.toml` keeps.
    pub name: &'static str,
    /// **The whole definition**: what is on, at which level. Nothing else is.
    pub set: &'static [(Show, Level)],
    /// What it gives you, as the card says it — *what will be on the screen*, never *what is
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

    /// **The table, in the order the card lists them.** `read-edits` sits next to
    /// `conversation` rather than at the end because it is `conversation` with one switch up —
    /// leticl puts it in the same place in its ring for the same reason
    /// (`+verbosity-ladder+`, `src/session/events.lisp:113`: *"it draws everything `:reading`
    /// draws PLUS the cards that say what the head CHANGED"*).
    pub const ALL: [Profile; 5] = [
        Self::CONVERSATION,
        Self::READ_EDITS,
        Self::TERSE,
        Self::NORMAL,
        Self::LOUD,
    ];

    /// **The profile whose set is exactly this set** — the table's own answer, walked rather
    /// than written down a second time.
    pub fn of(vis: Visibility) -> Option<Profile> {
        Self::ALL
            .into_iter()
            .find(|p| Show::ALL.iter().all(|s| vis.level(*s) == p.level(*s)))
    }

    /// The level this profile's set puts one switch at — `hidden` for a switch the row does not
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

/// **The set of switches in force** — and the questions asked of it.
///
/// *Is this switch showing* ([`Visibility::shows`]), *which profile is this set, if any*
/// ([`Visibility::profile`], [`Visibility::as_str`] — `custom …` when none), and the two
/// questions the render asks of the set ([`Visibility::keeps`],
/// [`Visibility::hides_the_working`]).
///
/// **The set is the state and the profile is a question asked of it.** That is the operator's
/// ask, whole: a reader who takes `normal` and turns one switch up has a set that is no profile,
/// and the head says `custom …` on the status row rather than pretending they are on one — which
/// is the difference between this and the ladder, where the only state was one of four names.
///
/// The three questions the render used to ask of a rung are asked of this instead, and there is
/// one place each: [`Visibility::keeps`] (is this row drawn), [`Visibility::hides_the_working`]
/// (is anything of the head's hidden, which is what the run markers are counted for) and
/// [`Visibility::rung`] (which rung the LADDER's own rows are drawn at).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Visibility {
    pub(crate) edits: Level,
    pub(crate) tools: Level,
    pub(crate) thinking: Level,
    pub(crate) system: Level,
    pub(crate) raw_calls: Level,
}

/// What a typed word means, once [`Visibility::parse`] has read it.
///
/// **Two shapes and not one**, because a switch named on its own is a question about the set it
/// is applied to and a profile is a whole set: `/verbosity tools=hidden` says *this switch, from
/// where I am*, and `/verbosity terse` says *that set and no other*. Reading the two as one
/// would make `edits=open` mean `{edits: open}` — every other switch hidden — which is
/// `read-edits`, a profile, and not what a reader who already had `loud` typed.
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

    /// The empty set — *"the conversation and nothing the head made"* — which is
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

    /// **The set with everything the ladder draws turned on** — what an OPEN run is drawn at.
    ///
    /// R37 AMENDED: *"opening a run is the rung lifted for its rows and no others"* — and
    /// "lifted" means the set that shows the lot, which is where the walk used to pass
    /// `Verbosity::Normal` for exactly this reason (`Normal::keeps` answers `true` for every
    /// row). It is `loud`'s set because that is the profile that shows everything the ladder
    /// can, and it carries `edits` with it so an open run draws its edit cards too.
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

    /// The set with one switch moved — **the only way a set is built**, so a switch that is in
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

    /// **Is this switch showing at all** — `folded` is showing: the row is drawn, and its body
    /// is the fold's.
    pub fn shows(self, s: Show) -> bool {
        self.level(s) != Level::Hidden
    }

    /// **Which profile this set is, if any** — `None` is `custom …`.
    pub fn profile(self) -> Option<Profile> {
        Profile::of(self)
    }

    /// **The name of this state**, for the status row, `/status` and `head.toml`: the profile's
    /// name when the set is one, and `custom …` with the switches that differ from the head's
    /// start when it is not (*"any set that is no profile reads as `custom …`"*).
    ///
    /// The spelling round-trips through [`Visibility::parse`], which is what makes it a name
    /// rather than a caption — a preference written here means the same set when it is read
    /// back, and that is the whole of *"a stored preference keeps meaning"*.
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

    /// **The one reader of a typed word** — a profile name, a switch name, or `SWITCH=LEVEL` —
    /// and it is the same function `head.toml` is read with ([`App::load_prefs`]) and the same
    /// one the key dispatch's names come from.
    ///
    /// A bare switch name turns that switch ON: *on* and *off* are the operator's words for the
    /// two ends of a switch and the middle level has no name in them, so `/verbosity edits` is
    /// `edits=open` and a reader who meant `folded` says `folded`.
    ///
    /// The refusal is a sentence and not `None`, because the caller's answer to a word this
    /// head does not know is to SAY what it does know — the list of profiles and of switches —
    /// and a parse that only said *no* would put that list in a second place, which is the copy
    /// that drifts.
    pub fn parse(typed: &str) -> Result<Change, String> {
        let t = typed.trim().to_ascii_lowercase();
        if t.is_empty() {
            return Err(VISIBILITY_REFUSAL.to_string());
        }
        if let Some(p) = Profile::parse(&t) {
            return Ok(Change::Set(Visibility::of(p)));
        }
        // **`custom edits=hidden thinking=open …`** — the name `as_str` writes for a set that is
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
                None if !whole => (word.as_ref(), None),
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
            // **A whole set, read against a BASE** — the head's own start, because that is what
            // `as_str` writes a custom name as: the switches that differ from it. One word is
            // `SWITCH=LEVEL` and is applied to the set IN FORCE; `custom …` is a set and replaces
            // it, which is the same distinction `Change` carries.
            let mut back = Visibility::starting();
            for (s, l) in named {
                back = back.with(s, l);
            }
            return Ok(Change::Set(back));
        }
        let (s, l) = named[0];
        Ok(Change::Switch(s, l))
    }

    /// **Is this row drawn** — and it is the ladder's answer plus the one clause the ladder
    /// cannot say.
    ///
    /// *"an `edit`/`write` call and its diff"* is kept when `edits` is showing, whatever the
    /// rung says. leticl differs from it in exactly one predicate and says so in as many words —
    /// *"`read-edits` KEEPS THE EDITS, AND THIS IS THE ONLY PLACE THAT DECIDES IT"*
    /// (`src/cards/hidden-run.lisp:40`) — and **this is our one place**: `item_lines` and the run
    /// finder both ask this, so a second opinion about which rows are hidden cannot put a marker
    /// beside a row that is still on the screen.
    ///
    /// **And the operator's own act is asked BEFORE the switch**, because the switches are
    /// verbosity too: `tools` is *"tool calls, their outcomes and their payloads"*, and the
    /// `ToolResult` of a `!` line is one of those rows — *"an act of the person at the keyboard
    /// is never hidden by verbosity"*. [`Verbosity::keeps`] carries the ruling and reads the
    /// `origin`; it is asked here as well so the `edits` clause below cannot hide a call a
    /// person made either. The door's list holds no `edit`/`write` name today and a list that can
    /// grow is not a reason to leave the hole in the one predicate that decides.
    pub fn keeps(self, item: &letibot_transcript::TranscriptItem) -> bool {
        if operator_act(item) {
            return true;
        }
        if is_edit_card(item) {
            return self.shows(Show::Edits);
        }
        self.rung().keeps(item)
    }

    /// Is anything of the head's hidden — the question the run markers and the counts are
    /// counted for.
    pub fn hides_the_working(self) -> bool {
        self.rung().hides_the_working()
    }

    /// **The rung the LADDER's own rows are drawn at** — `tools`, `thinking` and `system` are
    /// [`Verbosity::Terse`], `Normal` and `Loud`'s three switches, so a set that turns them on
    /// in the ladder's order names a rung, and a set that turns none of them on is the bottom.
    ///
    /// **Which is what makes `read-edits` the operator's rung.** *"all is hidden except
    /// edits"* turns none of the three on, so the tool rows, the thinking and the system rows
    /// are hidden by exactly the rung that has always hidden them — the ladder is still what
    /// hides those rows, and the one thing it could not be told is the edit card.
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

    /// **Can the ladder draw this set, switch by switch** — and which switch it cannot, when it
    /// cannot.
    ///
    /// The ladder turns its three switches on in one order: `tools`, then `thinking`, then
    /// `system`. So a set that hides a switch BELOW one it shows (`tools=hidden` while the
    /// thinking is on) is a set no rung can be — the rung that draws the thinking draws the tool
    /// rows too — and the verb refuses it rather than storing a word the screen would not carry.
    /// **This is the "a profile that changes nothing is worse than an unfinished rewrite"
    /// rule, made mechanical**: what cannot be drawn is not stored.
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

/// **Is this row the act of the person at the keyboard** — the one fact verbosity may not hide.
///
/// R24 part two's [`letibot_transcript::CallOrigin`] is what makes it knowable, and it is the
/// whole reason the field is on the row: a `ToolResult` the daemon appended for a call the
/// OPERATOR ran — the `bash` behind their `!` line, a `/web-fetch` from their own console —
/// carries `Some(CallOrigin::Operator { who })`, while one the model proposed carries `None`.
/// A head that inferred it from *no proposing assistant row above* would be guessing, which is
/// the defect the field was added to end.
///
/// **The `User` row beside such a result is the operator's too**, and it needs no predicate
/// here: `User` is kept at every rung already, because it is the conversation. This answers only
/// the half that was hidden — see [`Verbosity::keeps`] for the ruling and the measurement, and
/// [`Visibility::keeps`] for why it is asked before the switch list.
pub(crate) fn operator_act(item: &letibot_transcript::TranscriptItem) -> bool {
    matches!(
        item,
        letibot_transcript::TranscriptItem::ToolResult {
            origin: Some(letibot_transcript::CallOrigin::Operator { .. }),
            ..
        }
    )
}

/// **Is this row one that says what the head CHANGED** — the one predicate `read-edits` differs
/// by, and the only place it is decided.
///
/// **Two signals, both the tree's own** (leticl's `item-shows-an-edit-p`, `src/cards/hidden-run.lisp:53`):
/// the daemon sends the excerpt on a finished call — which is the signal a `bash` command
/// carries when the file changed under it — and the call's NAME through [`card::Verb::of`],
/// where `edit`, `patch`, `apply_patch`, `str_replace`, `write`, `write_file` and `create` are the
/// two verbs.
///
/// **An unknown tool name is not an edit**, and neither is a call whose outcome is not `Ok` —
/// leticl: *"this predicate is allowed to be wrong in the direction of hiding, never in the
/// direction of claiming."* A refused write changed nothing, and the renderer draws no diff for
/// it either (`item_lines`' own gate), so the two agree about the same row.
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

/// `a, b and c` — the refusal and the card name a list, and a join written per call site is a
/// join that disagrees with the one beside it about the last comma.
pub(crate) fn names(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
