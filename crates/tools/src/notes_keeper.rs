//! **The notes keeper — the reviewer that checks whether a note is still true, and proposes
//! removals.**
//!
//! The design, in the operator's own words: *"and then we need a periodic poke to review notes
//! for being up-to-dater"*, *"id even say us much us the actuality check is a job for another
//! hardcoded subagent"*, *"it can then chat with the main session"*, and *"like propose
//! removals"*.
//!
//! # A second instance of a shape this tree already has
//!
//! The precedent is [`crate::gatekeeper`], and the shape is copied deliberately rather than
//! reinvented: **a fixed prompt, a closed set of answers, and an answer that is parsed rather
//! than read as prose.** A notes keeper is not a new kind of thing.
//!
//! | | gatekeeper | notes keeper |
//! |---|---|---|
//! | asked with | [`crate::gatekeeper::review_prompt`] | [`keeper_prompt`] |
//! | the closed set | `accept \| reject \| needs_human` | [`KeeperDecision`] |
//! | the answer is read by | [`crate::gatekeeper::parse_verdict`] | [`parse_report`] |
//! | its seat | [`crate::runtime::roles::gatekeeper`] | [`crate::runtime::roles::notes_keeper`] |
//! | the seam the daemon lands on | [`crate::gatekeeper::wake`] | [`wake`] |
//!
//! # Its brief, in this order — the order IS the design
//!
//! 1. **The references first**, because they are checkable and they produce evidence rather than
//!    a feeling. [`crate::references`] resolves them *before* the session is asked, and the
//!    answer is handed over in the prompt: paths, commit shas, symbols and line ranges, and
//!    which of them no longer resolve.
//! 2. **Then the prose**, against the tree — the diff between what the note claims and what the
//!    tree now says.
//! 3. **Then the repair, through the `notes` tool.** Not through a file write and not through an
//!    edit: see [`roles::notes_keeper`](crate::runtime::roles::notes_keeper), whose seat has
//!    `notes` and no `write`, `edit` or `bash` at all. **That is what preserves authorship.** A
//!    keeper's edit is still an agent's edit — it lands through the same tool, into the same
//!    `.letibot/notes/` directory, keeping the author's abstract marker and the historical
//!    caveat every `read` of a note carries — so a note stays a record rather than becoming an
//!    authority. A keeper that could `write` a note file could make its own prose
//!    indistinguishable from the operator's, which is the one thing the notes tool's whole
//!    design is built to prevent.
//! 4. **And it raises what it cannot judge** — the `raised:` field, which reaches the session
//!    that ran it. (The lateral channel that will carry this across sessions is
//!    `agent/session-channel`; until it lands, the answer comes back the ordinary way, which is
//!    the path the merge queue's verdicts already take.)
//!
//! # Its answer is a closed set, and it says what it changed
//!
//! [`KeeperDecision`] is the four words the daemon can act on, and the failure it exists to
//! prevent is the design's own: *"the failure mode of a poke is an unactionable essay nobody
//! reads."*
//!
//! **And the diff of somebody's memory is the operator's to see**, exactly as a queue verdict
//! is. Two things carry it, and they are different in kind:
//!
//! * the keeper's own account (`changed:`, parsed into [`KeeperReport::changed`]) — which is a
//!   claim, and this tree's rule about claims by the party under review is to distrust them
//!   (the gatekeeper is given no report for exactly this reason);
//! * [`edits`], computed by the caller from the note's bytes before and after the session ran —
//!   which is evidence. [`KeeperReport::unaccounted`] names where the two disagree, and a
//!   disagreement is itself worth the operator's attention: a keeper that edited a note and did
//!   not say so, or said so and did not.
//!
//! # And it proposes removals, which are not acts
//!
//! A keeper that can only repair leaves the corpus growing for ever, and the index gets worse
//! exactly as the corpus gets bigger. So its report carries [`Proposal`]s, each with an
//! [`Evidence`] kind and the evidence itself:
//!
//! | evidence | how it is established |
//! |---|---|
//! | `references` | [`crate::references`] — the cheapest honest signal, and mechanical |
//! | `superseded` | the similarity door ([`Supersession`]), the same arithmetic as the write gate |
//! | `never_taken` | [`offers_log`] — `<workspace>/.letibot/notes-offers.jsonl` |
//! | `landed` | the work the note describes finished and landed; the keeper checks the shas |
//!
//! **A proposal is not an act.** Deleting a note is the one move that loses information rather
//! than reorganising it, so the keeper proposes and the decision is the session's, or the
//! person's — which is why a review that proposes anything is [`KeeperDecision::NeedsPerson`].
//!
//! # What is deliberately NOT here: the clock
//!
//! The design wants a **per-workspace periodic** — *"the nag clock that landed tonight is
//! per-session and per-plan; this clock is per-workspace — one, whoever is attached"* — and this
//! module is the role that clock would wake, not the clock. It is deferred for two reasons, both
//! of them the tree's:
//!
//! * **The scope does not exist.** There is no per-workspace scheduler in this tree; the nag
//!   clock (`8bd6fb8`) is per-session and per-plan, and `agent/idle-clock` (`e7db232`) is
//!   unlanded and editing the daemon's idle arm. Two unlanded writers on that arm is a certain
//!   conflict, which is the same reason the plan-as-DAG waits.
//! * **The pieces are separable, and this is the separable half.** Everything a clock would
//!   *call* is here and exercised by tests: the seat, the prompt, the parse, the reference
//!   checker and the repair path. A clock is then a small piece that does nothing but decide
//!   *when*.
//!
//! What that piece would need, named so it can be written afterwards:
//!
//! 1. **Where it would live.** A per-workspace clock belongs to the daemon, beside the sessions
//!    it serves — not to a session, because the notes are workspace files every session shares
//!    and the clock must be one, whoever is attached.
//! 2. **What scope it holds.** A scope keyed by the workspace path, owned by the daemon, holding
//!    one entry per workspace with the last review's timestamp, the tree's tip SHA at that
//!    review, and the note it named. The design's period is not wall-clock alone: *"the tree
//!    moved since the note was written"*, *"an offer was never taken"*, and *"a long backstop
//!    period, so a quiet tree still gets a sweep."* The tip SHA is what makes the first of those
//!    observable without walking every note.
//! 3. **How it would be armed and observed.** It is armed by a turn ending (the same primitive
//!    the child-nag clock uses: a clock that wakes the thread owning the work) and observed by a
//!    row — the keeper's report belongs on the standing-notes pane the design already asks for
//!    (*"the keeper's report lands there — stale references, what changed, proposals to remove —
//!    exactly as queue verdicts land on the queue's rows"*), which is a piece of the TUI.
//! 4. **And a hidden child needs a mark.** The daemon spawns the keeper as a child of the session
//!    it serves, exactly as `mergequeue::GatekeeperDoor` spawns the gatekeeper — and the head
//!    keeps that child out of the subagents it counts by reading
//!    `letibot_sessionlog::GATEKEEPER_TITLE_PREFIX`. [`keeper_prompt`] begins with
//!    [`TITLE_PREFIX`] for that reason, and the constant and the head's rule are the two lines
//!    that piece has to add.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::gatekeeper::{bullets, commas, label};
use crate::references::{self, ReferenceReport};

