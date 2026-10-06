//! The head frame format: ATTACH, RESYNC, snapshot, event envelope, `expected_seq`
//! and `client_request_id`.
//!
//! §13.2 and §13.4 give this in prose; `docs/workstreams.md` names it as the
//! second-highest-value interface to fix early, because *"fixing it makes W8 a leaf
//! that can start immediately against a recorded log"*. This module is that fix.
//!
//! # Transport-independent on purpose
//!
//! §13.4 says the remote head speaks *"the same frames as the socket"*. So the
//! frames are plain serde types and the framing ([`crate::wire`]) is one
//! newline-delimited-JSON codec over any `Read`/`Write`. A WebSocket head is then a
//! different `Read`/`Write`, not a different protocol.
//!
//! # The three rules the frame shapes enforce
//!
//! 1. **`Ack` carries `seq`, `rendered` and `filtered`, and `seq` comes from the
//!    batch, not from the kept events.** See [`crate::cursor`]. There is no
//!    constructor that takes a "last kept seq", because that is the bug.
//! 2. **`Hello.dropped` is a bare `u64`.** Present and zero. No `skip_serializing_if`
//!    anywhere in this module, and a test says so.
//! 3. **Every mutating command carries `expected_seq` and `client_request_id`**
//!    (§13.2). A command with a stale `expected_seq` is *rejected with both
//!    numbers*, so the head can say what it was looking at.

use serde::{Deserialize, Serialize};

use crate::event::Envelope;
use crate::registry::{SessionBrief, SessionWiring};
use crate::scrub::ScrubReport;
use crate::view::Snapshot;

/// Bumped when a frame's meaning changes. Both sides refuse a mismatch loudly —
/// §17-S6's rule, applied here because the head protocol has the same failure mode
/// as the control channel: a silent version skew that looks like a bug in the other
/// half.
///
/// **4** since a head can reach sessions that are not in the daemon yet.
///
/// `ResumeSession` and `RenameSession` are new client frames and
/// `SessionEvent::SessionRenamed` is a new event, so a version-3 head talking to a
/// version-4 daemon would fail to parse an event it is sent mid-session — which is a
/// deserialization error in the middle of a turn, and the worst possible place for
/// one. Both sides refuse the mismatch at ATTACH instead.
///
/// What made it necessary: a stored session is now **resumable**, and a head is
/// where the operator asks for that. Without a frame for it, `letibot --continue`
/// against a *running* daemon could only work by killing the daemon and restarting
/// it with `--session` — which would take down every other session on the box to
/// open one, and this box runs several.
///
/// **3** since a tool call's body got a channel of its own.
///
/// `DeltaTarget` gained `ToolCall` and `TurnView` gained `raw_calls`, closing
/// T13.5: the raw `<function=…>` markup a model writes inside `<tool_call>` used
/// to be announced as `Text`, so every head rendered it as prose until the closing
/// tag arrived and it was replaced by a card. That is a *protocol* fault — the
/// boundary exists in the engine, which is walking vocabulary ids, and is gone by
/// the time the markup is a string — so it is fixed here rather than guessed at in
/// a head. A version-2 head talking to a version-3 daemon would fail to parse the
/// new `target` value; a version-3 head talking to a version-2 daemon would show
/// the markup again. Both sides refuse a mismatch by name, so the failure is one
/// line in a terminal instead.
///
/// **2** was the daemon growing more than one session: `Hello` carrying what the
/// head is attached *to* (§4.4) and the session list a picker is drawn from,
/// `Attach`'s `session_id` naming one of several rather than being checked against
/// the only one, and `ToolCallProposed` carrying a display target (§4.1).
/// # 3 → 5, and why it skips 4
///
/// 4 is the `session-resume` branch's, landing separately. D10 asked for the
/// question-answer vocabulary to **coordinate to 5 rather than race it**, so this
/// takes 5 and leaves 4 where it was going. Both sides already refuse a mismatch by
/// name (`crates/sessionlog/src/server.rs` compares this constant and says both
/// numbers), so a head built against 3 or 4 is told which version it is speaking to
/// rather than failing on the first frame it does not understand.
///
/// What 5 adds: [`ClientFrame::AnswerQuestion`] and
/// [`crate::question::QuestionAnswer`] — a head answering a **question** rather than
/// granting a **permission**. See `crates/sessionlog/src/question.rs` for why those
/// are two vocabularies and not one.
///
/// # 5 → 6: a denial the operator can see
///
/// `docs/boundary-and-adjudication.md` §4b, which is a requirement and not a
/// nicety: *"a denial the operator cannot see manufactures the workaround"* — the
/// model infers the approach was wrong rather than forbidden, tries a variant, and
/// the task dies with the operator seeing only a dead task. Nothing on this wire
/// could carry a refusal. `Warning` is for §18's assertions and using it here would
/// make a decision taken on the operator's behalf look like a defect, which is the
/// same abuse [`crate::SessionEvent::CommandIssued`] exists to avoid one variant
/// along.
///
/// So 6 adds [`crate::SessionEvent::DenialRaised`], published **at the moment the
/// gate decides** rather than at turn end. Both sides refuse a mismatch by name, so
/// a head built against 5 is told which version it is speaking to rather than
/// silently missing every refusal — which would be the very defect, one layer down.
///
/// # 7: a head can answer
///
/// 6 gave the operator the *sight* of a refusal and 7 gives them the **reply**, which
/// is the half §4b actually turns on: *"the grant path is reachable at the moment of
/// denial, not after the task has died"*, and a path that only goes one way is not a
/// path. The frames to reply with have existed since 5; what did not exist was
/// anything on the daemon side that a reply could reach, because the queue they
/// landed on is drained by the thread that is waiting for them.
///
/// Two payload changes, and they are one seam rather than two because they are the
/// same missing half:
///
/// - [`crate::SessionEvent::DecisionRequested`] grows `choices` and `because`. A
///   question's plain-text options had nowhere to sit — `options` carries
///   `OptionKind`, an adjudication vocabulary answering *may this run* — so
///   `ask_user_question` could be posed only by discarding the choices, which is why
///   T25/D10 was specified and not built.
/// - [`crate::event::OptionKind`] grows `AllowSession`, so the widest an *answer*
///   goes has a spelling. Anything standing beyond one session is a **mode** rather
///   than a grant, and a mode is not an option on a prompt.
///
/// Both sides refuse a mismatch by name. A head built against 6 that was handed a 7
/// question would render an empty option list and ask a person to choose between
/// nothing.
///
/// # 8: a head can compact
///
/// [`ClientFrame::CompactSession`] is a new client frame, so a version-7 head
/// talking to a version-8 daemon is fine (it never sends the frame) but a
/// version-8 head talking to a version-7 daemon would send a frame the daemon
/// fails to parse — the same mid-session deserialization failure that forced
/// version 4, and the same ATTACH-time refusal applies. The daemon's answer to
/// the frame is the ordinary `Accepted`/`Rejected` pair; the compaction itself is
/// disclosed on the session's log as the turn and the summary item it produces,
/// so no new event kind was needed.
///
/// # 9: a session has a todo list
///
/// [`crate::SessionEvent::TodosUpdated`] is a new event, and a version-8 head
/// receiving one mid-session would fail to parse it — the version-4 argument
/// again, and the same ATTACH-time refusal. The event carries the whole list, in
/// the order the model wrote it; the pane that renders it also shows the repo's
/// own `TODO.md`, read-only, because an agent's plan and the operator's queue are
/// different lists and a head that conflated them would let one edit the other.
///
/// # 10: a head can move the running command to the background
///
/// [`ClientFrame::Promote`] is a new client frame (Ctrl+B), so a version-9 daemon
/// would fail to parse it — the same mid-session deserialization failure, and the
/// same ATTACH-time refusal. No new event: the promotion is the `bash` tool's own
/// `Backgrounded` result, attributed to the operator.
/// # 14: a session can be re-seated onto the tools that are seated now
///
/// [`ClientFrame::ReseatSession`] is a new client frame, so a version-13 daemon
/// would fail to parse it — the version-4 argument, and the same ATTACH-time
/// refusal. It exists because the tool schemas live in the stable prefix and a
/// session's prefix is fixed when it is created: a conversation opened by a daemon
/// with no shell could never call one, however the daemon that reopened it was
/// seated, and the banner — computed from the registry — said otherwise.
///
/// # 15: a background job's end is an event
///
/// [`crate::SessionEvent::JobSettled`] is a new event, so a version-14 head
/// receiving one mid-session would fail to parse it — the version-4 argument, and
/// the same ATTACH-time refusal. The **start** of a background job never needed an
/// event: the `bash` call finishes as `ToolOutcome::Backgrounded` and its `handle`
/// is the job id. The **end** did: a session-scoped job settles between turns,
/// when the only events a hub publishes are the daemon's, and without it every
/// head's picture of a background job was frozen at "running" forever.
///
/// # 16: a head can read a session it is not attached to
///
/// [`ClientFrame::Peek`] is a new client frame, so a version-15 daemon would fail
/// to parse it — the version-4 argument, and the same ATTACH-time refusal. It is
/// answered with [`ServerFrame::Peeked`]: another session's retained scrollback,
/// scrubbed exactly as a replay is and capped exactly as the daemon's ring is,
/// delivered **without moving the connection**. The subagent tree names child
/// sessions a head is not in, and reading one used to mean a `Switch` — which
/// rebuilds the head twice and blinds it to the parent's live events for the
/// whole read. Lazy by construction: nothing is read until the head asks, and
/// asking again is a fresh read.
///
/// # 22: the jobs pane reads a job's output without leaving the session
///
/// [`ClientFrame::ReadJobOutput`] is a new client frame and
/// [`crate::SessionEvent::JobOutput`] is a new event, so a version-21 daemon would
/// fail to parse the first and a version-21 head would fail to parse the second —
/// the version-4 argument, in both directions at once, and the same ATTACH-time
/// refusal covers it.
///
/// **This section is late, and that is the point of it.** Both landed at version
/// 21 with the constant left at 21, so ATTACH agreed and the mismatch surfaced
/// where it always does without a bump: mid-session, as a deserialization failure
/// that ends the connection. Measured 2026-09-20 on the operator's `stroppy-pfn`
/// session — a daemon started at 21:45 from the 21:17 binary, a head built after
/// `ReadJobOutput` landed, and Enter on a job row killed the head every time with
/// no message. *"when i went to job with enter in pfn project leticode just
/// exited"*.
///
/// The number is the only compatibility check there is. Adding a frame and moving
/// the number are two separate acts by the same person, and nothing used to check
/// they happened together — see `every_frame_is_accounted_for_at_this_version`,
/// which is a compile error rather than an assertion for exactly that reason.
///
/// # 17: a head can list the settings its session runs under
///
/// [`ClientFrame::Settings`] is a new client frame — the version-4 argument
/// again, and the same refusal at ATTACH. Answered with [`ServerFrame::Settings`]:
/// every setting the daemon resolved for this session as a [`SettingRow`] — its
/// value, where it came from, and whether it can change now. The config pane is
/// drawn from it. The rows that can change now are changed by the verbs that
/// already exist (`Mode`, `/supervise`), so this adds a way to SEE and not a
/// second way to set; a settings frame that also wrote would be a second path
/// into the same state, and the mode store already has one.
/// # 19: a head can take back what it queued
///
/// **[`ClientFrame::SetOperatorTodos`] is a new client frame, so this IS a bump.** The precedent it
/// follows is `WithdrawPrompts` below: an added, DEFAULTED FIELD on an existing struct needs no
/// version (a head that does not read it sees what it always did), while a new FRAME does — an older
/// daemon cannot deserialise it at all, and because `ClientFrame` is internally tagged that failure
/// takes the whole connection rather than one frame.
///
/// MEASURED before the bump: with both sides at 25, the new head's first `set_operator_todos`
/// reached an old daemon, failed the deserializer, and closed the socket — which the daemon says in
/// a sentence (*"this connection sent a frame this daemon could not read … restart the daemon"*) and
/// which would have happened on EVERY todo the operator added. At 26 the same combination is refused
/// once, at ATTACH, naming both numbers.
///
/// [`ClientFrame::WithdrawPrompts`] is a new client frame, so a version-18
/// daemon would fail to parse it — the version-4 argument, and the same
/// ATTACH-time refusal. It exists because the queue it names is real: a prompt
/// typed behind a long tool call sits unconsumed for minutes, and the operator
/// who pulls it back into the composer to edit it needs the original gone, not
/// stacked under the edit. The same version makes the operator's consecutive
/// queued messages **one** message: the engine merges them before the boundary,
/// so the model reads one user turn instead of a stack of fragments.
/// # 20: a head can ask the daemon to stop
///
/// [`ClientFrame::Stop`] is a new client frame, so a version-19 daemon would
/// fail to parse it — the version-4 argument again. It exists because `Ctrl+C`
/// twice used to mean one thing (this head leaves) when an operator often means
/// the other (the daemon goes too), and the only way to get the second was a
/// second terminal and `letibot --stop`. The head now asks which, and the
/// answer that stops the daemon travels over the protocol rather than a head
/// reaching around it to signal a pid.
/// # 23: a named operation reports how far it has filled
///
/// [`crate::SessionEvent::Filling`] is a new event, so a version-22 head receiving one
/// mid-session would fail to parse it — the version-4 argument, and the same ATTACH-time
/// refusal. It exists because the head used to *infer* a running operation from the
/// symptom *"rows have no bodies yet"* — which every ordinary turn, every reseat, every
/// compaction and every import all produce — so it drew *"carrying the conversation onto
/// the new prompt"* over ordinary replies, four times a second. **Only the daemon knows
/// which operation is running**, because it is the one running it, so it names the
/// operation and counts it here and the head draws what it is told. **Ephemeral** — a
/// tick from four minutes ago is a lie about now — so an operation's durable residue is
/// the rows and the finish note, not this.
///
/// **This section covers one ruling, not two.** `ImportProgress { done, total }` landed
/// an hour before it at this same version, for an opencode import alone; `Filling`
/// generalizes it in place — same version, same decision — because a carry is the same
/// kind of fact as an import and the two must not be two events kept in step.
/// # 24: an operator can run a call themselves, and the row says so
///
/// [`ClientFrame::OperatorCall`] and [`ClientFrame::OperatorResult`] are two new client
/// frames, so a version-23 daemon would fail to parse them — the version-4 argument, and the
/// same ATTACH-time refusal.
///
/// **Why two frames and not one, since the requirement asked for one.** The admission has to
/// be written *before* the call runs or `asked: true` is a lie; the row has to be written
/// *after* it or the model's view of the conversation is a lie. One frame can carry one of
/// those, so one frame buys either a decision the daemon never took or a conversation that
/// does not contain the result. The pair is the smallest honest shape.
///
/// **What it is for.** The operator's own sentence: *"the head as the environment that can
/// reach what the daemon cannot"*. An operator-run `web_fetch` runs where the OPERATOR runs,
/// and its result enters the ledger as a row like any other — with `origin` set, so a head
/// draws it as the person's act rather than the model's. The name is checked against
/// [`HEAD_RUN_TOOLS`] **by the daemon**, which is the only place the check means anything:
/// a head is going to run the thing either way, so what the list bounds is *what may be
/// recorded as part of the conversation*, not what may be executed.
/// # 25: a head can fetch the bytes that justified a decision
///
/// [`ClientFrame::FetchDiagnostic`] and [`ServerFrame::Diagnostic`] are a new client frame and
/// its answer, so a version-24 daemon would fail to parse the first — the version-4 argument,
/// and the same ATTACH-time refusal. R11 put the oracle's brief and its reply on the corpus row
/// (`shown`, `oracle_reply`) and neither reached a head; this is the locator that lets one ask,
/// by the same shape `FetchRow` already uses for a row the daemon's window has trimmed.
///
/// **This version is also where the signpost learned about `ServerFrame`.** The check that every
/// frame is accounted for covered the two directions that already existed and not the third, so
/// a new server frame broke an old head with nothing asking about it — found by adding one.
///
/// # 27: a compaction can show its own progress
///
/// [`crate::event::SessionEvent::CompactionProgress`] is a new server event, so a version-26 head
/// would fail to decode it — the version-4 argument, and the same ATTACH-time refusal.
///
/// **Why an event and not the field-with-a-default that `Warning`'s own note calls safe.** That
/// note's test is *"both directions are safe"*, and this fails it on one side: a defaulted field
/// on an existing variant is a frame an older head already has an arm for, while a new VARIANT is
/// one it has no arm for at all. `serde` has no catch-all on this enum — deliberately, so a head
/// cannot silently skip a fact it does not understand — which is what makes the difference real
/// rather than a matter of taste.
///
/// **What it is for, in the operator's words:** *"leticl compacts but why no progress bar?"*, and
/// then *"even more so for this overruns when we compact in turns"*. The overrun compaction
/// summarises a scratch transcript, so its progress could not be forwarded as the session's — a
/// `PromptProgress` from there is drawn as the session's own context, which it is not, and that
/// mislabel is what the suppression was for. This event is the same numbers under their own name.
/// # 28: the operator's own shell line
///
/// [`ClientFrame::OperatorShell`] is a new client frame, so a version-27 daemon would fail to
/// parse it — the version-4 argument, and the same ATTACH-time refusal. It exists because the
/// operator's ask — *"when prompt starts with `!` it is going to be a shell command from me"* —
/// is not the door's question: `HEAD_RUN_TOOLS` bounds what may be RECORDED as a tool call whose
/// name a tool owns, and a raw shell line has no tool name, no JSON arguments, and no admission
/// that a corpus row could stand behind. The frame carries the typed line and the daemon runs it
/// through the execution path `bash` already uses, so confine, the sudo askpass shim and the
/// scratch directory behave exactly as they do for a model's call. See the variant's own docs for
/// why the door was not widened instead.
/// # 29: the merge queue, whole, and its two moves
///
/// [`ClientFrame::ListMergeQueue`] and [`ServerFrame::MergeQueue`] are a new client frame and its
/// answer, and [`crate::SessionEvent::MergeEntryAdded`] and [`crate::SessionEvent::MergeEntryMoved`]
/// are two new events, so a version-28 daemon would fail to parse the first — the version-4
/// argument, and the same ATTACH-time refusal.
///
/// **What it is for, in the operator's words:** *"we need a gated merge to main, and worktree
/// cleanup. for this we might need a merge queue. look how flowy does it - it has a nice queue
/// with priorities and dependencies."* The snapshot carries the whole queue, every state, so a
/// head attaching mid-flight sees the whole queue rather than only later changes; from then on
/// the two events carry every change. The queue is daemon-level, not per-session: there is one
/// main branch and one queue, and the `session_id` on each entry is the entry's origin, not a
/// filter.
/// # 30: the model proposes `!` completions
///
/// [`ClientFrame::SuggestShell`] is a new client frame, so a version-29 daemon would fail to
/// parse it — the version-4 argument, and the same ATTACH-time refusal. Its answer,
/// [`ServerFrame::ShellSuggestions`], is a new server frame, and the version-25 argument
/// applies to it in the other direction: a head with no arm for it would fail to decode it
/// mid-session, so the one bump covers both halves.
///
/// It exists because the operator's ask — *"i want smart ! when a model suggest
/// completions"* — is not the head's to answer: a head has no HTTP client and no
/// transcript-wide context, while the daemon has both. The head sends the typed prefix and the
/// daemon builds the prompt from the conversation and asks the LOCAL model (the `[gatekeeper]`
/// endpoint, never a metered provider — a suggestion must not cost money per keystroke). The
/// answer is a list of candidate lines, and **nothing in the path submits**: a suggestion only
/// fills the composer, and Enter is still the operator's. See the variants' own docs for the
/// shape and the defensive parse.
pub const PROTOCOL_VERSION: u32 = 30;

