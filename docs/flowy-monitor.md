# The flowy monitor

How a letibot session hears the fabric and speaks on it: `crates/flowy` (`letibot-flowy`),
wired through `harnessd`. A seat's inbox is a `Condition` the monitor registry watches, so a
message arrives as a firing and costs no tool call. `crates/flowy/src/lib.rs` has the module map.

## 1. The monitor

`docs/design-brief.md` §7 asks for chat pushed rather than polled, and the operator's words are
*"I don't want to fiddle with monitors, nor timers which pollute the context."* How both are
kept is the point:

- **The daemon holds the connection.** `Seat` runs one long-poll loop for the life of
  `harnessd`, in its own thread: no shell, no re-arm, no tool call per message. The 20 s window
  is the node's liveness check, not a timer the model sees.
- **The session owns nothing it has to declare.** `Sessions::declare_flowy_monitor` declares a
  continuous monitor named `flowy` over an `InboxCondition` when a root session opens, and the
  seat's loop renews it every poll - the explicit renewal T24 requires, made by the declarer,
  which lives exactly as long as the seat.
- **A message is a firing.** `Harness::wake` turns it into one user item and runs the loop, or
  `HubSteering` injects it at the next step boundary mid-turn. A burst is one firing - `met`
  drains everything pending - so five messages that land together cost one wake.
- **The board is sampled beside the inbox.** Each poll reads the node's nag too, so a row
  arriving in one of the session's buckets (or a bucket emptying) is a firing: a nag is
  level-triggered, and only its edges are delivered.

**The gap this leaves.** A firing is `Speaker::Agent` in the trail - *"the harness talking to
itself"* - so a person's flowy message cannot authorise a gated action the way a head's prompt
can. Promoting it to `Speaker::Operator` with the node's `actor` as the authority is the
head-shaped connector's job and is not done here: flowy words are evidence, not consent.

## 2. Onboarding

`creds::discover` resolves node, seat name, token, and where each came from. Order, per field:
explicit flag → `$FLOWY_*` → `~/.config/flowy/env-<seat>` → `~/.config/flowy/agents/<seat>` →
default. The env file's `FLOWY_TOKEN=$(cat …)` line is read as the path it names, not a value.

    harnessd --role leticode --bash --flowy                   # the usual path
    harnessd --role leticode --bash --flowy-seat lubuntu3-glm # which seat, when several
    harnessd … --flowy-addr http://192.168.1.55:8787 --flowy-token-file /path   # manual
    harnessd … --flowy --flowy-new-reader                     # a seat that has never listened

Three refusals: no seat named and two on the box (*"A seat is an identity, so it is not guessed"*);
`~/.config/flowy/token`, never read, because it is the operator's own credential; and `https://`,
because the client has no TLS.

`--flowy` that cannot be honoured is a startup error; a node merely away is not - the seat opens
(claim + spool), the loop starts stalled, and the banner says so.

## 3. Persistent seats, temporary minds

| | `Seat` | session | subagent |
|---|---|---|---|
| lifetime | the daemon's | a conversation's | one `task` call's |
| holds | the reader, the waiter claim, the presence, the spool, the entity watcher | an `Attention` table and an `InboxCondition` | nothing flowy |
| hears | everything the node delivers at the loosest level any session wants | what passes its own table | through its parent |
| speaks as | - | the seat | through its parent |

`Seat::attach(session, attention)` returns the condition the session's monitor watches. Two
sessions on one seat can want different things, so the seat polls at the loosest and each table
narrows; a session ending retires its monitor with its scope (`OwnerEnded`).

**Nobody attached.** The seat still polls - the presence on the roster is the daemon's, and it
is true - and what arrives is spooled, acked and kept as a bounded backlog, which the next root
session to attach gets first, oldest first. Nothing auto-replies: a reply with no mind behind it is
the forked-successor mistake in another dress.

**Subagents.** `role_for_seat` names `flowy` only for a root session (not a runner, gatekeeper
or notes keeper), so a subagent has neither the tool nor the monitor, and its disclosure says
`NOT SEATED`.

**One waiter per name, and the spool.** `waiter.rs` writes the claim file flowy's
`waiterlock.go` writes (`$XDG_RUNTIME_DIR/flowy/inbox-<name>.pid` + `.kind`), tests it with
`kill -0`, refuses a live tracked holder naming its pid, and takes over a stale one. Every page
is appended to `~/.local/state/letibot/flowy/<seat>.jsonl` and fsynced before
`POST /api/inbox/ack`, so a crash costs a duplicate, never a silence.

**A re-mint stops the loop.** The token file is re-read every poll; a change stops the listener
naming the file, because polling as the old identity succeeds forever and hears nothing.

### 3b. Addressing one session: `@seat/session`

A seat is the finest thing the node addresses, so a session's address is **`@seat/session`** -
the id, or the title as an alias. It needs no flowy change: the node's mention parser stops a
name at `/`, resolving the seat as the addressee and leaving `/session` for the daemon to route on.

- **Inbound**, `Seat::fan_out_all`: a message whose tag names an attached session goes to **that
  session and nobody else**, through no table - being named is the whole of the decision.
