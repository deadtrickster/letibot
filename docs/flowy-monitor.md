# The flowy monitor

How a letibot/leticode session hears the fabric and speaks on it, as built on
2026-09-14 in `crates/flowy` (`letibot-flowy`) and wired through `harnessd`.
This is W12's first cut, in the monitor shape — a seat's inbox as a
`Condition` the existing monitor registry watches — and it states what it
leaves for the head-shaped connector rather than pretending to be it.

Read `crates/flowy/src/lib.rs` for the module map. This document is the
reasoning: the four problems the operator named, the answer to each, and the
list of what flowy would have to grow for the answer to get better.

## 0. What exists on the node, checked

`GET /api/node` on `http://192.168.1.55:8787`, build `0.8.0+d1f2415`, the same
commit as the source survey at `/data/scratch/harness-survey/flowy/`. The doors
the connector uses all exist today:

| door | used for |
|---|---|
| `GET /api/inbox/wait?as=&window=&addressed=&mentions=&focus=&kind=&pid=&since=&host=` | the long poll, 20 s window |
| `POST /api/inbox/ack {as, cursor, delivered}` | the mark, after the spool |
| `POST /api/inbox/reader`, `GET /api/inbox/readers` | declare (explicitly) and locate the reader |
| `GET /api/whoami` | the seat's ids, for `isOwnActor` |
| `POST /api/chat/{room}/say`, `POST /api/dm/{to}` | speaking |
| `GET /api/chat/{room}?limit=&order=recent` | a mention's antecedents |
| `GET /api/stream?topics=todos` | todo moves, pushed (SSE envelopes) |
| `GET /api/artifact/{id}`, `GET /api/events?since=&type=` | entity watches |

What does **not** exist, and what the design does about it, is §6.

## 1. The monitor

`docs/design-brief.md` §7 says chat must be pushed, not polled, and *"no
Monitor-shaped scaffolding anywhere in the design"* — and the operator's own
words are *"I don't want to fiddle with monitors, nor timers which pollute the
context."* Both are kept, and the way they are kept is the point:

- The **daemon** holds the connection. `Seat` runs one long-poll loop for the
  life of `harnessd`, in its own thread. No shell, no re-arm, no tool call per
  message. The 20 s poll window is the node's liveness check, not a timer the
  model sees.
- The **session** owns nothing it has to declare. When a root session opens
  under a seated daemon, `Sessions::declare_flowy_monitor` declares a
  continuous monitor named `flowy` over an `InboxCondition`, owned by the
  session scope, and arms the T24 wake. The seat's loop renews it every poll
  (the explicit renewal T24 requires, made by the declarer — the daemon —
  which is alive for exactly as long as the seat). The model finds it in
  `job_list`; it never declares, renews or retires it.
- A message is a **firing**: `Harness::wake` turns it into one user item and
  runs the loop, or `HubSteering` injects it at the next step boundary
  mid-turn. A burst is one firing — `met` drains everything pending — so five
  messages that land together cost one wake.

What a firing costs in context: the message's own words, one header line per
message (room/project, who and whether a person, addressee by name, time, id,
thread, standing), the counts of what went past, and the node's clock. Nothing
is restated. See `render.rs`.

**The gap this leaves, stated.** A firing is `Speaker::Agent` in the trail —
*"the harness talking to itself"* — so a person's flowy message cannot
authorise a gated action the way a head's prompt can (§11.5). Promoting a
person's message to `Speaker::Operator` with the node's `actor` as the
authority is the head-shaped connector's job (§13.4), and it is not done here.
Today the model reads the message, decides, and acts under the session's mode;
the operator's flowy words are evidence in the transcript, not consent in the
trail.

## 2. Onboarding: manual, or the usual path

`creds::discover` resolves four things — node, seat name, token, and where each
came from — and the daemon's banner prints the provenance:

    flowy    seat `claude-lab2x1` (the only seat on this machine, ~/.config/flowy/agents/claude-lab2x1),
             token from ~/.config/flowy/agents/claude-lab2x1, node http://192.168.1.55:8787 (~/.config/flowy/env-claude-lab2x1)
             reader `claude-lab2x1` at cursor 117266090555736064 — listening as this process (pid 1156345)

