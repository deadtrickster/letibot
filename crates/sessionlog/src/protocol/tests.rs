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
            | ClientFrame::ArchiveSession { .. }
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
            | ClientFrame::Github { .. }
            | ClientFrame::RoomChat { .. }
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
            | ClientFrame::UnarchiveSession { .. }
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
            | crate::SessionEvent::ProviderTrouble { .. }
            | crate::SessionEvent::ModelRetry { .. }
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
            | ServerFrame::Github { .. }
            | ServerFrame::RoomChat { .. }
            | ServerFrame::RoomChatPushed { .. }
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
        PROTOCOL_VERSION, 74,
        "the match above was last reconciled with the frame list at 74. 74 gave \
             `ClientFrame::NewSession` an `id`: a package's owner row and its branch are named by \
             the session id before the session exists, so the caller sends the id it wrote down. \
             `serde(default)`, so a version-73 daemon parses the frame and mints its own id - which \
             is why the number moves: the row and the branch would name a session that never \
             existed. 73 added \
             `SessionEvent::HeldResults`'s `room` and `SessionStatus`'s `held_rooms`: what a session \
             has been TOLD and its model has not been shown - a room line nothing rang for, held \
             until its quiet window passes and then said as one digest (R204). Both are \
             `serde(default)`, so a version-72 head parses the event and the status and drops the \
             field - which is why the number moves: the head would draw no held counter for the \
             lines that are waiting, and the picker's row would not say `N held` either, so the one \
             fact the operator asked for (*nothing is blackholed*) would be missing exactly where it \
             is looked for. 72 gave \
             `SessionEvent::Citations`'s `Citation` a `kind` and a `body`: a reply marks lines as well \
             as citing them (`note path:3-4` with the note inside, `suggest path:3-4` with the \
             replacement), and an editor draws a mark from it. Both fields are `serde(default)`, so a \
             version-71 head parses the event - and draws a note as a citation of lines it then never \
             shows the note beside, which is why the number moves: the bump is what a head reads to \
             refuse to promise a mark it cannot draw. 71 added \
             `ClientFrame::Prompt.context`, an ADDED, DEFAULTED field that is still a bump, for 50's \
             reason with attachments: what the operator pointed at in the editor is rendered into the \
             conversation by the DAEMON (`harnessd::editor_context`), and a version-70 daemon parses \
             the frame and DROPS it in silence, so the bump is what a head reads to refuse to promise \
             a selection it cannot send. 70 gave \
             `SessionEvent::JobSettled` an `edits` field: what changed in the job's tree while it ran, \
             from the snapshot taken at its spawn, so a job's writes are drawn on its card (R161). \
             `serde(default)`, so a version-69 head parses the event and drops the field - which is \
             why the number moves: the card would say nothing about a change the daemon measured. \
             69 made a tool's edit \
             excerpts a LIST: `SessionEvent::ToolFinished` and `view::CallState::Finished` carry \
             `edits` instead of the single `edit`, because one `bash` call can change several files \
             and each is drawn as its own diff (R161). No variant is new and every field is \
             `serde(default)`, so a version-68 peer still parses; what the bump records is that a \
             head built before it parses the event and DROPS the field, so a change to two files \
             would draw no diff at all. The old spelling is read, not migrated: `edits` takes \
             `alias = \"edit\"` and `one_or_many_edits` reads the one object as a one-item list, so a \
             stored transcript or a log written before this date draws exactly what it drew. 68 added \
             `ClientFrame::Github` and `ServerFrame::Github`, the daemon's GitHub reviewer seat \
             (R118): `letibot attach github <scope>` asks the DAEMON to hold a scope and is answered \
             with the snapshot its pane is drawn from, so a head that does not know the frames \
             could not attach a scope at all. Two new variants, so a version-67 peer fails to \
             parse whichever arrives first. 67 gave \
             `ServerFrame::Todos` a `seats` field: the boards the daemon's store keeps for Claude \
             Code seats (`docs/seat-todo.md`), which the head's todo pane draws beside the \
             session's. It is `serde(default)` and no variant is new, so a version-66 head parses \
             the frame and DROPS the field - which is why the number moves: the pane of a head \
             that asked for a board would draw the session's alone and never say a seat's was \
             there, and the operator's own words for this row are *the head's pane for a seat's \
             board*. A seat is not a session, so the boards cannot ride `TodosUpdated`: the seat \
             writes its own board in another process and the daemon hears of it only when a head \
             asks. 66 added \
             `Waiting::Limit`, the usage-limit fact a session's activity carries - `paused until \
             21:02:27 UTC`, or `glm limited until 21:02:27 UTC, on deepseek`. A version-65 head has \
             no arm for the variant and would fail to decode the whole `ActivityChanged` (serde has \
             no catch-all on it, deliberately), so a session that met a limit would blind an old \
             head to what it is waiting on. 65 added \
             `ServerFrame::RoomChatPushed`, the unsolicited half of the operator's chat: the \
             coordinator's push reaches a head without a poll, and a version-64 head cannot decode \
             it, so the number moves. 64 added \
             `TranscriptItem::Offer`, the display-only row that shows the operator a skill or \
             note offered to the model: a version-63 head cannot decode a snapshot or an \
             announcement that carries one, so the number moves. Every renderer skips it, so the \
             prompt is unchanged. 63 gave \
             `SessionEvent::TurnFinished` its `prefix_check` and `dialect_template_sha`: a head \
             could not tell a slot that did not hold the history from a prompt of ours that \
             diverged, which is §18.2's question, and could not say which template produced the \
             turn. Both fields are defaulted and skipped when absent, so a 62 head and a 63 daemon \
             still parse each other's frames; what the bump records is that an ending now says \
             what the engine measured. 62 gave \
             `view::Snapshot` an `items_lead`, the count of its rows that sit above the base and are \
             numbered by nothing: a resume republishes a compaction's ancestors in the same list, and \
             a head that guessed zero asked for every page above its window that many rows too far \
             into the conversation. The field is `serde(default)`, so a version-61 head still parses \
             every frame this daemon sends and simply ignores it. 61 added \
             `ClientFrame::RoomChat` and `ServerFrame::RoomChat`, the operator's chat through the \
             daemon: a version-60 daemon cannot decode the request, so the number moves. 60 put the row's body \
             on `SessionEvent::TranscriptAppended` (`item`, `body_bytes`) and gave `Caps` \
             `row_bodies`. Every new field is defaulted, so a version-59 head and a 60 daemon still \
             parse each other's frames, and the daemon keeps sending `TranscriptContent` to a head \
             that did not declare `row_bodies`; what the bump records is that an announcement now \
             carries the row. 59 added \
             `SessionEvent::ModelRetry`, the one row a series of retries is worth: a version-58 \
             head cannot decode it, and an event side has no version negotiation at parse time, so \
             the number moves - that head reports the frame as unreadable and skips it, which is \
             the settled-and-still-a-row behaviour it had. 58 added \
             `SessionEvent::ProviderTrouble`, the four ways a model's provider fails that an \
             automation reacts to differently: a version-57 head cannot decode it, and an event \
             side has no version negotiation at parse time, so the number moves. 57 gave \
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
    // `../` because this file is `src/protocol/tests.rs` and the module it checks is the
    // `src/protocol.rs` beside that directory.
    let offender = include_str!("../protocol.rs")
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
            context: None,
        },
        // A frame from a version-49 head has no `attachments` key at all, which is
        // asserted on the wire's own bytes in
        // `an_old_prompt_frame_decodes_with_no_attachments` below.
        ClientFrame::Prompt {
            client_request_id: "r1b".into(),
            expected_seq: 12,
            text: "no attachments here".into(),
            attachments: Vec::new(),
            context: None,
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
            // A head closing a row it is looking at rather than where it stands.
            session_id: "s-other".into(),
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
            id: None,
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
        // 45's pair, on the round trip like every other: the reason is the field an older head
        // cannot send, and `default` is what makes that a frame a daemon still parses.
        ClientFrame::ArchiveSession {
            client_request_id: "r7".into(),
            session_id: "s-2".into(),
            reason: "the question was answered".into(),
        },
        ClientFrame::UnarchiveSession {
            client_request_id: "r8".into(),
            session_id: "s-2".into(),
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
        archived: Vec::new(),
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
        seq: 0,
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
            cgroup: "3 processes, 1.2 GiB now of a 2.0 GiB cap".into(),
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
        seats: Vec::new(),
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
        seats: Vec::new(),
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
        seats: Vec::new(),
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

/// **A `seats` field is what an older head drops.** A version-66 head's `Todos` frame has two
/// fields; serde ignores one it has never heard of, so the pane draws the session's board and
/// never learns a seat had one - which is the whole of what the bump records, and the reason the
/// field could be added without a new variant.
#[test]
fn an_old_heads_todos_frame_drops_the_seats_it_does_not_know() {
    /// The version-66 head's reading of the frame, spelled out because that build cannot be
    /// linked in here: the two fields it has, and nothing for the one it does not.
    #[derive(serde::Deserialize)]
    struct OldTodos {
        session_id: String,
        todos: Vec<crate::event::TodoEntry>,
    }
    let f = ServerFrame::Todos {
        session_id: "s-1".into(),
        todos: vec![crate::event::TodoEntry {
            content: "the session's own row".into(),
            status: crate::event::TodoStatus::Pending,
            by: crate::event::TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }],
        seats: vec![crate::event::SeatBoard {
            seat: "cc@proj".into(),
            todos: vec![crate::event::TodoEntry {
                content: "collect the package".into(),
                status: crate::event::TodoStatus::InProgress,
                by: crate::event::TodoBy::Model,
                when: None,
                needs: vec![crate::event::TodoNeed::Child {
                    id: "s-1789462738453908838".into(),
                }],
            }],
        }],
    };
    let json = serde_json::to_string(&f).unwrap();
    assert!(
        json.contains(r#""seat":"cc@proj""#)
            && json.contains(r#""kind":"child","id":"s-1789462738453908838""#),
        "the daemon does say it, so the assertion below is not vacuous: {json}"
    );
    let old: OldTodos = serde_json::from_str(&json).expect("an older head parses the frame");
    assert_eq!(old.session_id, "s-1");
    assert_eq!(
        old.todos.len(),
        1,
        "the session's own board arrives whole, and the seats are dropped: {:?}",
        old.todos
    );
    // And this head reads its own field back, edge and all.
    assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());
}