/// **The first words of the keeper's brief** — the mark a head reads to keep a hidden child out
/// of the subagents the operator sees, on `GATEKEEPER_TITLE_PREFIX`'s rule.
///
/// The constant is *not* in `letibot-sessionlog` yet, and that is deliberate rather than an
/// oversight: nothing spawns a keeper until the clock lands, and a mark nothing reads is a mark
/// nobody can get wrong. The prompt begins with these words now so that the piece which adds the
/// spawn has nothing to change here.
pub const TITLE_PREFIX: &str = "You are the notes keeper.";

/// **The closed set the daemon can act on** — the four words a review comes back as.
///
/// The design's own list, verbatim: *"a closed set the daemon can act on (checked / stale
/// references / changed / needs a person)"*, and its reason: *"the failure mode of a poke is an
/// unactionable essay nobody reads."*
///
/// The **precedence is the order of the variants**, and the prompt states it as the rule: the
/// first that is true is the answer. `NeedsPerson` is first because a proposal to remove a note
/// is a decision only a person (or the session that ran the keeper) can make, and a review that
/// proposes one must not come back as `checked` and be filed away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum KeeperDecision {
    /// The keeper could not settle whether the note is still true — or it is proposing a
    /// removal. A person, or the session that ran the keeper, decides.
    ///
    /// **The default, and it is the safe direction on purpose**: a decision nobody set parks the
    /// note for a person rather than filing it away as `checked`.
    #[default]
    NeedsPerson,
    /// The tree moved under the note's prose, and the keeper repaired it through `notes`.
    Changed,
    /// A reference no longer resolves. The keeper repaired it where it could, or said why not.
    StaleReferences,
    /// The references resolve and the prose still holds: nothing to do.
    Checked,
}

impl KeeperDecision {
    /// **Every decision, in the order the daemon reads them** — the one list, for the reason
    /// [`crate::gatekeeper::Decision::ALL`] is one: the parse and the rendering both ask *which
    /// of the values is this*, and a second list is a second answer. The order is the
    /// precedence, most-demanding first.
    pub const ALL: [KeeperDecision; 4] = [
        KeeperDecision::NeedsPerson,
        KeeperDecision::Changed,
        KeeperDecision::StaleReferences,
        KeeperDecision::Checked,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            KeeperDecision::Checked => "checked",
            KeeperDecision::StaleReferences => "stale_references",
            KeeperDecision::Changed => "changed",
            KeeperDecision::NeedsPerson => "needs_person",
        }
    }

    /// The decision a word names, if it names one. `None` for a word outside the set, and that
    /// is a refusal rather than a default: reading an unknown word as `checked` would file away
    /// a note nobody reviewed.
    pub fn parse(stored: &str) -> Option<KeeperDecision> {
        Self::ALL.into_iter().find(|d| d.as_str() == stored)
    }
}

/// **Why a note should be removed** — a closed set, because a proposal whose evidence cannot be
/// read is a proposal nobody can weigh.
///
/// Each of the four is a different *question*, and each is established by a different piece of
/// machinery. The words are the ones the report renders and the parse reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    /// References that no longer resolve — [`crate::references`], mechanical.
    References,
    /// A note superseded by another — the similarity door ([`Supersession`]), the same
    /// arithmetic as the write gate, so the same floor.
    Superseded,
    /// Offered and never taken — [`offers_log`]. An offer nobody ever takes is a note nobody
    /// reads, which is a note to rewrite or retire.
    NeverTaken,
    /// The work it describes finished and landed — the note is a record of a branch that is now
    /// on `main`, and what is left of it is history.
    Landed,
}

impl Evidence {
    pub const ALL: [Evidence; 4] = [
        Evidence::References,
        Evidence::Superseded,
        Evidence::NeverTaken,
        Evidence::Landed,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Evidence::References => "references",
            Evidence::Superseded => "superseded",
            Evidence::NeverTaken => "never_taken",
            Evidence::Landed => "landed",
        }
    }

    pub fn parse(stored: &str) -> Option<Evidence> {
        Self::ALL.into_iter().find(|e| e.as_str() == stored)
    }
}

/// One note the keeper proposes removing, and why.
///
/// **A proposal, never an act.** The design: *"a finished row is a record of work, and only an
/// act takes it off. Here the keeper proposes and the decision is the main session's — or the
/// person's, when it matters."*
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// The note proposed for removal — a path, as the report spells it.
    pub note: String,
    pub evidence: Evidence,
    /// The evidence itself, in the keeper's words: which references, which other note and its
    /// score, how many offers and how many take-ups, which commit landed.
    pub because: String,
}

impl Proposal {
    /// The one line the report renders a proposal as — and the shape the prompt asks for, so
    /// that what a reader sees is what the keeper wrote.
    pub fn render(&self) -> String {
        format!(
            "remove [{}] {} — {}",
            self.evidence.as_str(),
            self.note,
            self.because
        )
    }
}

/// What the offer log says about one note.
///
/// The take-up is the measurement that keeps the offer floor honest, and it answers a second
/// question the design names: *"an offer nobody ever takes is a note to rewrite or retire."*
#[derive(Debug, Clone, PartialEq)]
pub struct Offered {
    pub path: PathBuf,
    /// How many times the note was offered at the tail of a request.
    pub offers: usize,
    /// How many of those the round actually **read**. A `grep` that mentions a note does not
    /// count — the take-up signal is a `read` of that path, which is the design's own rule.
    pub taken: usize,
    /// The best score the note ever earned, and when it was last offered, for the evidence line.
    pub best: f64,
    pub last_at: Option<i64>,
}

impl Offered {
    /// Offered, and never taken.
    pub fn never_taken(&self) -> bool {
        self.offers > 0 && self.taken == 0
    }

    /// The evidence line a `never_taken` proposal carries.
    pub fn render(&self) -> String {
        format!(
            "offered {} time(s), read {} time(s), best score {:.3}",
            self.offers, self.taken, self.best
        )
    }
}