/// **The names an operator may run through the head-run door, and record.**
///
/// Three: `web_search`, `web_fetch`, `read`. The principle is **read-only and bounded**, and
/// it is a *correction* of this list's first version, which said *the ones whose value is
/// fetching something the model cannot reach*. That reason describes `web_fetch` and nothing
/// else, and it was drawn around the example rather than around the reason — which showed
/// when `read` failed it while being the safest and most useful thing in the set. R31.
///
/// **What the door is for: the operator is not giving the model a task, they are changing
/// what it knows before it acts.** *"Go read this file"* costs a prefill, a decision, a
/// paraphrase of the intent, the call and then prose about it; the operator wanted the file
/// and already knows the path.
///
/// **`bash` and `write` stay out, and the new principle excludes them on its own terms** —
/// which is the test of whether a principle is real: `bash` is neither read-only nor bounded
/// (one call can do anything and return anything), and `write` changes the tree the model is
/// working in. `web_fetch` is `Access::Network` and is admitted because its *effect* is
/// bounded: it returns text and touches nothing. Reading that distinction off the access
/// class alone would have got this list wrong, which is why each name below says why.
///
/// **[`ServerFrame::Settings`] carries it as a row** (`key: "head-run.tools"`), so a head
/// reads the list instead of holding a copy that drifts. The constant here is what the daemon
/// *enforces*; the setting row is what the head *offers*; one list, two readers.
///
/// It is **not a security boundary** and must not be read as one: the head runs in the
/// operator's own terminal and can already run anything. It is a boundary on what the corpus
/// records, which is the requirement's actual subject.
pub const HEAD_RUN_TOOLS: [&str; 3] = ["web_search", "web_fetch", "read"];

/// The `key` [`HEAD_RUN_TOOLS`] travels under. Named here so the daemon that publishes the
/// row and the head that reads it cannot spell it two ways.
pub const HEAD_RUN_TOOLS_KEY: &str = "head-run.tools";

/// **The verbs the DAEMON answers** — R32's third constraint, published like the tool list
/// above and for the same reason.
///
/// A head completes `/`-commands from a table, and a head that does not recognise a verb
/// forwards it. So the namespace has two owners, and **neither may enumerate the other's
/// half**: the head offers its own verbs from its own dispatcher, and these from a row the
/// daemon publishes.
///
/// Measured on this box, 2026-09-23 — `docs/evidence/slash-completion-2026-09-23.py` — the
/// head's table offered 27 verbs while **five working daemon verbs were not in it**:
/// `/flowy`, `/gate`, `/job`, `/login`, `/supervise`. Every one of them ran. The cause was
/// not that completion was missing but that **it completed from a different list than the
/// one that dispatches**, which is the failure mode a second copy always has: nearly right,
/// and nothing says so.
///
/// `value` is the names, comma-joined with no spaces; an absent row means a daemon older
/// than this one, which a head reads as *my own verbs only* rather than guessing.
pub const DAEMON_VERBS_KEY: &str = "daemon.verbs";

/// **Which providers this box holds a key for** — the fact a model picker needs to know which
/// of its rows can actually be taken.
///
/// The operator, 2026-10-04: *"model peeker should green models we have keys for."* The
/// question is [`letibot_provider::keys::resolve`]'s — an environment variable, a stored key,
/// or the key opencode filed under its own provider id — so **only the daemon can answer it**,
/// and a head that guessed would green a row that refuses at the first turn.
///
/// `value` is the preset names, comma-joined with no spaces, in [`PRESETS`](letibot_provider::presets::ALL)
/// order. **`local` is deliberately absent**: there is nothing to authenticate, and a name in
/// this list means *this row has a credential behind it* rather than *this row is usable*.
///
/// An absent row is a daemon older than this one, which a head reads as *no greening* rather
/// than as *no keys* — the same rule [`DAEMON_VERBS_KEY`] follows, and for the same reason.
pub const MODEL_KEYS_KEY: &str = "models.keys";

/// **The picker rows that need no credential at all** — `local` and every local model
/// this fleet declares in `providers.toml`.
///
/// `value` is those names, comma-joined with no spaces, `local` first.
///
/// # Why it is a second row rather than a widening of [`MODEL_KEYS_KEY`]
///
/// That row answers *does this box hold a credential for this preset*, and its own
/// docstring is explicit that `local` is absent because "a name in this list means
/// this row has a credential behind it rather than this row is usable". A declared
/// local model has no credential either, so putting it there would make that sentence
/// false for both of them. Two facts, two rows.
///
/// # The defect it closes
///
/// The operator, 2026-10-05, on a declared local model the picker had just started
/// offering: *"dense78 needs a key this box does not hold."* It does not — it is a
/// box on the LAN with no key and no meter. A head greened `local` by its literal
/// name and asked this list about everything else, so the one new kind of keyless row
/// read as the one thing it could not be.
///
/// # No protocol bump
///
/// A settings row is data inside a frame that already exists, not a new frame and not
/// a new field, so `PROTOCOL_VERSION` is untouched. An absent row is a daemon older
/// than this one, and a head must then fall back to greening `local` alone — which is
/// exactly what it did before this existed, so an old daemon loses nothing.
pub const MODEL_KEYLESS_KEY: &str = "models.keyless";

/// **One door-tool as the daemon describes it to a head** — R31 and R32.
///
/// The head must be able to turn `/web_search blabla` into the JSON the wire wants *without
/// knowing anything about `web_search`*, and must be able to complete a path for `/read`
/// without holding a list of which verbs take paths. Both are the same fact published once:
/// which field a bare line goes into, and what that field is.
///
/// Carried on the `SettingRow` for [`HEAD_RUN_TOOLS_KEY`] in [`SettingRow::tools`], derived
/// from the tool's own declared parameters by the daemon that seats it — so a tool whose
/// schema changes changes this, and neither head is rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRunTool {
    /// The name the door takes. **This spelling and no other**: a head offers `/web_search`
    /// because the daemon said `web_search`.
    pub name: String,
    /// **Where a bare line goes.** `query` for `web_search`, `url` for `web_fetch`, `path`
    /// for `read`. Empty when the tool has no single obvious field, which is the case where
    /// the head must ask for JSON.
    pub field: String,
    /// **What that field is** — `path`, `url` or `text`. Drives the head's Tab: an argument
    /// position whose kind is `path` completes filenames, because the daemon said so and not
    /// because a head knows what `read` does. Empty when [`Self::field`] is.
    pub kind: String,
    /// **The fields that have defaults**, as the model would receive them. A head filling a
    /// bare line must send the same object a model's call would, or the same tool answers
    /// two different questions depending on who asked.
    #[serde(default)]
    pub defaults: std::collections::BTreeMap<String, String>,
    /// **Why there is no bare form**, when there is none. A head says this in the same
    /// sentence that refuses, rather than leaving the operator to guess which tools are
    /// which. Empty when [`Self::field`] is set.
    #[serde(default)]
    pub why_json: String,
}

/// The three kinds [`HeadRunTool::kind`] can take, and the vocabulary is closed on purpose: a
/// head branches on it, so a fourth value means a head rebuilt.
pub const HEAD_RUN_KIND_PATH: &str = "path";
pub const HEAD_RUN_KIND_URL: &str = "url";
pub const HEAD_RUN_KIND_TEXT: &str = "text";

/// **The verb a person types for a door tool: hyphens and no shift** — R34.
///
/// The operator, having had to reach for the shift key: *"lets change /web_search to
/// /web-search - no shift needed."*
///
/// The door's names were the only underscored verbs in either head's registry, and they were
/// underscored for one reason: they are spelled straight from the **tool** names. That is the
/// right spelling for the wire and the wrong one for a keyboard, and the two are not the same
/// surface.
///
/// **This does not move the wire.** The tool is still `web_search` — [`HEAD_RUN_TOOLS`], the
/// schema, the corpus row and the `CallOrigin` are untouched — and the head still holds no
/// schema: hyphen-to-underscore is a textual transform, not knowledge about the tool. R31's
/// *"the name is the daemon's spelling"* stands for the wire and is amended for the keyboard.
///
/// Lives here rather than in a head because **both** heads take it, and a transform written
/// twice is a transform that can be written differently.
pub fn head_run_verb(tool: &str) -> String {
    tool.replace('_', "-")
}

