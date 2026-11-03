//! The head frame format: ATTACH, RESYNC, snapshot, event envelope, `expected_seq` and
//! `client_request_id`.
//!
//! The frames are plain serde types and the framing is one newline-delimited JSON codec over any
//! `Read`/`Write`, so a remote head is a different transport and not a different protocol. An
//! `Ack` carries the batch's `seq` and never a "last kept" one, and a command with a stale
//! `expected_seq` is rejected with both numbers so the head can say what it was looking at.

use serde::{Deserialize, Serialize};

use crate::event::Envelope;
use crate::registry::{SessionBrief, SessionWiring};
use crate::scrub::ScrubReport;
use crate::view::Snapshot;

/// Bumped when a frame's meaning changes in a way the other side cannot parse: a new variant is a
/// deserialisation fault in the middle of a turn. Both sides compare it at ATTACH and refuse a
/// mismatch by name.
pub const PROTOCOL_VERSION: u32 = 57;

/// The names an operator may run through the head-run door: read-only and bounded, which admits
/// `web_fetch` and not a tool that writes.
pub const HEAD_RUN_TOOLS: [&str; 3] = ["web_search", "web_fetch", "read"];

/// The `key` [`HEAD_RUN_TOOLS`] travels under. Named here so the daemon that publishes the
/// row and the head that reads it cannot spell it two ways.
pub const HEAD_RUN_TOOLS_KEY: &str = "head-run.tools";

/// The verbs the DAEMON answers, published as a settings row because the namespace has two
/// owners: the head offers its own, and asks the daemon for the rest.
pub const DAEMON_VERBS_KEY: &str = "daemon.verbs";

/// The `model` row's key, and the code the registry publishes when that row MOVES: the rows
/// answer a request, so a head that did not make the switch would otherwise keep the old name.
pub const MODEL_KEY: &str = "model";
pub const MODEL_SET: &str = "model_set";

/// Which providers this box holds a key for, so a model picker greens only rows that can be
/// taken. Only the daemon can answer: the key may be in a file the head never reads.
pub const MODEL_KEYS_KEY: &str = "models.keys";

/// The picker rows that need no credential at all: `local` and every local model declared in
/// `providers.toml`. A second row rather than widening the first.
pub const MODEL_KEYLESS_KEY: &str = "models.keyless";

/// One door-tool as the daemon describes it to a head: which field a bare line goes into, and
/// what that field is, so a head can turn `/web_search` into a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRunTool {
    /// The name the door takes. This spelling and no other: a head offers `/web_search`
    /// because the daemon said `web_search`.
    pub name: String,
    /// Where a bare line goes (`query`, `url`, `path`). Empty when the tool has no single
    /// obvious field, which is the case where the head must ask for JSON.
    pub field: String,
    /// What that field is: `path`, `url` or `text`. Drives the head's Tab, so a `path`
    /// completes filenames because the daemon said so and not because the head knows what
    /// `read` does. Empty when [`Self::field`] is.
    pub kind: String,
    /// The fields that have defaults, as the model would receive them: a head filling a bare
    /// line must send the same object a model's call would, or the same tool answers two
    /// different questions depending on who asked.
    #[serde(default)]
    pub defaults: std::collections::BTreeMap<String, String>,
    /// Why there is no bare form, when there is none: the head says it in the sentence that
    /// refuses rather than leaving the operator to guess. Empty when [`Self::field`] is set.
    #[serde(default)]
    pub why_json: String,
}

/// The three kinds [`HeadRunTool::kind`] can take, and the vocabulary is closed on purpose: a
/// head branches on it, so a fourth value means a head rebuilt.
pub const HEAD_RUN_KIND_PATH: &str = "path";
pub const HEAD_RUN_KIND_URL: &str = "url";
pub const HEAD_RUN_KIND_TEXT: &str = "text";

/// The verb a person types for a door tool: hyphens and no shift. The wire keeps the tool's own
/// underscored name, and the mapping between them is textual.
pub fn head_run_verb(tool: &str) -> String {
    tool.replace('_', "-")
}

/// The tool a typed verb names, or `None`. Accepts both spellings and folds case: an
/// operator who types what the daemon calls it should not be told they are wrong, and a
/// slash verb is typed by a hand.
pub fn head_run_tool<'a>(typed: &str, list: &'a [&'a str]) -> Option<&'a str> {
    let want = typed.trim().replace('_', "-").to_ascii_lowercase();
    list.iter()
        .copied()
        .find(|t| head_run_verb(t).to_ascii_lowercase() == want)
}

/// The command a `!` line carries, or `None` when it carries nothing: a submitted line starting
/// with `!` is the operator's own shell command.
pub fn operator_shell_command(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('!')?;
    let cmd = rest.trim();
    (!cmd.is_empty()).then_some(cmd)
}

/// The sentence to show when a `!` line is more than one line, or `None` when it may be parsed:
/// the composer is one line, so a paste is refused with a reason rather than run.
pub fn operator_line_refusal(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    if !first.trim_start().starts_with('!') {
        return None;
    }
    let rest = lines.count();
    (rest > 0).then(|| {
        format!(
            "that is {} lines and its first one starts with `!`, so running it would run every \
             line of the pasted block as its own shell command. Nothing was run. A `!` line is \
             ONE line: send the command on its own.",
            rest + 1
        )
    })
}

#[cfg(test)]
mod operator_line_tests {
    use super::{operator_line_refusal, operator_shell_command};

    /// A pasted block is not a command. The parse says yes (correctly, which is why a check
    /// sits in front of it) and the refusal is what stops five lines of a copied notice from
    /// becoming five shell commands.
    #[test]
    fn a_pasted_block_that_starts_with_a_bang_is_refused_and_nothing_runs() {
        let notice = "! sudo apt install mc\nusage: sudo …\nit.\nso\n/proc";
        // The parse still says it is a `!` line - which is why the refusal has to exist.
        assert!(
            operator_shell_command(notice).is_some(),
            "the parse is right: this IS a `!` line"
        );
        let why = operator_line_refusal(notice).expect("a 5-line block must be refused");
        assert!(why.contains("5 lines"), "{why}");
        assert!(why.contains("Nothing was run"), "{why}");
        assert!(why.contains("ONE line"), "{why}");
    }

    /// One line is a command, and every ordinary paste is a prompt: neither is touched.
    #[test]
    fn one_line_and_prompt_pastes_pass_through() {
        assert_eq!(operator_line_refusal("! sudo apt install mc"), None);
        assert_eq!(operator_line_refusal("!ls"), None);
        assert_eq!(operator_line_refusal("!term mc"), None);
        // A pasted stack trace, a pasted diff, a pasted paragraph: all prompts.
        assert_eq!(
            operator_line_refusal("thread 'main' panicked\nat src/main.rs:12\nnote: run with"),
            None
        );
        assert_eq!(operator_line_refusal("explain this:\n    fn f() {}"), None);
        // A `!` that is not the first character of the line is not a `!` line at all.
        assert_eq!(operator_line_refusal("look at `!send`\nand this"), None);
    }
}

/// The command a `!term` line carries, or `None` when it is not one. Beside
/// [`operator_shell_command`] so the composer and the daemon share one parse.
pub fn term_command(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("!term")?;
    // A word boundary, not a prefix: the character after the verb must be whitespace or the
    // end of the line, or this is not the verb at all.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim())
}

/// What a `!term` line asks for: attach to the session's pane, end one, or run a line in a new
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermLine<'a> {
    /// The verb with nothing after it, or a pane's number: the pane this session already has.
    Attach(Option<u32>),
    /// The verb with `close` after it, and optionally a number: end that pane.
    Close(Option<u32>),
    /// The verb with a command: run it in a new pane.
    Run(&'a str),
}

/// The three readings of a `!term` line - see [`TermLine`]. `None` for a line that is not the
/// verb at all, so a caller can tell *not this verb* from *this verb, and what it asks for*.
pub fn term_line(line: &str) -> Option<TermLine<'_>> {
    let command = term_command(line)?;
    Some(match command {
        // The bare verb is an attach and never a request for the operator's `$SHELL` -
        // `term_command`'s own docs say why. An operator who wants their shell types
        // `!term bash`.
        "" => TermLine::Attach(None),
        "close" => TermLine::Close(None),
        n if pane_number(n).is_some() => TermLine::Attach(pane_number(n)),
        command => match command
            .strip_prefix("close ")
            .and_then(|n| pane_number(n.trim()))
        {
            Some(n) => TermLine::Close(Some(n)),
            None => TermLine::Run(command),
        },
    })
}

/// A pane's number as a person types it: digits and nothing else, and not zero.
fn pane_number(word: &str) -> Option<u32> {
    if word.is_empty() || !word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    word.parse().ok().filter(|n| *n > 0)
}

/// The line a `!send` line carries, or `None` when it is not one. Beside [`term_command`] for the
/// same reason: one parse, so the composer and the daemon agree.
pub fn send_line(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("!send")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim())
}

/// How a daemon's protocol version compares with this build's, as the one sentence a head says,
/// and `None` when they are the same: a silent skew looks like a bug in the other half.
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

/// Which half of the oracle's exchange a head is asking for: a head draws a decision card and may
/// want the bytes that justified it, but not the whole brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    /// The bytes the oracle was shown (`adjudication.shown`).
    Brief,
    /// The bytes it answered with (`adjudication.oracle_reply`).
    Reply,
}