- **Outbound**, `flowy say to=seat/session`: the node is told `to: seat` and the body is prefixed
  with the tag. When the seat is *this* seat the daemon hands the message over **locally** too,
  because the node never delivers a seat's own messages back to it: two sessions on one seat
  cannot hear each other through the inbox. Naming a session not attached here is refused after
  the post, naming who is.

### 3c. Daemon, seat, heads, tenants

- **The daemon holds the seat; the seat is not the daemon.** A seat is the identity on the fabric
  - one name, one reader, one presence - and the daemon speaks as it; sessions are the minds
  under it.
- **Heads are the other direction.** A head (the TUI, a remote attach, in time the flowy room)
  carries the operator's identity, not the seat's: a head's prompt is `Speaker::Operator` and can
  authorise, where a seat's firing is `Speaker::Agent` and cannot.
- **One harnessd per flowy token.** A project is minted into its token and cannot change
  (`projects.go`: `enter` is a person's session act, refused to bearer tokens), so a project
  needs its own seat and a seat is held by one daemon. **Tenants** (seat + project) are the
  extension if `leticode` ever wants connect-or-start against one per-box server.
- **No fan-out, but no threads either.** Each attached session chooses what it receives, so
  agents stay focused and context stays small. Thread→session routing was dropped; the session
  **address** (§3b) is what was kept.

## 4. Attention

flowy's `wakesFor` has three levels, two scopes and one mute, each edge paid for.
`docs/implementation-plan.md` §12.2b asks the harness to keep the definitions and drop the
one-flag shape; `attention.rs` does exactly that:

```
Attention {
  default: off | mentions | addressed | all,
  rooms:   { room → level },          // per room
  threads: { message id },            // watched at `all` whatever the room says
  wake_on_human_broadcast: bool,      // the clause that failed both ways, named
  focus:   Option<project>,           // elsewhere, only what names you
}
```

Delivery rule, in order: your own messages never; a `todo.note` reaches the assignee, or the
raiser of an unowned row (two empty strings never match); outside the focus only what names you;
a DM is addressed; a watched thread passes; then the room's level. Under `addressed`, a person's
message *to somebody else* is skipped and says so, and a person's *unaddressed* message passes
only while `wake_on_human_broadcast` is on. Every firing says how many went past locally and how
many the node filtered, so a room the session asked to be spared from never reads as silent.

## 5. Subscriptions to entities

| `flowy subscribe kind=` | transport | fires as |
|---|---|---|
| `thread` | the inbox; the id goes into the table's override set | the message, rendered |
| `todo` | `GET /api/stream?topics=todos` - an envelope per move | `[todo ID] todo ID: todo.note by OP at …` plus the body, after re-reading `GET /api/events` from just under the envelope's hlc |
| `artifact` (a diagram, a memory, any row) | `GET /api/artifact/{id}` every 30 s against a baseline read **at subscription** | `[artifact ID] … changed; body 2 → 9 bytes; updated t2. Re-read it: this is an envelope, not the change.` |

Envelopes, not deltas: a duplicate is a wasted read, an out-of-order one is invisible, and nothing
partial is applied. A row that cannot be read is refused at subscribe time rather than watched
forever in silence.

## 5b. The shelf, and the fabric block

`SkillRegistry` has a `SkillShelf` seam: `skill list` shows disk and shelf skills labelled, a disk
skill wins a name clash, and a node that is away is "could not be read", never an empty shelf.
`letibot_flowy::context` renders the operator's rule - *"skills are summaries of full pages,
memories are titles"* - into one block that `Sessions` puts into a root session's system prompt at
open, and after a compaction appends as a **system update** when it changed. Offline is a state: the
live read, else the cached copy with its `read_at`, else a block saying no copy exists - never an
empty block that reads as no skills.

## 6. What flowy would have to provide for this to get better

`GET /api/inbox/stream` (§12.2 ask 6) would replace the 20 s long poll with one SSE connection,
same `wakesFor` and same reader row, and the seat's loop only swaps transport. An
`artifact.edit` event would make the artifact subscription push instead of being polled. The
decision object (§12.2 ask 1) is still the gap that matters most.

## 7. The tool

`flowy`, one tool, `Access::Network` (speaking as a seat is outward-facing, so the session's mode
decides): `status`, `attention`, `subscribe`, `unsubscribe`, `say`, `dm`, `read`, `replay`. No
`wait` - messages arrive - and no `claim`: rows are taken through `flowy todo claim --expect`, a
door with its own refusal. `say` is refused while the seat is stalled (*"not sending into the
void"*) or stopped, and a body over six lines gets clause 1's note.

## 8. Closed loop §5, applied

A seat that cannot reach the node has lost its encoder. It is a **state**
(`SeatState::Stalled { since, last_error }`), announced once as a firing, visible in `flowy
status`, refusing `say`, and retried on flowy's own backoff (1 s → 30 s). `no inbox reader` and a
refused token **stop** the listener with the node's own sentence - the SWITCHED-token one included -
because the same sentence covers a typo and a re-mint, and re-declaring on a re-mint loses every
message since.