/// **The tool a typed verb names**, or `None` if it names nothing in `list`.
///
/// Accepts both spellings — R34: *"an operator who types what the daemon calls it should not
/// be told they are wrong."* `/web_search` and `/web-search` are the same verb; the first is
/// how the daemon spells it and the second is how a keyboard does, and a head that refused one
/// would be arguing with the person about a hyphen.
///
/// Case-folded, because a slash verb is typed by a hand: `/Web-Search` is the same verb and
/// there is no second one that differs only in case.
pub fn head_run_tool<'a>(typed: &str, list: &'a [&'a str]) -> Option<&'a str> {
    let want = typed.trim().replace('_', "-").to_ascii_lowercase();
    list.iter()
        .copied()
        .find(|t| head_run_verb(t).to_ascii_lowercase() == want)
}

/// **The command a `!` line carries, or `None` when it carries nothing.**
///
/// The one rule, in the one crate both halves of it live in: a submitted line whose **first
/// character** is `!` is the operator's own shell command, and the command is everything after
/// that bang, trimmed. Leading whitespace before the `!` is NOT tolerated — the same rule
/// `/`-verbs follow, so the two sigils a composer can start a line with behave the same way and
/// a line that begins with a space is prose, as it always was.
///
/// `None` for a line that is not a `!` line at all, and for one that is nothing but the bang and
/// whitespace: `!`, `!   `. The head refuses that spelling before sending; this is the daemon's
/// re-check, and it exists because a frame is a socket, not a keyboard — anything that can
/// connect must not be able to file an arbitrary sentence as the operator's shell line.
///
/// **`!!` is a command here, not a repeat.** The line `!! ls` carries the command `! ls` — there
/// is no history in this composer to repeat from, and inventing one reading would make the same
/// bytes mean two things depending on state nobody can see.
pub fn operator_shell_command(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('!')?;
    let cmd = rest.trim();
    (!cmd.is_empty()).then_some(cmd)
}

/// **How a daemon's protocol version compares with this build's** — as the one sentence a
/// head says, and `None` when they are the same.
///
/// # Why a head checks this at all, when the daemon already refuses a mismatch
///
/// The daemon's ATTACH check (`server.rs`, exact equality) is the first line and it is the
/// strict one. This is the second, and it exists because **the two halves are built and
/// run separately**, which is the whole shape of the defect this pairs with: a head built
/// against a newer protocol, a daemon started from a binary three weeks older, and
/// `ReadJobOutput` sent to a daemon that had never heard of it (`872f8dd`).
///
/// The daemon's refusal is a `Bye` naming both versions, and a head that sees one says so
/// — but it only fires if the daemon *has* that check, and a check is a thing a protocol
/// gains at some version. So there are two live cases this catches and the daemon's cannot:
/// a daemon older than the check's introduction, and a daemon whose check has been relaxed
/// — which is the direction R3 argues for, since *"a skew is usually survivable"* is
/// exactly why passing an unknown **event** through is right, and the same argument applies
/// to the version number. A head must not be silent about a skew because it is trusting the
/// other side to have mentioned it.
///
/// # The direction is not decoration: the two are different problems
///
/// **A newer daemon is a reading problem, and a head can survive it.** What arrives is a
/// frame this build may not know — a variant added after it was built. R3 is the answer and
/// it is already in: the line is kept, the head says what it could not read, counts it on
/// `/status`, and reads on. Nothing is lost except what the unknown frame said, which is
/// precisely the thing this build cannot use.
///
/// **An older daemon is a writing problem, and a head cannot survive it from its side.**
/// Everything this head reads parses — the older half wrote it. What breaks is the other
/// direction: a `ClientFrame` the daemon has never heard of fails *its* deserialiser, and
/// its read loop answers that by sending a `Bye` and closing the socket. The head did
/// nothing wrong, said nothing unusual, and the session ends on the next command the two
/// do not share. **So the older case is the one worth reading twice**: it is quiet until it
/// is fatal, and the operator is entitled to know that before they spend an hour in a
/// session that is going to drop on them.
///
/// Both are said with `head` first and the direction explicit, because a bare pair of
/// numbers makes the reader work out which side they are on, and the answer changes what
/// they should do.
pub fn protocol_skew(daemon: u32, head: u32) -> Option<String> {
    if daemon == head {
        return None;
    }
    Some(if daemon > head {
        format!(
            "this daemon speaks protocol {daemon} and this head speaks {head}: the daemon \
             is from a NEWER build. Frames it sends that this build does not know are \
             reported as they arrive, counted on /status, and skipped — the connection \
             stays up and the rest of the stream is unaffected. Restarting the daemon so \
             both halves are the same build is the way to stop seeing them."
        )
    } else {
        format!(
            "this daemon speaks protocol {daemon} and this head speaks {head}: the daemon \
             is from an OLDER build. Everything this head reads is fine; what is not safe \
             is what it sends — a command the daemon has never heard of fails its reader, \
             and it answers by saying goodbye and closing the socket. The session can end \
             on the next command the two do not share. Restarting the daemon is the way to \
             make them the same build."
        )
    })
}

/// **Which half of the oracle's exchange a head is asking for** — R11's locator.
///
/// A head draws a decision card and may want the bytes that justified it. It must not be
/// handed them on every frame: the brief runs to kilobytes and a session makes hundreds of
/// decisions. So a head asks for one, by name, and this is the name.
///
/// **Why a `kind` and not two client frames.** The two halves are one subject — the exchange
/// between this daemon and its oracle about one call — they are stored on one row, they are
/// read from one place, and a head that wants one very often wants the other next. Two frames
/// would be two round trips and two chances to disagree about the id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    /// The bytes the oracle was shown — `adjudication.shown`.
    Brief,
    /// The bytes it answered with — `adjudication.oracle_reply`.
    Reply,
}

/// One setting, as the daemon resolved it for this session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingRow {
    /// `mode`, `oracle.budget`, `spill`, … — the flag's own name where there is
    /// one, so an operator can find it in `harnessd --help`.
    pub key: String,
    /// The value, rendered. A path is a path, a duration is `2.5s`, a list is
    /// comma-joined; a secret is never here.
    pub value: String,
    /// Where it came from: `flag`, `default`, `project store`, `permission.json`,
    /// `providers.toml`, `store` (a resumed session's own row) — or empty when
    /// the daemon does not track it, which is said rather than guessed.
    pub source: String,
    /// Whether this can change in the running session, and by what: the slash
    /// verb (`/mode NAME`, `/supervise on|off`), or empty for a setting that
    /// takes a restart.
    pub editable: String,
    /// **The values this setting can take, from whoever owns them.** Empty for a
    /// setting with no closed set.
    ///
    /// Here because the head had its own copy of the mode names and it drifted:
    /// it listed `supervised`, which is not a mode, and did not list
    /// `automode-edits`, which is — so the config pane could not reach the point
    /// the daemon was already standing at (the operator, 2026-09-17: *"I started
    /// leticode and there is no automode-edits"*). A list of what a thing may be
    /// belongs with the thing, and travels; it is not re-typed at the other end.
    ///
    /// Added at `PROTOCOL_VERSION` 18. `#[serde(default)]` so an older daemon's
    /// rows still deserialise, and a head that gets none falls back to showing
    /// the value it was given.
    #[serde(default)]
    pub choices: Vec<String>,
    /// **The door's tools, described** — populated on the one row whose key is
    /// [`HEAD_RUN_TOOLS_KEY`] and empty everywhere else.
    ///
    /// A typed field rather than a grammar inside `value`, because the alternative was
    /// `"web_search query text"` — structure encoded as words in a string, which is the
    /// defect R31's own subject names: *a head that has to parse a sentence is holding a
    /// copy of the shape*. `choices` was the near miss: it is a `Vec<String>` for a list
    /// of *values*, and a tool is four facts.
    ///
    /// **No `PROTOCOL_VERSION` bump**: an added, defaulted field on an existing struct, the
    /// precedent `ModelAdvice::consulted` set. A head that does not read it sees the same
    /// `value` it always did.
    #[serde(default)]
    pub tools: Vec<HeadRunTool>,
}

/// A `Caps.features` string: this head can render a question with model-provided
/// options, let a person attach a note to a choice, and let them type a free answer.
///
/// It rides on the existing `features` list rather than a new `Caps` field, because
/// that list exists for exactly this and adding a bool per affordance is how a
/// capability struct becomes a changelog.
///
/// A head that does **not** advertise it can still be sent a question — and the
/// honest thing then is that it will not answer, which becomes `not_run` (*nobody
/// answered*) rather than a default. That is the same rule `Caps::can_decide`
/// already states: a head that cannot answer must say so, or a question routed to
/// it waits for its deadline and then times out, *"which is a real answer given for
/// a fake reason."*
pub const FEATURE_QUESTION_ANSWERS: &str = "question_answers_v1";

/// What a head can do and what it wants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// How many events the daemon will buffer for this head before demoting it to
    /// resync. The head asks, the daemon clamps. A head that renders slowly can ask
    /// for a bigger queue instead of resyncing constantly.
    pub queue: usize,
    /// Whether this head is willing to answer decisions. A read-only head must say
    /// so, or a question routed to it waits for its deadline and then times out —
    /// which is a real answer given for a fake reason.
    pub can_decide: bool,
    /// Free-form feature names, for forward compatibility.
    #[serde(default)]
    pub features: Vec<String>,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            queue: 1024,
            can_decide: true,
            features: Vec::new(),
        }
    }
}

/// **One background job, as the DAEMON sees it.**
///
/// The head used to build this itself, folding `ToolFinished`/`JobSettled` events
/// into rows and joining the command text out of the turn it happened to be
/// showing — so a job that outlived its turn lost its name, and every head had to
/// reimplement which jobs are worth listing and how a command is shortened. The
/// operator, 2026-09-20: *"regarding jobs, subagents, etc, i expect them to be
/// handled by harnessd not the heads"*.
///
/// So the daemon decides all of it — which jobs are listed, what the command
/// reads as, what the state word is — and a head renders what it is given. A
/// second head in another language gets the same answers for free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEntry {
    /// `j12`, the handle `job_output` and `/job ID` take.
    pub id: String,
    /// One line, already shortened by the daemon. Never empty: the daemon has the
    /// process table, so there is no "not in this head's window" case here.
    pub command: String,
    /// `asked`, `promoted`, `promoted by NAME`.
    pub how: String,
    /// The process's own word — `exited 0`, `killed by job_kill`, `running`.
    /// Deliberately not "ok"/"error": a non-zero exit is the command's answer.
    pub state: String,
    pub running: bool,
    /// **Whether anything was ever executed for this job** — the same fact
    /// [`crate::SessionEvent::JobOutput`] carries, on the row instead of the window.
    ///
    /// A job that never ran has no duration, and the row's tail claimed one anyway:
    /// `not run (could not join its scope) · 0 B out · ran 0.0s`, where *ran* is the one
    /// word the state beside it had just denied. Defaulted `false` and no
    /// `PROTOCOL_VERSION` bump, like every other added field: an older daemon said
    /// nothing, and "assume a process ran" renders exactly what those daemons rendered.
    #[serde(default)]
    pub never_ran: bool,
    /// **Where this job's output goes, when it does not go to its window** — R41's fact, on the
    /// row instead of in a payload.
    ///
    /// The daemon reads it out of the command text (`output_redirect_path`), so nothing has to run
    /// to know it: `cargo build > /tmp/log 2>&1` writes a great deal and none of it here. A reader
    /// who opens the job pane to watch such a job is looking at a window that will be empty
    /// however long it runs — which is why the status row says so before they open anything.
    ///
    /// `None` for the ordinary job, and for `2>&1` (which MERGES into stdout and is what makes a
    /// capture complete), for `<` and here-documents (stdin), and for a path built at run time —
    /// the same refusals `output_redirect_path` itself documents.
    ///
    /// Defaulted and no `PROTOCOL_VERSION` bump, like every other added field: an older daemon
    /// said nothing, and "assume it is watchable" renders exactly what those daemons rendered.
    #[serde(default)]
    pub redirect: Option<String>,
    pub produced: u64,
    pub elapsed_ms: u64,
}

/// **What a head wants back from a peek** — the scrub of a replay, or the rows of a session.
///
/// The original answer is [`PeekShape::Events`] and it is the **default**, so a head that sends no
/// such field gets exactly what it always got. That default is what makes this free: **a frame the
/// head ASKS for can grow a field**, where a frame the daemon VOLUNTEERS cannot grow a variant —
/// which is the version-4 argument the version notes keep making, and the reason `Peek` and
/// `ReadJobOutput` each cost a version number when they arrived as new *frames*.
///
/// # Why rows exist, and it is the operator's correction of 2026-10-03
///
/// *“yes subagents are not even scratch session they are session, just sub sessions”* — a child
/// **is** a session: the daemon holds it, it has rows, it has a store, and it can produce a
/// snapshot. [`ServerFrame::Peeked`]’s own docstring is what forced the alternative — the events it
/// returns are *“for reading, not for folding into the head's state”* — so a head could do nothing
/// with them but draw them **by hand**. That is `sub_out_lines` in letibot and `subagent-out-lines`
/// in leticl: the same plain-string renderer, written twice, because the wire left them nothing
/// else to do. A head that asks for rows gets what an attach returns, draws it with the renderer it
/// already has, and **deletes** its copy of the other one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeekShape {
    /// The retained scrollback, scrubbed exactly as a replay is. The default, and what a head that
    /// does not ask for anything else keeps getting.
    #[default]
    Events,
    /// **The session's own rows**, as a [`Snapshot`] — the same thing an attach answers with, and
    /// needing no scrub because a view is already the durable half of a session’s frame stream.
    Rows,
}

