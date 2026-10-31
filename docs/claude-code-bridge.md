# Claude Code ↔ leticode: the bridge

Written 2026-10-25. The operator: *"can we have a better integration between claude code and
leticode? not just a simple shell run"*.

## Why the shell run is not enough

Claude Code delegates well-scoped work to leticode to save Claude usage (leticode turns go to
DeepSeek at a few cents a package). Until now that was `cd <worktree> && leticode --mode allow-all
"<brief>" > log` in the background, and the first day of it showed three defects of the shape, not
of any one run:

1. **A one-shot run dies with its turn.** A model that backgrounds a long test run and ends its turn
   to wait for it exits the process; the job's completion never reaches it (plugins slice 1, W3).
2. **Permission is all or nothing.** One-shot runs cannot ask anybody, so the only working mode is
   allow-all, which needs the operator's recorded consent per project and enforces nothing the
   brief asks (no push, `--offline`, writes only in the worktree).
3. **Following up means restarting.** A review comment goes back as `leticode --continue "<text>"`,
   which re-runs the daemon start-up, the banner and the resume each time; and the result comes back
   as a 10,000-line log of banner and memwatch lines with the report at the end.

## The shape

**leticode sessions become things Claude Code holds, not processes it runs once.** The daemon
already keeps sessions alive across heads, and its protocol (`crates/sessionlog/src/protocol.rs`)
already has everything a client needs: `NewSession`, `Prompt`, `Answer` (a permission ask's
answer), `Interrupt`, `Detach`, the listings. So the bridge is **a head that is a program**: no
daemon change for its first slice.

```
Claude Code ──stdio MCP── letibot-mcp ──unix socket── harnessd (one per worktree folder)
     ▲                         │
     └── background `letibot-mcp wait …` exits when a session needs attention
```

### The daemon a package gets serves no queue

Added 2026-10-26, after the first day of packages: a worktree of this repository IS one of its
session daemons by the merge queue's own rule (`mergequeue::serves` — the checkout, under it, or
one of its worktrees), so the daemon the bridge started for package P1 took letibot's merge-queue
lease, found `main` red, and put an `EMERGENCY` in front of the package's model **before its
brief**. A package worker is not the repository's queue holder — the operator's own daemon is —
and two daemons interleaving landings for one repository is the second half of that defect.

So `daemon_args` passes **`--no-queue`** on every start: no lease is claimed, no target gated, no
branch landed, and none of the queue's model-facing notices (`queue_emergency_notices`,
`queue_notices`, `handover_notices`) is composed for a session of that daemon. The store watcher
still runs, so a head attached here redraws when another daemon moves a row. A daemon that is
already up is not the bridge's to change: `ensure` attaches to it as it is.

### The record a started daemon writes, and the launcher's four readers (R102)

`scripts/letibot` writes `<rundir>/<key>.json` beside the socket when it starts a daemon
(`record_write`), and that one file is what `--daemons` and `--ls` glob, what `--stop N` and
`--stop --all` resolve a handle through, and what the attach banner prints the model out of. A
daemon the bridge started wrote none — MEASURED 2026-10-27 on the operator's Mac: about forty
`harnessd` processes, every one started by a `letibot-mcp` bridge, and `letibot --daemons`
answered **no daemons recorded**.

`daemon::ensure` now writes the same record, field for field (`workspace`, `socket`, `pid`,
`role`, `model`, `answers`, `dialect`, `bash`, `started`), through a temp name and a rename so a
launcher reading the glob at the same moment sees one file or the other and never half of
either. It is written **after** the socket answers and not before, so a start that fails leaves
no record claiming a daemon that is not there; a record that cannot be written is said on
stderr and is not fatal, because the daemon is up and what is lost is a listing line.

**Why the record and not a listing that reads the sockets it can see**: the socket is
`<sha256(workspace)[..12]>.sock`, so a socket with no record names a hash and nothing else — not
the folder, not the role, not what answers its turns. A `--daemons` over sockets would print
forty lines of hex, and `--stop N` needs the pid and the folder the record holds.

