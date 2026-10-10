//! **What the app asks of the driver** (`Action`), and what applying a frame did (`Disposition`).

/// What `apply` did with a frame. The driver counts these into the ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// It changed the screen.
    Rendered,
    /// It was read and deliberately not shown. **Counted.**
    Filtered,
    /// Not an event: a Hello, a Resync, a command reply.
    Control,
}

/// Something the head wants the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Prompt(String),
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
        /// **What to tell the model**, typed after `deny_and_tell`. The option's
        /// label promised this and nothing carried it.
        note: Option<String>,
    },
    /// **Answer a `question`** (§1.7), which is a different frame from `Answer` for a
    /// reason this tree already records: `Answer` grants or denies a **permission**,
    /// whose failure mode is *something runs*, while a question answers *which way
    /// should I go*, whose failure mode is *a person is quoted as saying something
    /// they did not*. So the two vocabularies stay two frames and two types, and this
    /// is the head's half of the one that had no sender.
    ///
    /// The head could not answer a question **at all** before this: `answer_marked`
    /// returned `None` for an empty `options`, a question always carries
    /// `options: []` with the model's offered choices in `choices`, and nothing in
    /// this crate constructed `ClientFrame::AnswerQuestion`. A question rendered as a
    /// headline over an empty ladder and *"this ask offers no options — your line is
    /// held"*.
    AnswerQuestion {
        req_id: String,
        answer: letibot_sessionlog::question::QuestionAnswer,
    },
    Resync,
    /// Ask the daemon what sessions it holds.
    ListSessions,
    /// Ask for this session's todo list — the pane's bootstrap read.
    ListTodos,
    /// **The operator's half of the board, replaced wholesale** — R51 item 18's write half, and
    /// the frame that had no sender until now — **plus the state moves they asked for on rows that
    /// are not theirs.**
    ///
    /// The daemon stores and serves these rows and hands them to the model as part of one union;
    /// what it cannot do is invent one, because the words are the operator's. So the head owns them
    /// and sends the whole list on every change, which is the shape `TodoBoard::set_operator`
    /// documents: *"a delta protocol for a list of tens of items would be a second source of truth
    /// about them."*
    ///
    /// **`moved` is one act, not a second list.** `/todo postpone|resume` can name a row the MODEL
    /// wrote — a row that waits on the operator's own hand, which `todo_write` refuses to silence
    /// and which the pane's numbers never reach — and a state move is the whole of it. It rides this
    /// action because both halves are *the board as the operator wants it*, and two frames for one
    /// act is how a head's copy and the store come to disagree.
    SetOperatorTodos {
        items: Vec<letibot_sessionlog::event::TodoEntry>,
        moved: Vec<letibot_sessionlog::event::TodoState>,
    },
    /// Ask the daemon for its job table. The head renders the answer; it does
    /// not decide what is in it.
    ListJobs,
    /// **Ask the daemon for the standing notes, one row each.** The pane's bootstrap read,
    /// and the jobs pane's shape one mailbox over: the corpus and the form each file has —
    /// verbatim, or an index because it did not fit what was left of the budget — are decided
    /// by the module that assembles the section, with the session's own token counter. A head
    /// holds no counter and draws what it is given.
    ///
    /// Asked on every pane-open rather than held: the notes are the operator's own files, and
    /// this is the read that makes a note written a minute ago a row.
    ListNotes,
    /// **Ask for the merge queue, whole.** The queue pane's bootstrap read, and
    /// daemon-level: there is one `main` and one queue, so the answer is not scoped to this
    /// session — see `ClientFrame::ListMergeQueue`.
    ListMergeQueue,
    /// Make one. The head switches to it when the daemon says which id it minted;
    /// see [`App::apply`]'s `Sessions` arm.
    NewSession(String),
    /// Move this connection to another session.
    Switch(String),
    /// Read a subagent's output without leaving this session: the daemon answers
    /// with `Peeked`, and the pane the tree's read key opens is built from it —
    /// `p` on a subagent row, and `/peek ID` for the typed spelling.
    /// Lazy — nothing is read until this is sent.
    Peek(String),
    /// Read one background job's output into a pane, without leaving this session
    /// and without posting a slash to the conversation. Answered with a
    /// `SessionEvent::JobOutput` on the log, and the pane is built from it. The
    /// jobs pane's Enter sends this instead of `/job ID` — the operator asked for
    /// exactly that: *"when i press enter on jobs pane im not shown the job output
    /// im brought back to the main conversation with /job <id> posted - this is not
    /// what i want"*.
    ReadJobOutput {
        job: String,
        /// Where the window starts. 0 is the first byte; the pane pages by asking
        /// at the offset the previous answer named.
        offset: u64,
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
    /// Move this session's project to a named point, persisted by the daemon.
    /// See `D13`.
    /// `consented` is the operator's answer to the unconfined-`allow-all`
    /// confirmation. False for every other point, and false for `allow-all`
    /// until they say yes — the daemon reads it only where it matters and treats
    /// a missing answer as no.
    Mode {
        name: String,
        consented: bool,
    },
    /// Take back every prompt this head queued that the running turn has not
    /// consumed yet — the companion of a recall: Up pulled the queued line into
    /// the composer to edit it, and the original must not land behind the edit.
    WithdrawPrompts,
    /// A command the daemon handles: `flowy …`, `models …`. The line minus `/`.
    Slash {
        line: String,
    },
    /// **The operator's own tool call through the door** — R24 part two, R31, R34.
    ///
    /// `name` is the TOOL's spelling (`web_search`), never the typed verb; the hyphen is a
    /// keyboard transform and this is the wire. `arguments` is the JSON object the head
    /// built from the field the daemon published, and the head knows nothing else about the
    /// tool — see [`App::head_run_call`].
    ///
    /// **`execute` is true and there is no result to send back.** This head has no tool
    /// runtime and no HTTP client, so the daemon runs it, in this session, with the byte
    /// caps and spill policy a model's call gets. That is not a convenience: a head that
    /// fetched a page itself would write a corpus row saying *the operator ran `web_fetch`*
    /// about another program's answer. See `ClientFrame::OperatorCall::execute`.
    HeadRun {
        name: String,
        arguments: String,
    },
    /// **The operator's own shell line** — a submitted line that began with `!`.
    ///
    /// The line travels verbatim, bang included: the daemon strips it (one rule, at the
    /// execution site) and the row the operator gets back is the words they typed. The
    /// daemon runs it in this session's workspace through the `bash` path — confine, sudo
    /// askpass and byte caps identical to a model's call — and appends two rows: this
    /// line as the operator's own, and the output as a `bash` result the head folds and
    /// pages like every other tool row. See [`App::submit`]'s `!` arm for the typing
    /// surface and `ClientFrame::OperatorShell` for the wire.
    OperatorShell {
        line: String,
    },
    /// **`!term <command>` — open the pane and run a screen program in it.**
    ///
    /// `line` travels verbatim, `!term` first, exactly as [`Action::OperatorShell`]'s does: the
    /// daemon strips the verb (one rule, at the execution site) and the head has already
    /// checked at the composer that there *is* a command after it — see [`App::submit`].
    ///
    /// **The rectangle is not on this action and cannot be**: it is the terminal's, and the
    /// driver is the thing that knows it. `Link::tick` sends the frame with the size it was
    /// handed, which is also what makes a pane opened after a resize open at the right size
    /// rather than at the default of 80×24.
    TermOpen {
        line: String,
    },
    /// **The operator's keys, verbatim, to the pane's program.** Bytes and not a `Key`: the
    /// pane is a terminal and the head is not the thing that reads it — see
    /// [`ClientFrame::TermInput`](letibot_sessionlog::protocol::ClientFrame::TermInput) for why
    /// a decoded-and-re-encoded arrow would be a different byte string to a program that asked
    /// for the application-cursor spelling.
    TermInput {
        bytes: Vec<u8>,
    },
    /// **The pane's rectangle moved.** The head's fact — the daemon has no screen — and the
    /// only way the program is told the size it is being drawn at.
    TermResize {
        cols: usize,
        rows: usize,
    },
    /// **End the pane.** The head's own act, and it is sent **only after the operator has
    /// confirmed it** — the confirmation card is [`App::term_ask`], and the verb that raises it
    /// is `!term close`.
    ///
    /// **`ctrl-\` no longer sends this.** It detaches — the rectangle goes, the conversation
    /// comes back, and **nothing is sent at all** — which is the operator's own correction:
    /// *"but i dont want it to exit"*. Two acts, one frame, and the destructive one is the one
    /// that has to be spelled out. See [`TermPane`].
    TermClose,
    /// **Ask what this session's pane is running.** The answer is
    /// `ServerFrame::TermStatus`, and this is the read that lets a head which is **not drawing**
    /// the pane say that something is running in it — **without a transcript row**, because a
    /// detach is not an event. See [`App::term_fact`].
    ///
    /// Asked on attach (a head that has just switched sessions has no pane of its own and the
    /// pane is the session's), and asked before a `!term close` the head cannot answer from what
    /// it holds.
    TermStatus,
    /// **Ask the model to propose `!` completions for a prefix** — the smart half of
    /// the `!` completion. The history is this head's own and is the first answer;
    /// this is asked for only when the history has no match for the prefix (or its
    /// cycle is exhausted).
    ///
    /// `client_request_id` is minted here, by the head, because the answer comes back
    /// on the pump and the head has to recognise it: the id is the correlation, and a
    /// head that could not tell one answer from another would cache a suggestion under
    /// the wrong prefix. The daemon echoes it back in `ShellSuggestions`.
    ///
    /// **Nothing here submits.** The answer is a list of candidate lines for the
    /// composer, drawn as candidates with their provenance, and Enter is still the
    /// operator's.
    SuggestShell {
        prefix: String,
        client_request_id: String,
    },
    /// A password for `sudo`, or a refusal. Never logged by anything on the way.
    Secret {
        req_id: String,
        secret: Option<String>,
    },
    /// **The operator's answer to a command of their own that asked them something.**
    ///
    /// The card is raised by the daemon when the run is **blocked reading the device the
    /// daemon holds for it** — a reading of the process and not of its words, see
    /// `letibot_tools::exec::ask` — and this is the line the person typed into it.
    ///
    /// **Not `Action::Secret` and not a path to one.** A password has its own card, its own
    /// masked field and its own frame, and the two are separate variants on purpose: this one
    /// is drawn in the open and what it carries is a line for a program's stdin. See
    /// [`App::prompt_lines`].
    PromptAnswer {
        req_id: String,
        line: String,
    },
    /// **One line to the running command, on demand** — the `!send` verb.
    ///
    /// The manual floor under the card: a person watching the stream can answer whether or
    /// not anything looked like a question, so this needs no card, no request id and no
    /// signal at all. It addresses *whatever operator command this session is running right
    /// now*, which the daemon knows and this head does not.
    SendLine {
        line: String,
    },
    Quit,
    /// Leave AND stop the daemon. The head detaches after the daemon has been
    /// asked, so the notice reaches every other head first.
    StopDaemon,
}
