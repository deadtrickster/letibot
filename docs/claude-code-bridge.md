# Claude Code ↔ leticode: the bridge

A Claude Code session delegates well-scoped work to leticode to save Claude usage. As a shell
command that was three defects of the shape: **a one-shot run dies with its turn** (a model that
backgrounds a long test and ends its turn exits the process, so the completion never reaches it);
**permission is all or nothing** (only allow-all works, and it enforces nothing the brief asks); and
**following up means restarting** (`leticode --continue` re-runs start-up and the resume, and the
answer comes back as a 10,000-line log).

## The shape

**leticode sessions become things Claude Code holds, not processes it runs once.** The daemon keeps
sessions alive across heads, and its protocol (`crates/sessionlog/src/protocol.rs`) already has what
a client needs: `NewSession`, `Prompt`, `Answer`, `Interrupt`, `Detach`, the listings. The bridge is
**a head that is a program**: no daemon change for its first slice.

```
Claude Code ──stdio MCP── letibot-mcp ──unix socket── harnessd (one per worktree folder)
     ▲                         │
     └── background `letibot-mcp wait …` exits when a session needs attention
```

### The daemon a package gets serves no queue

A worktree of this repository IS one of its session daemons by the merge queue's own rule
(`mergequeue::serves`), so a daemon the bridge started for a package took letibot's merge-queue
lease, found `main` red, and put an `EMERGENCY` in front of the package's model **before its
brief**. A package worker is not the repository's queue holder.

So `daemon_args` passes **`--no-queue`** on every start: no lease claimed, no target gated, no
branch landed, and none of the queue's model-facing notices (`queue_emergency_notices`,
`queue_notices`, `handover_notices`) composed for its sessions. `ensure` also writes the launcher's
record (`<rundir>/<key>.json`) for a daemon it started - after the socket answers, so a failed start
leaves nothing behind and `--daemons` and `--stop N` see it.

### A package whose tools run on another machine

`docs/remote-hosts.md` places a session's **tools** on a declared ssh host - `bash`, `read`, `write`,
`edit` and the jobs run there while the model, the store and the gate stay here - and the bridge only
names it (`leticode_start { host: "lab2x1" }`); **it never speaks ssh**.

- **The name is checked before anything is started**, through the tree's single `hosts.toml` reader
  (`letibot_tools::remote::Hosts`) and the same refusal `task { where: "host:NAME" }` gives: a
  daemon started for an undeclared name would fail in a log the caller has to read, *after* this
  tool had answered that it started one.
- **A daemon already up for that folder is joined only where the call asked to be.** It writes where
  it puts the tools beside its socket (`<key>.placement`), and the record names the socket's own
  `(dev, ino)`, so a record left by a daemon that is gone reads as *nothing known*; `ensure` refuses
  a mismatch naming the placement and the command that ends it. **Nothing recorded is refused in
  both directions**, because `None` is *unknown* and not *local*.
- **Every answer says `placement`** - `host`, `host:lab2x1`, or `null`, which `leticode_start`
  refuses rather than assuming *local*.

## `letibot-mcp`: an MCP server over stdio

A binary in `crates/bridge` (package `letibot-bridge`, bin `letibot-mcp`): JSON-RPC 2.0 with
`initialize`, `tools/list` and `tools/call`. It lives as long as the Claude Code session and holds
one head connection per session it started or attached to, so asks reach it.