`crates/bridge/tests/daemon_record.rs` drives the bridge's own start path against a fake
`harnessd` that binds the socket it is handed, then runs the operator's real
`scripts/letibot --daemons` over the result and asserts the folder is listed as `up (pid N)`.
Pre-fix it fails on the missing file; the fix's absence is the whole of what it measures.

### A package whose tools run on another machine (RH2)

Written 2026-10-26. `docs/remote-hosts.md` places a session's **tools** on an ssh host the
operator declared: `bash`, `read`, `write`, `edit` and the jobs run there, while the model, the
store and the gate stay here. For a person that is `leticode --host lab2x1`; for a Claude Code
package it is `leticode_start { host: "lab2x1" }`, and what the bridge does with it is start (or
join) that folder's daemon with `--host lab2x1`. **The bridge never speaks ssh** — `harnessd` owns
the placement and the bridge only names it.

Three rules, and each one is a refusal rather than a best effort:

* **The name is checked before anything is started.** The tree's single `hosts.toml` reader
  (`letibot_tools::remote::Hosts`) and the same refusal `task { where: "host:NAME" }` gives — the
  file, and the names it does declare. A daemon started for an undeclared name would fail at its
  own start, in a log the caller has to go and read, *after* this tool had answered that it
  started one.
* **A daemon already up for that folder is joined only where the call asked to be** — the same
  placement, and no other, including *this box*. The daemon writes where it puts the tools beside
  its socket (`<key>.placement`; `letibot_sessionlog::placement`), and the record names the
  socket's own `(dev, ino)`, so a record left by a daemon that is gone reads as *nothing known*
  rather than as the new daemon's answer. `ensure` reads it before joining and refuses a mismatch
  naming the running daemon's placement and the command that ends it (`letibot --stop`): stopping
  somebody else's daemon — the operator's own, usually — is the operator's act, not a tool's.
  **Nothing recorded is refused in both directions**, because `None` is *unknown* and not *local*.
* **Every answer says `placement`** — on `leticode_start`, `leticode_status`, `leticode_list` and
  `leticode_attach`: `host` for this box, `host:lab2x1` for a machine, `null` when the daemon
  recorded nothing this build can read. `leticode_start` also answers `tools_run`, the same fact
  in the sentence a model reads, and that sentence carries what did **not** move — the model, the
  store and the gate — because a result from a remote session is read here and means something
  different when the toolchains were somebody else's.

**The one sharp edge is a daemon from an older build.** It recorded no placement, so this build
cannot place it, and `leticode_start` refuses it with the file it looked for rather than assuming
*local*. `letibot --stop` and the next call is its own: the daemon it starts carries the placement
and writes the record.

### `letibot-mcp`: an MCP server over stdio

A new binary (crate `crates/bridge`, package `letibot-bridge`, bin `letibot-mcp`). MCP over stdio is
JSON-RPC 2.0 with `initialize`, `tools/list` and `tools/call`; it is written with `serde_json` and
the sessionlog client, no MCP library. The process lives as long as the Claude Code session, and
holds one head connection per leticode session it started or attached to, so asks reach it.