/// One setting, as the daemon resolved it for this session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingRow {
    /// `mode`, `oracle.budget`, `spill`, ... - the flag's own name where there is
    /// one, so an operator can find it in `harnessd --help`.
    pub key: String,
    /// The value, rendered. A path is a path, a duration is `2.5s`, a list is
    /// comma-joined; a secret is never here.
    pub value: String,
    /// Where it came from: `flag`, `default`, `project store`, `permission.json`,
    /// `providers.toml`, `store` (a resumed session's own row) - or empty when
    /// the daemon does not track it, which is said rather than guessed.
    pub source: String,
    /// Whether this can change in the running session, and by what: the slash
    /// verb (`/mode NAME`, `/supervise on|off`), or empty for a setting that
    /// takes a restart.
    pub editable: String,
    /// The values this setting can take, from whoever owns them. Empty for a setting with no
    /// closed set. A list of what a thing may be belongs with the thing: a copy at the other
    /// end drifts, and the pane then cannot reach a value the daemon already stands at.
    #[serde(default)]
    pub choices: Vec<String>,
    /// The door's tools, described: populated on the one row whose key is [`HEAD_RUN_TOOLS_KEY`] and
    /// empty everywhere else. A typed field rather than a JSON blob.
    #[serde(default)]
    pub tools: Vec<HeadRunTool>,
}

/// A `Caps.features` string: this head can render a question with model-provided options, let a
/// person attach a note to a choice, and let them type their own.
pub const FEATURE_QUESTION_ANSWERS: &str = "question_answers_v1";

/// What a head can do and what it wants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// How many events the daemon will buffer for this head before demoting it to
    /// resync. The head asks, the daemon clamps. A head that renders slowly can ask
    /// for a bigger queue instead of resyncing constantly.
    pub queue: usize,
    /// Whether this head is willing to answer decisions. A read-only head must say so, or a
    /// question routed to it waits for its deadline and then times out - which is a real
    /// answer given for a fake reason.
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

/// One background job as the DAEMON sees it: the daemon decides which jobs are listed, what the
/// command reads as, and what the state word is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEntry {
    /// `j12`, the handle `job_output` and `/job ID` take.
    pub id: String,
    /// One line, already shortened by the daemon. Never empty: the daemon has the
    /// process table, so there is no "not in this head's window" case here.
    pub command: String,
    /// The name the caller gave this job, or empty when nobody said one. `j57` is a counter, so the
    /// name is what makes a row ring a bell.
    #[serde(default)]
    pub slug: String,
    /// `asked`, `promoted`, `promoted by NAME`.
    pub how: String,
    /// The process's own word - `exited 0`, `killed by job_kill`, `running`.
    /// Deliberately not "ok"/"error": a non-zero exit is the command's answer.
    pub state: String,
    pub running: bool,
    /// Whether anything was ever executed for this job: a job that never ran has no duration,
    /// and a row that claimed one denied it in the same line.
    #[serde(default)]
    pub never_ran: bool,
    /// Where this job's output goes when it does not go to its window: the daemon decides, and a head
    /// shows the path rather than pretending the window is empty.
    #[serde(default)]
    pub redirect: Option<String>,
    pub produced: u64,
    pub elapsed_ms: u64,
}

/// One monitor as the daemon's table has it: a watch the model armed, live or ended, so a head
/// can show what is watching.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorEntry {
    /// The model's name for it, which `monitor`'s `renew` and `retire` take.
    pub name: String,
    /// What it watches, in the daemon's words (`Watch::describe`).
    pub watching: String,
    /// Which scope reaps it.
    pub owner: String,
    /// Who armed it: `turn 12`.
    pub declared_by: String,
    /// Continuous rather than one-shot.
    pub repeat: bool,
    /// Still watching; an ended one carries `ended`.
    pub live: bool,
    /// When its time runs out, unix milliseconds; `0` for an ended one.
    pub ends_at_ms: u64,
    /// How many times it has fired.
    pub fired: u32,
    /// How it ended (`Fired::word`), empty while live.
    pub ended: String,
}

/// One standing note as the harness builds the section: the path, the abstract and the form. The
/// digest goes in the prompt, the offer's score decides what is offered, and a note injected
/// verbatim spends the budget the rest are indexed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteEntry {
    /// The file, as the index names it: the absolute path the section's heading
    /// carries, so a `read` of it and this row are the same file.
    pub path: String,
    /// The index's line for this note. `None` for a file with no prose at all - one
    /// that is nothing but headings - which is the one case the derivation refuses.
    #[serde(rename = "abstract")]
    pub abstract_line: Option<String>,
    /// `true` when that line is the author's own marker, `false` when the harness derived it from the
    /// note's first sentence.
    pub abstract_written: bool,
    /// Verbatim or indexed, right now - see the struct's own doc.
    pub form: NoteForm,
}

/// One skill, as the `/skills` pane draws it: the name, the one line and where it was found. No
/// body: the pane lists what can be handed over, and a body travels when somebody presses
/// Enter ([`ClientFrame::LoadSkill`]), because a corpus of every body is not a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRow {
    /// The name `skill`'s index and `LoadSkill` take.
    pub name: String,
    /// Where it was found, in the words the index uses: `this project`, `the operator's`,
    /// `installed from ...`, `this build`, `the fabric`.
    pub source: String,
    pub description: String,
}

/// Which of the two forms the section carries a note in: verbatim when it fits what is left of
/// the budget, or as an index row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteForm {
    /// The file fits what was left of the budget and is injected whole.
    Verbatim,
    /// It did not fit: the section carries its index instead of its text.
    Indexed,
}

/// What a head wants back from a peek: the scrub of a replay, or the rows of a session.
/// [`PeekShape::Events`] is the default, so a head that does not say gets what it always did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeekShape {
    /// The retained scrollback, scrubbed exactly as a replay is. The default, and what a head that
    /// does not ask for anything else keeps getting.
    #[default]
    Events,
    /// The session's own rows, as a [`Snapshot`] - the same thing an attach answers with, and
    /// needing no scrub because a view is already the durable half of a session's frame stream.
    Rows,
}

/// One file the operator attached to a prompt, as a chip in the composer. The head sends a PATH
/// and not the bytes: both processes are on one box, and the daemon reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Which of the two a chip said it was.
    pub kind: AttachmentKind,
    /// The absolute path, because the daemon's working directory is its own and a
    /// relative path would name a different file. The head resolves it before sending for
    /// the same reason: what it stat'ed to build `bytes_len` is what the daemon will read.
    pub path: String,
    /// `image/png`, or the head's guess for anything else (`text/plain`, `text/csv`), empty
    /// when the head had nothing to guess from. Advisory: the daemon trusts the bytes.
    #[serde(default)]
    pub media_type: String,
    /// The file's size when the head made the chip. A fact about the file, not about what is
    /// sent - the daemon re-reads it, and a file that grew is the daemon's to describe.
    #[serde(default)]
    pub bytes_len: u64,
    /// The file's own name, for a refusal that can be read: `report.csv`.
    #[serde(default)]
    pub name: String,
}

/// Image or file: an image is handed to the model and a file is either inlined or named by its
/// path, and which one a chip is decides what the operator's screen said it was. A daemon still
/// sniffs before it believes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    File,
}