| tool | input |
|---|---|
| `leticode_start` | `{worktree, brief, model, mode, host, vm}` - starts the folder's daemon if needed, opens a session, sends the brief, stays attached |
| `leticode_attach` | `{worktree, session, watch_only}` - joins a running daemon (never starts one); `session` is an id or `gatekeeper` |
| `leticode_send` | `{session, text}` - an operator line, queued behind a running turn |
| `leticode_status` | `{session}` - `state`, `model`, `last_trouble`, `placement`, `turn`, `todos`, `last_message`, `report?`, `cost_usd`, `tokens`, `spent`, `pending_asks`, `activity`, `waiting_on`, `last_rejection` |
| `leticode_events` | `{session, since, limit}` - the progress since the cursor: calls, jobs, failures, asks, turns, **subagents**, and a `provider` entry when the provider itself was stuck |
| `leticode_transcript` | `{session, since, limit, reasoning}` - the CONVERSATION, row by row, up to the stated `cap` (B8) |
| `leticode_mode` | `{session, mode}` - a point that needs nobody applies at once; `allow-all` with no boundary asks the operator (B8) |
| `leticode_model` | `{session, model}` - moves a RUNNING session to another model; the daemon's `/models` verb, so a switch that arrives mid-turn takes when the turn ends |
| `leticode_asks` | `{session}` - the pending asks: id, the call, the options offered |
| `leticode_answer` | `{session, ask, option, note}` - sent as the protocol's `Answer`, so it lands in the decision corpus |
| `leticode_diff` | `{session, full}` - the branch against its merge base: `git log --oneline`, `git diff --stat`, and the diff when `full` |
| `leticode_stop` | `{session}` - interrupts a running turn, closes the session here and detaches; a placement settles as it closes (`branch` names the branch the work comes home on), and the rows stay on disk |
| `leticode_collect` | `{session, host, folder, vm}` - fetches what a session that is already stopped committed on `host` into `remote/<host>/<short id>`, in `folder` or this process's own directory. A stop that never reached the close - `letibot --stop`, a daemon that died - leaves the work on that machine, and this is the way back that needs nothing of the daemon |
| `leticode_list` | - the sessions this bridge holds, with `placement`, watched or driven |
| `rooms`, `room_read`, `room_say`, `room_dm`, `room_roster`, `room_inbox` | see `docs/room.md` § *Claude Code*: Claude Code as the external seat `claude-code` in the host's chat rooms |

### Rooms: the stream and `room-wait`

The bridge declares the `claude/channel` experimental capability and pushes each room message for
`claude-code` as `notifications/claude/channel`. `letibot-mcp room-wait [--attention to_me] [--timeout
2h]` is the same wake as a background command (exit 0 with the message, 20 coordinator unreachable, 30
timeout). Both are in `docs/room.md` § *Claude Code*.

### `letibot-mcp wait`: the wake-up

`letibot-mcp wait <worktree> <session> --until attention [--timeout 2h]` connects as a head and
**exits when the session needs Claude**: turn ended (0, the last assistant message on stdout), an
ask pending (10, the ask on stdout), failed (20), timeout (30), **the provider itself is in trouble
(40, the event on stdout)** - a rate limit (with the reset instant when the provider gave one), an
exhausted account, a model API timeout, or a round that stalled past the first-byte threshold. Claude
runs it after every `leticode_start` and `leticode_send`; its exit is the notification.

**"Its turn ended" means the DAEMON says the session is quiet** - `PROTOCOL_VERSION` 52's
`Activity`: no prompt in flight (**the whole prompt, not one of its rounds**), no settlement owed to
the model, no job or child whose ending will wake it, no monitor armed, no card open. Exit 10 is
`Activity::ask()`, the one entry that does not end without a person. `TurnStarted`/`TurnFinished` are
published once per ROUND, which is why a turn's tool calls read as *idle*; the daemon holds the jobs,
the calls, the monitors, the children and the cards, so the daemon answers.

The exception is the job the model PARKED (`bash {background: true, wake: "later"}`): `wait` does
not hold on it, and that is the whole of the `wake` argument - absent, the session's own end wakes
it; `now`, the completion comes the moment it ends; `later`, the model is not waiting.
**A daemon that never started is exit 20, not an hour of waiting.** A daemon whose placement fails
publishes a `daemon_start_failed` warning (`Class::Failure`) on the session's hub before its hubs
close (`cli::Boot::failed`) and shuts down, and the bridge's head is already attached. It sets
`State::boot_failed`, and `wait` answers `Attention::Failed` with the daemon's own sentence: exit 20,
deliberately NOT subject to the *a failure that was already there does not count* rule, which is
about a previous TURN's death. A connection the daemon closed (`Bye`) ends the wait for the same
reason, and `leticode_status` says `failed` rather than `idle`.