| tool | input | answers |
|---|---|---|
| `leticode_start` | `worktree` (absolute path), `brief`, `model` (e.g. `deepseek`), `mode` (default `writes-allowed`), `host` (a machine from `hosts.toml`; omitted = this box) | `{session, placement, tools_run}` — starts the folder's daemon if needed (as the `letibot` launcher does), opens a session, sends the brief as the first prompt, stays attached; with `host` the daemon is started `--host NAME` and the session's tools run there |
| `leticode_attach` | `worktree` (a folder whose daemon holds the session), `session` (an id, or `gatekeeper`), `watch_only` (default `true`) | as `leticode_start` minus the brief — **joins** a running daemon (never starts one), attaches a head, answers `{session, daemon_started: false, watch_only, can_decide, placement, state, pending_asks, last_message}` |
| `leticode_send` | `session`, `text` | `{queued: true}` — an operator line into the running session (queued behind a running turn, as the head's own) |
| `leticode_status` | `session` | `{state: running\|idle\|asking\|failed, placement, turn, todos, last_message, report?, cost_usd, tokens, pending_asks, activity, waiting_on}` — `activity` is what the daemon says the session is waiting on (`quiet`, or `busy` with the entries tagged by kind) and `waiting_on` is the same list in a reader's words (`["job j8032", "monitor ci"]`); `placement` is `host`, or `host:NAME` when its tools run on another machine |
| `leticode_events` | `session`, `since` (cursor) | the compact progress since the cursor: tool calls (name + one line), job starts and ends, failures, asks, the **subagents a session spawns** (slug, the branch their task named, how they ended) — never banner or memwatch lines — and the next cursor |
| `leticode_transcript` | `session`, `since` (cursor), `limit` (default 100), `reasoning` (default false) | the session's CONVERSATION, row by row, as the daemon serves it to a head — prompts, answers, tool calls (name + arguments) and tool results (outcome + payload), full text up to the `cap` the answer states, plus `next` as the cursor (B8) |
| `leticode_mode` | `session`, `mode` | `{mode, applied, answer, said}` — a point that needs nobody is applied at once (`answer: nobody`); `allow-all` on a box with no boundary is put in front of the operator as the card a terminal shows for `/mode allow-all`, and applies only on their `yes` (B8) |
| `leticode_asks` | `session` | the pending permission asks: id, the call (tool, arguments), the options the gate offers |
| `leticode_answer` | `session`, `ask`, `option` (one the ask offered), `note` | `{answered: true}` — sent as the protocol's `Answer`, so it lands in the decision corpus like the operator's own |
| `leticode_diff` | `session` | the worktree's branch against its merge base: `git log --oneline` and `git diff --stat`, plus the full diff when asked |
| `leticode_stop` | `session` | interrupts a running turn and detaches; the session stays on disk |
| `leticode_list` | — | the sessions this bridge holds, with state, `placement` (where each one's tools run) and whether each is watched or driven |

### `letibot-mcp wait`: the wake-up

Claude Code wakes its model when a background command exits, so the bridge binary also has a CLI
verb: `letibot-mcp wait <worktree> <session> --until attention [--timeout 2h]` connects to the
folder's daemon as a head and **exits when the session needs Claude**: its turn ended (exit 0, the
last assistant message on stdout), an ask is pending (exit 10, the ask on stdout), it failed (exit
20), or the timeout passed (exit 30). Claude runs it as a background command after every
`leticode_start` and `leticode_send`; its exit is the notification.

**"Its turn ended" means the DAEMON says the session is quiet** — `PROTOCOL_VERSION` 52's
`Activity`: no prompt in flight (**the whole prompt, not one of its rounds**), no settlement owed to
the model, no job or child whose ending will wake it, no monitor armed, no card open. Exit 10 is
`Activity::ask()` — the one entry of that list that does not end without a person.

**This used to be inferred from outside, and every inference was a timer.** The engine publishes
`TurnStarted`/`TurnFinished` once per ROUND, so a turn's tool calls and the boundary between two
rounds read as *idle*; the bridge answered that with a ten-second settle window, then with a
`wake_due` flag raised when a `JobSettled` an unfinished todo row needed, and QA found the next
hole inside a day (a turn that ended *waiting on the last probe target*). Each guess fixed one
false wake and left the next, and the operator's question is the whole argument for the event:
*"it is easy to see if any jobs are running, etc etc, why timeouts?"* — the daemon holds the jobs,
the calls, the monitors, the children and the cards, so the daemon answers.

A job counts when its settlement is one the model is **woken** by (R79's `delivery::decide`: an
open row needs it or has a `when:` condition on it, a wait asked for it, `wake: "now"`), not every
open handle — a dev server left running would otherwise hold every `wait` for its whole life. What
keeps a merely-held settlement from slipping through is `Delivery`: the moment the result is queued
the session is owed a turn, and that is the window a `wait` used to exit in.

### Claude Code as the approver

With the bridge attached, the session no longer needs allow-all: `writes-allowed` (approve a class
once) puts the routine through and sends what the gate asks about — a push, network, a write
outside the worktree, the always-ask list — to the attached head, which is the bridge. Claude reads
it with `leticode_asks` and answers with `leticode_answer`. The corpus records each answer with who
gave it. A session whose head is the bridge needs no project consent, because a reviewer is in the
loop; one that is left with no head attached refuses gated calls as any session does.

### The report as data

The brief's closing report (built, tested, could not do, contract changes, commits) is asked for in
a fixed markdown shape, and `leticode_status` parses it into fields when the last assistant message
has it, so the review starts from data.

### `/delegate`: the skill

A Claude Code skill that packages the workflow, so any Claude session delegates the same way:
contracts first and written into the repository; one worktree per package; the brief template with
the hard rules; `leticode_start` + background `wait`; review with `leticode_diff` and the report;
follow up with `leticode_send`; merge only after re-running the checks.

### Later: the other direction

A leticode session escalating a design question it should not decide alone: an `escalate` tool
that posts to the bridge, surfacing in `leticode_status` as `asking`, answered with
`leticode_send`.

### The queue, from the orchestrator's side (B5)

Written 2026-10-26. The operator: *"[workers] shouldnt bother [with the queue] — i dont think they
should communicate with queue at all, unless you want to add queue support to mcp. that would be
cool"*.

Two rules, one per side:

* **A package's worker never touches the queue.** A daemon the bridge starts runs `--no-queue`
  (Q1): it takes no lease, and its sessions get no model-facing queue notice. A worker's job ends at
  a commit on its branch; whether that branch lands is not its question.
* **The orchestrator drives the queue through the bridge.** Today Claude Code lands a package by
  hand — rebase, the whole workspace's tests, a fast-forward push — which re-runs on the
  orchestrator's box the gate the queue already owns, and bypasses the queue's history. With B5 the
  bridge holds a head on the repository's queue and Claude does what the operator does with
  `/queue`, recorded under the bridge's name (`bridge:<identity>`), so the history says who:

| tool | input | answers |
|---|---|---|
| `leticode_queue` | `repository` (a checkout or worktree path) | both queues as `ServerFrame::Queues` has them — rows, states, verdicts, the target's health — folded to one line a row |
| `leticode_submit` | `worktree`, `branch` (default: the worktree's) | puts the branch on the review queue as the operator would; `{row}` |
| `leticode_queue_verb` | `row`, `verb` (`approve`, `restart`, `veto`, `rm`, `regate`), `note` | the verbs `mergequeue::verbs` already has, as the operator's — approve still runs the gate (decision 5) |

and a CLI verb for the wake-up, like `wait`: `letibot-mcp wait-queue <repository> <row> [--timeout]`
exits when the row lands (0), is rejected or vetoed (10, the reviewers' words on stdout), its gate
fails (20), or the timeout passes (30).

**Not before the queue is stable.** The operator, same day: *"if things are stable enough i would
even let it run unattended and autostart. totally depends on the stability. right now it is flaky
and fragile"*. B5 makes the queue the landing path for every package, so it waits on a stability
pass over the queue itself; until then landing stays manual and the merge queue's own home (which
daemon serves it) stays undecided.

### Watching a session the bridge did not start (B6)

Written 2026-10-26. The operator: *"the operator cannot see who in the merge queue is doing
what"*. The review queue's own session lives in each folder's daemon and starts a reviewer
subagent per claimed row (`s-gatekeeper-<key>-sub-*`). Until B6 the only way to follow it from
Claude Code was to read the store and guess.

`leticode_attach` is the same head `leticode_start` keeps, on a session somebody else opened:

* **It joins a running daemon and never starts one.** Nothing listening on that folder's socket is
  a refusal naming the socket, not a start — the defect Q1 fixed in the other direction, and worst
  in the folder an attach is usually pointed at, which is the operator's own.
* **`watch_only` (the default) is a real cap, not a promise.** A watching head never sends a
  prompt, never answers a card and never interrupts; `leticode_send`, `leticode_answer` and
  `leticode_stop` refuse on it, saying it is watched and not held. `leticode_start`'s own session,
  and an attach with `watch_only: false`, are heads that hold their session and act on it.
* **`session: "gatekeeper"`** is THIS folder's review-queue session. **One per daemon**: the id is
  `s-gatekeeper-<workspace key>`, derived by `letibot_sessionlog::gatekeeper_session` from the same
  key the launcher names the folder's socket with — because every daemon on the box shares one store,
  and two of them appending to one transcript is silent corruption (MEASURED 2026-10-26,
  `s-gatekeeper#t0`: `transcript_item seq must be the next one`). So the name resolves through the
  worktree the attach is joining: `leticode_attach {worktree, session: "gatekeeper"}` lands on the
  daemon it found by that folder's key, on that daemon's queue. `harnessd`'s
  `sessions::gatekeeper::session` and the bridge's `gatekeeper_session_for_key` are one function's
  two callers, so a head says *which session* without linking the daemon.

**What a watcher must NOT do is count as somebody who can answer.** Attaching a second head steals
nothing from the operator's screen: the daemon publishes a card to every head attached to the
session (`pose` → `Hub::publish`) and settles it with whichever answers first, so the operator's
TUI still shows it and still wins the race. What a head *can* do is be counted: `Answers::ask` and
`ask_question` read `Hub::deciding_heads` **before** posting anything and answer `gate:no-head` in
microseconds when it is empty, because posting to a head that will never answer and sitting out the
five-minute deadline is *"a real answer given for a fake reason"*. So a watching head declares
`Caps { can_decide: false }`, and the daemon then refuses any `Answer` from it with *"this head
declared can_decide: false"* — the second belt. Measured in `crates/bridge/tests/end_to_end.rs`:
two heads attached to one session, one of them a watcher, and `deciding_heads()` still holds exactly
the one that holds it; the review queue's session, whose only head is the watcher, has none.

**The `wait` verb attaches a watcher too**, and that is a fix rather than a detail: a `wait` is a
process that never answers anything, and declaring it able meant a session whose only head was a
`wait` would post a card, be watched exit `10` with it, and then sit on it until the deadline.

**The reviewers are child sessions, and one line each is what the parent's hub carries.**
`SessionEvent::Subagent` (`opening`/`running` → *started*, `done`/`failed` → *ended*/*failed*) is
folded into `leticode_events` as a `subagent` entry: the reviewer's slug, the branch its task named
(*"- branch: agent/P1"* — the task is the only place a child's branch appears on the wire), and on
the finish its own first line as the verdict. A head that attaches in the middle of a review reads
the ones still running from the snapshot's `subagents`, so the flow *attach while the queue is
busy* works; a child that finished before the attach is history and is not announced as news.
**Following one reviewer's own turns needs a second `leticode_attach` on that child's session id** —
a child's turns are on its own hub, and the parent is told only that it started and ended.

## As built (B8)

Written 2026-10-26. Two tools, and the second one needed a protocol.

### `leticode_transcript`: the conversation, not the store

The operator's Claude Code pane follows a package's session read-only and tailed, and until B8 it
did that through `leticode_events` — a progress ring whose texts are cut to one line. Reading the
session store directly is their ruling against: *"not a good idea"*. Right for two reasons that
are not taste. `sessions.db` is one file every daemon on the box shares, so a reader there sees
sessions it was never told about; and a row there is the STORE's shape (tokens, a hash chain,
`item_json`), where what a reader wants is the item.

So the tool asks the daemon what a head would be handed: `ClientFrame::Peek { shape: Rows }`, whose
answer is the session's own snapshot — *"the same thing an attach answers with"* — and renders each
`SnapshotItem` as a row of its own. **No file is opened and no second reader exists**; a session
this bridge only WATCHES answers the same read, because reading is not acting on somebody's session.

The row vocabulary is one word per item kind — `user`, `assistant`, `tool_result`, `system`,
`reasoning`, `segment_mark` — plus three facts a reader cannot derive:

* **`i`, the row's position in the SESSION**, and `next`, the cursor to ask from. The arithmetic is
  against the snapshot's `items_dropped`, because a snapshot is the TAIL of a conversation: a
  cursor counting window positions would silently re-read or skip every time the window moved. A
  cursor older than the window gets the rows that remain and a `dropped` count — the rule
  `leticode_events` already follows for its ring.
* **`cap`, stated in every answer**, and `cut` on any string that was cut at it: 8 000 CHARACTERS
  (not bytes — a reader counting what it got and a cap counting bytes disagree the first time a row
  carries a glyph). Generous on purpose: this is a reader's view, not a model's context, and the
  whole point is that an orchestrator can read a real tool result rather than its first line.
* **`content: null` for a row the daemon announced and has not reconciled** — *there is a row here
  and this build cannot show you what is in it*, which is a different statement from an empty
  message and is drawn as one.

A tool result carries its `outcome` in the same words `leticode_events` files a call under
(`ok`, `failed`, `denied`, `not_run`, `backgrounded` …), and for `failed`/`not_run` the reason;
`backgrounded` also carries its job handle and the call that gets its output. A user row carries
`speaker` (`operator`/`agent`), so a reader can tell the person's words from a notice the harness
wrote — and `attachments` names the pictures and files by type, never their bytes: an image's
`data_ref` is a data URI of the whole file, and a reader that asked for the conversation did not
ask for that. `reasoning` is left out unless the caller asks for it (default false): it is the one
part of a transcript nothing downstream reads, and a tail of `+ 0 + 0` costs a reader more than it
buys.

If a head asks for a session the daemon does not hold, the read is answered with `Rejected` — and
the bridge turns that into an error naming the session rather than waiting out its patience: a
`peek` is the one frame whose answer carries no client id, so the refusal is keyed by
`peek:<session>` and folded for exactly this waiter.

### `leticode_mode`: the bridge asks, a person answers

A bridge session's point was fixed at `leticode_start`, and the bridge sends `consented: false`
there — so `allow-all` on an unconfined session could only be reached by a person typing
`/mode allow-all` into a head attached to it. That is the right rule (a person's answer about their
own box is not a program's to give) and a hole in the workflow: a package that needs the point has
no way to ask for it.

**`Mode { name, consented }` could not carry this.** It is a head saying *set it, and I confirm
it*, and a head that is a program is exactly what must not confirm. So the ask and the set are two
frames, in `PROTOCOL_VERSION` 54:

| frame | direction | meaning |
|---|---|---|
| `ClientFrame::ModeConfirm { name }` | head → daemon | *put the confirmation for this point in front of a person* — it consents to nothing |
| `ServerFrame::ModeConfirm { req_id, name, asked_by }` | daemon → the OTHER heads | the card, on every head that declared `can_decide` except the one that asked |
| `ClientFrame::ModeConsent { req_id, yes }` | head → daemon | a person's answer, which settles the request it names |
| `ServerFrame::ModeSettled { req_id, client_request_id, name, applied, answer, said }` | daemon → every head | how it ended: the asker's answer, and every other head's card coming down |

**The rule is structural, not a promise.** The daemon pushes the card to every attached head with
`Caps::can_decide` **except the asking head** (`Hub::deciding_heads_except`, `Hub::push_frame_where`),
and `Harness::answer_mode_confirm` refuses a consent from any head that was not asked. So a program
asks, a person answers, and one head cannot do both for one request. The bridge has no code path
that sends `consented: true` and none that sends `ModeConsent`; its half is `Head::ask_mode`, whose
whole vocabulary is the point's name.

**Three ends, and the caller is told which:**

* **the point needs nobody** — `writes-allowed`, `read-only`, `allow-all` inside a VM — is applied
  at once and settled `answer: nobody, applied: true`. Whether a point needs a person is the
  DAEMON's fact (`Harness::mode_needs_consent`: `allow-all` on a session with no `Confinement`),
  not a table in the bridge, so the two cannot disagree about which points are questions.
* **nobody can be asked** — the session's only decidable head is the one that asked — is a
  `Rejected` written on the asker's own stream: *”`allow-all` on this session needs the operator's
  confirmation in a head … run `leticode --session <id>`”*. Nothing is changed, and the sentence
  names the command that opens the head.
* **a person answers.** `yes` applies the point through the same `Harness::set_mode_consented` the
  operator's `/mode` reaches — one path into that state, two frames arriving at it — and `no`
  applies nothing and says so on the settle frame.

**The card on the person's screen is the same card `/mode allow-all` raises** (`App::mode_confirm`),
with one line in front of it naming the head that asked, and it is answered by `Action::ModeConsent`
— a frame carrying the request's id — because the daemon has to settle the one request it raised.
A settlement takes the card down whoever answered it: another head may have said yes first, and a
card left up would own the keyboard for a request nobody can settle. A `ModeConfirm` for a session
this head is not in is ignored, and so is a settlement for a request it is not holding.

**Nothing is written down for the point.** The consented `allow-all` lives for this session, as
`set_mode_consented` has always said (`It lasts for this session only: nothing was written down,
and a daemon restart drops it`), and B8 does not change that: a request a program made and a person
agreed to is not the project's standing point, and writing the row would make that sentence false
in the very message the person read. `/mode allow-all` typed at a head still writes the row, with
the stamp — that is the operator's own act about their own project.

**One open request per session, and a new ask supersedes it**: a requester whose own patience ran
out would otherwise leave a card up that refuses every later attempt. The superseded request is
settled `answer: nobody, applied: false` with the reason, so the screen that was showing it takes it
down before the new one lands.

**What a caller reads back is the point IN FORCE, not the point it asked for** — a consented
`allow-all` on a bare host lands on `allow-all (this box, consented)`, the same coordinate with the
boundary told truthfully, and an answer that said `allow-all` would be naming something no session
is at.

### What is measured, and where

`crates/bridge/tests/transcript.rs` (its own binary, because the environment is process-wide): a
turn's rows come back in order with their content; a 9 000-character answer comes back at exactly
`cap` characters with `cut: true`; the cursor from before a second turn returns that turn's rows and
nothing older; and a `watch_only` attach reads the same conversation. The daemon is harnessd's own
`Daemon::serve` over a real store; only the MODEL is a fake, on loopback.

`crates/bridge/tests/mode_consent.rs`: `read-only` applies and says `nobody` was asked; `allow-all`
with no other head refuses, naming `leticode --session <id>`, and the session's settings row is
where it was; the same request with a real `tui` head attached — which answers the card on its own
thread, because the ask blocks until it is settled — applies, and the settings row afterwards is the
CONSENTED spelling; and their `no` applies nothing and leaves the row where it was. Every *did it
change* is read back from the daemon through a second, watching head (`can_decide: false`), so no
assertion is made against the tool's own report of its work.

The TUI's half is `a_confirmation_another_head_asked_for_is_the_same_card_answered_by_its_id`
(`crates/tui/src/app/tests/settings.rs`): the card names who asked, `y` sends `ModeConsent` with the
request's id, `Esc` sends it with `no`, a settlement clears the card, and a settlement for somebody
else's request does not.

**This box cannot run the two bridge suites that need a delegated cgroup** — `end_to_end.rs` and
`no_spin.rs` both seat `--bash`, whose backend is `HostBackend::executable`, and that fails in
`Harness::open` with *"a delegated cgroup v2 subtree is needed; … it is not writable"*. That is
apparatus rather than this change: neither file is modified here, and the path that fails
(`Harness::open`'s backend construction) is one B8 does not touch. B8's two files seat the writable
backend instead and run anywhere.

## Packages

| id | builds | depends on |
|---|---|---|
| B1 | `crates/bridge`: the MCP server (`initialize`, `tools/list`, `tools/call`) with `leticode_start`, `send`, `status`, `asks`, `answer`, `diff`, `stop`, `list`; the `wait` verb; tests against a daemon started on a temp workspace with a fake provider (as harnessd's own integration tests do), never a real provider | protocol as it is |
| B2 | `leticode_events` with the compact progress and the cursor; the report parsed into fields in `status` | B1 |
| B3 | the `/delegate` skill, and registering the server with Claude Code (`claude mcp add`, the operator's to run) | B1 |
| B4 | `escalate` | B1, a protocol addition |
| Q1 | a bridge-started daemon runs `--no-queue`: no lease, no model-facing queue notices | B1 |
| B5 | `leticode_queue`, `leticode_submit`, `leticode_queue_verb`, the `wait-queue` verb | Q1, a stable queue |
| B6 | `leticode_attach`: a head that JOINS a running daemon on a session it did not start, `watch_only` by default (no prompt, no answer, no interrupt, `can_decide: false` so a watcher is never counted as somebody who answers); the subagents a session spawns folded into `leticode_events` one line each | B1, B2 |
| B8 | `leticode_transcript` (the conversation as rows — prompts, answers, calls, results, a cursor and a stated cap — read from the daemon, never the store) and `leticode_mode` (the point that needs nobody applies; `allow-all` asks the operator through a head and applies only on their yes); protocol 54's four mode-confirmation frames | B1, B2, B6 |