/// Head → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum ClientFrame {
    /// `since_seq = 0` means "I have seen nothing" and gets a snapshot. Any other value is a
    /// resume: the gap is delivered, or `RESYNC` is answered, and resync is a normal outcome,
    /// never an error.
    Attach {
        protocol_version: u32,
        session_id: String,
        since_seq: u64,
        /// `tui`, `remote`, `flowy`, `acp`.
        kind: String,
        identity: String,
        #[serde(default)]
        caps: Caps,
    },
    /// The read mark. Sent AFTER the head has written the batch out, never on receipt: a crash
    /// then costs a duplicate, never a silence.
    Ack(Ack),
    /// The head gave up on its own state and wants a fresh snapshot. Also what a
    /// head sends after it receives [`ServerFrame::Resync`].
    Resync,
    /// A user message. If a turn is running this is queued as a follow-up user item rather than
    /// rejected, and the queuing is announced. `attachments` is defaulted, because a head built
    /// before the field sends a prompt with nothing attached and must keep parsing.
    Prompt {
        client_request_id: String,
        expected_seq: u64,
        text: String,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    /// Take back what this head queued: every prompt from it the running turn has not consumed, plus
    /// the operator's held run.
    WithdrawPrompts {
        client_request_id: String,
        expected_seq: u64,
    },
    /// The operator's todos, from a head that owns them: one list with two authors, and this is
    /// the half the head is the source of truth for.
    SetOperatorTodos {
        client_request_id: String,
        expected_seq: u64,
        /// Absent means the empty half, which is the only reading that works: a head whose operator has
        /// no rows has an empty list, and its encoder omits it.
        #[serde(default)]
        items: Vec<crate::event::TodoEntry>,
        /// The state moves the operator asked for on rows that are NOT theirs: a model row that waits on
        /// a person, set aside or lifted again.
        #[serde(default)]
        moved: Vec<crate::event::TodoState>,
    },
    /// Stop the daemon, not just this head: the same bell `SIGTERM` rings, so there is one shutdown
    /// sequence and a shared session's other heads learn of it.
    Stop {
        client_request_id: String,
        expected_seq: u64,
        /// Who asked, for the announcement. The head's identity, not a name it
        /// invents.
        who: String,
    },
    /// Close THIS session here and let go of it: what a migration sends before carrying the rows to
    /// another store, and what a bridge's stop sends so a placement settles now instead of at the
    /// daemon's end. Not [`ClientFrame::Stop`], which ends the daemon.
    CloseSession {
        client_request_id: String,
        expected_seq: u64,
        /// Who asked, for the announcement on the session's log - the head's identity.
        who: String,
        /// Why, in the announcer's own words: the two callers mean different things by it, and a
        /// head reading the log is owed the one that is true. Defaulted, so a 56 head and a daemon
        /// built before it still parse each other's frames.
        #[serde(default)]
        reason: String,
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
    /// Compact this session: one summary turn over the history as it stands, then the history is
    /// replaced by that summary through a transcript fork. The frame only asks; the summary turn
    /// streams like any turn and the forked transcript's first item says what replaced what.
    CompactSession {
        client_request_id: String,
        expected_seq: u64,
    },
    /// Rebuild this conversation's prompt from the tools the daemon seats now, forking it onto the
    /// new prefix. A turn, queued like a compaction.
    ReseatSession {
        client_request_id: String,
        expected_seq: u64,
        /// Summarise the conversation as well, rather than carrying it: a re-seat changes message zero and
        /// should not cost the conversation.
        #[serde(default)]
        summarise: bool,
    },
    /// Move this session's project to a named point (`allow-all`, `writes-allowed`, an opencode
    /// name). The daemon persists it in the mode store, so it applies to the project from here on
    /// without a restart.
    Mode {
        client_request_id: String,
        expected_seq: u64,
        name: String,
        /// The operator confirmed an unconfined `allow-all`. Read only for that point, and only when the
        /// session has no confinement, where it is the whole of the consent.
        #[serde(default)]
        consented: bool,
    },
    /// Ask a PERSON to confirm the point that needs one. [`ClientFrame::Mode`] sets a point; this only
    /// asks, because a head that is a program cannot consent.
    ModeConfirm {
        client_request_id: String,
        expected_seq: u64,
        name: String,
    },
    /// A person's answer to a [`ClientFrame::ModeConfirm`]: `yes` is a consent and nothing else.
    ModeConsent {
        client_request_id: String,
        expected_seq: u64,
        /// The `req_id` [`ServerFrame::ModeConfirm`] carried. An id that is no longer open is
        /// refused, the way an answer to a closed decision is: a consent to a question asked ten
        /// minutes ago is input arriving from nowhere.
        req_id: String,
        yes: bool,
    },
    /// A slash command the head does not handle, handed to the daemon as the line the operator typed
    /// without the leading `/`. One frame for every such verb.
    Slash {
        client_request_id: String,
        expected_seq: u64,
        line: String,
    },
    /// An operator's mark: `/fb`, `/fb+`, `/fb-`, alt-g / alt-b. Answered on the connection thread
    /// and never queued, because the worker can be pinned for tens of seconds and a mark that
    /// waits behind a review pass lands on the wrong turn.
    Feedback {
        /// The head's clock at the key press, in ms - not the daemon's receipt time.
        at_ms: u64,
        /// `good`, `bad` or `note`.
        polarity: String,
        /// The operator's own words, `#tags` inside. Empty for a bare key.
        #[serde(default)]
        note: String,
        /// The row the operator was reading back, when they were; `None` is "now".
        #[serde(default)]
        item_id: Option<String>,
    },
    /// `sudo` wants a password: sent by the askpass helper the session's shell runs as
    /// `SUDO_ASKPASS`, attached as a head of its own kind.
    Askpass { prompt: String, command: String },
    /// A head's answer to a `SecretRequested`: the password, or `None` for a refusal. Never logged,
    /// never persisted, never in a `CommandIssued`.
    Secret {
        req_id: String,
        secret: Option<String>,
    },
    /// A head's own screen as it drew it: the answer to `ScreenRequested`, the exact rows this head
    /// last rendered.
    Screen {
        req_id: String,
        cols: usize,
        rows_n: usize,
        /// One string per row, escape codes included. Defaulted because a screen with no rows is
        /// degenerate but not impossible, and a head whose encoder omits NIL would otherwise send
        /// no `rows` at all and lose its socket over a frame that still answers the question.
        #[serde(default)]
        rows: Vec<String>,
    },
    /// Answer an open permission: grant or deny, by option id. A question's answer is
    /// [`ClientFrame::AnswerQuestion`].
    Answer {
        client_request_id: String,
        req_id: String,
        option_id: String,
        /// The operator's own glob, when they answer *always allow* and want it to cover more than this
        /// one call. Without it the rule is derived from the call itself.
        #[serde(default)]
        pattern: Option<String>,
        /// What the operator wants the model told, for `deny_and_tell`. Ignored on the other
        /// options for `pattern`'s reason: a note attached to an `allow_once` would be a sentence
        /// nobody reads, and attaching it silently is worse than dropping it.
        #[serde(default)]
        note: Option<String>,
    },
    /// Answer an open question: a choice, a note on that choice, a typed reply, both, or an
    /// abstention. There is no vocabulary here for a permission: that is `ClientFrame::Answer`.
    AnswerQuestion {
        client_request_id: String,
        req_id: String,
        answer: crate::question::QuestionAnswer,
    },
    /// Answer the question a todo row is waiting on: the row's `operator` edge. `content` is the
    /// row's exact words, which is the board's one key.
    AnswerTodo {
        client_request_id: String,
        content: String,
        question: String,
        answer: crate::question::QuestionAnswer,
    },
    /// What sessions does this daemon hold? Answered with [`ServerFrame::Sessions`]. Read-only and
    /// unserialised: a list is a question about the daemon, not a change to it.
    ListSessions,
    /// What is this session's todo list? Answered with [`ServerFrame::Todos`], the bootstrap read,
    /// because a snapshot does not carry the board.
    ListTodos,
    /// The standing-notes pane's bootstrap read: what the harness reads into this session's prompt
    /// right now, and which of those notes is over the budget.
    ListNotes,
    /// The skills pane's bootstrap read: what can be handed to this session, one row each.
    /// Answered with [`ServerFrame::Skills`], read-only and unserialised like
    /// [`ClientFrame::ListNotes`].
    ListSkills,
    /// The operator hands this session one skill's body: `/skills NAME`, and the pane's Enter. The
    /// daemon loads it, writes the operator's message and answers.
    LoadSkill {
        client_request_id: String,
        expected_seq: u64,
        /// The skill's name, as [`ServerFrame::Skills`] listed it.
        name: String,
        #[serde(default)]
        then: String,
    },
    /// This session's background jobs, from the daemon's process table. Read-only and unserialised
    /// like [`ClientFrame::ListTodos`], and deliberately not pushed.
    ListJobs,
    /// The review queue and the merge queue, whole. Answered with [`ServerFrame::Queues`], and the
    /// bootstrap read, so a head never draws a queue it has not been told about.
    ListMergeQueue,
    /// Make a new session in this daemon. It does not switch to it: the head sends
    /// [`ClientFrame::Switch`] once it has seen the id, because a head that wants a session ready
    /// for later should not have to leave the one it is in.
    NewSession {
        client_request_id: String,
        /// A human name, or empty. A title is set ONCE; a daemon may name an unnamed session from
        /// the message that opened it, and after that only a deliberate rename changes it. What
        /// must not happen is a row that renames itself as the conversation goes on.
        title: String,
        /// The tree this session is about, from the head that asked. Empty means wherever the daemon is,
        /// and it is here because the daemon's working directory is not the head's.
        workspace: String,
    },
    /// Bring a session that is in the store but not in this daemon back to life. Separate from
    /// [`ClientFrame::NewSession`] because this one NAMES an existing session.
    ResumeSession {
        client_request_id: String,
        session_id: String,
    },
    /// Name a session, or clear its name with an empty title. It carries a `session_id` rather
    /// than acting on the current one: renaming is wanted from a picker, where the session you are
    /// looking at is usually not the one you are in.
    RenameSession {
        client_request_id: String,
        session_id: String,
        title: String,
    },
    /// Move this connection to another session: the head detaches from the session it is in and
    /// attaches to the named one on the same socket.
    Switch {
        session_id: String,
        /// As `Attach`: `0` takes a snapshot, anything else resumes. A head coming back to a session
        /// it was watching sends the seq it had.
        since_seq: u64,
    },
    /// Read another session's retained scrollback WITHOUT moving there: a subagent row's Enter shows
    /// what that child produced while the head stays where it is.
    Peek {
        session_id: String,
        /// What to read back. Absent is [`PeekShape::Events`], which is what every caller already
        /// gets, so this is additive in both directions: an older daemon ignores it and an older
        /// head never sends it.
        #[serde(default)]
        shape: PeekShape,
    },
    /// The daemon answers from its caches or its store: its two in-memory rings are tuned for the
    /// normal case, and a request that misses falls through rather than failing.
    FetchRow {
        session_id: String,
        /// The row's position in the session, oldest first. Not an index into the daemon's window
        /// and not an item id - see the doc above for why the head can only express this one.
        row: usize,
        /// Byte offset into the body. Clamped to its length rather than refused: a head
        /// paging towards the end does not know where the end is, and asking past it is
        /// the ordinary way to find out.
        at: usize,
        /// How many bytes to send. Capped by the daemon, like `read`'s own windows: one request
        /// must not be able to return a megabyte because a head asked for one.
        len: usize,
    },
    /// A page of the session's rows by ordinal. A head holds a window of the conversation and a
    /// reader can scroll above it, so what it needs is the rows behind its own copy rather than a
    /// window inside one row's body: the whole row, block and all.
    FetchRows {
        session_id: String,
        /// The first ordinal wanted, oldest first: the same numbering [`ClientFrame::FetchRow`]
        /// takes, so a head names a position and never a tier of the daemon's cache.
        from: usize,
        /// How many rows. Capped by the daemon for `len`'s reason: one request must not be able
        /// to pull a whole conversation.
        count: usize,
    },
    /// A call the OPERATOR ran themselves: the daemon checks `name` against [`HEAD_RUN_TOOLS`] and
    /// records the admission as the operator's, so the row says a person ran it.
    OperatorCall {
        client_request_id: String,
        expected_seq: u64,
        /// The head's handle for this call. Unique within the session, and the key the
        /// result comes back on.
        call_id: String,
        /// One of [`HEAD_RUN_TOOLS`]. The daemon refuses anything else by name.
        name: String,
        /// The call's arguments as JSON, the same shape a model's call carries, because the row it
        /// becomes is the same row. The bare form is built by the head.
        arguments: String,
        /// Whether the DAEMON runs it. Both shapes are legitimate: `false` means the head has the tool
        /// and runs it, then reports the result.
        #[serde(default)]
        execute: bool,
    },
    /// What the operator's call produced. Appends a `ToolResult` whose origin says a person ran it,
    /// so the model sees the operator's own work as the operator's.
    OperatorResult {
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        payload: String,
    },
    /// The operator's own shell line: a `!` command, run by the DAEMON, in this session's workspace.
    /// A frame of its own rather than a widened [`ClientFrame::RunTool`], because the daemon mints
    /// the call and the head has no say in its arguments.
    OperatorShell {
        client_request_id: String,
        expected_seq: u64,
        /// The line as submitted, `!` first. The command is everything after that bang, trimmed -
        /// one rule, applied at the execution site, so the head never sends a different spelling
        /// than the one the operator typed.
        line: String,
    },
    /// Ask the model to propose `!` completions for a prefix, when the head's own history completion
    /// has no match: the head has no HTTP client and no key.
    SuggestShell {
        client_request_id: String,
        expected_seq: u64,
        prefix: String,
    },
    /// Ask for the bytes that justified one decision: a locator, not a payload. The brief and the
    /// reply are on the corpus row and on no frame, so without this a head can only show a
    /// sentence about them. Answered with [`ServerFrame::Diagnostic`].
    FetchDiagnostic {
        request_id: String,
        kind: DiagnosticKind,
    },
    /// List the settings this session runs under. Answered with
    /// [`ServerFrame::Settings`]; never moves the connection.
    Settings,
    /// Ask for a window of one background job's output, for a head's jobs pane. A command, so it
    /// reaches the worker that owns the exec host.
    ReadJobOutput {
        client_request_id: String,
        job: String,
        offset: u64,
    },
    /// A clean goodbye. Not required: TCP close is detach too, and detach is never abort.
    Detach,
    /// `!term <command>`: open a pane and run a program that owns the screen in it. The line is the
    /// operator's own, verb included, and the parse is [`term_command`]'s.
    TermOpen {
        line: String,
        cols: usize,
        rows: usize,
    },
    /// The operator's keys, verbatim: the down direction of the pane's byte stream. Bytes and not
    /// keycodes, because the pane is a terminal.
    TermInput { pane: u32, bytes: Vec<u8> },
    /// The pane's rectangle moved. The head's fact, sent down because the daemon has no screen: it
    /// sets the pty's size, and the kernel raises `SIGWINCH` in the program.
    TermResize { pane: u32, cols: usize, rows: usize },
    /// Is something running in this session's pane, and what? A head that has detached or switched
    /// must still know the program is there, and say so.
    TermStatus,
    /// The operator's answer to a command that asked them something: the daemon raised a
    /// `PromptRequested` for a run of their own that is blocked reading.
    PromptAnswer {
        /// The request this answers, as `PromptRequested` named it.
        req_id: String,
        /// The line, verbatim. Empty is a bare Enter.
        line: String,
    },
    /// One line to the running command, on demand: the manual floor under the prompt card, whose
    /// detection is a heuristic and has misses.
    SendLine {
        /// The line to write, verbatim. Empty is a bare Enter.
        line: String,
    },
    /// End the pane: the daemon ends its scope, which kills the program and everything it started.
    /// Not the way out, which is a detach: this is the kill.
    TermClose { pane: u32 },
    /// Open a plugin's pane: the head opened it, so the daemon runs the pane's look now and answers
    /// with [`crate::SessionEvent::PluginPane`].
    PaneOpen {
        /// `<layer name>/<hook name>` - see [`crate::SessionEvent::PluginPane`].
        pane: String,
        /// The pane's drawable rectangle, chrome excluded. Defaulted: an older head sends neither, and
        /// `0` asks the host for its own default rather than drawing into nothing.
        #[serde(default)]
        w: u32,
        #[serde(default)]
        h: u32,
    },
    /// The head closed a plugin's pane: stop refreshing it and forget its state, because the daemon
    /// keeps a pane on its clock for as long as it is open.
    PaneClose {
        /// `<layer name>/<hook name>`.
        pane: String,
    },
    /// One key of one stop, back to the pane that drew it: the `event` word the stop's action named,
    /// or `"look"` to re-run the look now.
    PaneAction {
        /// `<layer name>/<hook name>`.
        pane: String,
        /// The stop's id, as the plugin wrote it; `""` when the stop had none (a markdown item).
        item: String,
        /// The action word.
        action: String,
    },
}

/// One live pane of a session, as [`ServerFrame::TermStatus`] lists it: its number and what is
/// running in it, verb stripped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBrief {
    pub pane: u32,
    pub command: String,
}