/// Head → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum ClientFrame {
    /// `ATTACH {session_id, since_seq, identity, caps}` (§13.2).
    ///
    /// `since_seq = 0` means "I have seen nothing" and gets a snapshot. Any other
    /// value is a resume: the gap is delivered, or `RESYNC` is answered, and
    /// **resync is a normal outcome, never an error**.
    Attach {
        protocol_version: u32,
        session_id: String,
        since_seq: u64,
        /// `tui`, `remote`, `flowy`, `acp` — §13.4's table.
        kind: String,
        identity: String,
        #[serde(default)]
        caps: Caps,
    },
    /// The read mark. Sent **after** the head has written the batch out, never on
    /// receipt: a crash then costs a duplicate, never a silence (§13.2b).
    Ack(Ack),
    /// The head gave up on its own state and wants a fresh snapshot. Also what a
    /// head sends after it receives [`ServerFrame::Resync`].
    Resync,
    /// A user message. If a turn is running this is **queued as a follow-up user
    /// item**, not rejected (§13.2), and the queuing is announced.
    Prompt {
        client_request_id: String,
        expected_seq: u64,
        text: String,
    },
    /// Take back what this head queued: every [`ClientFrame::Prompt`] from this
    /// head that has not been consumed by the running turn yet is dropped, and
    /// the operator's held steering text with it. The head that recalls its
    /// queued line into the composer to edit it sends this first, so the edited
    /// resend replaces the original instead of stacking onto it. Between turns
    /// there is nothing held to drop, and the frame is a quiet no-op.
    WithdrawPrompts {
        client_request_id: String,
        expected_seq: u64,
    },
    /// **The operator's todos, from a head that owns them.** See
    /// [`CommandKind::SetOperatorTodos`]: one list, two authors, and this carries the half the head
    /// is the source of truth for. A new frame, so a version-19 daemon refuses it by name rather
    /// than reading the rest of the line as something else.
    SetOperatorTodos {
        client_request_id: String,
        expected_seq: u64,
        /// **Absent means the empty half, and that is not a loose reading — it is the only one that
        /// works.** A head that has no operator rows has an empty list, and the other head's encoder
        /// writes *"a JSON object, OMITTING every key whose value is NIL"* — which is deliberate
        /// there and argued at length (`src/json.lisp`): NIL is both `false` and `nothing` in Lisp,
        /// and eliding is the spelling every serde shape accepts.
        ///
        /// **A bare required `Vec` is the one shape that accepts NEITHER missing nor null**, and
        /// that is what this was: an operator whose todo list was empty sent
        /// `{"frame":"set_operator_todos","client_request_id":"leticl-1","expected_seq":4537}`,
        /// the daemon refused it as `missing field items`, and **the head was disconnected** —
        /// repeatedly, on every connect, so the session looked like a daemon that would not start.
        /// MEASURED from the live log, and the two neighbouring cases are named in that encoder's own
        /// docstring (`Mode.consented`, `ReseatSession.summarise`, both fixed by hand on the head's
        /// side): *"a hazard fixed three times by hand is a pattern, not an accident."*
        ///
        /// So it is fixed on the side that can fix it once. An absent `items` is an empty half, which
        /// is what the head meant, and a head that CAN send the key is unaffected.
        #[serde(default)]
        items: Vec<crate::event::TodoEntry>,
    },
    /// **Stop the daemon**, not just this head.
    ///
    /// Rings the same bell a `SIGTERM` does — one shutdown sequence, not two —
    /// so every head wakes with `Closed`, the socket goes, and the last turn's
    /// rows are written the way an orderly stop writes them. Announced first,
    /// because a daemon may be serving more than the head that asked: a shared
    /// session's other heads learn who stopped it rather than finding a dead
    /// socket.
    ///
    /// A running turn is NOT interrupted by this. The signal path does not
    /// abort one either — `letibot --stop --force` is the verb that does, and it
    /// interrupts over the protocol first. Naming this `Stop` rather than
    /// `Shutdown` keeps it the same word the launcher uses for the same act.
    ///
    /// **It does not travel through the command queue**, and that is the point.
    /// Its first version submitted a `CommandKind` like every other frame; one
    /// worker drains that queue and a running turn owns it, so a stop asked for
    /// mid-turn sat behind the turn and nothing happened. The server handles
    /// this frame on the connection's own thread — announce, ack, close the
    /// registry — which is the same thing `catch_signals` does from its thread,
    /// and the reason a `SIGTERM` never had the bug.
    Stop {
        client_request_id: String,
        expected_seq: u64,
        /// Who asked, for the announcement. The head's identity, not a name it
        /// invents.
        who: String,
    },
    /// Idempotent, issuable by any attached head, announced with the issuer.
    Interrupt {
        client_request_id: String,
        expected_seq: u64,
        reason: String,
    },
    /// A head asked to move the running command to the background (Ctrl+O). The
    /// daemon's exec backend honours it mid-turn; between turns it is announced as
    /// idle.
    Promote {
        client_request_id: String,
        expected_seq: u64,
    },
    /// Compact this session: one summary turn over the history as it stands,
    /// then the history is replaced by that summary through a transcript fork.
    ///
    /// The frame only *asks*; what happens next is the daemon's, and it is
    /// disclosed on the session's own log — the summary turn streams like any
    /// turn, and the forked transcript's first item says what replaced the
    /// history. Queued like a prompt (it runs a turn) and accepted on a stale
    /// `expected_seq` for the same reason.
    CompactSession {
        client_request_id: String,
        expected_seq: u64,
    },
    /// Rebuild this conversation's prompt from the tools the daemon seats now,
    /// forking it onto the new prefix. A turn, queued like a compaction.
    ///
    /// A new frame, so a version-13 daemon would fail to parse it — the version-4
    /// argument again, and the same ATTACH-time refusal covers it. No new event:
    /// the re-seat announces itself as the fork it produces plus a `reseated`
    /// warning naming the tools that changed.
    ReseatSession {
        client_request_id: String,
        expected_seq: u64,
        /// **Summarise the conversation as well, rather than carrying it.**
        ///
        /// A re-seat exists to change message zero. Doing that should not cost
        /// the conversation, so the default carries every item verbatim and the
        /// lossy kind is the one you ask for — the operator's rule: *"id say flip
        /// it - reset is loseless and reset summarize will be not"*.
        ///
        /// `#[serde(default)]` is `false`, so a head too old to send this field
        /// gets the lossless fork. That is a change in what an old head receives,
        /// and it is the safe direction: it costs a cold prefill rather than a
        /// conversation.
        #[serde(default)]
        summarise: bool,
    },
    /// Move this session's project to a named point (`allow-all`, `writes-allowed`,
    /// an opencode name…). The daemon persists it in the mode store, so it applies
    /// to this session's project from here on without a daemon restart (D13).
    Mode {
        client_request_id: String,
        expected_seq: u64,
        name: String,
        /// **The operator confirmed an unconfined `allow-all`.** Only ever read for
        /// that one point, and only when the session has no confinement: `allow-all`
        /// requires one, there is none on a bare host, and the operator's answer to
        /// "this box is the boundary — confirm?" is the whole difference between
        /// refusing and opening. See `Mode::ALLOW_ALL_HERE`.
        ///
        /// `#[serde(default)]`, so an older head that never sends it is read as
        /// *nobody confirmed anything* — the fail-closed direction, and the reason
        /// this is additive without a `PROTOCOL_VERSION` bump.
        #[serde(default)]
        consented: bool,
    },
    /// A slash command the head does not handle itself, handed to the daemon as
    /// the line the operator typed, without the leading `/`: `flowy login
    /// lab2x1`, `models deepseek/deepseek-chat`. One frame for every such verb,
    /// because each one is a daemon act with feedback on the session log, and a
    /// frame per verb would have every head learn every verb. Added at
    /// `PROTOCOL_VERSION` 11.
    Slash {
        client_request_id: String,
        expected_seq: u64,
        line: String,
    },
    /// **`sudo` wants a password.** Sent by `letibot-askpass`, the helper the
    /// session's shell runs as `SUDO_ASKPASS`, attached as a head of kind
    /// `askpass`. The daemon raises [`crate::event::SessionEvent::SecretRequested`]
    /// to every head, waits for a [`ClientFrame::Secret`], and answers this
    /// connection with [`ServerFrame::Secret`] — the one frame that carries a
    /// password, on the one connection that hands it to `sudo`. `prompt` is
    /// sudo's own; `command` is what the session was running, so the person
    /// typing the password sees what it is for. Added at `PROTOCOL_VERSION` 12.
    Askpass { prompt: String, command: String },
    /// A head's answer to a `SecretRequested`: the password, or `None` for a
    /// refusal. **Never logged, never persisted, never in a `CommandIssued`.** It
    /// goes from this frame to the waiting `Askpass` connection and nowhere else;
    /// the log gets a `SecretSettled` saying whether one was given, by whom.
    Secret {
        req_id: String,
        secret: Option<String>,
    },
    /// **A head's own screen, as it drew it.** The answer to
    /// [`crate::event::SessionEvent::ScreenRequested`]: the exact rows this head
    /// last rendered, ANSI and all, at its real terminal size.
    ///
    /// Only a head can answer this. The daemon holds the log and the view; it has
    /// never seen a rendered cell, and what a person is looking at depends on
    /// their width, their scroll position, their theme and which folds they have
    /// open. A daemon-side re-render would be a reconstruction, and calling one
    /// "your screen" is the kind of claim this tree refuses everywhere else.
    /// Added at `PROTOCOL_VERSION` 13.
    Screen {
        req_id: String,
        cols: usize,
        rows_n: usize,
        /// One string per row, escape codes included.
        ///
        /// **[`items`]: same shape, same hazard, found by the sweep rather than by a report.**
        /// A screen with no rows is degenerate but it is not impossible — and a head whose encoder
        /// omits NIL would send this frame with no `rows` at all, which was `missing field \`rows\``
        /// and a dropped connection. A frame that answers *what are you looking at* with nothing is
        /// still an answer; refusing it costs the head its socket.
        ///
        /// See `SetOperatorTodos::items` for the full argument, including why no version bump.
        #[serde(default)]
        rows: Vec<String>,
    },
    /// Answer an open **permission**: grant or deny, by option id.
    ///
    /// This is the adjudication half. A question's answer is
    /// [`ClientFrame::AnswerQuestion`], and they are two frames because they are two
    /// vocabularies with different consequences — a permission that goes wrong runs
    /// something, an answer that goes wrong is attributed to a person.
    Answer {
        client_request_id: String,
        req_id: String,
        option_id: String,
        /// **The operator's own glob**, when they are answering *always allow* and
        /// want it to cover more than this one call.
        ///
        /// > *"please add globbing to my answers somehow too"*
        ///
        /// Without it the rule an `always allow` writes is derived from the call:
        /// the exact path, or the program and its verb. That is a good default and
        /// it is only ever the shape in front of you — an operator who means *"any
        /// test under crates/"* had no way to say so, and had to answer the same
        /// question again for every sibling.
        ///
        /// Meaningful **only** for `AllowAlways`, which writes a rule. It is ignored
        /// on every other option id rather than quietly widening one: an
        /// `allow_once` carrying a glob would be a grant nobody named.
        ///
        /// No `PROTOCOL_VERSION` bump: an added, defaulted field on an existing
        /// client frame. An older daemon ignores it and writes the derived pattern,
        /// which is what it did before; a newer one reading an older head's frame
        /// gets `None` and does the same.
        // `default` but NOT `skip_serializing_if`: this module's own law, checked
        // by `no_frame_field_is_elided_when_zero_or_empty`, is that an absent
        // field and an empty one must not be the same bytes. Skipping it would
        // make "this head sent no pattern" and "this head is too old to have the
        // field" identical on the wire, which is the distinction the rule exists
        // to keep. `default` still lets an older head's frame parse.
        #[serde(default)]
        pattern: Option<String>,
        /// **What the operator wants the model told**, for `deny_and_tell`.
        ///
        /// Meaningful only for that option, and ignored on the others for
        /// `pattern`'s reason: a note attached to an `allow_once` would be a
        /// sentence nobody reads, and attaching it silently is worse than
        /// dropping it.
        ///
        /// No `PROTOCOL_VERSION` bump, by the same argument written above: an
        /// added, defaulted field on an existing client frame. An older daemon
        /// ignores it — the denial still lands, without the reason, which is
        /// exactly what happened before this existed.
        #[serde(default)]
        note: Option<String>,
    },
    /// Answer an open **question**: a choice, a note on that choice, a typed reply,
    /// or a choice and a note together (§D10). Added at `PROTOCOL_VERSION` 5.
    ///
    /// There is no variant for *"not now"*. A head that wants to defer simply does
    /// not send this, and the question stays open until its deadline, at which point
    /// the tool reports `not_run` — *nobody answered*. Claude Code's *"chat later"*
    /// is the thing this absence is designed as: a deferral that travels as an
    /// answer is how a turn continues on an assumption nobody made.
    AnswerQuestion {
        client_request_id: String,
        req_id: String,
        answer: crate::question::QuestionAnswer,
    },
    /// What sessions does this daemon hold? Answered with [`ServerFrame::Sessions`].
    ///
    /// Read-only and unserialised: it does not go through the command queue,
    /// because a list is a question about the daemon rather than an act on a
    /// session, and making it wait behind a running turn would mean a head could
    /// not open the picker while the model was talking.
    ListSessions,
    /// What is this session's todo list? Answered with
    /// [`ServerFrame::Todos`], for the session this connection is in.
    ///
    /// Read-only and unserialised like [`ClientFrame::ListSessions`]: a list is
    /// a question, not an act. This is the **bootstrap** read — the snapshot
    /// carries transcript items, not events, so a head attaching fresh has no
    /// `TodosUpdated` to replay; from then on the events carry every change.
    ListTodos,
    /// This session's background jobs, from the daemon's process table.
    ///
    /// Read-only and unserialised like [`ClientFrame::ListTodos`], and for the
    /// same reason: a list is a question, not an act. Deliberately **not** a
    /// `Slash` — those ride the command queue and are answered between turns, so
    /// `/job` during a long turn arrived after it finished. A pane that opens
    /// must answer now. Added at `PROTOCOL_VERSION` 21.
    ListJobs,
    /// The merge queue, whole: every entry, every state, in the order the queue was filled.
    ///
    /// Read-only and unserialised like [`ClientFrame::ListJobs`], and for the same reason: a
    /// list is a question, not an act. This is the **bootstrap** read — the snapshot carries
    /// the whole queue, so a head attaching mid-flight sees the whole queue rather than only
    /// later changes; from then on the `MergeEntryAdded` and `MergeEntryMoved` events carry
    /// every change.
    ///
    /// The queue is daemon-level, not per-session: there is one main branch and one queue, and
    /// the `session_id` on each entry is the entry's origin, not a filter. So the read is not
    /// scoped to the connection's session, and the answer is the whole queue.
    ListMergeQueue,
    /// Make a new session in this daemon.
    ///
    /// It does **not** switch to it — the head does that with [`ClientFrame::Switch`]
    /// once it has seen the id in the `Sessions` reply. Two frames rather than one
    /// because "create" and "go there" are separately useful: a head that wants a
    /// session ready for later should not have to leave the one it is in.
    NewSession {
        client_request_id: String,
        /// A human name, or empty. A title is set **once**; a daemon may name an
        /// unnamed session from the message that opened it, and after that only a
        /// deliberate rename changes it. What must not happen is a row that renames
        /// itself as the conversation goes on.
        title: String,
        /// The tree this session is about, from the head that asked.
        ///
        /// Empty means "wherever the daemon is", which is what a head that does not
        /// know sends. It is here because the daemon's own working directory is a
        /// fact about the daemon and not about the conversation: a `letibot --new`
        /// typed in `~/Projects/rano` against a daemon started in `~` used to seat
        /// the new session's read-only tools at `~`, and every path in it resolved,
        /// so the only symptom was answers about the wrong tree.
        workspace: String,
    },
    /// Bring a session that is **in the store but not in this daemon** back to life.
    ///
    /// Separate from [`ClientFrame::NewSession`] because the two differ in the one
    /// way that matters: this one **names** the session and `NewSession` deliberately
    /// does not. An id minted daemon-side is right for a new session (two heads
    /// racing to create "scratch" must not collide) and wrong for a resume, where the
    /// whole point is *that* conversation and no other.
    ///
    /// Idempotent. A session the daemon already holds is answered with the list and
    /// its own id, not refused: "resume the one I am already in" is a no-op the
    /// operator is allowed to ask for, and a refusal there would send `letibot
    /// --continue` down an error path on the most ordinary case there is.
    ///
    /// It does **not** switch to it, for the same reason `NewSession` does not: the
    /// head sends [`ClientFrame::Switch`] once it has the id.
    ResumeSession {
        client_request_id: String,
        session_id: String,
    },
    /// Name a session, or clear its name with an empty title.
    ///
    /// Carries a `session_id` rather than acting on the current one: a picker is
    /// where renaming is wanted, and in a picker the session you are looking at is
    /// usually not the session you are in.
    RenameSession {
        client_request_id: String,
        session_id: String,
        title: String,
    },
    /// Move this connection to another session.
    ///
    /// The head detaches from the session it is in and attaches to the named one,
    /// **on the same socket**, and is answered with a second `Hello`. Reconnecting
    /// would do as well and is what a first cut does; it costs the head its
    /// `client_request_id` sequence and, on a busy box, a window in which it is
    /// attached to neither — which is the window a mid-turn attach exists to close.
    Switch {
        session_id: String,
        /// As `Attach`: `0` takes a snapshot, anything else resumes. A head that
        /// is coming *back* to a session it was watching sends the seq it had.
        since_seq: u64,
    },
    /// Read another session's retained scrollback **without moving there**.
    ///
    /// The subagent tree names child sessions, and a row's Enter should show what
    /// that subagent produced while the head stays in the session it is in — a
    /// [`ClientFrame::Switch`] would do the reading and lose the room: the head
    /// rebuilds itself twice and is attached to the child for the whole read.
    /// Answered with [`ServerFrame::Peeked`] on the same stream; the connection's
    /// seat, its acks and its live events are untouched. Lazy by construction:
    /// nothing is read until this is sent, and sending it again is a fresh read.
    Peek {
        session_id: String,
        /// **What to read back.** Absent is [`PeekShape::Events`], which is what every existing
        /// caller already gets, so this is additive on the wire in both directions: a daemon older
        /// than the field ignores it and answers with events, and a head older than it never sends
        /// it. See [`PeekShape`] for why rows exist at all.
        #[serde(default)]
        shape: PeekShape,
    },
    /// **The daemon is a proxy: it answers from its caches, or from the store.**
    ///
    /// Its two in-memory rings are a **cache tuned for the normal case** — the tail of a
    /// conversation and some scrollback, which is what a head shows — and a request for a row
    /// outside them is an ordinary cache miss. So the daemon reads the store, which has every
    /// row, and the head neither knows nor should know which of the three answered. That is
    /// why this takes an ordinal and not a tier: there is one name for a row, and resolving it
    /// is the daemon's business.
    ///
    /// `row` is the **session ordinal**: `0` is the session's first row ever. That is the
    /// number a head can always construct, because it knows the rows it holds and
    /// `items_dropped` says how many came before them. "Scroll up past my oldest row" is then
    /// `row = items_dropped - 1`.
    ///
    /// `at` is a byte offset into that row's body and `len` how much to send back. The model is
    /// `read`'s own `ranges`, one layer down.
    ///
    /// Answered with [`ServerFrame::RowFetched`]. The seat, the acks and the live events are
    /// untouched — a read that moves you is a switch, and this is not one.
    FetchRow {
        session_id: String,
        /// The row's **position in the session**, oldest first. Not an index into the
        /// daemon's window and not an item id — see the doc above for why the head can only
        /// express this one.
        row: usize,
        /// Byte offset into the body. Clamped to its length rather than refused: a head
        /// paging towards the end does not know where the end is, and asking past it is
        /// the ordinary way to find out.
        at: usize,
        /// How many bytes to send. **Capped by the daemon**, like `read`'s own windows —
        /// one request must not be able to return a megabyte because a head asked for
        /// one.
        len: usize,
    },
    /// **A call the OPERATOR ran themselves, before they run it** — R24 part two, decision 4.
    ///
    /// The first of two frames. The daemon checks `name` against [`HEAD_RUN_TOOLS`], records
    /// the admission as the operator's own act (`human:<who>`, `asked: true`, so the corpus
    /// separates it from an auto-admit and from the guard's answer by the column that already
    /// exists), and answers with [`ServerEvent::OperatorCallAllowed`] on the log. Only then
    /// does the head run it.
    ///
    /// **Refused by name, in a sentence.** A name outside the list is answered with
    /// [`ServerFrame::Rejected`] carrying why — not a dropped frame, because a head that
    /// cannot say why is a head that retries.
    ///
    /// `call_id` is the head's own handle for the call, and it is what
    /// [`ClientFrame::OperatorResult`] comes back under. The head chooses it because the head
    /// is the side that will be running it.
    OperatorCall {
        client_request_id: String,
        expected_seq: u64,
        /// The head's handle for this call. Unique within the session, and the key the
        /// result comes back on.
        call_id: String,
        /// One of [`HEAD_RUN_TOOLS`]. The daemon refuses anything else by name.
        name: String,
        /// The call's arguments, as JSON — the same shape a model's call carries, because
        /// the row it becomes is the same row.
        ///
        /// **The bare form is built here, by the head, from [`HeadRunTool::field`]** — a head
        /// that knows nothing about `web_search` turns `/web_search blabla` into
        /// `{"query":"blabla"}` because the daemon published which field a bare line goes
        /// into. R31: the knowledge stays the daemon's and the typing gets short.
        arguments: String,
        /// **Whether the DAEMON runs it.** (R31.)
        ///
        /// Both shapes are legitimate and the difference is which side has the tool:
        ///
        /// * `false` — the head runs it and sends [`ClientFrame::OperatorResult`]. That is
        ///   today's wire and it is what a head with its own client does; leticl's live proof
        ///   is this shape, and it omits the field, so its behaviour is unchanged byte for
        ///   byte.
        /// * `true` — the daemon runs it, through the tool this session already seats, and
        ///   appends the row itself. **The result is then the same program's output a model's
        ///   call would have produced**, bounded by the same byte caps, spilled by the same
        ///   policy and subject to the same network rules. A head that fetched a page with its
        ///   own HTTP client would write a corpus row saying *the operator ran `web_fetch`*
        ///   about a different program's answer — and the model would then be reading text
        ///   `web_fetch` never returned.
        ///
        /// The admission is unchanged either way: same list, same two frames, same row as the
        /// human's act. **Who runs it is not who authorised it.**
        ///
        /// `#[serde(default)]` so a head built before this field reads as `false`; no
        /// `PROTOCOL_VERSION` bump for an added, defaulted field.
        #[serde(default)]
        execute: bool,
    },
    /// **What the operator's call produced** — the second of the two frames.
    ///
    /// Appends a `TranscriptItem::ToolResult` with `origin: Some(CallOrigin::Operator { who })`,
    /// so the model sees the result and every head draws it as the person's act.
    ///
    /// **No `expected_seq`**: this frame does not move the session, it hands over a fact the
    /// session is missing. A `call_id` the daemon is not holding is refused by name rather
    /// than appended — see [`ServerFrame::Rejected`].
    OperatorResult {
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        payload: String,
    },
    /// **The operator's own shell line** — a `!` command, run by the DAEMON, in this session's
    /// workspace.
    ///
    /// The operator's ask, in their words: *"when prompt starts with `!` it is going to be a shell
    /// command from me. it obviously must be allowed, full result … and sent to model as a
    /// message. so say `! ls .` does ls of the project dir and sends it to model. note - sudo is a
    /// must. so you need to implement asking me for a password."*
    ///
    /// # Why a new frame rather than the door
    ///
    /// [`ClientFrame::OperatorCall`] exists for calls the operator runs while a turn is running,
    /// and its allowlist ([`HEAD_RUN_TOOLS`]) is enforced by the daemon. `bash` is deliberately
    /// not on that list, and the reason is recorded beside it: the door RECORDS a call as a tool's
    /// act, with the tool's own name and JSON arguments and an admission row a corpus can stand
    /// behind. A raw shell line is none of that — it has no tool-owned name, and its "arguments"
    /// are the line itself. Widening the list to let `bash` through would make the door's own
    /// recorded reason false and its `field`-completion machinery (`HeadRunTool::field`) a lie for
    /// the one tool that takes a whole line. So this is a separate frame for a separate act: **the
    /// operator typed a shell line**, not *the operator ran a tool call for a tool that exists*.
    ///
    /// # What the daemon does with it
    ///
    /// Queued as [`crate::hub::CommandKind::OperatorShell`] like every other verb, and run by the
    /// session's worker — through `letibot_tools`' runtime as an operator call, which is to say
    /// the SAME execution path a model's `bash` call takes: the same backend, the same confinement
    /// and scratch directory, the same byte caps, and the same `SUDO_ASKPASS` standing
    /// environment, which is what makes `sudo` able to raise
    /// [`crate::event::SessionEvent::SecretRequested`] to a head. **The gate is not consulted** —
    /// `ToolRuntime::invoke_operator` is the door's own ruling ("nobody left to ask") applied
    /// here. Nothing in the handling of this frame writes an adjudication row, because there was
    /// no decision: the operator typed the line.
    ///
    /// What lands in the transcript is two rows, in order: the operator's own line as a `User`
    /// row with `speaker: Operator` (their words, verbatim, bang included), and the result as a
    /// `ToolResult` row named `bash` with `origin: CallOrigin::Operator` — which is what every
    /// head already draws with the tool-output treatment (folded, paged, sanitised per §3.1) and
    /// what the next prompt replays to the model.
    ///
    /// # The `!` rule, and where it is checked
    ///
    /// `line` is the submitted line **verbatim, bang included** — `! ls .`. The head owns the
    /// typing surface and refuses a line that is nothing but the bang (see its `submit`); the
    /// daemon re-checks here, because a frame that reached this daemon without a head — a
    /// hand-written socket, a future head with a different surface — must not be able to file an
    /// arbitrary sentence as the operator's shell line. A `line` that does not start with `!`, or
    /// has nothing but the bang and whitespace, is answered with [`ServerFrame::Rejected`] naming
    /// why, and nothing is queued.
    ///
    /// Stale-tolerant like a prompt and for the door's own reason: the operator who asked while
    /// the screen moved still meant it, and a turn running means the line is queued for the next
    /// round boundary, not refused.
    OperatorShell {
        client_request_id: String,
        expected_seq: u64,
        /// The line as submitted, `!` first. The command the daemon runs is everything after
        /// that first `!`, trimmed — one rule, applied at the execution site, so the head never
        /// sends a different spelling than the one the operator typed.
        line: String,
    },
    /// **Ask the model to propose `!` completions for a prefix** — the smart half of the
    /// `!` completion the operator asked for: *"i want smart ! when a model suggest
    /// completions"*.
    ///
    /// # Why a frame at all
    ///
    /// The head's history completion (the commands this session has actually run) is the first
    /// answer, and it is the head's own: the rows are on the screen. But when the history has
    /// no match for the prefix, the head has nothing left to offer — and it is the wrong
    /// party to invent one, because it has **no HTTP client and no transcript-wide context**,
    /// while the daemon has both. So the head sends what is typed and the daemon builds the
    /// prompt from the conversation and asks the model.
    ///
    /// # What the daemon does with it
    ///
    /// Builds the prompt from the session's own rows — the last handful condensed, the
    /// commands already run, the workspace path, and the prefix — and asks the **local**
    /// model: the `[gatekeeper]` endpoint the daemon already resolved into `cfg.oracle`,
    /// never a metered provider, because a suggestion must not cost money per keystroke. The
    /// call is bounded (a small output cap and a timeout), and a suggestion that does not
    /// arrive is nothing: the daemon answers with an empty list rather than waiting.
    ///
    /// **Nothing in the path submits.** The answer is a list of candidate lines for the
    /// composer; the head draws them as candidates, with their provenance, and Enter is
    /// still the operator's. This frame queues nothing, moves no seq, and writes no row —
    /// it is a read of the conversation through a model, like `FetchDiagnostic` is a read
    /// of the corpus.
    ///
    /// `prefix` is the composer's line as typed, `!` first — the same spelling the history
    /// completion matches, so the two halves of the feature share one needle.
    SuggestShell {
        client_request_id: String,
        expected_seq: u64,
        prefix: String,
    },
    /// **Ask for the bytes that justified one decision** — R11's locator, leticl's ask.
    ///
    /// A LOCATOR, not a payload: a head names one decision and one half of its exchange and
    /// the daemon answers with those bytes or with *not recorded*. The same shape as
    /// [`ClientFrame::FetchRow`] — *the head asks the daemon for something big it does not
    /// normally hold* — and for the same reason: the brief and the reply are on the corpus row
    /// and on no frame, so without this a head can only show a sentence ABOUT them.
    ///
    /// `request_id` is the adjudication's own id, the one `/gate` takes and the row is keyed by.
    /// Answered with [`ServerFrame::Diagnostic`].
    FetchDiagnostic {
        request_id: String,
        kind: DiagnosticKind,
    },
    /// List the settings this session runs under. Answered with
    /// [`ServerFrame::Settings`]; never moves the connection.
    Settings,
    /// Ask for a window of one background job's output, for a head's jobs pane.
    ///
    /// A command, so it reaches the worker that owns the exec host — see
    /// [`CommandKind::ReadJobOutput`]. Answered with
    /// [`crate::event::SessionEvent::JobOutput`] on the log, which is what every other verb's
    /// answer is.
    ReadJobOutput {
        client_request_id: String,
        job: String,
        offset: u64,
    },
    /// A clean goodbye. **Not** required: TCP close is detach too, and detach is
    /// never abort (§13.2).
    Detach,
}