### The channel's events, and the cursor a client resumes from

A held session's events arrive as `notifications/claude/channel` with `meta.source: leticode` and
`meta.event` naming the fact. Four are a turn's: `turn_ended`, `ask`, `failed`, `trouble`. Two are
not, and were the ones an orchestrator had no news of at all: **`idle_with_work`** - the session is
between turns with `meta.jobs` running or `meta.rows` todo rows pending, said once per change to
that work and not once per poll - and **`job_ended`** - `meta.job` and `meta.state` as the daemon
settled it, pushed only for a job the session is PARKED on (the daemon's `waiting_on`, no turn
running), because a session that is working reads its own job results and one `job_ended` per job
of every held package was R172's noise. A session the bridge itself stopped with `leticode_stop`
pushed no `failed`: the `Bye` behind that close is the bridge's own act, not a failure of the
session's.

Each session's events are kept in `<rundir>/<folder key>.push-<session>`: the key of every fact
already said, the frames a client has not taken, and the `cursor` it has been told up to. An event
that happens while nobody is reading - a client between connections, a bridge restarted - is pushed
once when a client comes back, and nothing already pushed is pushed again, including by a bridge
restarted on the same folder.

### Claude Code as the approver, and the report as data

With the bridge attached the session no longer needs allow-all: `writes-allowed` (approve a class
once) puts the routine through and sends what the gate asks about - a push, network, a write outside
the worktree, the always-ask list - to the attached head, which Claude reads with `leticode_asks` and
answers with `leticode_answer`. The brief's closing report is asked for in a fixed markdown shape and
parsed into fields by `leticode_status`. `/delegate` packages the workflow as a skill, and an
`escalate` tool would later let a session post a design question it should not decide alone.

## The queue, from the orchestrator's side (B5)

