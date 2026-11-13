# The daemon socket, and what a tool call may do through it

A session's tools talk to the daemon over a unix socket in the runtime directory,
`$XDG_RUNTIME_DIR/letibot/<12 hex>.sock`, mode `0600`. That mode says *the operator's user*, which is
not the same as *the operator*: a model's `bash` call runs as that user too, and it is handed
`LETIBOT_SOCKET` and `LETIBOT_SESSION` because that is how `sudo`'s password prompt reaches the person
in front of a head.

So the socket cannot tell a head from a command by its permissions, and it cannot tell them by the
protocol either: a frame's `kind` and `identity` are text a command can type. The daemon asks the
kernel instead, at `connect`, and answers as one of two things.

## Who is on the other end

| the kernel says | the connection is | and gets |
|---|---|---|
| a process descended from this daemon, or one in a cgroup this daemon opened (`letibot.<daemon pid>.<n>`) | a tool call | the password ask, and the bookkeeping of being attached |
| anything else, including the daemon's own operator-launched head | a head | every door |

The pid comes from `SO_PEERCRED` on Linux and `LOCAL_PEERPID` on macOS, read before the stream is
handed to the reader, because it is a fact the kernel filled in at `connect` and nothing later can be
asked about it. The cgroup arm is what a double-forked child cannot leave; the parent-chain arm is
what covers a host without cgroups.

**A peer the kernel will not name is treated as a tool call.** A platform that cannot answer, or a
process that exited under the question, falls on the side that refuses, never the other way.

## What a tool call may do

- **Ask for the password `sudo` needs.** That is the whole reason the socket address is in a command's
  environment: `letibot-askpass` connects, asks, and the answer goes back to `sudo` through the
  helper's stdout. It is never logged, never stored, and never in the transcript.
- **Attach, acknowledge, resync and detach**, which is the bookkeeping of being a connection at all.

Nothing else. Every operator door is refused, by name, in the operator's own words:

```text
a ! line is the operator's own act, and this connection came from inside a session's tool tree:
pid 4711 on the other end of the socket is a process this daemon started, in its own tree or its
cgroup, so nothing of it was done. The one door served to a tool call is the password ask `sudo`
needs.
```

The doors that answer this way are the ones that would otherwise run something as the operator,
ungated by the permission gate, because the gate judged the outer command that opened the socket and
not what it then typed. In the daemon's own words they are: `typing a prompt to the model`, `a ! line`,
`!term opening a pane`, `!term keys`, `!term closing a pane`, `/mode`, `a / verb`, `the settings rows`,
`the session list`, `a decision`, `stopping the daemon`, `an operator call's result`, `a plugin pane's
key`, and the rest of the same list.

The refusal names the door it refused and the pid it read, so a person seeing it in a transcript knows
what was attempted and which process attempted it.

## What this does not close

- **A terminal inside tmux or screen.** An unconfined command can type into the operator's terminal
  through the multiplexer (`tmux send-keys`, `screen -X stuff`) without touching letibot's socket, and
  that is outside what letibot can see. A confined session cannot reach the operator's tmux socket, so
  the exposure is the unconfined seats.
- **A double fork on macOS.** There is no cgroup there, so a child that calls `setsid` leaves the
  parent chain and is read as a head. The cgroup arm on Linux is what makes the same trick fail there.
- **A program the daemon started outside a tool call is not a head either.** A `!term` pane's program
  and a background job are `inside` by this rule, so they are refused the operator's doors too. Nothing
  in the tree attaches as a head that way; the rule is stated so a future reader does not discover it
  as a surprise.

## The editor head

A connection is either a tool call or a head, as above. One head is rano's standalone panel:
`rano::letibot`, a client written against this wire and living in another repository. It cannot
link `letibot-sessionlog` — the two projects would pin each other, and a typed client fails on a
variant it has never heard of in the middle of a turn — so it reads each line as JSON and acts on
a subset. `rano-integration.md` §3.2 has the reasoning; this is the subset itself, and the table is
a contract in both directions.

