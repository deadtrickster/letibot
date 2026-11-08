//! The head frame format: ATTACH, RESYNC, snapshot, event envelope, `expected_seq`,
//! `client_request_id`. Plain serde types over one NDJSON codec, so a remote head is a
//! different transport, not a protocol. A stale `expected_seq` is rejected with both numbers.

use serde::{Deserialize, Serialize};

use crate::event::Envelope;
use crate::registry::{SessionBrief, SessionWiring};
use crate::scrub::ScrubReport;
use crate::view::Snapshot;

/// Bumped when a frame's meaning changes in a way the other side cannot parse: a new variant is a
/// deserialisation fault in the middle of a turn. Both sides compare it at ATTACH and refuse a
/// mismatch by name.
pub const PROTOCOL_VERSION: u32 = 70;

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
    /// This head draws a row from its own `TranscriptAppended`, so a body that rode one need
    /// not be sent again as `TranscriptContent`. Absent is a head built before the field: it reads
    /// nothing off an announcement, and the daemon keeps sending it the second event.
    #[serde(default)]
    pub row_bodies: bool,
}

impl Caps {
    /// The caps of a head that draws rows: the defaults, plus reading a body off its announcement.
    pub fn drawing_rows() -> Caps {
        Caps {
            row_bodies: true,
            ..Caps::default()
        }
    }
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            queue: 1024,
            can_decide: true,
            features: Vec::new(),
            row_bodies: false,
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
    /// Its `SKILL.md` on the daemon's box, when it is a file: what Enter opens in the editor.
    #[serde(default)]
    pub path: Option<String>,
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
    /// A chat request as the operator, through the daemon's operator seat on the host coordinator.
    /// `op` is a room `Request` as JSON; the answer is [`ServerFrame::RoomChat`]. Refused on a
    /// connection from a tool call (`peer::door`), so a model cannot speak as the operator.
    RoomChat {
        client_request_id: String,
        op: serde_json::Value,
    },
    /// The daemon's GitHub reviewer seat (R118): `op` is one verb - `attach` with a scope, `pause`,
    /// `resume`, `poll`, or `list` - and the answer is [`ServerFrame::Github`] carrying the snapshot
    /// the seat's pane would be drawn from. The seat is the DAEMON's, so `letibot attach github
    /// <scope>` asks here rather than polling in its own process.
    Github {
        client_request_id: String,
        op: serde_json::Value,
    },
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
        /// The boards the daemon's store holds for Claude Code seats (`docs/seat-todo.md`), read
        /// with the session's because the pane draws both and asks once. Defaulted, so a daemon
        /// that predates them says nothing and an older head drops the field it does not know.
        #[serde(default)]
        seats: Vec<crate::event::SeatBoard>,
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
        /// The session's head seq when the list was built. A head that has seen a job event
        /// past it discards the list and asks again. 0 is a daemon that does not stamp.
        #[serde(default)]
        seq: u64,
    },
    /// The answer to [`ClientFrame::ListNotes`]: the standing notes, one row each, in the order the
    /// section carries them, because that order is what the prompt shows.
    StandingNotes {
        session_id: String,
        notes: Vec<NoteEntry>,
        /// The project's notes directory, which the pane watches even while it is empty.
        /// Defaulted, so an older daemon's frame reads as none.
        #[serde(default)]
        dir: Option<String>,
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
    /// The answer to [`ClientFrame::RoomChat`]: the room `Response` as JSON, or `{"error": "..."}`
    /// when the coordinator is unreachable or the request is not a chat request.
    RoomChat {
        client_request_id: String,
        reply: serde_json::Value,
    },
    /// The answer to [`ClientFrame::Github`]: the seat's snapshot as its pane is drawn from it, or
    /// `{"error": "..."}` when the daemon holds no seat or the verb was not one it knows.
    Github {
        client_request_id: String,
        reply: serde_json::Value,
    },
    /// A chat message the coordinator pushed to the operator's seat, straight to every head that has
    /// chatted: the room `Message` as JSON and whether it names this reader. Unsolicited, so a head
    /// that merges it must recognise a `seq` its own poll already brought.
    RoomChatPushed {
        message: serde_json::Value,
        names_you: bool,
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
mod tests;

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