/// The read mark, as a type.
///
/// `seq` is *the last seq this head consumed*, whether or not it rendered it. It is
/// produced by [`crate::cursor::Batch::last_seq`] and there is deliberately no
/// other way to obtain one: a mark that advances only over what a filtering
/// consumer kept makes it reread its own output forever, which §13.2b calls the
/// single most reusable bug in flowy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub seq: u64,
    /// How many of those the head actually displayed.
    pub rendered: u64,
    /// How many it suppressed. *"Busy, and none of it was for me"* is a different
    /// fact from *"quiet"*, and this field is the difference. Not `Option`: a head
    /// that filters nothing reports zero.
    pub filtered: u64,
}

/// Daemon → head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum ServerFrame {
    /// The answer to ATTACH. Always first, and **once per attachment**.
    ///
    /// It used to be "exactly once", which was true while a connection could only
    /// ever be in one session. [`ClientFrame::Switch`] makes a connection able to
    /// leave one and join another, and the frame that says *"you are now in this
    /// session, here is its state as of seq N"* is exactly this one — inventing a
    /// second frame that said the same thing would leave two attach paths to keep
    /// in step, which is how a resync path rots.
    Hello {
        protocol_version: u32,
        session_id: String,
        head_id: String,
        /// Events that fell off the back of the scrollback and this head will never
        /// see. **Present and zero** — the disclosure is the field, not its absence.
        dropped: u64,
        /// `Some` for a snapshot attach or a demoted resume; `None` for a resume
        /// that was served from the scrollback, whose gap follows as `Event` frames.
        ///
        /// Boxed because a snapshot is two orders of magnitude larger than every
        /// other frame, and an unboxed one makes *every* `ServerFrame` — including
        /// the `Event` that carries a three-character delta — 528 bytes. On the
        /// hot path there is one delta per token and one Hello per lifetime.
        snapshot: Option<Box<Snapshot>>,
        /// For a resume: the seq the gap starts after.
        resumed_from: Option<u64>,
        /// What the replay scrub stripped on the way here. The daemon's half of
        /// "report what was filtered": zero here on a live-only attach, nonzero
        /// whenever a decision had already settled.
        scrubbed: ScrubReport,
        /// **What this session is talking to** — `crates/ui/DESIGN.md` §4.4.
        ///
        /// `TurnStarted { model }` was the only one of these that ever reached a
        /// head, and only when a turn started, so a freshly attached head with no
        /// turn yet could say nothing at all and rendered `no turn yet`. The daemon
        /// has known all four since its own command line was parsed. Two daemons on
        /// one box serving two models on two ports is the normal case here, and a
        /// head that cannot say which one it is attached to is a head you have to
        /// guess about.
        ///
        /// Empty strings when the daemon's owner supplied none — never a plausible
        /// default, which would be a guess a head then quotes as a fact.
        wiring: SessionWiring,
        /// Every session this daemon holds, so a picker is populated by the attach
        /// itself and not by a second round trip. Includes the one just joined.
        sessions: Vec<SessionBrief>,
    },
    /// The answer to [`ClientFrame::ListSessions`] and to
    /// [`ClientFrame::NewSession`].
    ///
    /// `NewSession` is answered with the whole list rather than with the new id
    /// alone, because a head that has just created a session is a head about to
    /// draw a picker, and the list it would then ask for is this one.
    /// The password for the `Askpass` this connection sent, or `None`: nobody
    /// gave one before the deadline, or a head refused. Only ever written to an
    /// `askpass` head. Added at `PROTOCOL_VERSION` 12.
    Secret { secret: Option<String> },
    Sessions {
        sessions: Vec<SessionBrief>,
        /// The session this connection is in right now.
        current: String,
        /// The id `NewSession` created, when that is what this is answering.
        /// `None` for a plain list — present and null, not omitted.
        created: Option<String>,
    },
    /// The answer to [`ClientFrame::ListTodos`], for the session the connection
    /// is in. The whole list as of now — later changes arrive as
    /// [`crate::SessionEvent::TodosUpdated`].
    Todos {
        session_id: String,
        todos: Vec<crate::event::TodoEntry>,
    },
    /// The answer to [`ClientFrame::Peek`]: the named session's retained
    /// scrollback, scrubbed exactly as a replay is. `dropped` is what fell off the
    /// daemon's ring before the peek — the same disclosure a `Hello` makes. The
    /// connection's own session is untouched; these events are for reading, not
    /// for folding into the head's state.
    /// The answer to [`ClientFrame::Settings`].
    Settings { rows: Vec<SettingRow> },
    /// The answer to [`ClientFrame::ListJobs`]: the whole list as of now. Later
    /// changes arrive as [`crate::SessionEvent::JobSettled`], the way todos work.
    Jobs {
        session_id: String,
        jobs: Vec<JobEntry>,
    },
    /// The answer to [`ClientFrame::ListMergeQueue`]: the whole queue as of now, every state.
    ///
    /// The snapshot half of the snapshot-plus-events pattern: a head attaching mid-flight gets
    /// this, and from then on the [`crate::SessionEvent::MergeEntryAdded`] and
    /// [`crate::SessionEvent::MergeEntryMoved`] events carry every change. The queue is
    /// daemon-level, so there is no `session_id` — the `session_id` on each entry is the
    /// entry's origin, not a filter.
    ///
    /// **Nothing is dropped silently**: the queue is the whole queue, and an entry the queue
    /// cannot act on is listed with its reason — the `evidence` on the row says what it is
    /// waiting on, why it failed, or why it is stale. A shorter list would say *"that is all
    /// the work there is"*, which is false.
    MergeQueue {
        entries: Vec<crate::event::MergeEntry>,
        /// **The verdicts, beside the entries.** `serde(default)` and additive: a head older
        /// than the field reads past it and draws the queue without the reviews, which is what
        /// it did before — and a daemon older than it sends none, which is not an error but
        /// *nobody recorded one*. See [`crate::event::MergeReview`] for why it travels here
        /// rather than inside an entry.
        #[serde(default)]
        reviews: Vec<crate::event::MergeReview>,
    },
    Peeked {
        session_id: String,
        dropped: u64,
        events: Vec<Envelope>,
        /// **The session's rows, when the head asked for [`PeekShape::Rows`].**
        ///
        /// `Some` exactly when the request said `Rows`, and `None` otherwise — so a head can tell
        /// *this daemon does not know the field* from *there was nothing to send*, which is the same
        /// rule `Hello`'s own `snapshot` follows. Both are absent-means-old; neither is an error.
        ///
        /// **Boxed for `Hello`'s reason, and it is the same frame-size argument**: a snapshot is
        /// two orders of magnitude larger than everything else on this wire, and an unboxed field
        /// would cost *every* `ServerFrame` — the `Event` carrying a three-character delta
        /// included — the space of the largest one.
        #[serde(default)]
        snapshot: Option<Box<Snapshot>>,
    },
    /// The answer to [`ClientFrame::FetchRow`]: a window of one row's body.
    ///
    /// `total` is the whole body's length, so the head knows **what is on either side of
    /// the window** without holding it — which is what lets it draw `… +N lines above`
    /// and `… +M below` honestly. `at` is echoed because the request is clamped rather
    /// than refused, so where the answer starts is the daemon's decision and not a
    /// restatement of the request.
    ///
    /// **`body: None` means the row does not exist, not that the daemon must be asked
    /// elsewhere.** The daemon is a **proxy** for anything its own caches do not hold —
    /// see [`ClientFrame::FetchRow`] — so a head asks once and never learns which tier
    /// answered. `None` for a row past the end of the session, and for a session this
    /// daemon cannot reach the store of; an empty string would read as "the row is empty"
    /// rather than "there is no such row", and the two must not look alike.
    RowFetched {
        session_id: String,
        /// The session ordinal that was asked for, echoed so the answer names its row.
        row: usize,
        /// Byte offset this window actually starts at.
        at: usize,
        /// The window itself, starting on a **character boundary** — a head cannot render
        /// half a glyph and the daemon is the side that knows the encoding.
        body: Option<String>,
        /// The whole body's length in bytes, so a head can say what is on either side.
        total: usize,
    },
    /// **The bytes a head asked for, or that there are none** — the answer to
    /// [`ClientFrame::FetchDiagnostic`].
    ///
    /// **`body: None` is "not recorded" and it is not an empty string.** The store holds `NULL`
    /// on every row written before R11 kept the exchange, and an oracle that never answered has
    /// no reply either; a head must tell *"nobody kept this"* from *"here it is, and it is
    /// empty"*, which is the rule [`ServerFrame::RowFetched`]'s own doc states one field over.
    ///
    /// # Which of the two the store can actually produce, measured (2026-09-23, leticl's ask)
    ///
    /// leticl measured the corpus and found **zero empty strings**: 14,439 rows, `shown` on
    /// 411 and `oracle_reply` on 264, none of them `''`. That raises the question the count
    /// cannot answer on its own — *is the third state theoretical, or is the WRITER destroying
    /// it?* — and the two have opposite fixes. Measured on this box, at 14,528 rows, the same
    /// zero holds, and the answer is **the first**:
    ///
    /// * **The writer preserves it.** `Store::record_adjudication` binds `shown` and `reply`
    ///   straight into the `INSERT`, so a `Some("")` lands as `''` and reads back as
    ///   `Some("")`. Nothing flattens an empty string to `NULL` on the way in.
    /// * **The readers preserve it.** `Store::diagnostic` flattens `Option<Option<String>>`, so
    ///   *no row* and `NULL` are one sentence — and `''` stays `Some("")`. `total` agrees: it is
    ///   a length, so `0` with `Some(body)` is an empty kept reply and `0` with `None` is
    ///   nothing kept.
    /// * **So the absence is a fact about the two PRODUCERS, not about the seam.** `shown` is
    ///   `ModelBrief::render()`, which opens with a fixed paragraph and therefore cannot be
    ///   empty. `oracle_reply` is `choices[0].message.content` from a successful HTTP answer
    ///   (`harnessd/src/oracle.rs`), which *could* be `""` if a server returned one — and has
    ///   not, on this corpus, once.
    ///
    /// **Which is why the `Option` stays.** It costs nothing (an empty `String` and a `None`
    /// are the same size here), it documents the intent at the seam that has to keep the two
    /// apart, and the day a guard model answers with nothing the row will say *it answered and
    /// said nothing* rather than *nobody kept this*. Leaving it also means the wire does not
    /// need a second look when a producer changes, which is the point of a seam: the layers
    /// below may be wrong about a fact without the layer that carries it being wrong too.
    Diagnostic {
        request_id: String,
        kind: DiagnosticKind,
        body: Option<String>,
        /// Bytes the field holds, or 0 when there is none. Present rather than inferred, so a
        /// head renders a length from a fact and not from `body.map(len).unwrap_or(0)` — which
        /// cannot tell "empty" from "absent" either.
        total: usize,
    },
    /// **The model's proposed `!` completions** — the answer to
    /// [`ClientFrame::SuggestShell`].
    ///
    /// `lines` are candidate shell lines, `!` first, in the order the model offered them.
    /// **Empty when the model said nothing usable** — no local endpoint, a timeout, or a
    /// reply the defensive parse ([`crate::suggest::parse_suggestions`]) dropped to nothing.
    /// An empty list and a missing frame are the same fact to the head (*no suggestion*),
    /// so the daemon always answers rather than staying silent: a head that could not tell
    /// "the model had no idea" from "the daemon never answered" would keep waiting on a
    /// suggestion that is not coming.
    ///
    /// `prefix` is the ask's prefix, echoed back so the head can key its cache by it
    /// without holding a request-id table — the same shape `Peeked` uses to name the
    /// session it is about.
    ///
    /// **Nothing here is a command.** The head draws the lines as candidates, marked as
    /// the model's rather than the operator's, and only a Tab fills the composer with one.
    /// Enter is still the operator's.
    ShellSuggestions {
        client_request_id: String,
        prefix: String,
        lines: Vec<String>,
    },
    /// One appended event, in seq order, with no gaps between consecutive frames.
    Event(Envelope),
    /// The head's queue overflowed, or its resume gap was too large. **Not an
    /// error.** The head resets to the enclosed snapshot and carries on from
    /// `snapshot.seq + 1`.
    Resync {
        reason: String,
        dropped: u64,
        snapshot: Box<Snapshot>,
        scrubbed: ScrubReport,
    },
    /// A command was serialized and applied.
    Accepted {
        client_request_id: String,
        /// The seq at which its effect is visible.
        seq: u64,
        note: String,
    },
    /// A command was refused. Both numbers travel so the head can say what it was
    /// looking at when it acted.
    Rejected {
        client_request_id: String,
        reason: String,
        expected_seq: u64,
        actual_seq: u64,
    },
    /// The daemon is going away. Detach is not abort; this is the case that is.
    Bye { reason: String },
}

