//! What the app asks of the driver (`Action`), and what applying a frame did (`Disposition`).

/// What `apply` did with a frame. The driver counts these into the ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// It changed the screen.
    Rendered,
    /// It was read and deliberately not shown. Counted.
    Filtered,
    /// Not an event: a Hello, a Resync, a command reply.
    Control,
}

/// One message to send: the words and whatever the composer chipped into them. A struct rather
/// than two fields on the variant, so the overwhelming majority of prompts with no chips keep
/// [`Prompt::new`] as the door with nothing attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The words, chips and all - the same string the operator saw in the composer.
    pub text: String,
    /// The files and pictures, in the order their chips were minted. Empty is the ordinary case.
    pub attachments: Vec<letibot_sessionlog::Attachment>,
}

impl Prompt {
    /// A message with nothing attached.
    pub fn new(text: impl Into<String>) -> Self {
        Prompt {
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    /// A message with the chips the composer carries.
    pub fn with(text: impl Into<String>, attachments: Vec<letibot_sessionlog::Attachment>) -> Self {
        Prompt {
            text: text.into(),
            attachments,
        }
    }
}

/// A message with nothing attached, from its words, so the call sites that have never heard of
/// attachments keep their spelling and a test that compares against one is asserting no chips as
/// well as the words.
impl From<&str> for Prompt {
    fn from(text: &str) -> Self {
        Prompt::new(text)
    }
}

impl From<String> for Prompt {
    fn from(text: String) -> Self {
        Prompt::new(text)
    }
}

/// A message compares with its words. What a caller means by `prompt == "hi"` is the sentence;
/// the attachments are asserted where they are made (`app::attachments`) and sent (the daemon's
/// own tests), not at every site that checks what the composer said.
impl PartialEq<str> for Prompt {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for Prompt {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

/// A message reads as its words: every reader that is not sending it wants the sentence, and
/// `prompt.starts_with(..)` is that sentence. The attachments are the second thing a caller asks
/// for when it is asking about them.
impl std::ops::Deref for Prompt {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for Prompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Something the head wants the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The operator's words, with the chips they put beside them.
    /// The attachments ride the action rather than a field on the app because the action IS the
    /// message: a second copy could be read after a refusal and would have to be undone.
    Prompt(Prompt),
    Interrupt(String),
    /// Move the running command to the background (Ctrl+B), like Claude Code.
    Promote,
    Answer {
        req_id: String,
        option_id: String,
        /// The glob typed after the option id, for an *always allow*. `None` means
        /// the daemon derives the pattern from the call, which is what every answer
        /// did before this existed.
        pattern: Option<String>,
        /// What to tell the model, typed after `deny_and_tell`. The option's
        /// label promised this and nothing carried it.
        note: Option<String>,
    },
    /// Answer a `question`, a different frame from `Answer` because their failure modes differ:
    /// a permission's is that something runs, a question's is that a person is quoted as saying
    /// what they did not. So the two vocabularies stay two frames and two types.
    AnswerQuestion {
        req_id: String,
        answer: letibot_sessionlog::question::QuestionAnswer,
    },
    /// Answer the question a todo ROW waits on (`ClientFrame::AnswerTodo`) - the same card
    /// and the same four shapes as [`Action::AnswerQuestion`], sent to the row rather than to an
    /// open decision: the row's words, the question, and what the person did.
    AnswerTodo {
        content: String,
        question: String,
        answer: letibot_sessionlog::question::QuestionAnswer,
    },
    Resync,
    /// Ask the daemon what sessions it holds.
    ListSessions,
    /// Ask for this session's todo list - the pane's bootstrap read.
    ListTodos,
    /// The operator's half of the board, replaced wholesale, plus the state moves they asked for
    /// on rows that are not theirs. The whole list, because a delta for tens of items would be a
    /// second source of truth; one action, because two frames for one act let the copies drift.
    SetOperatorTodos {
        items: Vec<letibot_sessionlog::event::TodoEntry>,
        moved: Vec<letibot_sessionlog::event::TodoState>,
    },
    /// Ask the daemon for its job table. The head renders the answer; it does
    /// not decide what is in it.
    ListJobs,
    /// Ask the daemon for the standing notes, one row each: the corpus and each file's form are
    /// decided by the module that assembles the section, with the session's token counter, and a
    /// head holds no counter. Asked on every open, so a note written a minute ago is a row.
    ListNotes,
    /// Ask what this session can be handed - the `/skills` pane's bootstrap read. Rows only:
    /// the name, its one line and where it was found. A body travels on [`Action::LoadSkill`],
    /// because a listing of bodies is not a listing.
    ListSkills,
    /// One chat request as the operator, a room `Request` as JSON. The answer comes back as
    /// `ServerFrame::RoomChat` and is routed to the `/rooms` view. `id` is the request id a
    /// send was minted under, so its answer can be told from a poll's: a send that failed is
    /// kept until the operator deals with it, and no other answer may clear it.
    RoomChat {
        op: serde_json::Value,
        id: Option<String>,
    },
    /// Hand this session one skill's body - the pane's `s`, and `/skills NAME rest`. The
    /// daemon loads it, writes the operator's message and runs the turn; `then` is what followed
    /// the name on the line.
    LoadSkill {
        name: String,
        then: String,
    },
    /// Ask for the merge queue, whole. The queue pane's bootstrap read, and
    /// daemon-level: there is one `main` and one queue, so the answer is not scoped to this
    /// session - see `ClientFrame::ListMergeQueue`.
    ListMergeQueue,
    /// Make one. The head switches to it when the daemon says which id it minted;
    /// see [`App::apply`]'s `Sessions` arm.
    NewSession(String),
    /// Move this connection to another session.
    Switch(String),
    /// Read a subagent's output without leaving this session: the daemon answers with `Peeked`
    /// and the pane is built from it. Lazy - nothing is read until this is sent.
    Peek(String),
    /// Ask the daemon for a page of whole rows by session ordinal: what a reader who scrolls past the oldest row
    /// this head holds, or back down toward a tail it let go of, is missing. The one door for rows, since a window
    /// inside one row's body does not carry the row's kind.
    FetchRows {
        session: String,
        /// The first ordinal wanted, oldest first.
        from: usize,
        /// How many: the daemon caps a page, so this is a request and not a promise.
        count: usize,
    },
    /// Read one background job's output into a pane, without leaving this session and without
    /// posting a slash to the conversation. Answered with a `SessionEvent::JobOutput`, and the
    /// pane is built from it.
    ReadJobOutput {
        job: String,
        /// Where the window starts. 0 is the first byte; the pane pages by asking
        /// at the offset the previous answer named.
        offset: u64,
    },
    /// Open a plugin's pane: the daemon runs the look now and answers with `PluginPane`, because
    /// a view this head holds may be an hour old. `w`/`h` can only be guessed here - the room a
    /// pane gets depends on chrome a key handler does not know - so `0, 0` means the default.
    PaneOpen {
        pane: String,
        w: u32,
        h: u32,
    },
    /// A plugin's pane is off the screen, so the daemon stops refreshing it: the opposite of
    /// [`Action::PaneOpen`], and the reason a closed dashboard does not keep reading files every
    /// `every_s` until the session ends.
    PaneClose {
        pane: String,
    },
    /// One key of one stop, back to the pane that drew it - the `event` word its action named, or
    /// `"look"`. A `say` never travels here: it is a line to the session's model, so it goes
    /// through [`Action::Prompt`], attributed to the operator who pressed the key.
    PaneAction {
        pane: String,
        item: String,
        action: String,
    },
    /// Ask the daemon for the settings this session runs under (`/config`).
    Settings,
    /// Bring a session that is in the store but not in this daemon back to life.
    /// The head switches to it on the same `Sessions` reply a `NewSession` produces.
    ResumeSession(String),
    /// Name a session, or clear its name with an empty title.
    Rename {
        session_id: String,
        title: String,
    },
    /// Compact the session this head is in: one summary turn, then the history
    /// is replaced by that summary through a transcript fork.
    Compact,
    /// Rebuild this conversation's prompt from the tools seated now, forking onto
    /// it. The only thing that changes a live session's tool list.
    Reseat {
        /// Summarise as well, replacing the conversation. The default carries it.
        summarise: bool,
    },
    /// Move this session's project to a named point, persisted by the daemon. `consented` is the
    /// operator's answer to the unconfined-`allow-all` confirmation: false for every other point,
    /// and the daemon treats a missing answer as no.
    Mode {
        name: String,
        consented: bool,
    },
    /// A person's answer to a confirmation ANOTHER head asked for. Its own action rather than a
    /// second spelling of [`Action::Mode`], because this settles a request by its id and says
    /// nothing about the point except through the daemon's record of what was asked.
    ModeConsent {
        req_id: String,
        yes: bool,
    },
    /// Take back every prompt this head queued that the running turn has not
    /// consumed yet - the companion of a recall: Up pulled the queued line into
    /// the composer to edit it, and the original must not land behind the edit.
    WithdrawPrompts,
    /// A command the daemon handles: `flowy …`, `models …`. The line minus `/`.
    Slash {
        line: String,
    },
    /// An operator's mark - `/fb`, `/fb+`, `/fb-`, alt-g / alt-b. Its own frame,
    /// `ClientFrame::Feedback`; `at_ms` is this head's clock at the press.
    Feedback {
        at_ms: u64,
        polarity: String,
        note: String,
        item_id: Option<String>,
    },
    /// The operator's own tool call through the door. `name` is the TOOL's spelling, never the
    /// typed verb - the hyphen is a keyboard transform and this is the wire - and `execute` is
    /// true with no result to send back: the daemon runs it, so no head writes a corpus row.
    HeadRun {
        name: String,
        arguments: String,
    },
    /// The operator's own shell line - a submitted line that began with `!`. The line travels
    /// verbatim, bang included, and the daemon strips it at the execution site, so the row the
    /// operator gets back is the words they typed.
    OperatorShell {
        line: String,
    },
    /// A link the operator clicked, opened by the platform: the one act here the daemon knows
    /// nothing about, since a URL is not a session's and the head is the process with the desktop
    /// on it. It carries the URL the FRAME drew, so words and destination cannot disagree.
    OpenUrl(String),
    /// `!term <command>`: open the pane and run a screen program in it. The line travels verbatim
    /// and the daemon strips the verb, one rule at the execution site. The rectangle is not here
    /// and cannot be: it is the terminal's, and the driver is what knows it.
    TermOpen {
        line: String,
    },
    /// The operator's keys, verbatim, to the pane's program. Bytes and not a `Key`, because the
    /// pane is a terminal and the head is not the thing that reads it: a decoded-and-re-encoded
    /// arrow would be a different byte string to a program asking for application-cursor.
    TermInput {
        pane: u32,
        bytes: Vec<u8>,
    },
    /// The pane's rectangle moved. The head's fact - the daemon has no screen - and the
    /// only way the program is told the size it is being drawn at.
    TermResize {
        pane: u32,
        cols: usize,
        rows: usize,
    },
    /// End the pane: the head's own act, sent only after the operator confirmed it, because the
    /// destructive one is the one that has to be spelled out. `ctrl-\` detaches instead and sends
    /// nothing at all.
    TermClose {
        pane: u32,
    },
    /// Ask what this session's pane is running, so a head that is not drawing it can say that
    /// something is running there - without a transcript row, because a detach is not an event.
    /// Asked on attach, and before a `!term close` the head cannot answer from what it holds.
    TermStatus,
    /// Ask the model to propose `!` completions for a prefix: the history is the first answer and
    /// this is asked only when it has no match. The head mints `client_request_id`, because the
    /// answer comes back on the pump; nothing here submits, so Enter is still the operator's.
    SuggestShell {
        prefix: String,
        client_request_id: String,
    },
    /// A password for `sudo`, or a refusal. Never logged by anything on the way.
    Secret {
        req_id: String,
        secret: Option<String>,
    },
    /// The operator's answer to a command of their own that asked them something, raised when the
    /// run is blocked reading the device the daemon holds for it. Not `Action::Secret` and not a
    /// path to one: this is drawn in the open, and what it carries is a line for stdin.
    PromptAnswer {
        req_id: String,
        line: String,
    },
    /// One line to the running command, on demand - the `!send` verb, the manual floor under the
    /// card: a person watching can answer whether or not anything looked like a question, so this
    /// needs no card, no request id and no signal. It addresses whatever command is running now.
    SendLine {
        line: String,
    },
    Quit,
    /// Leave AND stop the daemon. The head detaches after the daemon has been
    /// asked, so the notice reaches every other head first.
    StopDaemon,
}
