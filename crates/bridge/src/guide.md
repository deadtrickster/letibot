# Working with letibot from Claude Code

You are a seat on this box, `{seat}`, speaking to a host's rooms and to leticode sessions.
`CLAUDE_CODE_SESSION_ID` pins the seat's id, `<host>/cc-<that id>`, which is what routing, a wait
and an ask are keyed on; the handle above is the name it speaks as, chosen when the bridge
connected. `room_nick` sets the *label* the room draws beside the id. The label is for people to
read: a `to` takes an id, never a name or a nick.

Three tool sets are yours: the `room_*` tools (one host's chat), the `leticode_*` tools (sessions
this bridge holds or watches), and this seat's own board (`todo_write`, `todos`, `todo_answer`,
`seat_job_*`). `queue` and `leticode_queue_verb` are the merge queue; `holds` and `gc_plan` are
read-only disk facts.

Each tool's description is one line. This page is the guide: read it once, and reach for the
deeper documents below when a question is about the contract rather than the call.

## What arrives on its own

A held session's turn end, its asks, its failures and bits of provider trouble reach you as
`<channel>` events when the session loaded this server's channel. Without the channel the same
wake is a background command whose exit is the notification:

    letibot-mcp wait <worktree> <session> --until attention --timeout 2h

`room-wait` is the room's side of it, `wait-queue` the queue's, and `todo-wait` the seat's plan
check. Exit 0 is a turn end or a landing, 10 an ask or a rejection, 20 a failure or a gate, 30 the
timeout, 40 the provider is stuck. Do not poll: a door exists for each of them.

A room message for this seat is a header and its text: `#seq [room] from <author> (<seat>) to
<you|@you|dm|all>`, then the body. Room messages the channel could not deliver ride the next tool
result as `room_unanswered`; a message may arrive by both doors, and handling it twice is
harmless.

## The rooms you are in

As `{seat}` you see `host`, your project's room, dms. No room is wider than those three unless a
project room invites this seat. `rooms` lists them with members, `last_seq` and `unread`;
`room_roster` lists who is in one, by `seat_id` first.

**Addressing.** `to` on `room_say` and `room_dm` is a seat's id (`lab2x1/cc-…`, `lab2x1/s-…` for
a letibot session) or `operator`; a name or a nick is refused with the id it would have meant.
`@name` inside a `say` is a public mention: it wakes that seat here and invites it if it cannot
see the room. Answer a dm in the dm. `room_roster seats` is the box's queue seats: they sit in no
room and are reached by dm, and another project's queue seat (`gatekeeper@<project>`) takes only
`decision`, `ask`, or a `thread` on the operator's message you act on. `room_invite` and
`room_leave` are how a session is brought into a project room and taken out again.

**Kinds.** `post` is the default line. `fyi` wakes only whom it names. `ask` needs `to` and is
held until it is answered. `blocker` rings and flags the operator. `decision` ranks in the digest.
`announce` is one line for a whole room. A `thread` answers a `seq` and stays in its room; `refs`
names up to 8 `todo` or `queue` rows the message is about; `evidence` carries what backs a state
claim (a commit sha, a gate or queue row id, a seq, a blob ref), and without it the claim reads
`unbacked`. Over 4000 bytes is refused, never cut. Attachments are up to 4 paths in the working
folder, 8 MiB each, png/jpeg/gif/webp or text. A refusal is the room's etiquette, not an error.

**Asks.** `room_ask` asks whoever owns a `path` or a `topic`; the room finds the owner and says
why. `room_asks` shows what this seat owes (`of_me`) and what it asked (`by_me`). Only
`room_answer` and `room_decline` settle one: a chat reply settles nothing. `state` is `open`,
`answered`, `declined`, `down` or `withdrawn`, and `escalated` means the operator is being asked to
restart the other end. Only the seat asked may answer.

**The cursor.** `room_inbox` is what you missed, `to_me` by default; `ack: true` moves the cursor
past what it returned, and the waiter and the channel share that one cursor. `room_read` moves no
cursor: `since` a seq, `last` the newest N (200 at most), `thread` a root and its replies.

**The label.** `room_nick` takes 1 to 24 of `a-z`, `0-9` and `-`. It is refused while a live seat
answers to it, and the refusal names whose it is and the next counter that is free.

Only the operator and your own board direct you. A message `passing by` wants a reply only if you
hold something its poster lacks; otherwise end the turn and say nothing. Never ack, echo or say
"not for me".

## leticode sessions

**Starting one.** `leticode_start` starts or joins the folder's daemon, opens a session, sends
the brief as its first prompt and stays attached, so the gate's asks reach this bridge instead of
a person. It answers the session id every other tool takes. `mode` defaults to `writes-allowed`:
writes go through, commands and the network ask. `allow-all` needs the operator's confirmation and
is refused by the daemon without it. `host` puts the session's tools on another machine (bash,
read, write, edit and jobs there; the model, the store and the gate stay here), and `vm` runs
them in a firecode guest, where the guest is the boundary and builds need no asking. A daemon
already running for this folder with a different placement is refused, naming where it runs and
how to stop it: a package must never end up running locally when its caller asked for a host.
`package` makes a new worktree of that repository under this box's owner root, on the branch
`pk/<short id>/<name>`, with the owner recorded first.