| need | the panel sends | the daemon answers |
|---|---|---|
| attach | `Attach {protocol_version, session_id, since_seq, kind: "editor", identity, caps}` | `Hello {protocol_version, session_id, head_id, dropped, snapshot.seq, sessions[]}` — always first, and once per attach |
| sessions | `ListSessions`; `NewSession {client_request_id, title, workspace}`; `Switch {session_id, since_seq}` | `Sessions {sessions, current, created}` |
| history | `FetchRows {session_id, from, count}` | `RowsFetched {session_id, from, rows}` |
| prompt | `Prompt {client_request_id, expected_seq, text, attachments}` | `Accepted {client_request_id, seq, note}`, or `Rejected {client_request_id, reason, expected_seq, actual_seq}` |
| stream | — | `Event {seq, event}` carrying `turn_started`, `delta`, `tool_started`, `tool_finished`, `turn_finished`, `turn_interrupted`, `turn_failed` and `citations` |
| cards | `Answer {client_request_id, req_id, option_id, pattern, note}`; `AnswerQuestion {client_request_id, req_id, answer}` | `decision_requested {req_id, kind, call_id, summary, target, options, choices, because, deadline}`, then `Accepted` |
| interrupt | `Interrupt {client_request_id, expected_seq, reason}` | `Accepted` with the note `interrupt requested` |
| mark | `Ack {seq, rendered, filtered}`, sent *after* the batch is drawn | nothing: the mark is applied to this head's seat |
| resync | `Resync` | `resync {reason, dropped, snapshot}` — the panel resets to the snapshot and carries on, which is a normal outcome and not an error |

What keeps that table true:

- **The field names are the contract, not the types.** The panel reads a missing field as its
  empty value — an empty string, zero, `false` — so a renamed field is a blank in the editor and
  never an error. A frame the *daemon* cannot read is the opposite and is loud: the connection is
  closed with a `Bye` saying what could not be read and which protocol this daemon speaks
  (`server.rs`). And `crates/tui/tests/rano_editor_head.rs` runs `rano::letibot::Client` against a
  real `Hub`: it attaches, prompts, folds a stream, answers a card and interrupts, and asserts each
  field's *value* — which is the only way a rename in the quiet direction is caught here rather
  than in a user's editor.
- **A new variant, or a new word in a closed enum, moves `PROTOCOL_VERSION`; an added
  `#[serde(default)]` field does not.** `Prompt.context` is the second kind (70 to 71): a panel
  written against 70 sends none, and the daemon renders nothing for it. A differing number is served
  rather than refused — the daemon says so on its own log, because nobody can tell which side is
  wrong — so the number is a FACT to read, not a gate: the panel takes the daemon's from `Hello` and
  compares it to the one it was written against (the client pinned today says 70). What a panel then
  refuses to offer is the panel's business; this side promises only that the newer field is defaulted.
- **What it does not know, it counts and skips.** An unknown `frame` or `event` tag is delivered
  as `Unknown` and counted (`Client::skipped`), never fatal: a newer daemon must not blank the
  pane, and an event folded into the reply would be drawn as prose. Everything the rest of this
  file's sections describe — rooms, todos, panes, settings, the subagent tree — is in that set.
  The panel is one conversation.
- **A seat that may decide says so.** The panel attaches with `kind: "editor"`, `can_decide: true`
  and `features: ["editor_v1"]` — rano's `Seat::default()`. That bit is the client's to set, and it
  is what makes an answer land: a head that attached `can_decide: false` has its card answers refused
  by name (`this head declared can_decide: false`) instead of being sent a question it cannot answer.

## Why not a per-head token

Because there is nothing to put it in that a command cannot read. A head is a process the operator
launched, so a credential would have to live in a file the head reads and a tool call could read too;
a second socket would be a second thing to place and to find. The kernel's answer to *which process
is this* is the one fact a command cannot forge, and the two limits above are the price of it.

See [one-daemon.md](one-daemon.md) for the socket's place among the runtime records, and
[boundary-and-adjudication.md](boundary-and-adjudication.md) for what a command may do once it is
running.