- **A package's worker never touches the queue** (Q1): its job ends at a commit on its branch.
- **The orchestrator drives the queue through the bridge**, so landing does not re-run on the
  orchestrator's box the gate the queue already owns, nor bypass its history. With B5 the bridge
  holds a head on the repository's queue and Claude does what the operator does with `/queue`,
  recorded under `bridge:<identity>`: `leticode_queue`, `leticode_submit` (`{row}`),
  `leticode_queue_verb` (the verbs `mergequeue::verbs` already has), and `letibot-mcp wait-queue
  <repository> <row>` exiting on land (0), rejection or veto (10, the reviewers' words), gate
  failure (20), or timeout (30).

**Not built, and not before the queue is stable**: B5 makes the queue the landing path for every
package, so it waits on a stability pass over the queue itself.

## Watching a session the bridge did not start (B6)

The review queue's own session lives in each folder's daemon and starts a reviewer subagent per
claimed row (`s-gatekeeper-<key>-sub-*`). `leticode_attach` is the same head `leticode_start` keeps,
on a session somebody else opened:

- **It joins a running daemon and never starts one.** Nothing listening on that folder's socket is a
  refusal naming the socket, not a start - worst in the folder an attach is usually pointed at.
- **`watch_only` (the default) is a real cap, not a promise.** A watching head never sends a prompt,
  never answers a card, never interrupts; `leticode_send`, `leticode_answer` and `leticode_stop`
  refuse on it.
- **`session: "gatekeeper"`** is THIS folder's review-queue session, **one per daemon**: the id is
  `s-gatekeeper-<workspace key>`, derived by `letibot_sessionlog::gatekeeper_session` from the same
  key the launcher names the folder's socket with, because every daemon on the box shares one store
  and two of them appending to one transcript is silent corruption.

**A watcher must not count as somebody who can answer.** Attaching a second head steals nothing from
the operator's screen - the daemon publishes a card to every attached head and settles it with
whichever answers first - but it *can* be counted: `Answers::ask` and `ask_question` read
`Hub::deciding_heads` **before** posting anything and answer `gate:no-head` in microseconds when it
is empty, because posting to a head that will never answer and sitting out the five-minute deadline
is *"a real answer given for a fake reason"*. So a watching head declares `Caps { can_decide: false
}`, and the daemon refuses any `Answer` from it.
**The reviewers are child sessions, and one line each is what the parent's hub carries.**
`SessionEvent::Subagent` becomes a `subagent` entry in `leticode_events` - the slug, the branch its
task named (*"- branch: agent/P1"*, the only place a child's branch appears on the wire) and its
first line as the verdict. A head that attaches mid-review reads the running ones from the
snapshot's `subagents`; **following one reviewer's turns needs a second `leticode_attach` on its
session id.**

## `leticode_transcript`: the conversation, not the store

`leticode_events` is a progress ring whose texts are cut to one line, and a pane that follows a
session read-only needs the conversation. Reading the store directly is the operator's ruling
against (*"not a good idea"*): `sessions.db` is one file every daemon on the box shares, and a row
there is the STORE's shape. So the tool asks the daemon what a head would be handed -
`ClientFrame::Peek { shape: Rows }`, whose answer is the session's own snapshot - and **no file is
opened and no second reader exists**; a session this bridge only WATCHES answers the same read.

One word per item kind - `user`, `assistant`, `tool_result`, `system`, `reasoning`, `segment_mark` -
plus three facts a reader cannot derive: **`i`**, the row's position in the SESSION, with `next` as
the cursor, counted against the snapshot's `items_dropped` (a snapshot is the TAIL of a
conversation, so a cursor of window positions would re-read or skip when the window moved);
**`cap`**, stated in every answer with `cut` on any string cut at it, 8 000 CHARACTERS and not
bytes; and **`content: null`** for a row the daemon announced and has not reconciled, which is not
the same statement as an empty message. A tool result carries its `outcome` (`ok`, `failed`,
`denied`, `not_run`, `backgrounded` …) and the reason for `failed`/`not_run`; a user row carries
`speaker` (`operator`/`agent`) and names its `attachments` by type, never their bytes. A read for a
session the daemon does not hold is answered `Rejected`, keyed `peek:<session>` because a `peek` is
the one frame whose answer carries no client id.

## `leticode_mode`: the bridge asks, a person answers

A bridge session's point is fixed at `leticode_start`, and the bridge sends `consented: false` there
- so `allow-all` on an unconfined session could only be reached by a person typing `/mode allow-all`
into a head. That is the right rule and a hole in the workflow. **`Mode { name, consented }` could
not carry this**: it is a head saying *set it, and I confirm it*, and a head that is a program is
exactly what must not confirm. So the ask and the set are two frames, added in `PROTOCOL_VERSION`
54:

| frame | direction | meaning |
|---|---|---|
| `ClientFrame::ModeConfirm { name }` | head → daemon | *put the confirmation for this point in front of a person* - it consents to nothing |
| `ServerFrame::ModeConfirm { req_id, name, asked_by }` | daemon → the OTHER heads | the card, on every head that declared `can_decide` except the one that asked |
| `ClientFrame::ModeConsent { req_id, yes }` | head → daemon | a person's answer, which settles the request it names |
| `ServerFrame::ModeSettled { req_id, client_request_id, name, applied, answer, said }` | daemon → every head | how it ended, and every other head's card coming down |

**The rule is structural, not a promise.** The daemon pushes the card to every attached head with
`Caps::can_decide` **except the asking head** (`Hub::deciding_heads_except`,
`Hub::push_frame_where`), and `Harness::answer_mode_confirm` refuses a consent from any head that was
not asked. The bridge has no code path that sends `consented: true` and none that sends
`ModeConsent`; its half is `Head::ask_mode`, whose whole vocabulary is the point's name.

Three ends: **the point needs nobody** (`writes-allowed`, `read-only`, `allow-all` inside a VM)
applies at once and settles `answer: nobody, applied: true`, because whether a point needs a person
is the DAEMON's fact (`Harness::mode_needs_consent`); **nobody can be asked** - the session's only
decidable head is the one that asked - is a `Rejected` on the asker's stream naming `leticode
--session <id>`, and nothing changes; **a person answers**, and `yes` applies the point through the
same `Harness::set_mode_consented` the operator's `/mode` reaches, while `no` applies nothing and
says so on the settle frame. The card is the same card `/mode allow-all` raises (`App::mode_confirm`),
answered by `Action::ModeConsent` carrying the request's id.

**Nothing is written down for the point.** The consented `allow-all` lives for this session, as
`set_mode_consented` has always said (*"nothing was written down, and a daemon restart drops it"*);
a request a program made and a person agreed to is not the project's standing point, and `/mode
allow-all` typed at a head still writes the row. **One open request per session, and a new ask
supersedes it**, settled `answer: nobody, applied: false` with the reason so the old card comes down
first. **A caller reads back the point IN FORCE**: a consented `allow-all` on a bare host lands on
`allow-all (this box, consented)`.

## Reading a Claude Code conversation

`crates/claude` reads a Claude Code transcript into this head's rows, one JSONL line at a time
(`letibot --session cc-<uuid>`), rendering nothing and publishing nothing - no credentials, no
subagent directories, no writes - so its rules are testable in isolation. Four things the format
makes non-obvious:

- **Most of the file is not conversation.** Fifteen of its seventeen line types are never a row, so
  a notice is said once per TYPE with a count; one scrap per unknown line buries the transcript.
  `aiTitle` is the title.
- **A message is one block per line, and nothing repeats** - no repeated `text` block and no repeated
  `tool_use` id - so an importer must not dedupe by `message.id`.
- **The speaker is DECLARED**: `origin.kind` and `promptSource`, with `isMeta: true` winning over
  both and absent meaning the operator. A subagent is a FILE - `<uuid>/subagents/agent-*.jsonl`,
  its `.meta.json` naming the `toolUseId` of the `Task` call that spawned it - not inline
  `isSidechain` lines.
- **`cost-state` is cumulative**: take the last, never the sum.

Three parser traps, each of which attributes machinery to the operator without saying so: `<` is a
byte, so a frame parser that strips it and then reports offsets into the remainder slices one short
and hands the whole frame back as their words; a "first line found" flag set before the check
reports "(no operator line)" for a transcript whose first `user` line is machinery; and a
`<pasted_content>` frame's closing tag carries its id, so matching `</tag>` exactly misses every
paste.

## What is measured

`crates/bridge/tests/transcript.rs` and `crates/bridge/tests/mode_consent.rs` drive the daemon's own
`Daemon::serve` over a real store, with only the MODEL a fake on loopback, and read every *did it
change* back through a second, watching head (`can_decide: false`); `daemon_record.rs` runs
`scripts/letibot --daemons` over a bridge-started fake `harnessd`. The TUI's half is
`a_confirmation_another_head_asked_for_is_the_same_card_answered_by_its_id`
(`crates/tui/src/app/tests/settings.rs`). The two suites that seat `--bash` need a delegated cgroup
v2 subtree.

## Packages

| id | builds |
|---|---|
| B1 | `crates/bridge`: the MCP server and the `wait` verb; tests against a daemon on a temp workspace with a fake provider |
| B2 | `leticode_events` with the compact progress and the cursor; the report parsed into fields in `status` |
| B3 | the `/delegate` skill, and registering the server with Claude Code (`claude mcp add`) |
| B4 | `escalate` |
| Q1 | a bridge-started daemon runs `--no-queue` |
| B5 | `leticode_queue`, `leticode_submit`, `leticode_queue_verb`, `wait-queue` (needs a stable queue) |
| B6 | `leticode_attach` with `watch_only` (`can_decide: false`); the subagents a session spawns in `leticode_events` |
| B8 | `leticode_transcript` and `leticode_mode`; protocol 54's four mode-confirmation frames |