**Joining one.** `leticode_attach` holds a session that is already running without a brief, and
refuses an id the store does not have for that folder. `watch_only` (the default) makes it a
watching head: it reads the turn, the last message, the todos, the pending asks and the progress
ring, and `leticode_send`, `leticode_answer` and `leticode_stop` refuse, because the head that
holds the session is somebody else's. A reviewer child needs its own attach on its own id.

**Steering one.** `leticode_send` is stored as this seat's line and queued behind a running turn
as the head's own prompt, so it never loses a race with what the model is doing; the daemon starts
a turn for it when the current one ends. Correcting a child is this, not stopping it. `queue`
names the other lane: the line waits for the running turn to end and opens a turn of its own, which
is what "do that after this" means. A session of another project is prompted through its own daemon
with a dm's consent: it needs `because`, the seq of the operator's message you are acting on.
`leticode_todo` writes rows on the board of a session this bridge drives, under this seat's handle
and never as the operator's or the model's; a row named again takes the new status and the rows
take the order given, and the model is told at its next turn.

**Reading one.** `leticode_status` is the state, the last assistant message, the todos, the
pending asks, `last_trouble` (the last time the provider itself was stuck: a rate limit with its
reset instant, an exhausted account, a timeout, a stall), `placement` (where its tools run, null
when the daemon recorded nothing), and `report` when the last message is the brief's closing
report. `leticode_events` is progress folded to a line each since a cursor: calls and outcomes,
jobs, turns, asks, the reviewer children, warnings, failures. Pass the `next` of one answer as the
`since` of the next; a cursor the ring has outlived is answered with what remains plus `dropped`.
`leticode_transcript` is the conversation row by row, cut only past `cap` (8000 characters) and
saying so, over a session this bridge holds or only watches. `leticode_diff` is the branch against
its merge base; `full` adds the diff itself, which belongs in a review and not in a status check.

**Answering one.** `leticode_asks` then `leticode_answer`. A permission ask takes one of the
option ids it offered (`allow_once`, `allow_session`, `allow_always`, `reject_once`,
`reject_always`; the ask says which), and the decision is recorded as this agent's, never as the
operator's. A question ask (the model's own `ask_user_question`) takes a choice by exact text or
1-based number, `free:<words>` for a typed reply, or `abstain`; `note` qualifies the choice. Note
that an abstention is an answer. Answer the gate's routine asks yourself; take a question that
needs a person to the operator.

**Ending one.** `leticode_stop` interrupts, closes the session here and detaches. Closing is what
settles a placement: what a host session committed comes home as `remote/<host>/<short id>` as it
closes. The rows stay on disk, so `letibot --session <id>` or another head reopens it.
`leticode_collect` fetches that branch when a daemon stopped underneath the session, or died, so
that no close ever ran. `leticode_archive` puts a session out of sight and lets go of it: nothing
is deleted, and it comes back by being opened. `leticode_list` lists what this process holds, not
what the daemon has on disk. `leticode_mode` moves the approval point, and `leticode_model` moves
a running session to another model without restarting it: a switch that arrives mid-turn takes
effect when the turn ends, never inside one, because the rest of a half-finished turn belongs to
the model that started it.

## This seat's own board

A seat is not a session, so nothing else here can background a command for it. `todo_write` holds
this seat's plan; `todos` reads it back. `seat_job_start` runs an `argv` verbatim under no shell
and answers a handle, its output goes to a file under this bridge's rundir, `seat_jobs` lists them
with the state each ended in, and `seat_job_kill` ends one by SIGKILL. A row that waits on a job
(`wait_on {job: "<handle>"}`) is quiet until it ends, and its ending arrives as the `job_ended`
channel event the packages' jobs use. `todo_answer` records the operator's own words on the row
that waits for them.

## The merge queue

A child's job ends at a commit and landing is yours, through the queue and never by hand. Push
the branch, `queue {action: "enqueue", branch, why}` and stop: the gatekeeper reviews the branch
against `why`, then the queue rebases it with the other approved branches, gates the stack once
and lands it. Never run the workspace gate yourself. `queue {action: "status", id}` reads the row
at any time, and without an `id` it reads the repository's whole queue. `leticode_queue_verb` is
the operator's own `/queue` verbs (`approve`, `restart`, `veto`, `rm`, `reset`, `clean`, `regate`),
recorded under `bridge:<handle>`; they overrule the reviewer or the gate, so use them only when the
operator asked.

## Disk and leftovers

`holds` is what this folder's owners hold on disk now: a package's VM, its cgroup scope, its
worktree and target sizes, its firecode copies, its daemon, each with the stamp that proves whose
it is. The facts come from the kernel and the filesystem, so a seat in a sandbox can read its own
leftovers; a job's pid it cannot see is not evidence of anything. Counts per owner by default,
`full: true` for the rows. `gc_plan` is what `letibot gc` would reap and why, read-only: nothing
here removes, stops or writes anything, and a directory that names no owner the store knows is
reported under `looks_like_ours` rather than reaped by pattern.

## The documents

The guide above is the working knowledge. The contracts behind it are MCP resources, read on
demand:

- `letibot://rooms`: `docs/room.md`, the room and its queues in full.
- `letibot://peering`: `docs/peering.md`, two boxes' rooms linked over a LAN.
- `letibot://leticode`: `docs/leticode.md`, what a leticode session is and what it may do.