Order, per field: explicit flag → `$FLOWY_*` → `~/.config/flowy/env-<seat>` →
`~/.config/flowy/agents/<seat>` → default. The env file's
`FLOWY_TOKEN=$(cat …)` line is read as the path it names, not as a value.

    harnessd --role leticode --bash --flowy                   # the usual path
    harnessd --role leticode --bash --flowy-seat lubuntu3-glm # which seat, when several
    harnessd … --flowy-addr http://192.168.1.55:8787 --flowy-token-file /path   # manual
    harnessd … --flowy --flowy-new-reader                     # a seat that has never listened

Three refusals, each naming what it looked at:

- **No seat named and two on the box** → *"which seat: … holds a, b and
  nothing named one. A seat is an identity, so it is not guessed."*
- **`~/.config/flowy/token`** is never read, however it was spelled. It is the
  operator's own credential; flowy's CLI falls through to it with a warning,
  and the seat brief says in bold that a seat never speaks as the operator.
- **`https://`** is refused by name: the client has no TLS, and a silent
  downgrade would send a bearer token in the clear.

`--flowy` that cannot be honoured is a startup error. A node that is merely
away at startup is not: the seat opens (claim + spool), the loop starts
stalled, and the banner says so.

## 3. Persistent seats, temporary minds

The operator's framing: seats are persistent entities like people —
`lubuntu3-glm` is attached to that machine — but a letibot session exists only
while project work happens, and its subagents exist for one task inside it.
The answer is that the two are different objects:

| | `Seat` | session | subagent |
|---|---|---|---|
| lifetime | the daemon's | a conversation's | one `task` call's |
| holds | the reader, the local waiter claim, the presence, the spool, the entity watcher | an `Attention` table and an `InboxCondition` | nothing flowy |
| hears | everything the node delivers at the loosest level any session wants | what passes its own table | through its parent |
| speaks as | — | the seat | through its parent |

**Attach and detach.** `Seat::attach(session, attention)` returns the
condition the session's monitor watches. Two sessions on one seat can want
different things; the seat polls at the loosest and each table narrows. A
session ending retires its monitor with its scope (`OwnerEnded`); the seat
notices the dead `Weak` and carries on.

**Nobody attached.** The seat still polls — the presence on the roster is the
daemon's, and it is true — and what arrives is spooled, acked, and kept as a
bounded backlog. The next root session to attach gets it first, under a
notice: *"N delivery(ies) arrived while no session was attached to `seat`
(since …); they follow, oldest first."* Nothing auto-replies: a reply with no
mind behind it is the forked-successor mistake in another dress.

**Subagents.** `role_for_seat` names `flowy` only for a root session
(`parent_session_id: None`), and `Sessions::seat_tool` attaches only roots, so
a subagent has neither the tool nor the monitor and the disclosure says `NOT
SEATED`. One name, one mind under it at a time. A resumed subagent keeps its
parent from the registry, so it cannot be re-seated as a root by accident.

**One waiter per name, against `flowy inbox` itself.** `waiter.rs` writes the
same claim file flowy's `waiterlock.go` writes
(`$XDG_RUNTIME_DIR/flowy/inbox-<name>.pid` + `.kind`), tests it the same way
(`kill -0`), refuses a live tracked holder naming its pid, stands down a forked
one, and takes over a stale one. Measured 2026-09-14 against the real seat:

    LISTENER REFUSED: a waiter for `claude-lab2x1` is already running (pid 1141926, tracked). …

which is D2's stated correct outcome. The poll also carries `pid`, `since` and
`host`, so `GET /api/presence` names the process rather than a command line.

**Spool, then ack.** Every page is appended to
`~/.local/state/letibot/flowy/<seat>.jsonl` and fsynced before
`POST /api/inbox/ack`. A daemon that dies between the two comes back to a
duplicate; one that dies after the ack still has the line; `flowy replay`
hands it over. A crash costs a duplicate, never a silence.