/// Why a `Rejected` was sent, as a stable code a head can branch on.
pub const REJECT_STALE_SEQ: &str = "stale expected_seq";
/// The `note` on a prompt that was accepted with nothing unusual about it.
///
/// A constant rather than a literal in two places because a head has a reason to
/// recognise it: telling the operator who just pressed enter that their prompt was
/// queued is not news, while telling them that *another head's* prompt was queued
/// is the whole point of §13.2's announcement.
pub const NOTE_PROMPT_QUEUED: &str = "queued as a user item";
/// The `note` on a `/compact` that was accepted with nothing unusual about it.
///
/// Distinct from [`NOTE_PROMPT_QUEUED`] on purpose: a compaction that lands
/// behind an already-running turn happens **after** that turn, and the operator
/// who asked for it should be able to tell "queued behind the running turn" from
/// "queued as something the model will read" — the two notes are both a queue,
/// but they are not the same queue.
pub const NOTE_COMPACT_QUEUED: &str = "queued after the running turn";
/// **The `note` on a `Stop` that this daemon is about to honour** (R30).
///
/// The only `Accepted` a head treats as more than bookkeeping. It is written **before**
/// the registry closes and before the socket goes, on purpose: it is the head's one piece
/// of evidence that its request was *read* rather than merely written, and a head that
/// asked the daemon to stop waits for it (or for the process to go) before it exits.
///
/// Its wording is the head's own reason for that wait written on the wire: *stopping* is
/// what the daemon intends, and the head still checks that it happened — because the
/// incident this constant exists for was a daemon that **never read the frame at all** and
/// a head that exited as though it had.
pub const NOTE_STOPPING: &str = "stopping";
pub const REJECT_UNKNOWN_DECISION: &str = "no such open decision";
pub const REJECT_READ_ONLY: &str = "this head declared can_decide: false";
/// A `Switch` or an `Attach` named a session this daemon does not hold.
///
/// Refused by name rather than answered with the default session: a typo that
/// seats you in somebody else's conversation looks exactly like a working attach
/// to an empty one, and you find out by prompting into it.
pub const REJECT_UNKNOWN_SESSION: &str = "no such session";
/// A `ResumeSession` named a session that is in neither the daemon nor the store.
///
/// Distinct from [`REJECT_UNKNOWN_SESSION`] on purpose: "this daemon does not hold
/// it" and "nothing anywhere has ever heard of it" send an operator to two different
/// places, and collapsing them is how a typo becomes half an hour with a database.
pub const REJECT_NOT_IN_STORE: &str = "no such session in the store";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{SessionView, ViewBounds};

    /// **A signpost at the version, where the compiler does not put one.**
    ///
    /// `PROTOCOL_VERSION` is the only compatibility check the two halves have, and
    /// on 2026-09-20 three frames landed at version 21 without it moving —
    /// `ListJobs`/`Jobs`, `ReseatSession.summarise`, and `ReadJobOutput`/`JobOutput`,
    /// which is the one that cost an afternoon: ATTACH agreed, and the skew
    /// surfaced mid-session as a deserialization failure that hung the connection
    /// up in silence.
    ///
    /// **The compiler already catches the variant.** Measured, by adding one to
    /// each enum: a new `ClientFrame` fails `server.rs`'s read loop, and a new
    /// `SessionEvent` fails four matches across the view and the head. So "nothing
    /// noticed" was never true — what is true is that every one of those errors
    /// points at a HANDLER, the author writes the handler because they must, and
    /// the version never comes up. The check existed and pointed at the wrong
    /// decision.
    ///
    /// This one points at the right one, and it is a match rather than a count so
    /// that it names WHICH frame is new. Same device `mode.rs` uses for `Boundary`:
    /// *"written as 'not `Operator`' rather than as a two-arm match so that adding
    /// a third boundary later is a compile error here, where the decision
    /// belongs."*
    ///
    /// Both directions, because the skew runs both ways: a client frame kills the
    /// DAEMON's read, an event kills the HEAD's. `JobOutput` was the second kind.
    ///
    /// **If you are here because this stopped compiling:** you added a frame or an
    /// event. Bump `PROTOCOL_VERSION`, add a `# N:` section to the history above
    /// saying which half would fail to parse it, then add the arm.
    #[test]
    fn every_frame_is_accounted_for_at_this_version() {
        fn client(f: &ClientFrame) {
            match f {
                ClientFrame::Ack { .. }
                | ClientFrame::Answer { .. }
                | ClientFrame::AnswerQuestion { .. }
                | ClientFrame::Askpass { .. }
                | ClientFrame::Attach { .. }
                | ClientFrame::CompactSession { .. }
                | ClientFrame::Detach { .. }
                | ClientFrame::FetchDiagnostic { .. }
                | ClientFrame::FetchRow { .. }
                | ClientFrame::Interrupt { .. }
                | ClientFrame::ListJobs { .. }
                | ClientFrame::ListMergeQueue { .. }
                | ClientFrame::ListSessions { .. }
                | ClientFrame::ListTodos { .. }
                | ClientFrame::Mode { .. }
                | ClientFrame::NewSession { .. }
                | ClientFrame::OperatorCall { .. }
                | ClientFrame::OperatorResult { .. }
                | ClientFrame::OperatorShell { .. }
                | ClientFrame::Peek { .. }
                | ClientFrame::Promote { .. }
                | ClientFrame::Prompt { .. }
                | ClientFrame::ReadJobOutput { .. }
                | ClientFrame::RenameSession { .. }
                | ClientFrame::ReseatSession { .. }
                | ClientFrame::ResumeSession { .. }
                | ClientFrame::Resync { .. }
                | ClientFrame::Screen { .. }
                | ClientFrame::Secret { .. }
                | ClientFrame::Settings { .. }
                | ClientFrame::Slash { .. }
                | ClientFrame::SuggestShell { .. }
                | ClientFrame::Stop { .. }
                | ClientFrame::Switch { .. }
                | ClientFrame::SetOperatorTodos { .. }
                | ClientFrame::WithdrawPrompts { .. } => {}
            }
        }
        fn event(e: &crate::SessionEvent) {
            match e {
                crate::SessionEvent::CommandIssued { .. }
                | crate::SessionEvent::DecisionAnswered { .. }
                | crate::SessionEvent::DecisionRequested { .. }
                | crate::SessionEvent::Delta { .. }
                | crate::SessionEvent::DenialRaised { .. }
                | crate::SessionEvent::Explain { .. }
                | crate::SessionEvent::HeadAttached { .. }
                | crate::SessionEvent::HeadDetached { .. }
                | crate::SessionEvent::JobOutput { .. }
                | crate::SessionEvent::JobSettled { .. }
                | crate::SessionEvent::MergeEntryAdded { .. }
                | crate::SessionEvent::MergeEntryMoved { .. }
                | crate::SessionEvent::OperatorCallAllowed { .. }
                | crate::SessionEvent::Filling { .. }
                | crate::SessionEvent::CompactionProgress { .. }
                | crate::SessionEvent::PromptProgress { .. }
                | crate::SessionEvent::ScreenRequested { .. }
                | crate::SessionEvent::SecretRequested { .. }
                | crate::SessionEvent::SecretSettled { .. }
                | crate::SessionEvent::SessionRenamed { .. }
                | crate::SessionEvent::Subagent { .. }
                | crate::SessionEvent::TodosUpdated { .. }
                | crate::SessionEvent::TokensGenerated { .. }
                | crate::SessionEvent::ToolCallProposed { .. }
                | crate::SessionEvent::ToolFinished { .. }
                | crate::SessionEvent::ToolProgress { .. }
                | crate::SessionEvent::ToolStarted { .. }
                | crate::SessionEvent::TranscriptAppended { .. }
                | crate::SessionEvent::TranscriptContent { .. }
                | crate::SessionEvent::TurnFailed { .. }
                | crate::SessionEvent::TurnFinished { .. }
                | crate::SessionEvent::TurnInterrupted { .. }
                | crate::SessionEvent::TurnStarted { .. }
                | crate::SessionEvent::Warning { .. } => {}
            }
        }
        // **And the server frames, which this did not cover until 2026-09-23.**
        //
        // A new `ServerFrame` reaches a head that cannot parse it exactly as a new
        // `ClientFrame` reaches a daemon that cannot — and this match was written for the two
        // directions that were already here, so the third had no signpost at all. Found by
        // adding `Diagnostic` and noticing nothing asked about it.
        fn server(f: &ServerFrame) {
            match f {
                // The first line is unindented because this list was generated from the enum's
                // own body; the tuple variant `Event(_)` had to be added by hand, which is
                // itself the argument for the match existing.
                ServerFrame::Hello { .. }
                | ServerFrame::Event(_)
                | ServerFrame::Secret { .. }
                | ServerFrame::Sessions { .. }
                | ServerFrame::Todos { .. }
                | ServerFrame::Settings { .. }
                | ServerFrame::Jobs { .. }
                | ServerFrame::MergeQueue { .. }
                | ServerFrame::Peeked { .. }
                | ServerFrame::RowFetched { .. }
                | ServerFrame::Diagnostic { .. }
                | ServerFrame::ShellSuggestions { .. }
                | ServerFrame::Resync { .. }
                | ServerFrame::Accepted { .. }
                | ServerFrame::Rejected { .. }
                | ServerFrame::Bye { .. } => {}
            }
        }
        let _ = client;
        let _ = event;
        let _ = server;
        assert_eq!(
            PROTOCOL_VERSION, 30,
            "the match above was last reconciled with the frame list at 30 — bumped for \
             `SuggestShell`, a NEW client frame (a version-29 daemon would fail to parse \
             it at ATTACH, the version-4 argument), and `ShellSuggestions`, its NEW server \
             frame (a version-29 head would fail to decode it mid-session, the version-25 \
             argument), as opposed to an added defaulted field, which is the case that \
             needs no bump. 29 was `ListMergeQueue`/`MergeQueue` and the two merge-queue \
             events"
        );
    }

    /// **A re-seat that does not say keeps the conversation.**
    ///
    /// The operator flipped the default here — *"id say flip it - reset is
    /// loseless and reset summarize will be not"* — and the flip has a wire
    /// consequence worth pinning: a head too old to send `summarise` sends the
    /// frame without it, and what it then gets is the LOSSLESS fork, not the
    /// summarising one it used to get. That is the safe direction (it costs a
    /// prefill, not a conversation), and it is a decision, so it is asserted
    /// rather than left to `#[serde(default)]`'s reputation.
    /// **A frame with no `items` is an EMPTY HALF, not a malformed frame.**
    ///
    /// MEASURED from the live log before this existed: an operator whose todo list was empty sent
    /// `{"frame":"set_operator_todos","client_request_id":"leticl-1","expected_seq":4537}` — no
    /// `items` at all, because the sending head's encoder omits every key whose value is NIL and an
    /// empty list is NIL. `items` was a bare required field, so the daemon answered
    /// `malformed frame (missing field \`items\`)` and **dropped the head connection**, on every
    /// connect. From the operator's side that is a daemon that will not start.
    ///
    /// **An absent list and an empty list are the same statement** — *I have no rows* — which is what
    /// makes `#[serde(default)]` the honest reading rather than a tolerance. The head sends its half
    /// WHOLE on every change (its own docstring: *"the whole list and not a delta"*), so a frame with
    /// nothing to say about rows says nothing, and the half is empty.
    ///
    /// No version bump: accepting a key that used to be required is strictly more permissive, and the
    /// frame-list check's own message says the added-defaulted-field case is the one that "needs no
    /// bump".
    /// **The two other collection fields on a client frame, checked rather than assumed.**
    ///
    /// The `items` defect is not "a required field" — it is *a field the sending head can omit and
    /// the daemon treats as fatal*. That head's encoder drops every NIL, so any collection it can
    /// send empty is the same hazard, and `Secret.secret` and `Screen.rows` are the only other two on
    /// this enum.
    ///
    /// `Option<T>` is safe by serde's own rule — a missing key deserialises to `None` — which is the
    /// claim this asserts rather than trusting, because a head that REFUSES a password sends no
    /// `secret` and being disconnected for a refusal would be a spectacular bug.
    #[test]
    fn a_secret_refusal_and_an_empty_screen_do_not_take_the_socket_with_them() {
        let refusal: ClientFrame = serde_json::from_str(
            r#"{"frame":"secret","client_request_id":"r1","expected_seq":1,"req_id":"q1"}"#,
        )
        .expect("a refused password sends no secret at all");
        let ClientFrame::Secret { secret, .. } = refusal else {
            panic!("not a secret frame")
        };
        assert_eq!(secret, None, "an absent secret is a refusal, not an error");

        // And the screen's rows: the same shape as `items`, so it carries the same default.
        let empty: ClientFrame = serde_json::from_str(
            r#"{"frame":"screen","client_request_id":"r2","expected_seq":2,"req_id":"q2",
                 "cols":80,"rows_n":0}"#,
        )
        .expect("a screen with no rows must not be fatal");
        let ClientFrame::Screen { rows, rows_n, .. } = empty else {
            panic!("not a screen frame")
        };
        assert!(rows.is_empty(), "{rows:?}");
        assert_eq!(rows_n, 0);
    }

    #[test]
    fn a_set_operator_todos_frame_without_items_is_an_empty_half_not_a_malformed_frame() {
        let f: ClientFrame = serde_json::from_str(
            r#"{"frame":"set_operator_todos","client_request_id":"leticl-1","expected_seq":4537}"#,
        )
        .expect("the frame an empty half sends must parse");
        let ClientFrame::SetOperatorTodos {
            expected_seq,
            items,
            ..
        } = f
        else {
            panic!("not a set_operator_todos: {f:?}");
        };
        assert_eq!(expected_seq, 4537, "the rest of the frame still reads");
        assert!(
            items.is_empty(),
            "an absent list is an empty half: {items:?}"
        );

        // And the spelling WITH items is unaffected — the two neighbours this could break are a
        // full half and a one-row half, so both are asserted here rather than assumed.
        let full: ClientFrame = serde_json::from_str(
            r#"{"frame":"set_operator_todos","client_request_id":"r2","expected_seq":9,
                 "items":[{"content":"push leticl to github","status":"pending","by":"operator"}]}"#,
        )
        .expect("a full half still parses");
        let ClientFrame::SetOperatorTodos { items, .. } = full else {
            panic!("not a set_operator_todos");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].content, "push leticl to github");
        assert_eq!(items[0].by, crate::event::TodoBy::Operator);
    }

    #[test]
    fn a_reseat_frame_without_the_field_is_the_lossless_kind() {
        let f: ClientFrame = serde_json::from_str(
            r#"{"frame":"reseat_session","client_request_id":"r1","expected_seq":7}"#,
        )
        .expect("an older head's frame still parses");
        let ClientFrame::ReseatSession {
            expected_seq,
            summarise,
            ..
        } = f
        else {
            panic!("not a reseat: {f:?}");
        };
        assert_eq!(expected_seq, 7, "the rest of the frame still reads");
        assert!(
            !summarise,
            "a frame that does not ask to summarise must not summarise"
        );
    }

    #[test]
    fn dropped_is_present_and_zero_on_hello() {
        let f = ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            session_id: "s".into(),
            head_id: "h1".into(),
            dropped: 0,
            snapshot: Some(Box::new(
                SessionView::new("s", ViewBounds::default()).snapshot(0, 0),
            )),
            resumed_from: None,
            scrubbed: ScrubReport::default(),
            wiring: SessionWiring::default(),
            sessions: Vec::new(),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""dropped":0"#), "{json}");
        assert!(json.contains(r#""prompt_progress":0"#), "{json}");
        // The §4.4 fields are present and empty rather than absent, for the same
        // reason `dropped` is present and zero: "this daemon does not know what it
        // is talking to" and "this build does not report it" must not be the same
        // bytes.
        assert!(json.contains(r#""endpoint":"""#), "{json}");
        assert!(json.contains(r#""sessions":[]"#), "{json}");
    }

    #[test]
    fn no_frame_field_is_elided_when_zero_or_empty() {
        // The whole module is a disclosure surface. `skip_serializing_if` here would
        // make "nothing was filtered" and "this build does not report filtering"
        // identical on the wire.
        // Assembled at runtime so the assertion does not match itself, and applied
        // to attribute lines only so that prose about the rule is not the rule.
        let needle = format!("skip_serializing{}if", "_");
        let offender = include_str!("protocol.rs")
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("#[serde") && l.contains(&needle));
        assert_eq!(
            offender, None,
            "an absent field and a zero field must not look the same"
        );
    }

    #[test]
    fn every_frame_round_trips() {
        let ack = ClientFrame::Ack(Ack {
            seq: 7,
            rendered: 3,
            filtered: 4,
        });
        for f in [
            ClientFrame::Attach {
                protocol_version: PROTOCOL_VERSION,
                session_id: "s".into(),
                since_seq: 0,
                kind: "tui".into(),
                identity: "dead@lab2x1".into(),
                caps: Caps::default(),
            },
            ack,
            ClientFrame::Resync,
            ClientFrame::Prompt {
                client_request_id: "r1".into(),
                expected_seq: 12,
                text: "hello".into(),
            },
            ClientFrame::WithdrawPrompts {
                client_request_id: "r1w".into(),
                expected_seq: 12,
            },
            ClientFrame::Stop {
                client_request_id: "r1s".into(),
                expected_seq: 12,
                who: "dead".into(),
            },
            ClientFrame::Interrupt {
                client_request_id: "r2".into(),
                expected_seq: 12,
                reason: "wrong file".into(),
            },
            ClientFrame::Answer {
                client_request_id: "r3".into(),
                req_id: "d1".into(),
                option_id: "allow_once".into(),
                // The round trip must cover the glob too: an added field that is
                // never exercised is an added field that silently stops encoding.
                pattern: Some("crates/**/*.rs".into()),
                note: None,
            },
            ClientFrame::ListSessions,
            ClientFrame::ListTodos,
            ClientFrame::NewSession {
                client_request_id: "r4".into(),
                title: "the cache question".into(),
                workspace: "/home/dead/Projects/letibot".into(),
            },
            ClientFrame::ResumeSession {
                client_request_id: "r5".into(),
                session_id: "s-1788987496351498881".into(),
            },
            ClientFrame::RenameSession {
                client_request_id: "r6".into(),
                session_id: "s-2".into(),
                title: "the cache question".into(),
            },
            ClientFrame::Switch {
                session_id: "s-2".into(),
                since_seq: 0,
            },
            ClientFrame::Detach,
        ] {
            let s = serde_json::to_string(&f).unwrap();
            assert_eq!(f, serde_json::from_str::<ClientFrame>(&s).unwrap(), "{s}");
        }
    }

    #[test]
    fn a_sessions_frame_says_which_one_you_are_in() {
        // A list with no "you are here" is a list you cannot act on: every row
        // looks equally switchable and one of them is a no-op.
        let f = ServerFrame::Sessions {
            sessions: Vec::new(),
            current: "s-1".into(),
            created: None,
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""current":"s-1""#), "{json}");
        assert!(json.contains(r#""created":null"#), "{json}");
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());
    }

    #[test]
    fn a_todos_frame_round_trips_with_its_statuses() {
        let f = ServerFrame::Todos {
            session_id: "s-1".into(),
            todos: vec![
                crate::event::TodoEntry {
                    content: "read the harness".into(),
                    status: crate::event::TodoStatus::Completed,
                    by: crate::event::TodoBy::Model,
                    when: None,
                },
                crate::event::TodoEntry {
                    content: "render the pane".into(),
                    status: crate::event::TodoStatus::InProgress,
                    by: crate::event::TodoBy::Model,
                    when: None,
                },
            ],
        };
        let json = serde_json::to_string(&f).unwrap();
        // The statuses spell the way the store spells them, so a `sqlite3`
        // reader and a head reader agree.
        assert!(json.contains(r#""status":"completed""#), "{json}");
        assert!(json.contains(r#""status":"in_progress""#), "{json}");
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());
    }

    /// **A row's condition is TAGGED on the wire, and that tag is the contract.**
    ///
    /// The operator's shape: *"More like Option<TodoCondition> and then we can have many
    /// conditions, and we can instantiate them programmatically or via a form"*. A tagged enum is
    /// what makes a new kind ADDITIVE, and what lets a reader tell **a condition it does not know**
    /// from **no condition at all** — the distinction `TodoItem::when` exists for, because a
    /// condition nobody can evaluate must never read as *met*.
    ///
    /// Both halves of the round trip are asserted, and the literal `"kind":"job"` is the point:
    /// this is the one fact two independently written heads have to agree about, so a rename here
    /// is a wire change and must fail a test rather than quietly change a spelling.
    #[test]
    fn a_todo_condition_is_tagged_on_the_wire_and_survives_the_round_trip() {
        let f = ServerFrame::Todos {
            session_id: "s-1".into(),
            todos: vec![crate::event::TodoEntry {
                content: "push once CI lands".into(),
                status: crate::event::TodoStatus::Pending,
                by: crate::event::TodoBy::Operator,
                when: Some(crate::event::TodoCondition::Job {
                    handle: "j121".into(),
                }),
            }],
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains(r#""when":{"kind":"job","handle":"j121"}"#),
            "the condition's own spelling is the wire contract: {json}"
        );
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());

        // **And a row written before the field — no `when` at all — reads as unconditional**,
        // which is what it was. Without this half, a head would refuse every row already in a
        // store, and `serde(default)` would be a claim rather than a fact.
        let old = json.replace(r#","when":{"kind":"job","handle":"j121"}"#, "");
        assert!(!old.contains("when"), "the field was not removed: {old}");
        let ServerFrame::Todos { todos, .. } =
            serde_json::from_str::<ServerFrame>(&old).expect("a row with no condition reads")
        else {
            panic!("a todos frame")
        };
        assert_eq!(
            todos[0].when, None,
            "an absent condition is `None` — not an error, and not a default condition"
        );
    }
}

#[cfg(test)]
mod skew_tests {
    use super::protocol_skew;

    /// **The sentence says nothing when the two match, and the direction when they do
    /// not** — because the two directions are different answers to the same numbers.
    ///
    /// A matching version says nothing at all: every attach would otherwise carry a line
    /// announcing the normal case, and an operator who learns to skip one line learns to
    /// skip the one that matters. A newer daemon means frames this head may not know,
    /// which it reports and skips — that is R3, and it is why this check does not exit. An
    /// older one means the head's own frames are the hazard: a command the daemon has
    /// never heard of fails *its* reader, and its answer is to say goodbye and close the
    /// socket, so the session can end on the next thing typed.
    #[test]
    fn a_match_says_nothing_and_a_skew_names_its_direction() {
        assert_eq!(protocol_skew(22, 22), None);

        let newer = protocol_skew(24, 22).expect("a newer daemon is worth a sentence");
        assert!(newer.contains("24") && newer.contains("22"), "{newer}");
        assert!(newer.contains("NEWER"), "{newer}");
        assert!(
            newer.contains("skipped"),
            "it must say what the head will do about the frames: {newer}"
        );

        let older = protocol_skew(20, 22).expect("an older daemon is worth a sentence");
        assert!(older.contains("OLDER"), "{older}");
        assert!(
            older.contains("closing the socket"),
            "the older case is quiet until it is fatal, so it has to say so: {older}"
        );

        // Two situations, two sentences — a test that only checked `!= None` would pass on
        // a function that said the same wrong thing twice.
        assert_ne!(newer, older);
        // Neither of them tells anybody to leave. A skew is usually survivable; that is
        // R3's whole argument, and exiting here would be the failure it argues against.
        for s in [newer, older] {
            assert!(!s.contains("quit"), "{s}");
        }
    }
}

#[cfg(test)]
mod the_door_verbs_transform {
    use super::*;

    /// **The hyphen is a keyboard transform and the wire keeps the tool's name** (R34).
    ///
    /// Asserted against [`HEAD_RUN_TOOLS`] itself, so the day a door verb is added the test
    /// says what its typed spelling is without anybody writing it down twice.
    #[test]
    fn every_door_tool_has_a_hyphened_verb_and_the_tool_keeps_its_own_name() {
        for tool in HEAD_RUN_TOOLS {
            let verb = head_run_verb(tool);
            assert!(!verb.contains('_'), "`/{verb}` still needs the shift key");
            // **The wire is unmoved**: the tool's own spelling answers, and the transform is
            // `_` → `-` and nothing else.
            assert_eq!(head_run_tool(&verb, &HEAD_RUN_TOOLS), Some(tool));
            assert_eq!(head_run_tool(tool, &HEAD_RUN_TOOLS), Some(tool));
        }
        assert_eq!(head_run_verb("web_search"), "web-search");
        assert_eq!(head_run_verb("read"), "read", "no underscore, no change");
    }

    /// **Both spellings are one verb**, which is R34's second clause: *an operator who types
    /// what the daemon calls it should not be told they are wrong.* Case too, because a slash
    /// verb is typed by a hand.
    #[test]
    fn the_underscore_spelling_is_accepted_and_resolves_to_the_same_tool() {
        for typed in [
            "web_fetch",
            "web-fetch",
            "Web-Fetch",
            "WEB_FETCH",
            " web_fetch ",
        ] {
            assert_eq!(
                head_run_tool(typed, &HEAD_RUN_TOOLS),
                Some("web_fetch"),
                "`{typed}`"
            );
        }
        // And a name that is not a door tool resolves to nothing, however it is spelled —
        // the transform must not turn an unknown word into a known one.
        for typed in ["bash", "write", "web-push", "re-ad", "read_"] {
            assert_eq!(head_run_tool(typed, &HEAD_RUN_TOOLS), None, "`{typed}`");
        }
    }

    /// **`read_` and `read` are not the same word.** The transform maps `_` to `-` on both
    /// sides of the comparison, so a trailing underscore becomes a trailing hyphen and no
    /// longer matches — which is the right answer: it is not how anybody spells it, and
    /// accepting it would mean accepting anything one edit away from anything.
    #[test]
    fn the_transform_is_exact_and_not_fuzzy() {
        assert_eq!(head_run_tool("read-", &HEAD_RUN_TOOLS), None);
        assert_eq!(head_run_tool("re", &HEAD_RUN_TOOLS), None);
        assert_eq!(head_run_tool("", &HEAD_RUN_TOOLS), None);
    }
}