/// The read mark, as a type. `seq` is the last seq this head CONSUMED, whether or not it
/// rendered it, and it is produced by the cursor rather than by a reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub seq: u64,
    /// How many of those the head actually displayed.
    pub rendered: u64,
    /// How many it suppressed. *Busy, and none of it was for me* is a different fact from *quiet*,
    /// and this field is the difference. Not `Option`: a head that filters nothing reports zero.
    pub filtered: u64,
}

/// Daemon → head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // the wire's own enum; a boxed variant changes every head and daemon match site
pub enum ServerFrame {
    /// The answer to ATTACH: always first, and once per attachment. Once per attachment rather than
    /// once per connection, because [`ClientFrame::Switch`] answers with one too.
    Hello {
        protocol_version: u32,
        session_id: String,
        head_id: String,
        /// Events that fell off the back of the scrollback and this head will never see. Present
        /// and zero: the disclosure is the field, not its absence.
        dropped: u64,
        /// `Some` for a snapshot attach or a demoted resume; `None` for a resume served from the
        /// scrollback, whose gap follows as `Event` frames. Boxed because it is large.
        snapshot: Option<Box<Snapshot>>,
        /// For a resume: the seq the gap starts after.
        resumed_from: Option<u64>,
        /// What the replay scrub stripped on the way here. The daemon's half of
        /// "report what was filtered": zero here on a live-only attach, nonzero
        /// whenever a decision had already settled.
        scrubbed: ScrubReport,
        /// What this session is talking to. `TurnStarted { model }` was the only one of these that ever
        /// reached a head, and only when a turn started.
        wiring: SessionWiring,
        /// Every session this daemon holds, so a picker is populated by the attach
        /// itself and not by a second round trip. Includes the one just joined.
        sessions: Vec<SessionBrief>,
    },
    /// The password for the `Askpass` this connection sent, or `None`, and when it is `None` which of
    /// the three facts that was: nobody asked, the ask failed, or a person refused.
    Secret {
        secret: Option<String>,
        /// Why there was no password, in the daemon's own words, or `None` when one was given.
        #[serde(default)]
        why: Option<String>,
    },
    /// The answer to [`ClientFrame::ListSessions`] and to [`ClientFrame::NewSession`]. `NewSession`
    /// is answered with the whole list rather than with the new id alone.
    Sessions {
        sessions: Vec<SessionBrief>,
        /// The session this connection is in right now.
        current: String,
        /// The id `NewSession` created, when that is what this is answering.
        /// `None` for a plain list - present and null, not omitted.
        created: Option<String>,
    },
    /// The answer to [`ClientFrame::ListTodos`], for the session the connection
    /// is in. The whole list as of now - later changes arrive as
    /// [`crate::SessionEvent::TodosUpdated`].
    Todos {
        session_id: String,
        todos: Vec<crate::event::TodoEntry>,
    },
    /// The answer to [`ClientFrame::Settings`], and pushed to every head attached to the session
    /// whenever one of the rows changes: a head draws these, so an unasked frame means what an
    /// answer means, and no protocol version moves for it.
    Settings { rows: Vec<SettingRow> },
    /// The answer to [`ClientFrame::ListJobs`]: the whole list as of now. Later
    /// changes arrive as [`crate::SessionEvent::JobSettled`], the way todos work.
    Jobs {
        session_id: String,
        jobs: Vec<JobEntry>,
        /// The session's monitors, live and ended - the monitors pane's rows and the composer
        /// edge's count, on the frame that already answers *what is this session running*.
        /// Defaulted, so an older daemon saying nothing reads as none.
        #[serde(default)]
        monitors: Vec<MonitorEntry>,
    },
    /// The answer to [`ClientFrame::ListNotes`]: the standing notes, one row each, in the order the
    /// section carries them, because that order is what the prompt shows.
    StandingNotes {
        session_id: String,
        notes: Vec<NoteEntry>,
    },
    /// The answer to [`ClientFrame::ListSkills`]: what this session can be handed, in the order the
    /// index lists it, and never a body - a listing of bodies is not a listing. An empty list is an
    /// answer.
    Skills {
        session_id: String,
        skills: Vec<SkillRow>,
    },
    /// The answer to [`ClientFrame::ListMergeQueue`]: both queues as of now, every state, with each
    /// repository's lease and heartbeat.
    Queues {
        snapshot: crate::event::QueueSnapshot,
    },
    Peeked {
        session_id: String,
        dropped: u64,
        events: Vec<Envelope>,
        /// The session's rows, when the head asked for [`PeekShape::Rows`]. `Some` exactly when the
        /// request said `Rows`, so a head can tell which answer it asked for.
        #[serde(default)]
        snapshot: Option<Box<Snapshot>>,
    },
    /// A confirmation a head asked for, put in front of the heads that may answer it: the other
    /// half of [`ClientFrame::ModeConfirm`], and the only frame that asks a person.
    ModeConfirm {
        session_id: String,
        /// The daemon's own id for this request, which is also the name a [`Reply`] and the
        /// settle frame are correlated by.
        req_id: String,
        /// The point being confirmed, spelled as the requester spelled it (`allow-all`).
        name: String,
        /// Who asked, so the card can say so and the person can put it down to a machine rather
        /// than to their own keystroke.
        asked_by: String,
    },
    /// How a confirmation ended. To every head of the session, because the head that asked reads it
    /// as the answer and the rest must stop offering the card.
    ModeSettled {
        session_id: String,
        req_id: String,
        /// The asking head's own request id, so the head that asked can correlate the answer
        /// without having been told the `req_id` first, and everyone else reads `""`.
        #[serde(default)]
        client_request_id: String,
        name: String,
        applied: bool,
        /// `yes`, `no`, or `nobody`. Not the identity: a caller that needs *which* person said yes
        /// reads the session log.
        answer: String,
        said: String,
    },
    /// The answer to [`ClientFrame::FetchRows`], in ordinal order starting at `from`. Fewer rows
    /// than were asked for means the walk stopped - the session begins above `from`, or nothing
    /// could answer the next row - and a head must not read that as a shorter conversation.
    RowsFetched {
        session_id: String,
        /// The ordinal `rows[0]` occupies, echoed so a head places the page without counting.
        from: usize,
        rows: Vec<crate::view::SnapshotItem>,
    },
    /// The answer to [`ClientFrame::FetchRow`]: a window of one row's body. `total` is the whole
    /// body's length, so a head knows what it has not been sent.
    RowFetched {
        session_id: String,
        /// The session ordinal that was asked for, echoed so the answer names its row.
        row: usize,
        /// Byte offset this window actually starts at.
        at: usize,
        /// The window itself, starting on a character boundary - a head cannot render half a glyph
        /// and the daemon is the side that knows the encoding.
        body: Option<String>,
        /// The whole body's length in bytes, so a head can say what is on either side.
        total: usize,
    },
    /// The bytes a head asked for, or that there are none: the answer to
    /// [`ClientFrame::FetchDiagnostic`]. `body: None` is *not recorded*, not *empty*.
    Diagnostic {
        request_id: String,
        kind: DiagnosticKind,
        body: Option<String>,
        /// Bytes the field holds, or 0 when there is none. Present rather than inferred, because a
        /// length computed from `body` cannot tell *empty* from *absent* either.
        total: usize,
    },
    /// The model's proposed `!` completions: the answer to [`ClientFrame::SuggestShell`]. `lines`
    /// are candidate shell lines, `!` first, and an empty list is a completed answer.
    ShellSuggestions {
        client_request_id: String,
        prefix: String,
        lines: Vec<String>,
    },
    /// One appended event, in seq order, with no gaps between consecutive frames.
    Event(Envelope),
    /// The head's queue overflowed, or its resume gap was too large. Not an error: the head
    /// resets to the enclosed snapshot and carries on from `snapshot.seq + 1`.
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
    /// What this session's panes are running, or nothing: the answer to
    /// [`ClientFrame::TermStatus`]. An empty list is not a refusal, it is no pane.
    TermStatus { panes: Vec<PaneBrief> },
    /// The pane you asked to attach to, and what is running in it: the answer to a
    /// [`ClientFrame::TermOpen`] whose line is the bare verb.
    TermAttached {
        pane: u32,
        command: String,
        opened: bool,
    },
    /// A pane's program wrote these bytes: the up direction of the pane's byte stream. Raw, because
    /// the head's half of this pair is a terminal emulator.
    TermOutput { pane: u32, bytes: Vec<u8> },
    /// The pane is over, and this is why. Written once per pane, and it answers a
    /// [`ClientFrame::TermOpen`] as much as it ends one.
    TermEnded { pane: Option<u32>, reason: String },
}