/// **Read the take-up log** — `<workspace>/.letibot/notes-offers.jsonl`, one line per settled
/// offer.
///
/// # This reader is deliberately tolerant, and the writer says why
///
/// The offering half owns the file and writes one JSON object per offer (`path`, `score`,
/// `taken`, `at`, `session`). It is **best-effort by its own design** — *"a hint that fails to
/// write a line about itself is still a hint, and a notes feature that breaks a turn because a
/// file could not be appended to would be worse than no notes"* — so a malformed line, an
/// unknown field or a missing file is *no evidence* rather than an error. A keeper that refused
/// to run because a log line was truncated would be a keeper that never runs.
///
/// The file does not exist until the offering half lands. An absent log is an empty log, and
/// the prompt says so rather than pretending the question was answered.
pub fn offers_log(workspace: &Path) -> Vec<Offered> {
    let path = workspace.join(".letibot").join("notes-offers.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut by_path: std::collections::BTreeMap<PathBuf, Offered> =
        std::collections::BTreeMap::new();
    for line in text.lines() {
        let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(p) = row.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        let taken = row.get("taken").and_then(|v| v.as_bool()).unwrap_or(false);
        let score = row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let at = row.get("at").and_then(|v| v.as_i64());
        let entry = by_path.entry(PathBuf::from(p)).or_insert_with(|| Offered {
            path: PathBuf::from(p),
            offers: 0,
            taken: 0,
            best: 0.0,
            last_at: None,
        });
        entry.offers += 1;
        if taken {
            entry.taken += 1;
        }
        entry.best = entry.best.max(score);
        if at > entry.last_at {
            entry.last_at = at;
        }
    }
    by_path.into_values().collect()
}

/// **The score at which one note is called a duplicate of another.**
///
/// MEASURED, and the number is not this module's: it is the write gate's floor, where the
/// closest honest pair in this box's corpus scored **0.183** and the weakest near-copy **0.363**,
/// so 0.30 sits in the gap. *"A duplicate gate and a supersession proposal are the same
/// arithmetic asking the same question"* — and when `letibot_tools::similarity` lands this
/// constant should be replaced by an import of `builtins::notes::DUPLICATE_FLOOR`, so that the
/// two cannot drift apart.
pub const SUPERSEDED_FLOOR: f64 = 0.30;

/// What the similarity door said about this note.
#[derive(Debug, Clone, PartialEq)]
pub enum Nearest {
    /// **No door is attached**, so nobody asked. The keeper is told this in as many words, and
    /// it is the reason it must not propose a `superseded` removal: a supersession is a
    /// similarity judgement, and there is no score to cite.
    Unavailable,
    /// The door was asked and no other note reached [`SUPERSEDED_FLOOR`].
    NothingClose,
    /// The note most like this one, and the score that makes it so.
    Note { path: PathBuf, score: f64 },
}

/// **The door a "superseded by another note" judgement goes through.**
///
/// The arithmetic is `letibot_tools::similarity`'s — cosine over TF-IDF on a note's title,
/// abstract and headings, with its floors measured rather than chosen — and it is **not in this
/// tree**: the offering half (`agent/notes-offer`) owns it and is enqueued and unlanded. So the
/// keeper asks through a door, exactly as [`crate::gatekeeper::wake`] is the door the merge queue
/// lands on.
///
/// A keeper is a **caller** of that arithmetic, never a second implementation of it: two
/// similarity scores one crate apart are two answers to one question, and the floor that decides
/// whether a note is a duplicate would then be a floor that means two things.
pub trait Supersession {
    /// The note most like `text`, and its score, **excluding `skip`** — a note is never its own
    /// duplicate. `None` when the door cannot say, which the keeper is told rather than guessed
    /// around.
    fn nearest(&self, text: &str, skip: &Path) -> Option<(PathBuf, f64)>;
}

/// A door with no arithmetic behind it: nobody asked, and the keeper is told so.
pub struct NoSupersession;

impl Supersession for NoSupersession {
    fn nearest(&self, _text: &str, _skip: &Path) -> Option<(PathBuf, f64)> {
        None
    }
}

/// **Everything the keeper is asked with, and nothing else.**
///
/// The gatekeeper's request is deliberately three fields, with no room for the child's report.
/// This one is deliberately **gathered** rather than assembled by hand: the references are
/// resolved, the offer log is read and the similarity door is asked by [`KeeperRequest::gather`],
/// so that the prompt is a function of evidence somebody actually collected. A field a caller
/// could fill in with a guess is a field that will be.
#[derive(Debug, Clone)]
pub struct KeeperRequest {
    /// The note under review.
    pub note: PathBuf,
    /// The workspace the note belongs to — the directory whose `.letibot/notes/` the harness
    /// reads, and the roots the references were resolved against
    /// ([`crate::references::tree_roots`]).
    pub workspace: PathBuf,
    /// The note, verbatim. The artifact, given whole, on the brief-first rule.
    pub text: String,
    /// What the checker found. Empty of references is a real answer: the note cites nothing this
    /// can resolve.
    pub references: ReferenceReport,
    /// Every note the offer log knows about, this one among them.
    pub offers: Vec<Offered>,
    /// What the similarity door said.
    pub superseded: Nearest,
}

impl KeeperRequest {
    /// **Gather the evidence for one note** — the three things the prompt is built from, in the
    /// order the brief puts them in.
    pub fn gather(
        workspace: &Path,
        note: &Path,
        supersession: &dyn Supersession,
    ) -> Result<KeeperRequest, String> {
        let text = std::fs::read_to_string(note)
            .map_err(|e| format!("`{}` could not be read: {e}", note.display()))?;
        let references = references::check(workspace, &text);
        let offers = offers_log(workspace);
        let superseded = match supersession.nearest(&text, note) {
            Some((path, score)) if score >= SUPERSEDED_FLOOR => Nearest::Note { path, score },
            Some(_) => Nearest::NothingClose,
            None => Nearest::Unavailable,
        };
        Ok(KeeperRequest {
            note: note.to_path_buf(),
            workspace: workspace.to_path_buf(),
            text,
            references,
            offers,
            superseded,
        })
    }

    /// What the log says about **this** note, when it says anything.
    pub fn offered_here(&self) -> Option<&Offered> {
        self.offers
            .iter()
            .find(|o| o.path == self.note || self.note.ends_with(&o.path))
    }
}

/// **The exact text the keeper is asked with** — the fixed prompt, a function of the request and
/// nothing else.
///
/// The brief's order is the rules' order, and that is the design rather than a layout choice:
/// the references first because they are checkable, the prose next, the repair third, and what
/// cannot be judged last. The closing block is stated field by field, on the gatekeeper's own
/// reasoning: *"re-parsing prose to recover a label is how a corpus rots"* — and the label here
/// decides whether a note is rewritten, repaired, or put in front of a person.
pub fn keeper_prompt(req: &KeeperRequest) -> String {
    format!(
        "{TITLE_PREFIX} The standing notes are the memory this harness injects into every \
         session, and nothing has ever reviewed one. A note records what was true when somebody \
         wrote it; the tree has moved since. You review ONE note against the tree, you repair \
         what you can through the `notes` tool, and you raise what you cannot judge.\n\n\
         The note: {note}\n\
         The workspace: {workspace}\n\n\
         The note, verbatim:\n\n\
         {text}\n\n\
         ---\n\n\
         **The references, already resolved mechanically against the tree.** This is the evidence \
         you start from, and a reader of your report will check it first. Do not re-derive it: if \
         you think the checker read something wrong, say so under `raised:` rather than quietly \
         disagreeing.\n\n\
         {references}\n\
         **The offer log for this note.** {offers}\n\n\
         **The corpus's nearest note.** {superseded}\n\n\
         The rules, in this order — the order is the design:\n\n\
         1. **The references first.** Go through the ones that did not resolve. Decide, for each, \
         whether the note is wrong or the checker's reading is: a path cited relative to another \
         tree, a symbol quoted from a quotation, a line range a note about a DIFFERENT version of \
         the file is allowed to exceed.\n\
         2. **Then the prose, against the tree.** The note's claims are about code, branches, \
         commits and behaviour. `read`, `grep` and `glob` are yours; you have no shell and you do \
         not need one — the commits in the note were looked up for you above. Find the sentences \
         the tree now contradicts, and say which sentence and what the tree says instead.\n\
         3. **Then the repair, through the `notes` tool.** Repair with `notes` action=\"append\" or \
         action=\"replace\" — never by writing a file (you have no `write` or `edit`), and never \
         by touching code. That is what keeps the note a note: the tool writes into \
         `.letibot/notes/` only, keeps the author's abstract line, and every read of it carries \
         the caveat that says it is a record. **Do not rewrite history to make a note look \
         current.** A sentence that says MEASURED on a date is a record of a measurement, not a \
         claim about now; what you add or correct is the reference that moved, the fact the tree \
         has changed, and the sentence that now reads as an instruction nobody should follow. \
         When you leave a stale reference in place deliberately, say so under `reasons:`.\n\
         4. **Raise what you cannot judge.** A claim you cannot check, a reference the checker \
         could not resolve, a question only the operator can answer — put it under `raised:` in \
         your own words. It reaches the session that ran you.\n\
         5. **And propose removals, each with its evidence.** A note whose references no longer \
         resolve, one superseded by another, one offered and never taken, or one whose work has \
         finished and landed — the corpus cannot keep growing, and the index gets worse as it \
         does. **A proposal is not an act:** you never delete a note. You name it, you say which \
         of the four kinds of evidence it is, and the session that ran you decides.\n\n\
         You may reason in prose for as long as you like. **Then end your reply with exactly this \
         block and nothing after it**, one field per line, the labels spelled exactly as they are \
         here:\n\n\
         decision: {decisions}\n\
         reasons: one reason per line, each beginning with `- `\n\
         changed: one line per note you changed, each beginning with `- `, as `<note path> — what \
         you changed`; `none` if you changed nothing\n\
         proposals: one line per note you propose removing, each beginning with `- `, as \
         `remove [<evidence>] <note path> — the evidence`; `none` if you propose nothing\n\
         raised: one line per thing you could not judge, each beginning with `- `; `none` if \
         there is nothing\n\
         files: comma-separated paths you read, or `none`\n\n\
         `decision:` is the only line that is required, and it is the first of these four that is \
         true:\n\
         - `needs_person` — you could not settle whether the note is still true, or you are \
         proposing a removal. Only a person, or the session that ran you, can decide.\n\
         - `changed` — the tree moved under the note's prose, and you repaired it.\n\
         - `stale_references` — a reference does not resolve, and you repaired it (or said under \
         `reasons:` why you did not).\n\
         - `checked` — the references resolve and the prose still holds: nothing to do.\n\n\
         `<evidence>` is one of {evidences}. A reply whose closing block is missing, whose \
         `decision:` is not one of the four words, or whose proposal line is not in the shape \
         above, is refused by name and the note is left exactly as it was — a report nobody can \
         read is a report the daemon cannot act on.",
        note = req.note.display(),
        workspace = req.workspace.display(),
        text = req.text.trim_end(),
        references = req.references.render(),
        offers = describe_offers(req),
        superseded = describe_nearest(&req.superseded),
        decisions = KeeperDecision::ALL
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>()
            .join(" | "),
        evidences = Evidence::ALL
            .iter()
            .map(|e| format!("`{}`", e.as_str()))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// What the offer log says, as the prompt's own sentence — including the case where the log does
/// not exist yet, which is the case today.
fn describe_offers(req: &KeeperRequest) -> String {
    match req.offered_here() {
        Some(o) if o.never_taken() => format!(
            "**it has been {} — this is the evidence for a `never_taken` proposal.**",
            o.render()
        ),
        Some(o) => format!("{}.", o.render()),
        None if req.offers.is_empty() => {
            "the log is empty or absent, so nobody has ever offered this note or any other — no \
             `never_taken` evidence either way."
                .to_string()
        }
        None => "this note has never been offered, so there is no take-up to read.".to_string(),
    }
}

/// What the similarity door said, as the prompt's own sentence — and when it was not asked, the
/// keeper is told not to guess.
fn describe_nearest(nearest: &Nearest) -> String {
    match nearest {
        Nearest::Unavailable => format!(
            "**not consulted.** No similarity door is attached to this daemon — the arithmetic is \
             `agent/notes-offer`'s and it is not in this tree yet. You therefore have NO score, \
             and a `superseded` proposal without one is a guess: do not make one."
        ),
        Nearest::NothingClose => format!(
            "no other note reached the {SUPERSEDED_FLOOR:.2} floor at which one note is called a \
             duplicate of another, so there is no `superseded` evidence here."
        ),
        Nearest::Note { path, score } => format!(
            "`{}` scored {score:.3}, at or above the {SUPERSEDED_FLOOR:.2} floor — if this note \
             says what that one says, that is `superseded` evidence, and your proposal must name \
             it and the score.",
            path.display()
        ),
    }
}

/// **The keeper's answer, read out of the shape the prompt asks for** — the one place a reply
/// becomes a report.
///
/// The prompt states a closing block and this reads exactly that, from the LAST `decision:` line
/// onwards: prose above it is the keeper thinking, which is worth reading in the session's
/// transcript and is not evidence.
///
/// **A reply without a readable decision is an `Err`, never a default.** Reading an unknown word
/// as `checked` would file away a note nobody reviewed; reading it as `needs_person` would put
/// work in front of the operator on the strength of a typo. And a proposal whose evidence cannot
/// be read is refused for the same reason one step down: it is a request to delete somebody's
/// memory, and a request nobody can weigh is not one.
pub fn parse_report(req: &KeeperRequest, said: &str) -> Result<KeeperReport, String> {
    let lines: Vec<&str> = said.lines().collect();
    let Some(start) = lines.iter().rposition(|l| label(l, "decision").is_some()) else {
        return Err(format!(
            "the keeper's reply for `{}` has no `decision:` line, so there is no report to read. \
             The reply is in the keeper's own session, and the note is left exactly as it was.",
            req.note.display()
        ));
    };
    let word = label(lines[start], "decision").unwrap_or("").trim();
    let Some(decision) = KeeperDecision::parse(word) else {
        return Err(format!(
            "the keeper's reply for `{}` ends with `decision: {word}`, which is not one of {}. A \
             decision outside the closed set is not a decision, and the note is left as it was.",
            req.note.display(),
            KeeperDecision::ALL
                .iter()
                .map(|d| d.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let mut answer = KeeperAnswer {
        decision,
        ..KeeperAnswer::default()
    };
    let mut current: Option<&str> = None;
    for line in &lines[start + 1..] {
        if let Some(v) = label(line, "reasons") {
            current = Some("reasons");
            answer.reasons.extend(bullets(v));
        } else if let Some(v) = label(line, "changed") {
            current = Some("changed");
            answer.changed.extend(bullets(v));
        } else if let Some(v) = label(line, "raised") {
            current = Some("raised");
            answer.raised.extend(bullets(v));
        } else if let Some(v) = label(line, "files") {
            current = Some("files");
            answer.files.extend(commas(v));
        } else if let Some(v) = label(line, "proposals") {
            current = Some("proposals");
            for bullet in bullets(v) {
                answer.proposals.push(proposal(&bullet, &req.note)?);
            }
        } else if let Some(bullet) = line.trim().strip_prefix("- ") {
            // A continuation line under the last label, on the gatekeeper's rule: a model that
            // writes the label once and then a list has answered the question, one layout over.
            let b = bullet.trim();
            if b.is_empty() {
                continue;
            }
            if current == Some("proposals") {
                answer.proposals.push(proposal(b, &req.note)?);
            } else {
                answer.reasons.push(b.to_string());
            }
        }
    }
    Ok(KeeperReport::for_request(req, answer))
}

/// One proposal line, with the refusal wrapped in the note it is about.
fn proposal(line: &str, note: &Path) -> Result<Proposal, String> {
    parse_proposal(line).map_err(|why| {
        format!(
            "the keeper's proposal `{line}` for `{}` could not be read: {why} Nothing was taken \
             from it — a request to delete a note has to be legible.",
            note.display()
        )
    })
}

/// `remove [<evidence>] <note path> — the evidence`, as the prompt asks for it.
fn parse_proposal(line: &str) -> Result<Proposal, String> {
    let t = line.trim();
    let Some(rest) = t.strip_prefix("remove") else {
        return Err(format!(
            "every proposal line begins with `remove [<evidence>] <note path> — <the evidence>`; \
             this one begins `{}`",
            t.chars().take(24).collect::<String>()
        ));
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('[') else {
        return Err(format!(
            "the evidence kind must be in square brackets, one of {}; this line has `{}`",
            Evidence::ALL
                .iter()
                .map(|e| e.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            rest.chars().take(24).collect::<String>()
        ));
    };
    let Some((kind, rest)) = rest.split_once(']') else {
        return Err("the evidence kind's `[` is never closed".into());
    };
    let Some(evidence) = Evidence::parse(kind.trim()) else {
        return Err(format!(
            "`{}` is not one of {}",
            kind.trim(),
            Evidence::ALL
                .iter()
                .map(|e| e.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let rest = rest.trim();
    // The separator's length comes from the literal itself: `" — ".len()` is five bytes (two
    // spaces and a three-byte em dash) and a hand-written `3` here was a real bug, caught by a
    // test slicing inside the dash.
    let (at, sep) =
        match (rest.find(" — "), rest.find(": ")) {
            (Some(d), Some(c)) if c < d => (c, 2),
            (Some(d), _) => (d, " — ".len()),
            (None, Some(c)) => (c, 2),
            (None, None) => return Err(
                "the note and the evidence must be separated by ` — ` or `: `, so that a reader \
                 can tell which is which"
                    .to_string(),
            ),
        };
    let note = rest[..at].trim().to_string();
    let because = rest[at + sep..].trim().to_string();
    if note.is_empty() || because.is_empty() {
        return Err("either the note or the evidence is empty".into());
    }
    Ok(Proposal {
        note,
        evidence,
        because,
    })
}

/// The keeper's answer, before it is attached to the note it is about.
///
/// `Default` is derived and its decision is [`KeeperDecision::NeedsPerson`] — the safe
/// direction, so a struct literal that forgets the field parks the note for a person rather than
/// filing it away. The parse always sets it from the reply's own word.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeeperAnswer {
    pub decision: KeeperDecision,
    pub reasons: Vec<String>,
    /// The keeper's own account of what it changed — a claim, not the evidence. The evidence is
    /// [`KeeperReport::diffs`].
    pub changed: Vec<String>,
    pub proposals: Vec<Proposal>,
    /// What it could not judge, in its own words. This is what reaches the session that ran it.
    pub raised: Vec<String>,
    /// The files it read.
    pub files: Vec<String>,
}

/// **The keeper's report** — the note it is about, the answer, and the diff of the memory it
/// touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperReport {
    /// The note reviewed. It comes from the request ([`KeeperReport::for_request`]), so a report
    /// can never name a note nobody asked about.
    pub note: PathBuf,
    pub decision: KeeperDecision,
    pub reasons: Vec<String>,
    /// What the keeper says it changed.
    pub changed: Vec<String>,
    pub proposals: Vec<Proposal>,
    pub raised: Vec<String>,
    pub files: Vec<String>,
    /// **What the caller measured**: the note's bytes before the keeper ran and after. Empty
    /// until [`KeeperReport::measured`] is called, and the report says so rather than implying
    /// nothing changed.
    pub diffs: Vec<Edit>,
}

impl KeeperReport {
    /// A report against the request it was asked about: the note comes from the request, so a
    /// report can never be filed against a note nobody reviewed.
    pub fn for_request(req: &KeeperRequest, answer: KeeperAnswer) -> Self {
        KeeperReport {
            note: req.note.clone(),
            decision: answer.decision,
            reasons: answer.reasons,
            changed: answer.changed,
            proposals: answer.proposals,
            raised: answer.raised,
            files: answer.files,
            diffs: Vec::new(),
        }
    }

    /// Attach the diff the caller measured — see [`edits`].
    pub fn measured(mut self, diffs: Vec<Edit>) -> Self {
        self.diffs = diffs;
        self
    }

    /// **Where the keeper's own account and the disk disagree.**
    ///
    /// A keeper is the party under review, so its account of its own edit is the weakest form of
    /// the evidence — and the tree's answer to that is to compare it against something it did
    /// not write. Two shapes, and both are worth a reader's attention: a note that changed on
    /// disk and is not named under `changed:`, and a note named under `changed:` that did not
    /// change.
    pub fn unaccounted(&self) -> Vec<String> {
        let mut out = Vec::new();
        for d in &self.diffs {
            let named = self.changed.iter().any(|c| {
                c.contains(&d.path.display().to_string()) || c.contains(&file_name(&d.path))
            });
            if !named {
                out.push(format!(
                    "{} changed on disk and the report does not name it",
                    d.path.display()
                ));
            }
        }
        for c in &self.changed {
            let measured = self.diffs.iter().any(|d| {
                c.contains(&d.path.display().to_string()) || c.contains(&file_name(&d.path))
            });
            if !measured && !self.diffs.is_empty() {
                out.push(format!(
                    "the report says it changed `{}` and nothing on disk did",
                    clip(c)
                ));
            }
        }
        out
    }

    /// The operator-visible rendering, in the house voice — what a queue verdict's rendering is
    /// to the merge queue.
    pub fn render(&self) -> String {
        let mut out = format!(
            "notes keeper report\n  note      {}\n  decision  {}\n",
            self.note.display(),
            self.decision.as_str()
        );
        out.push_str("  reasons\n");
        push_list(&mut out, &self.reasons);
        out.push_str("  changed (the keeper's own account)\n");
        push_list(&mut out, &self.changed);
        out.push_str("  changed (measured)\n");
        if self.diffs.is_empty() {
            out.push_str("    (no diff was measured — the note is byte-identical to before)\n");
        } else {
            for d in &self.diffs {
                out.push_str(&d.render());
            }
        }
        out.push_str("  proposals (not acts)\n");
        if self.proposals.is_empty() {
            out.push_str("    (none)\n");
        } else {
            for p in &self.proposals {
                out.push_str(&format!("    - {}\n", p.render()));
            }
        }
        out.push_str("  raised\n");
        push_list(&mut out, &self.raised);
        out.push_str("  looked at\n");
        if self.files.is_empty() {
            out.push_str("    (nothing named)\n");
        } else {
            out.push_str(&format!("    files {}\n", self.files.join(", ")));
        }
        let unaccounted = self.unaccounted();
        if !unaccounted.is_empty() {
            out.push_str("  unaccounted\n");
            push_list(&mut out, &unaccounted);
        }
        out
    }
}

fn push_list(out: &mut String, items: &[String]) {
    if items.is_empty() {
        out.push_str("    (none)\n");
        return;
    }
    for i in items {
        out.push_str(&format!("    - {i}\n"));
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn clip(s: &str) -> String {
    const CAP: usize = 80;
    if s.chars().count() <= CAP {
        return s.to_string();
    }
    format!("{}…", s.chars().take(CAP).collect::<String>())
}

/// **One note's bytes before and after the keeper ran** — the diff of somebody's memory, which
/// is the operator's to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
}

impl Edit {
    /// A line diff with the common head and tail trimmed off, which is what makes an append read
    /// as an append rather than as a whole note rewritten. Not an LCS: a keeper's edits are
    /// additions and corrections in a note of a few hundred lines, and a prefix/suffix trim is
    /// the whole of what a reader needs to see them.
    pub fn render(&self) -> String {
        const CAP: usize = 40;
        let before: Vec<&str> = self.before.lines().collect();
        let after: Vec<&str> = self.after.lines().collect();
        let mut head = 0;
        while head < before.len() && head < after.len() && before[head] == after[head] {
            head += 1;
        }
        let mut tail = 0;
        while tail < before.len().saturating_sub(head)
            && tail < after.len().saturating_sub(head)
            && before[before.len() - 1 - tail] == after[after.len() - 1 - tail]
        {
            tail += 1;
        }
        let gone = &before[head..before.len() - tail];
        let added = &after[head..after.len() - tail];
        let mut out = format!("    ### {}\n", self.path.display());
        if gone.is_empty() && added.is_empty() {
            out.push_str("      (no line changed)\n");
            return out;
        }
        for l in gone.iter().take(CAP) {
            out.push_str(&format!("      - {l}\n"));
        }
        if gone.len() > CAP {
            out.push_str(&format!(
                "      - … {} more line(s) removed\n",
                gone.len() - CAP
            ));
        }
        for l in added.iter().take(CAP) {
            out.push_str(&format!("      + {l}\n"));
        }
        if added.len() > CAP {
            out.push_str(&format!(
                "      + … {} more line(s) added\n",
                added.len() - CAP
            ));
        }
        out
    }
}

/// **The notes that changed between two readings of the corpus**, as diffs.
///
/// The caller reads the notes' bytes before it wakes the keeper and after the keeper's session
/// has run, and this says what moved. A note that is gone from `after` is an `Edit` too, with an
/// empty `after`: the keeper has no tool that can delete a note, and a note that vanished
/// anyway is exactly the thing the operator has to see.
pub fn edits(before: &[(PathBuf, String)], after: &[(PathBuf, String)]) -> Vec<Edit> {
    let mut out = Vec::new();
    for (path, old) in before {
        match after.iter().find(|(p, _)| p == path) {
            Some((_, new)) if new != old => out.push(Edit {
                path: path.clone(),
                before: old.clone(),
                after: new.clone(),
            }),
            Some(_) => {}
            None => out.push(Edit {
                path: path.clone(),
                before: old.clone(),
                after: String::new(),
            }),
        }
    }
    out
}

/// **The seam the clock lands on.**
///
/// A wake is two acts on the daemon's side — write the request where the keeper's session will
/// find it, and ring the bell that starts its turn — and neither is expressible in this crate,
/// which has no daemon. So [`Keeper`] is the caller's own door and this function is the
/// protocol's precondition, on [`crate::gatekeeper::wake`]'s rule exactly.
///
/// # What this refuses, and why it is not a formality
///
/// A request with no note, no text or no workspace is a request nobody can act on, and it is
/// refused BY NAME rather than handed on: a keeper given an empty note would review nothing and
/// its report would be an opinion about the corpus, which is exactly what the brief-first
/// protocol exists to prevent.
pub fn wake(req: KeeperRequest, keeper: &dyn Keeper) -> Result<String, String> {
    if req.note.as_os_str().is_empty() {
        return Err(
            "a review was asked for with no note, so a report could not be attached to the note \
             it is about. Nothing was asked."
                .into(),
        );
    }
    if req.text.trim().is_empty() {
        return Err(format!(
            "`{}` is empty, so there is nothing to review and no reference to resolve. Nothing \
             was asked.",
            req.note.display()
        ));
    }
    if req.workspace.as_os_str().is_empty() {
        return Err(format!(
            "`{}` was given no workspace, so there is no tree to check it against — and a note \
             reviewed against nothing is a note nobody reviewed. Nothing was asked.",
            req.note.display()
        ));
    }
    keeper.wake(&req)
}

/// **The door a wake goes through** — the daemon's own two acts, and nothing else.
///
/// The keeper is a session the daemon serves and a review is minutes long, so the ask is a fact
/// written down plus a bell rung, and the answer comes back on a later pass — the same
/// asynchrony [`crate::gatekeeper::Reviewer`] carries, for the same reason.
pub trait Keeper {
    /// **Write the request down and ring the keeper's bell.** `Ok` is what was done, in the
    /// words the operator gets; `Err` is why it could not be, said rather than swallowed.
    fn wake(&self, req: &KeeperRequest) -> Result<String, String>;
}

/// **A door that refuses by name** — what a build with no keeper session attached answers.
pub struct NoKeeper;

impl Keeper for NoKeeper {
    fn wake(&self, req: &KeeperRequest) -> Result<String, String> {
        Err(format!(
            "this daemon has no notes keeper attached, so `{}` was not reviewed and is exactly \
             as it was. The keeper is the role this crate defines \
             (`letibot_tools::notes_keeper`); the daemon starts one, and no clock starts the \
             daemon yet.",
            req.note.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "letibot-notes-keeper-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".letibot/notes")).expect("mkdir");
        std::fs::write(root.join(".letibot/notes/a.md"), "the note's own text\n").expect("write");
        root
    }

    fn request(name: &str) -> KeeperRequest {
        let root = workspace(name);
        let note = root.join(".letibot/notes/a.md");
        KeeperRequest::gather(&root, &note, &NoSupersession).expect("gather")
    }

    /// **The closed set is closed.** All four words round-trip, and a word outside the set is
    /// refused rather than defaulted.
    #[test]
    fn the_decision_is_a_closed_set() {
        for d in KeeperDecision::ALL {
            assert_eq!(KeeperDecision::parse(d.as_str()), Some(d));
        }
        assert_eq!(KeeperDecision::parse("fine"), None);
        assert_eq!(KeeperDecision::parse(""), None);
        // The order is the precedence, and it is the prompt's rule rather than a comment.
        assert_eq!(KeeperDecision::ALL[0], KeeperDecision::NeedsPerson);
        assert_eq!(KeeperDecision::ALL[3], KeeperDecision::Checked);
        for e in Evidence::ALL {
            assert_eq!(Evidence::parse(e.as_str()), Some(e));
        }
        assert_eq!(Evidence::parse("because"), None);
    }

    /// A reply with no block, or with a word outside the set, is refused by name — and the note
    /// is left as it was.
    #[test]
    fn a_report_without_a_readable_decision_is_refused() {
        let req = request("refuse");
        let no_block =
            parse_report(&req, "I read the note and it all looks fine to me.").unwrap_err();
        assert!(no_block.contains("no `decision:` line"), "{no_block}");
        let wrong = parse_report(&req, "thought about it\n\ndecision: looks_fine").unwrap_err();
        assert!(wrong.contains("not one of"), "{wrong}");
        assert!(wrong.contains("checked"), "the set is named: {wrong}");
    }

    /// The block is read field by field, from the last `decision:` line, and prose above it is
    /// not read as evidence.
    #[test]
    fn the_block_is_read_from_the_last_decision_line() {
        let req = request("block");
        let said = "I wondered whether `decision:` would confuse the parser.\n\n\
                    decision: needs_person\n\
                    reasons: - the reference to `crates/tools/src/gone.rs` no longer resolves\n\
                    - and the note's own claim about the queue is now false\n\
                    changed: - .letibot/notes/a.md — replaced the dead path with the live one\n\
                    proposals: - remove [references] .letibot/notes/old.md — three of its four paths are gone\n\
                    raised: - I cannot tell whether the 2026-10-01 measurement still holds\n\
                    files: crates/tools/src/references.rs, .letibot/notes/a.md\n";
        let r = parse_report(&req, said).expect("a report");
        assert_eq!(r.decision, KeeperDecision::NeedsPerson);
        assert_eq!(r.note, req.note);
        assert_eq!(r.reasons.len(), 2, "{:?}", r.reasons);
        assert_eq!(r.changed.len(), 1);
        assert_eq!(r.proposals.len(), 1);
        assert_eq!(
            r.proposals[0].evidence,
            Evidence::References,
            "the evidence kind is read, not guessed"
        );
        assert_eq!(r.proposals[0].note, ".letibot/notes/old.md");
        assert!(r.proposals[0].because.contains("three of its four paths"));
        assert_eq!(r.raised.len(), 1);
        assert_eq!(r.files.len(), 2);
    }

    /// A proposal whose evidence cannot be read is refused, and the refusal names the shape —
    /// because a request to delete somebody's memory that nobody can weigh is not a request.
    #[test]
    fn a_proposal_outside_the_shape_is_refused_by_name() {
        let req = request("proposal");
        let cases = [
            (
                "proposals: - delete [references] x.md — it is stale",
                "begins with `remove",
            ),
            (
                "proposals: - remove references x.md — it is stale",
                "square brackets",
            ),
            (
                "proposals: - remove [because_i_said_so] x.md — it is stale",
                "is not one of",
            ),
            ("proposals: - remove [references] x.md", "separated by"),
        ];
        for (line, expected) in cases {
            let said = format!("decision: needs_person\n{line}\n");
            let why = parse_report(&req, &said).unwrap_err();
            assert!(why.contains(expected), "`{line}` → {why}");
        }
        // And the shape the prompt asks for is read.
        let ok = parse_report(
            &req,
            "decision: needs_person\nproposals: - remove [landed] .letibot/notes/old.md — the work landed in 8421416\n",
        )
        .expect("a report");
        assert_eq!(ok.proposals[0].evidence, Evidence::Landed);
        assert!(ok.proposals[0].render().contains("[landed]"));
    }

    /// **The prompt is the brief's order**, states the closed set, and carries the evidence the
    /// checker produced — the unresolved ones named.
    #[test]
    fn the_prompt_carries_the_order_the_set_and_the_evidence() {
        let root = workspace("prompt");
        std::fs::create_dir_all(root.join("crates/tools/src")).expect("mkdir");
        std::fs::write(root.join("crates/tools/src/here.rs"), "one\ntwo\n").expect("write");
        let note = root.join(".letibot/notes/a.md");
        std::fs::write(
            &note,
            "the checker is `crates/tools/src/here.rs` and `crates/tools/src/gone.rs`.\n",
        )
        .expect("write");
        let req = KeeperRequest::gather(&root, &note, &NoSupersession).expect("gather");
        let p = keeper_prompt(&req);
        assert!(p.starts_with(TITLE_PREFIX), "{p}");
        for word in KeeperDecision::ALL {
            assert!(
                p.contains(word.as_str()),
                "`{}` is in the set",
                word.as_str()
            );
        }
        for word in Evidence::ALL {
            assert!(p.contains(word.as_str()), "`{}` is evidence", word.as_str());
        }
        // The four rules, in the design's order.
        let refs = p.find("The references first").expect("rule 1");
        let prose = p.find("Then the prose, against the tree").expect("rule 2");
        let repair = p
            .find("Then the repair, through the `notes` tool")
            .expect("rule 3");
        let raise = p.find("Raise what you cannot judge").expect("rule 4");
        assert!(
            refs < prose && prose < repair && repair < raise,
            "the order is the design"
        );
        // The evidence itself.
        assert!(p.contains("crates/tools/src/gone.rs"), "{p}");
        assert!(
            p.contains("unresolved"),
            "the checker's verdict is carried: {p}"
        );
        // And the door that was not asked says so rather than letting a guess stand in.
        assert!(p.contains("not consulted"), "{p}");
        assert!(p.contains("do not make one"), "{p}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The seat's repair path, in the prompt's own words**: repair through `notes`, never by
    /// writing a file, and never by rewriting history to look current.
    #[test]
    fn the_prompt_says_how_to_repair_and_what_not_to_do() {
        let req = request("repair");
        let p = keeper_prompt(&req);
        assert!(p.contains("action=\"append\" or action=\"replace\""), "{p}");
        assert!(p.contains("never by writing a file"), "{p}");
        assert!(
            p.contains("Do not rewrite history to make a note look current"),
            "{p}"
        );
        assert!(p.contains("A proposal is not an act"), "{p}");
        assert!(p.contains("you never delete a note"), "{p}");
    }

    /// A `never_taken` proposal's evidence is the log, and the prompt says it out loud when the
    /// log says so.
    #[test]
    fn the_offer_log_is_the_evidence_for_a_never_taken_proposal() {
        let root = workspace("offers");
        std::fs::write(
            root.join(".letibot/notes-offers.jsonl"),
            "{\"at\":1,\"session\":\"s1\",\"path\":\".letibot/notes/a.md\",\"score\":0.31,\"taken\":false}\n\
             {\"at\":2,\"session\":\"s1\",\"path\":\".letibot/notes/a.md\",\"score\":0.44,\"taken\":false}\n\
             {\"at\":3,\"session\":\"s2\",\"path\":\".letibot/notes/b.md\",\"score\":0.20,\"taken\":true}\n\
             not json at all\n\
             {\"at\":4,\"path\":\".letibot/notes/a.md\"}\n",
        )
        .expect("write");
        let log = offers_log(&root);
        assert_eq!(log.len(), 2, "{log:?}");
        let a = log.iter().find(|o| o.path.ends_with("a.md")).expect("a");
        assert_eq!(a.offers, 3, "the malformed line is skipped, not fatal");
        assert_eq!(a.taken, 0);
        assert!((a.best - 0.44).abs() < 1e-9);
        assert!(a.never_taken());
        let b = log.iter().find(|o| o.path.ends_with("b.md")).expect("b");
        assert!(!b.never_taken());

        let note = root.join(".letibot/notes/a.md");
        let req = KeeperRequest::gather(&root, &note, &NoSupersession).expect("gather");
        assert!(req.offered_here().is_some_and(Offered::never_taken));
        let p = keeper_prompt(&req);
        assert!(p.contains("never_taken"), "{p}");
        assert!(p.contains("offered 3 time(s), read 0 time(s)"), "{p}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An absent log is an empty log — the case today, since the offering half has not landed —
    /// and the prompt says which it is rather than implying a measurement.
    #[test]
    fn an_absent_log_is_an_empty_log_and_says_so() {
        let req = request("nolog");
        assert!(req.offers.is_empty());
        let p = keeper_prompt(&req);
        assert!(p.contains("the log is empty or absent"), "{p}");
    }

    /// **The similarity door, and the honesty it buys**: not asked is not the same as nothing
    /// close, and the keeper is told not to guess.
    #[test]
    fn the_similarity_door_distinguishes_unasked_from_nothing_close() {
        struct Door(Option<(PathBuf, f64)>);
        impl Supersession for Door {
            fn nearest(&self, _t: &str, _s: &Path) -> Option<(PathBuf, f64)> {
                self.0.clone()
            }
        }
        let root = workspace("door");
        let note = root.join(".letibot/notes/a.md");

        let unasked = KeeperRequest::gather(&root, &note, &NoSupersession).expect("gather");
        assert_eq!(unasked.superseded, Nearest::Unavailable);
        assert!(keeper_prompt(&unasked).contains("do not make one"));

        let nothing = KeeperRequest::gather(
            &root,
            &note,
            &Door(Some((PathBuf::from("b.md"), SUPERSEDED_FLOOR - 0.01))),
        )
        .expect("gather");
        assert_eq!(nothing.superseded, Nearest::NothingClose);
        assert!(keeper_prompt(&nothing).contains("no other note reached"));

        let close = KeeperRequest::gather(
            &root,
            &note,
            &Door(Some((PathBuf::from(".letibot/notes/b.md"), 0.81))),
        )
        .expect("gather");
        match &close.superseded {
            Nearest::Note { path, score } => {
                assert!(path.ends_with("b.md"));
                assert!((*score - 0.81).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
        let p = keeper_prompt(&close);
        assert!(p.contains("0.810"), "{p}");
        assert!(p.contains("superseded"), "{p}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `wake` refuses an empty note, an empty text and a missing workspace by name, and the door
    /// with no keeper behind it refuses rather than reporting a review that never happened.
    #[test]
    fn a_wake_that_could_not_be_asked_is_refused_by_name() {
        struct Recorded;
        impl Keeper for Recorded {
            fn wake(&self, _req: &KeeperRequest) -> Result<String, String> {
                Ok("asked".into())
            }
        }
        let mut req = request("wake");
        assert_eq!(wake(req.clone(), &Recorded).unwrap(), "asked");

        let mut empty_text = req.clone();
        empty_text.text = "   \n".into();
        let why = wake(empty_text, &Recorded).unwrap_err();
        assert!(why.contains("is empty"), "{why}");

        let mut no_note = req.clone();
        no_note.note = PathBuf::new();
        let why = wake(no_note, &Recorded).unwrap_err();
        assert!(why.contains("no note"), "{why}");

        req.workspace = PathBuf::new();
        let why = wake(req.clone(), &Recorded).unwrap_err();
        assert!(why.contains("no workspace"), "{why}");

        let root = workspace("nokeeper");
        let note = root.join(".letibot/notes/a.md");
        let req = KeeperRequest::gather(&root, &note, &NoSupersession).expect("gather");
        let why = wake(req, &NoKeeper).unwrap_err();
        assert!(why.contains("no notes keeper attached"), "{why}");
        assert!(why.contains("a.md"), "the note is named: {why}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The measured diff.** An append reads as an append, and a note that vanished is an edit
    /// too — because nothing the keeper can call may delete a note.
    #[test]
    fn the_measured_diff_shows_what_moved() {
        let a = PathBuf::from(".letibot/notes/a.md");
        let b = PathBuf::from(".letibot/notes/b.md");
        let before = vec![
            (a.clone(), "one\ntwo\nthree\n".to_string()),
            (b.clone(), "kept\n".to_string()),
        ];
        let after = vec![
            (a.clone(), "one\ntwo\nthree\nfour\n".to_string()),
            (b.clone(), "kept\n".to_string()),
        ];
        let d = edits(&before, &after);
        assert_eq!(d.len(), 1, "{d:?}");
        let out = d[0].render();
        assert!(out.contains("+ four"), "{out}");
        assert!(
            !out.contains("- three"),
            "the common head is trimmed: {out}"
        );

        let gone = edits(&before, &[]);
        assert_eq!(gone.len(), 2, "{gone:?}");
        assert!(gone[0].render().contains("- one"), "{:?}", gone[0].render());
        assert!(gone[0].after.is_empty());
    }

    /// **Where the keeper's account and the disk disagree**, which is the whole reason the diff
    /// is measured rather than asked for.
    #[test]
    fn an_edit_nobody_reported_and_a_report_nobody_measured_are_both_named() {
        let req = request("unaccounted");
        let note = req.note.clone();
        let mut answer = KeeperAnswer {
            decision: KeeperDecision::Changed,
            changed: vec![format!("{} — replaced a dead path", note.display())],
            ..KeeperAnswer::default()
        };
        let honest = KeeperReport::for_request(&req, answer.clone()).measured(vec![Edit {
            path: note.clone(),
            before: "old\n".into(),
            after: "new\n".into(),
        }]);
        assert!(
            honest.unaccounted().is_empty(),
            "{:?}",
            honest.unaccounted()
        );
        assert!(honest.render().contains("- old"));
        assert!(honest.render().contains("+ new"));

        // The keeper says it changed nothing, and the disk says otherwise.
        let silent = KeeperReport::for_request(
            &req,
            KeeperAnswer {
                decision: KeeperDecision::Checked,
                ..KeeperAnswer::default()
            },
        )
        .measured(vec![Edit {
            path: note.clone(),
            before: "old\n".into(),
            after: "new\n".into(),
        }]);
        let said = silent.unaccounted();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("does not name it"), "{said:?}");

        // And the other way: a claim with nothing behind it.
        answer.changed = vec![format!("{} — I tidied the prose", note.display())];
        let claimed = KeeperReport::for_request(&req, answer).measured(vec![Edit {
            path: PathBuf::from(".letibot/notes/other.md"),
            before: "x\n".into(),
            after: "y\n".into(),
        }]);
        let said = claimed.unaccounted();
        assert!(
            said.iter().any(|s| s.contains("nothing on disk did")),
            "{said:?}"
        );
        let _ = std::fs::remove_dir_all(&req.workspace);
    }

    /// A report with no diff measured says so, rather than implying the note was untouched.
    #[test]
    fn a_report_with_no_measured_diff_says_so() {
        let req = request("nodiff");
        let r = KeeperReport::for_request(
            &req,
            KeeperAnswer {
                decision: KeeperDecision::Checked,
                reasons: vec!["every reference resolves".into()],
                ..KeeperAnswer::default()
            },
        );
        let out = r.render();
        assert!(out.contains("no diff was measured"), "{out}");
        assert!(out.contains("checked"), "{out}");
        assert!(
            r.unaccounted().is_empty(),
            "nothing measured, nothing unaccounted"
        );
        let _ = std::fs::remove_dir_all(&req.workspace);
    }

    /// The report is rendered in the house voice, with the proposals marked as proposals.
    #[test]
    fn the_report_renders_proposals_as_proposals() {
        let req = request("render");
        let r = KeeperReport::for_request(
            &req,
            KeeperAnswer {
                decision: KeeperDecision::NeedsPerson,
                reasons: vec!["a reference moved".into()],
                proposals: vec![Proposal {
                    note: ".letibot/notes/old.md".into(),
                    evidence: Evidence::Landed,
                    because: "the work landed in 8421416".into(),
                }],
                raised: vec!["whether the 2026-10-01 measurement still holds".into()],
                files: vec!["crates/tools/src/references.rs".into()],
                ..KeeperAnswer::default()
            },
        );
        let out = r.render();
        assert!(out.contains("proposals (not acts)"), "{out}");
        assert!(
            out.contains("remove [landed] .letibot/notes/old.md"),
            "{out}"
        );
        assert!(out.contains("needs_person"), "{out}");
        assert!(out.contains("raised"), "{out}");
        let _ = std::fs::remove_dir_all(&req.workspace);
    }

    /// Gathering resolves the references against the note's own text — the first rule, done
    /// before anybody is asked anything.
    #[test]
    fn gathering_resolves_the_references_before_anyone_is_asked() {
        let root = workspace("gather");
        std::fs::create_dir_all(root.join("crates")).expect("mkdir");
        std::fs::write(root.join("crates/here.rs"), "one\n").expect("write");
        let note = root.join(".letibot/notes/a.md");
        std::fs::write(&note, "`crates/here.rs` and `crates/gone.rs`\n").expect("write");
        let req = KeeperRequest::gather(&root, &note, &NoSupersession).expect("gather");
        assert_eq!(req.references.references.len(), 2);
        assert_eq!(req.references.resolved().len(), 1);
        assert_eq!(req.references.unresolved().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A note that cannot be read is a refusal, not an empty review.
    #[test]
    fn a_note_that_cannot_be_read_refuses() {
        let root = workspace("unreadable");
        let why =
            KeeperRequest::gather(&root, &root.join(".letibot/notes/nope.md"), &NoSupersession)
                .unwrap_err();
        assert!(why.contains("could not be read"), "{why}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