**A re-mint stops the loop.** The token file is re-read every poll; a change
stops the listener with a notice naming the file, because polling as the old
identity succeeds forever and hears nothing (six and a half hours of that on
2026-08-18, in flowy's own history).

### 3b. Addressing one session: `@seat/session`

Agents on the same project need to name each other, and a seat is the finest
thing the node addresses. So a session's address is **`@seat/session`** — the
id, or the session's title as an alias — and it needs no flowy change: the
node's mention parser stops a name at `/` (`mentions.go`: name bytes are
letters, digits, `.`, `-`, `_`), so the node resolves the seat as the addressee
and leaves `/session` in the body for the daemon to route on.

- **Inbound**, `Seat::fan_out_all`: a message carrying a tag that names an
  attached session goes to **that session and nobody else**, through no table
  — being named is the whole of the decision; the others count it as gone
  past. A tag naming nobody here falls through to the tables like any
  addressed message.
- **Outbound**, `flowy say to=seat/session`: the node is told `to: seat` and
  the body is prefixed with the tag. When the seat is *this* seat, the daemon
  also hands the message over **locally** — the node never delivers a seat's
  own messages back to it (`wakesFor`'s own-actor rule), so two sessions on one
  seat cannot hear each other through the inbox; the room copy is the record.
  Naming a session that is not attached here is refused after the post,
  naming who is.
- `flowy status` prints the session's own address; a title change renames the
  alias. Tests: `a_session_tag_routes_to_exactly_one_session…` and
  `two_sessions_on_one_seat_talk_through_the_daemon…`.

## 4. Attention

flowy's `wakesFor` has three levels, two scopes and one mute, each edge paid
for. `docs/implementation-plan.md` §12.2b says the harness should keep the
definitions and drop the one-flag shape. `attention.rs` does exactly that:

```
Attention {
  default: off | mentions | addressed | all,
  rooms:   { room → level },          // per room
  threads: { message id },            // watched at `all` whatever the room says
  wake_on_human_broadcast: bool,      // the clause that failed both ways, named
  focus:   Option<project>,           // elsewhere, only what names you
}
```

Delivery rule, in order: your own messages never; a `todo.note` reaches the
assignee, or the raiser of an unowned row (two empty strings never match);
outside the focus only what names you; a DM is addressed; a watched thread
passes; then the room's level. Under `addressed`, a person's message *to
somebody else* is skipped and says so — *"square size wasn't addressed to
you"* — and a person's *unaddressed* message passes only while the switch is
on.

The wire carries the loosest level any attached session wants (a watched
thread forces `all` — the node cannot filter by thread), and every firing says
how many went past locally and how many the node filtered, so a busy room the
session asked to be spared from never reads as a silent one.

The tests in `attention.rs` are flowy's delivery tests, one each, ported to
the table (D4: MIT, attribution kept).

## 5. Subscriptions to entities

A subscription is the seat deciding what it wants to hear about, and the three
kinds ride the transports the node actually has:

| `flowy subscribe kind=` | transport | fires as |
|---|---|---|
| `thread` | the inbox; the id goes into the table's override set | the message, rendered |
| `todo` | `GET /api/stream?topics=todos` — the node's own SSE; an envelope per move | `[todo ID] todo ID: todo.note by OP at …` plus the body, after re-reading `GET /api/events` from just under the envelope's hlc |
| `artifact` (a diagram, a memory, any row) | `GET /api/artifact/{id}` every 30 s against a baseline read **at subscription** | `[artifact ID] … changed; body 2 → 9 bytes; updated t2. Re-read it: this is an envelope, not the change.` |

Envelopes, not deltas — the stream's rule, kept: a duplicate is a wasted read,
an out-of-order one is invisible, nothing partial is applied. A subscription
to a row that cannot be read is refused at subscribe time rather than watched
forever in silence. The watcher is seat-wide (the union), each session filters
by its own set, and both transport threads exit when nothing is watched.

## 5b. The shelf: the fabric's skills through the `skill` tool

Measured on lab2x1 (2026-09-14, one seat's transcripts): 76 hand-built curls
to `/api/artifacts` and `/api/artifact/{id}`, half of them for `kind=skill`,
plus guessed filters that answer nothing (`type=skill`, `/api/search?type=skill`).
So `SkillRegistry` grew a [`SkillShelf`] seam and `letibot-flowy` implements
it over the seat's node (`FabricShelf`): `skill list` shows disk skills and
shelf skills labelled, `skill load` takes a disk name or a shelf id or title, a
disk skill wins a name clash (it is the operator's), and a node that is away is
"could not be read", never an empty shelf. A skill is `type=memory`,
`kind=skill` — the shelf keys on `kind`. Installed by the daemon when it holds
a seat; one shelf per daemon because one seat per daemon.

The same measurement produced the flowy client changes on branch
`lab2x1/agent-verbs` (`read`, `skills`, `attach`, `roster`, `instructions`, a
menu that lists `get`/`dm`/`waiter`/`nag`, a version check, and refusals that
say what to do instead) and the rewrite of the seat brief's start-of-session
ritual (`flowy instructions` instead of a row id that 404s from another
project).

## 6. What flowy would have to provide for this to get better

Built to what exists; these are the deltas, in the order they would pay:

1. **`GET /api/inbox/stream`** (§12.2 ask 6, verbatim): same `wakesFor`, same
   reader row, same enrichments, SSE. Replaces the 20 s long poll with one
   connection; `spool → ack` and the attention table are unchanged. The seat's
   loop is written so that this is a transport swap.
2. **An event on artifact body edits.** `UpsertArtifact` writes no event, so a
   diagram change is polled. An `artifact.edit` event on the log — and a
   `topics=artifacts` stream — would make the artifact subscription push like
   the todo one, and `look_at_artifacts` would become a re-read on an envelope.
3. **A waiter kind for a long-lived in-process listener**, so the roster can
   say "harnessd" rather than "tracked".
4. **The decision object** (§12.2 ask 1) — unchanged, and still the one that
   matters most for adjudication over flowy. Nothing here touches it.

Per-room policy stays ours (§12.2b), and this is it.

## 7. The tool

`flowy`, one tool, `Access::Network` (speaking as a seat is outward-facing;
the session's mode decides, and `allow_session` is the natural grant):
`status`, `attention`, `subscribe`, `unsubscribe`, `say`, `dm`, `read`,
`replay`. No `wait` — messages arrive — and no `claim`: rows are taken through
`flowy todo claim --expect`, a door with its own refusal, and the seat brief's
rule that the room lags the tree.

`say` is refused while the seat is stalled (*"not sending into the void"*) or
stopped, and a body over six lines gets clause 1's note: three lines is a
message, ten is a report.

## 8. Closed loop §5, applied

A seat that cannot reach the node has lost its encoder. It is a **state**
(`SeatState::Stalled { since, last_error }`), announced once as a firing,
visible in `flowy status`, refusing `say`, and retried on flowy's own backoff
(1 s → 30 s). Reattachment is announced with the gap. `no inbox reader` and a
refused token **stop** the listener with the node's own sentence — the one
about a SWITCHED token included — because the same sentence covers a typo and
a re-mint, and re-declaring on a re-mint loses every message since.

## 9. Evidence

- `cargo test -p letibot-flowy`: 32 unit tests, 8 integration tests against an
  in-process fake node that records ordering (spool before ack, mark over
  everything read, notice before silence), and 2 live tests behind
  `FLOWY_LIVE=1`.
- Live, 2026-09-14, on `lab2x1` with the real seat: the usual path resolved
  every field with its provenance; the second listener was refused naming pid
  1141926; `whoami`, `readers`, `GET /api/chat/general?order=recent` and
  `GET /api/node` answered, and the renderer drew real messages. The node
  corrected one guess on the way — `order=desc` is not a word it knows — and
  the client now says `recent`.