/// Why a `Rejected` was sent, as a stable code a head can branch on.
pub const REJECT_STALE_SEQ: &str = "stale expected_seq";
/// The `note` on a prompt accepted with nothing unusual about it. A head has a reason to
/// recognise it: it tells the operator their line arrived.
pub const NOTE_PROMPT_QUEUED: &str = "queued as a user item";
/// The `note` on a `/compact` accepted with nothing unusual about it. Distinct from
/// [`NOTE_PROMPT_QUEUED`], because a compaction has its own progress to report.
pub const NOTE_COMPACT_QUEUED: &str = "queued after the running turn";
/// The `note` on a `Stop` this daemon is about to honour: the only `Accepted` a head treats as
/// more than bookkeeping.
pub const NOTE_STOPPING: &str = "stopping";
/// The `note` on a `CloseSession` this daemon accepted: one session is going and the daemon is
/// staying, which is its own word rather than a `Stop`.
pub const NOTE_SESSION_CLOSED: &str = "session closed";
pub const REJECT_UNKNOWN_DECISION: &str = "no such open decision";
pub const REJECT_READ_ONLY: &str = "this head declared can_decide: false";
/// A `Switch` or an `Attach` named a session this daemon does not hold. Refused by name rather
/// than answered with the default session: a typo that seats you in somebody else's conversation
/// looks exactly like a working attach to an empty one, and you find out by prompting into it.
pub const REJECT_UNKNOWN_SESSION: &str = "no such session";
/// A `ResumeSession` named a session that is in neither the daemon nor the store. Distinct from
/// [`REJECT_UNKNOWN_SESSION`], which is about a session that never existed.
pub const REJECT_NOT_IN_STORE: &str = "no such session in the store";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{SessionView, ViewBounds};

    /// A signpost at the version, where the compiler puts none: `PROTOCOL_VERSION` is the only
    /// compatibility check the two halves have, so a change here must move it.
    #[test]
    fn every_frame_is_accounted_for_at_this_version() {
        fn client(f: &ClientFrame) {
            match f {
                ClientFrame::Ack { .. }
                | ClientFrame::Answer { .. }
                | ClientFrame::AnswerQuestion { .. }
                | ClientFrame::AnswerTodo { .. }
                | ClientFrame::Askpass { .. }
                | ClientFrame::Attach { .. }
                | ClientFrame::CloseSession { .. }
                | ClientFrame::CompactSession { .. }
                | ClientFrame::Detach
                | ClientFrame::Feedback { .. }
                | ClientFrame::FetchDiagnostic { .. }
                | ClientFrame::FetchRow { .. }
                | ClientFrame::FetchRows { .. }
                | ClientFrame::Interrupt { .. }
                | ClientFrame::ListJobs
                | ClientFrame::ListMergeQueue
                | ClientFrame::ListNotes
                | ClientFrame::ListSkills
                | ClientFrame::LoadSkill { .. }
                | ClientFrame::ListSessions
                | ClientFrame::ListTodos
                | ClientFrame::Mode { .. }
                | ClientFrame::ModeConfirm { .. }
                | ClientFrame::ModeConsent { .. }
                | ClientFrame::NewSession { .. }
                | ClientFrame::OperatorCall { .. }
                | ClientFrame::OperatorResult { .. }
                | ClientFrame::OperatorShell { .. }
                | ClientFrame::PaneAction { .. }
                | ClientFrame::PaneClose { .. }
                | ClientFrame::PaneOpen { .. }
                | ClientFrame::Peek { .. }
                | ClientFrame::Promote { .. }
                | ClientFrame::Prompt { .. }
                | ClientFrame::PromptAnswer { .. }
                | ClientFrame::ReadJobOutput { .. }
                | ClientFrame::RenameSession { .. }
                | ClientFrame::ReseatSession { .. }
                | ClientFrame::ResumeSession { .. }
                | ClientFrame::Resync
                | ClientFrame::Screen { .. }
                | ClientFrame::Secret { .. }
                | ClientFrame::SendLine { .. }
                | ClientFrame::Settings
                | ClientFrame::Slash { .. }
                | ClientFrame::SuggestShell { .. }
                | ClientFrame::Stop { .. }
                | ClientFrame::Switch { .. }
                | ClientFrame::SetOperatorTodos { .. }
                | ClientFrame::TermClose { .. }
                | ClientFrame::TermInput { .. }
                | ClientFrame::TermOpen { .. }
                | ClientFrame::TermResize { .. }
                | ClientFrame::TermStatus
                | ClientFrame::WithdrawPrompts { .. } => {}
            }
        }
        fn event(e: &crate::SessionEvent) {
            match e {
                crate::SessionEvent::CommandIssued { .. }
                | crate::SessionEvent::ActivityChanged { .. }
                | crate::SessionEvent::DecisionAnswered { .. }
                | crate::SessionEvent::DecisionRequested { .. }
                | crate::SessionEvent::Delta { .. }
                | crate::SessionEvent::DenialRaised { .. }
                | crate::SessionEvent::Explain { .. }
                | crate::SessionEvent::HeadAttached { .. }
                | crate::SessionEvent::HeadDetached { .. }
                | crate::SessionEvent::JobOutput { .. }
                | crate::SessionEvent::JobSettled { .. }
                | crate::SessionEvent::QueueMoved { .. }
                | crate::SessionEvent::OperatorCallAllowed { .. }
                | crate::SessionEvent::Filling { .. }
                | crate::SessionEvent::CompactionProgress { .. }
                | crate::SessionEvent::HeldResults { .. }
                | crate::SessionEvent::PromptProgress { .. }
                | crate::SessionEvent::PromptRequested { .. }
                | crate::SessionEvent::PromptSettled { .. }
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
                | crate::SessionEvent::Citations { .. }
                | crate::SessionEvent::PluginPane { .. }
                | crate::SessionEvent::PluginPaneGone { .. }
                | crate::SessionEvent::TranscriptForked { .. }
                | crate::SessionEvent::TurnFailed { .. }
                | crate::SessionEvent::TurnFinished { .. }
                | crate::SessionEvent::TurnInterrupted { .. }
                | crate::SessionEvent::TurnStarted { .. }
                | crate::SessionEvent::Warning { .. } => {}
            }
        }
        // And the server frames: a new `ServerFrame` reaches a head that cannot parse it exactly
        // as a new `ClientFrame` reaches a daemon that cannot, and a signpost for the two client
        // directions leaves this one unasked.
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
                | ServerFrame::StandingNotes { .. }
                | ServerFrame::Skills { .. }
                | ServerFrame::Queues { .. }
                | ServerFrame::Peeked { .. }
                | ServerFrame::ModeConfirm { .. }
                | ServerFrame::ModeSettled { .. }
                | ServerFrame::RowFetched { .. }
                | ServerFrame::RowsFetched { .. }
                | ServerFrame::Diagnostic { .. }
                | ServerFrame::ShellSuggestions { .. }
                | ServerFrame::TermAttached { .. }
                | ServerFrame::TermOutput { .. }
                | ServerFrame::TermEnded { .. }
                | ServerFrame::TermStatus { .. }
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
            PROTOCOL_VERSION, 57,
            "the match above was last reconciled with the frame list at 57. 57 gave \
             `ClientFrame::CloseSession` a `reason`, because a close has two callers that mean \
             different things by it: a migration carries the rows to another store, and a \
             bridge's stop ends the session's placement so its branch comes home. The field is \
             defaulted, so a version-56 head and a 57 daemon still parse each other's frames; \
             what the bump records is that the SENTENCE a head reads is now the caller's. 56 added \
             `ClientFrame::FetchRows` and `ServerFrame::RowsFetched`: the head holds a window of \
             the conversation and a reader scrolls above it, so it asks for whole rows by ordinal \
             and the daemon answers them from its view or its store. Two new variants, so a \
             version-55 peer fails to parse whichever arrives first. 55 added the \
             three frames of the skills pane: `ClientFrame::ListSkills` (the rows), \
             `ClientFrame::LoadSkill` (the operator hands one body to the session, and the \
             DAEMON writes the message, because a row is the daemon's to write and a fabric \
             skill's body is a read only the daemon may make) and `ServerFrame::Skills` (the \
             answer). New variants in both directions, so a version-54 peer fails to parse \
             whichever arrives first. 54 added the \
             four frames of one feature: `ClientFrame::ModeConfirm` (a head ASKS for the point \
             that needs a person), `ServerFrame::ModeConfirm` (the card, to every other head \
             that declared `can_decide`), `ClientFrame::ModeConsent` (that head's answer) and \
             `ServerFrame::ModeSettled` (how it ended, to everybody). New variants in both \
             directions, so a version-53 peer fails to parse whichever arrives first — the \
             version-4 argument. The reason `Mode` could not grow a field instead is the \
             feature: `Mode` with `consented: true` is a HEAD confirming, and the whole point is \
             that the head asking (the Claude Code bridge, B8) must never be the one that \
             answers. 53 added \
             `ClientFrame::CloseSession`, which closes ONE session and lets go of its harness \
             (a version-52 daemon fails to parse it, the version-4 argument; a head either \
             sends it or does not, so an EVENT-side skew does not arise). It exists because a \
             migration (`letibot --migrate`, `docs/remote-hosts.md` RH4) has to take a still \
             copy of a conversation, and neither verb that existed made a session still: \
             `Stop` ends the whole daemon, and a detach changes nothing about what the session \
             does — a settled job is still delivered, a monitor still fires. 52 added \
             `SessionEvent::ActivityChanged`, what a session is waiting on (a version-51 head cannot \
             decode it mid-session; the snapshot's `activity` field is defaulted, but the EVENT is \
             what makes the bump). 51 added \
             `ClientFrame::PaneClose` (a new frame, so a version-50 daemon fails to parse it) and \
             `PaneOpen`'s `w`/`h` (defaulted, and a version-50 daemon would ignore a size a head \
             promised its pane; the frame is what makes the bump, not the fields). 50 was \
             `ClientFrame::Prompt.attachments`, an ADDED, DEFAULTED field that is still a bump: a \
             version-49 daemon parses the frame and DROPS the attachments in silence, so the bump \
             is what a head reads to refuse to promise a picture it cannot send. 49 was \
             `SessionEvent::PluginPane`/`PluginPaneGone` and `ClientFrame::PaneOpen`/`PaneAction`, \
             plugin panes (a version-48 head cannot decode the first event, a version-48 daemon \
             cannot parse the first frame). 48 was \
             `SessionEvent::Citations`, the lines a reply cited (a version-47 head cannot decode \
             it). 47 was `ClientFrame::AnswerTodo`, the operator's answer to a todo row's question (a \
             version-46 daemon cannot parse it). 46 was `SessionEvent::HeldResults`, the counts of what a session holds back from its model \
             (a version-45 head cannot decode it). 45 was `ClientFrame::Feedback`, \
             an operator's mark answered on the connection thread (a version-44 daemon cannot parse \
             it). 44 was several panes per \
             session (R60 item 2): `TermInput`, `TermResize`, `TermClose`, `TermOutput` and \
             `TermEnded` carry the pane's number, `TermAttached` also answers an open, and \
             `TermStatus` lists every live pane (a version-43 head would fail to decode the first \
             `TermOutput`, a version-43 daemon the first `TermInput`). 43 was R61's two queues: \
             `ServerFrame::MergeQueue` became `ServerFrame::Queues` (a snapshot of both queues, the \
             leases, the targets' health and the last moves, with its states as WORDS so a state a \
             newer daemon adds never takes a frame down again), and the three `MergeEntry*` events \
             became `SessionEvent::QueueMoved`. 42 was `TodoBy::Queue`, a \
             NEW VARIANT on an existing enum: **no frame is added and the number still has to move**, \
             because a version-41 head cannot DECODE `\"by\":\"queue\"` and the failure takes the \
             whole `TodosUpdated`/`Todos` frame down with it (the version-36 argument, at the level \
             of a variant again — and the version-37 argument, one author over). The operator's \
             design is what it is for: *\"reviews are subagents managed by the main gatekeeper \
             session which is hardcoded … it is a normal auto-driving session that treats review \
             queue as todo list - so nags and all\"*. So the merge queue fills a SESSION's board — \
             one row per entry waiting for a verdict — and a row's author is the queue, which is \
             neither the model (which may not strike a queue row off), nor the operator (whose half \
             the head replaces wholesale), nor a `Parent <session-id>` (the daemon is not that \
             session, and an author string says what is true). The rows are a VIEW and are replaced \
             from the store every pass: `MergeEntry::state` and `ReviewRecord::decision` stay the \
             only place a state lives. 41 was \
             `TodoStatus::Cancelled`, a \
             NEW VARIANT on an existing enum: **no frame is added and the number still has to move**, \
             because a version-40 head cannot DECODE `\"cancelled\"` and the failure takes the whole \
             `TodosUpdated`/`Todos` frame down with it (the version-36 argument, one word over). The \
             operator's own rulings are what it is for — *\"only i should be able to delete todo \
             items. as a rule everything that ever created stays in history\"* — so `/todo rm N` is a \
             STATUS: the row keeps its words, its author and its place on the board, the model still \
             sees it (marked `[c]`), and the idle check never asks about it again. 41 also carries \
             `SetOperatorTodos.moved` — an ADDED, DEFAULTED field (the zero-bump kind this file names \
             everywhere) which rides this number because 41 is owed anyway and the ATTACH-time refusal \
             is what the number buys: a version-41 head and a version-40 daemon never meet, so the key \
             an older daemon would ignore is not a fact anybody can silently skip. It is there so the \
             operator's `/todo postpone|resume` can name a row the MODEL wrote — the verb's REACH, \
             which is the whole of that half: `todo_write` refuses `postponed` (a model that could \
             silence its own plan could abandon it) and the pane numbers only the operator's own rows, \
             so a model's row that waits on a person's hand had no door at all. 40 was \
             `TranscriptForked`, one \
             NEW event (a version-39 head would fail to decode it mid-session, the version-25 \
             argument), and the bump is owed because the fact it carries is one a head MUST NOT \
             skip: an added, defaulted field would have been the zero-bump route and an older head \
             would have rendered exactly what it rendered before, which here is the conversation \
             drawn TWICE. A fork — `/reseat`, `/compact`, a re-seat onto a rebuilt prompt — opens a \
             new transcript and publishes every carried row under its ids; nothing said the rows a \
             reader already held belonged to a transcript the session had left, so every reader \
             folded both conversations into one list (MEASURED in the head's fixtures: 40 rows plus \
             a 60-row carry left 100). 39 was \
             `ListNotes` and \
             `StandingNotes`, one NEW client frame and one NEW server frame carrying `NoteEntry`, \
             the row the standing-notes pane draws (a version-38 daemon would fail to parse the \
             first, a version-38 head would fail to decode the second mid-session — the version-34 \
             argument). The notes section is verbatim under a budget and an index over it, and the \
             index half has never been visible from outside: the operator asked for \"the usual - \
             notes pane\" over a corpus where which notes the model was GIVEN WHOLE could not be \
             seen at all. 38 was \
             `MergeState::Vetoed`, a NEW VARIANT on an existing enum: **no frame is added and the \
             number still has to move**, because the word travels inside `MergeEntryMoved` and \
             `ServerFrame::MergeQueue` — both carry the state — and a version-37 head cannot DECODE \
             `\"state\":\"vetoed\"`: the failure takes the whole snapshot down with it, \
             mid-session (the version-36 argument, at the level of a variant again). The operator \
             may now VETO an entry — their own ask, *\"i want to be able to approve / veto / \
             delete\"* — and a person's rejection is not a gate's failure, so it is a state a head \
             has to be able to name (and must not draw as red). 37 was \
             `TodoBy::Parent`, a NEW VARIANT on an existing enum: **no frame is added — the parent's \
             write is daemon-internal, nothing new crosses the wire as a frame — and the number \
             still has to move**, because a version-36 head cannot DECODE `\"by\":\"Parent s-…\"` \
             and the failure takes the whole `TodosUpdated`/`Todos` frame down with it (the \
             version-36 argument, at the level of a variant again). A parent session may now write \
             its child's board, and the rows carry the author the operator named — `Parent <full \
             session id>` — so a child can tell what it decided from what it was told. 35 is \
             skipped rather than spent: it was reserved for `agent/agent-refresh` — a head that \
             re-seats itself asks what its session's pane is running — and that branch has not \
             landed. 36 was `TodoStatus::Postponed`, the same argument one enum over: \
             the operator can set a row ASIDE — it persists, the model still sees it marked, and \
             the idle check stops asking — so the four words the store spells are four words a \
             head has to be able to read. 34 was \
             `TermStatus`, one NEW client frame and one NEW server frame (a version-33 daemon \
             would fail to parse the first, a version-33 head would fail to decode the second \
             mid-session): `ctrl-\\` now DETACHES — it sends nothing at all — and `!term close` \
             is the deliberate ending, so a head has to be able to ask what is running in a \
             pane it is not drawing. 33 was \
             `PromptAnswer` and `SendLine`, two NEW client frames (a version-32 daemon would \
             fail to parse the first of them, the version-4 argument), and `PromptRequested` \
             and `PromptSettled`, two NEW events (a version-32 head would fail to decode them \
             mid-session, the version-25 argument): the operator's own run can be ANSWERED, \
             which is what `! sudo apt install mc` aborting at `Continue? [Y/n]` asked for. 32 \
             was `TermAttached`, one NEW server frame, the answer to a bare `!term`: the \
             pane a session already has, and what is running in it. 31 was `!term` whole — \
             `TermOpen`, `TermInput`, `TermResize` and `TermClose`, four NEW client frames (a \
             version-30 daemon would fail to parse the first at ATTACH, the version-4 \
             argument), and `TermOutput` and `TermEnded`, two NEW server frames — as opposed \
             to an added defaulted field, which is the case that needs no bump. 30 was \
             `SuggestShell`/`ShellSuggestions`, and 29 `ListMergeQueue`/`MergeQueue` and the \
             two merge-queue events"
        );
    }

    /// A collection a head's encoder can omit must not be fatal: that encoder drops every NIL, so
    /// an empty list arrives as no key at all, and a decoder that demanded one would refuse a head.
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

        // And the spelling WITH items is unaffected - the two neighbours this could break are a
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

        // And the second half of the frame: the state moves on rows that are NOT the head's.
        // `moved` is defaulted, so a frame carrying none still parses; this is the other
        // direction, asserted on the literal bytes because it is what the daemon resolves a row by.
        let with_moves: ClientFrame = serde_json::from_str(
            r#"{"frame":"set_operator_todos","client_request_id":"r3","expected_seq":11,
                 "moved":[{"content":"operator: paste the token","status":"postponed"}]}"#,
        )
        .expect("a frame with state moves parses");
        let ClientFrame::SetOperatorTodos { moved, items, .. } = &with_moves else {
            panic!("not a set_operator_todos: {with_moves:?}");
        };
        assert!(
            items.is_empty(),
            "a move does not put anything in the head's own half: {items:?}"
        );
        assert_eq!(
            moved,
            &vec![crate::event::TodoState {
                content: "operator: paste the token".into(),
                status: crate::event::TodoStatus::Postponed,
            }],
            "the row is named by its words and the state is the one the operator asked for"
        );
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

    /// A prompt from a head that never heard of attachments: the wire's own bytes. The field is
    /// defaulted for exactly this, and an older head keeps working.
    #[test]
    fn an_old_prompt_frame_decodes_with_no_attachments() {
        let f: ClientFrame = serde_json::from_str(
            r#"{"frame":"prompt","client_request_id":"leticl-1","expected_seq":41,
                 "text":"what is in this picture?"}"#,
        )
        .expect("a version-49 head's prompt must still parse");
        let ClientFrame::Prompt {
            expected_seq,
            text,
            attachments,
            ..
        } = f
        else {
            panic!("not a prompt: {f:?}");
        };
        assert_eq!(expected_seq, 41, "the rest of the frame still reads");
        assert_eq!(text, "what is in this picture?");
        assert!(attachments.is_empty(), "none attached: {attachments:?}");

        // And the NEW spelling, on the wire: the kind is a word, and the path is what the
        // daemon opens. Asserted as JSON rather than as a struct so the spelling a head in
        // another language has to produce is pinned here and not only in serde's defaults.
        let with: ClientFrame = serde_json::from_str(
            r#"{"frame":"prompt","client_request_id":"r2","expected_seq":9,"text":"look",
                 "attachments":[{"kind":"image","path":"/tmp/a.png","media_type":"image/png",
                                  "bytes_len":1024,"name":"a.png"}]}"#,
        )
        .expect("the new spelling parses");
        let ClientFrame::Prompt { attachments, .. } = with else {
            panic!("not a prompt")
        };
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, AttachmentKind::Image);
        assert_eq!(attachments[0].path, "/tmp/a.png");
        assert_eq!(attachments[0].bytes_len, 1024);
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
        // The wiring fields are present and empty rather than absent, for the same reason
        // `dropped` is present and zero: this daemon does not know, and empty says so.
        assert!(json.contains(r#""endpoint":"""#), "{json}");
        assert!(json.contains(r#""sessions":[]"#), "{json}");
    }

    #[test]
    fn no_frame_field_is_elided_when_zero_or_empty() {
        // The whole module is a disclosure surface, so `skip_serializing_if` here would make
        // "nothing was filtered" and "this build does not report it" the same bytes.
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
                attachments: vec![Attachment {
                    kind: AttachmentKind::Image,
                    path: "/tmp/shot.png".into(),
                    media_type: "image/png".into(),
                    bytes_len: 94_208,
                    name: "shot.png".into(),
                }],
            },
            // A frame from a version-49 head has no `attachments` key at all, which is
            // asserted on the wire's own bytes in
            // `an_old_prompt_frame_decodes_with_no_attachments` below.
            ClientFrame::Prompt {
                client_request_id: "r1b".into(),
                expected_seq: 12,
                text: "no attachments here".into(),
                attachments: Vec::new(),
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
            // 53's frame, on the round trip like every other: a variant whose fields are
            // never exercised is a variant that stops encoding without anything saying so.
            ClientFrame::CloseSession {
                client_request_id: "r1c".into(),
                expected_seq: 12,
                who: "dead".into(),
                reason: "it is being carried to another machine".into(),
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

    /// A job's name rides the `Jobs` frame, and a row written without one still reads: the name has
    /// a key of its own on the wire, and a daemon too old to send one produces exactly the row it
    /// always produced. *An id with no name* is spelled empty and not as a row that fails to parse.
    #[test]
    fn a_jobs_frame_carries_the_name_and_a_row_without_one_still_reads() {
        let f = ServerFrame::Jobs {
            monitors: Vec::new(),
            session_id: "s-1".into(),
            jobs: vec![JobEntry {
                id: "j65".into(),
                command: "cargo build --release".into(),
                slug: "release-build".into(),
                how: "asked".into(),
                state: "running".into(),
                running: true,
                never_ran: false,
                redirect: None,
                produced: 12_288,
                elapsed_ms: 2_000,
            }],
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains(r#""slug":"release-build""#),
            "the key is the contract two heads agree about: {json}"
        );
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());

        // The older daemon's row: the same frame with no `slug` key at all. It reads as
        // empty - *nobody named this* - which is the row a head drew before the field existed,
        // and the reason this is additive rather than a bump.
        let old = json.replace(r#","slug":"release-build""#, "");
        assert!(!old.contains("slug"), "the field was not removed: {old}");
        let ServerFrame::Jobs { jobs, .. } =
            serde_json::from_str::<ServerFrame>(&old).expect("a row with no slug reads")
        else {
            panic!("a jobs frame")
        };
        assert_eq!(jobs[0].slug, "");
        assert_eq!(jobs[0].id, "j65");
        assert_eq!(jobs[0].command, "cargo build --release");
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
                    needs: Vec::new(),
                },
                crate::event::TodoEntry {
                    content: "render the pane".into(),
                    status: crate::event::TodoStatus::InProgress,
                    by: crate::event::TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
                // The fourth word, which a head has to read back to draw the state: the row the
                // operator set aside.
                crate::event::TodoEntry {
                    content: "push once CI lands".into(),
                    status: crate::event::TodoStatus::Postponed,
                    by: crate::event::TodoBy::Operator,
                    when: Some(crate::event::TodoCondition::Job {
                        handle: "j121".into(),
                    }),
                    needs: Vec::new(),
                },
                // And the fifth: the row the operator struck off. Here for the postponed row's
                // reason, and because the row KEEPS ITS PLACE in the list.
                crate::event::TodoEntry {
                    content: "push leticl to github".into(),
                    status: crate::event::TodoStatus::Cancelled,
                    by: crate::event::TodoBy::Operator,
                    when: None,
                    needs: Vec::new(),
                },
            ],
        };
        let json = serde_json::to_string(&f).unwrap();
        // The statuses spell the way the store spells them, so a `sqlite3`
        // reader and a head reader agree.
        assert!(json.contains(r#""status":"completed""#), "{json}");
        assert!(json.contains(r#""status":"in_progress""#), "{json}");
        assert!(json.contains(r#""status":"postponed""#), "{json}");
        assert!(json.contains(r#""status":"cancelled""#), "{json}");
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());
    }

    /// A parent's row carries its author as the operator's own string, and nothing else in the
    /// frame moves: the wire spelling is `"by":"Parent <id>"`.
    #[test]
    fn a_parents_row_carries_the_authors_own_string_on_the_wire() {
        let f = ServerFrame::Todos {
            session_id: "s-child".into(),
            todos: vec![
                crate::event::TodoEntry {
                    content: "mine, the child's own".into(),
                    status: crate::event::TodoStatus::InProgress,
                    by: crate::event::TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
                crate::event::TodoEntry {
                    content: "told by the parent".into(),
                    status: crate::event::TodoStatus::Pending,
                    by: crate::event::TodoBy::parent_of("s-1789462738453908838"),
                    when: None,
                    needs: Vec::new(),
                },
            ],
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains(r#""by":"Parent s-1789462738453908838""#),
            "the author is the operator's string, in full: {json}"
        );
        assert!(
            json.contains(r#""by":"model""#),
            "and the other words are unchanged: {json}"
        );
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());
    }

    /// A row's condition is TAGGED on the wire, and that tag is the contract: a tagged enum makes a
    /// new kind additive, and lets a reader tell a kind it does not know from no condition at all.
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
                needs: vec![
                    crate::event::TodoNeed::Row {
                        content: "run the tests".into(),
                    },
                    crate::event::TodoNeed::Child {
                        id: "s-1-sub-2".into(),
                    },
                    crate::event::TodoNeed::Job {
                        handle: "j2070".into(),
                    },
                ],
            }],
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains(r#""when":{"kind":"job","handle":"j121"}"#),
            "the condition's own spelling is the wire contract: {json}"
        );
        assert!(
            json.contains(r#""needs":[{"kind":"row","content":"run the tests"},"#),
            "and an edge is a tagged kind on the wire too: {json}"
        );
        assert!(
            json.contains(r#"{"kind":"child","id":"s-1-sub-2"},"#),
            "a child need is a session id under its own kind: {json}"
        );
        // And a job need is a handle under its own kind: that spelling is what a head has to
        // agree about to carry the edge back through `SetOperatorTodos`.
        assert!(
            json.contains(r#"{"kind":"job","handle":"j2070"}]"#),
            "a job need is a handle under its own kind: {json}"
        );
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());

        // A row written before the field, with no `when` at all, reads as unconditional, which
        // is what it was: without this half a head would refuse the older row.
        let old = json
            .replace(r#","when":{"kind":"job","handle":"j121"}"#, "")
            .replace(
                r#","needs":[{"kind":"row","content":"run the tests"},{"kind":"child","id":"s-1-sub-2"},{"kind":"job","handle":"j2070"}]"#,
                "",
            );
        assert!(!old.contains("when"), "the field was not removed: {old}");
        assert!(!old.contains("needs"), "the field was not removed: {old}");
        let ServerFrame::Todos { todos, .. } =
            serde_json::from_str::<ServerFrame>(&old).expect("a row with no condition reads")
        else {
            panic!("a todos frame")
        };
        assert_eq!(
            todos[0].when, None,
            "an absent condition is `None` — not an error, and not a default condition"
        );
        assert!(
            todos[0].needs.is_empty(),
            "and an absent edge list is no edges — not an error, and not a default need"
        );
    }
}

#[cfg(test)]
mod skew_tests {
    use super::protocol_skew;

    /// The sentence says nothing when the two match, and the direction when they do not, because
    /// the two directions are different answers to a person asking whether their head is old.
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

        // Two situations, two sentences: a test that only checked `!= None` would pass on a
        // function that said the same wrong thing twice.
        assert_ne!(newer, older);
        // Neither of them tells anybody to leave. A skew is usually survivable, and exiting here
        // would be the failure that argument is against.
        for s in [newer, older] {
            assert!(!s.contains("quit"), "{s}");
        }
    }
}

#[cfg(test)]
mod the_door_verbs_transform {
    use super::*;

    /// The hyphen is a keyboard transform and the wire keeps the tool's name. Asserted against
    /// [`HEAD_RUN_TOOLS`] itself, so a new door verb says what its typed spelling is without
    /// anybody writing it down twice.
    #[test]
    fn every_door_tool_has_a_hyphened_verb_and_the_tool_keeps_its_own_name() {
        for tool in HEAD_RUN_TOOLS {
            let verb = head_run_verb(tool);
            assert!(!verb.contains('_'), "`/{verb}` still needs the shift key");
            // The wire is unmoved: the tool's own spelling answers, and the transform is
            // `_` to `-` and nothing else.
            assert_eq!(head_run_tool(&verb, &HEAD_RUN_TOOLS), Some(tool));
            assert_eq!(head_run_tool(tool, &HEAD_RUN_TOOLS), Some(tool));
        }
        assert_eq!(head_run_verb("web_search"), "web-search");
        assert_eq!(head_run_verb("read"), "read", "no underscore, no change");
    }

    /// Both spellings are one verb, and case does not matter: an operator who types what the
    /// daemon calls it should not be told they are wrong, and a slash verb is typed by a hand.
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
        // And a name that is not a door tool resolves to nothing, however it is spelled: the
        // transform must not turn an unknown word into a known one.
        for typed in ["bash", "write", "web-push", "re-ad", "read_"] {
            assert_eq!(head_run_tool(typed, &HEAD_RUN_TOOLS), None, "`{typed}`");
        }
    }

    /// `read_` and `read` are not the same word. The transform maps `_` to `-` on both sides of
    /// the comparison, so a trailing underscore becomes a trailing hyphen and no longer matches -
    /// which is right: accepting it would mean accepting anything one edit away from anything.
    #[test]
    fn the_transform_is_exact_and_not_fuzzy() {
        assert_eq!(head_run_tool("read-", &HEAD_RUN_TOOLS), None);
        assert_eq!(head_run_tool("re", &HEAD_RUN_TOOLS), None);
        assert_eq!(head_run_tool("", &HEAD_RUN_TOOLS), None);
    }
}
