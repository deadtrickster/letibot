# Status

What letibot is and where it stands, for a reader with no context. Every line names the file behind
it, so a line that has gone stale can be fixed where it lives.

Checked against `main` at `dd7d45de`, 3 November 2026.

## What this is

A local coding agent in two processes. `harnessd` owns the sessions, the store, the model connection
and the permission gate; a head attaches to it over a unix socket, so closing the terminal mid-turn
does not stop the turn ([README.md](../README.md), [one-daemon.md](one-daemon.md),
[socket.md](socket.md)). It runs on Linux and on Apple silicon, with llama.cpp locally or a cloud
provider, for one person's box ([README.md:46-56](../README.md)).

## What works today

| area | what it does now | where it is described |
|---|---|---|
| Sessions | outlive the head; a daemon that comes back restores them; a late head is drawn from stored facts, not a summary | [implementation-plan.md:274](implementation-plan.md), [socket.md](socket.md) |
| The binary | roles and verbs (`daemon`, `tui`, `askpass`, `stats`, `fb`, `migrate`, `attach github`), each with its own `--help` | `crates/letibot/src/main.rs:328` |
| Models | llama.cpp locally, or DeepSeek, GLM, Grok; per level; typed trouble; one retry on an interrupted send; `[fallback]` | [providers.md](providers.md) |
| Templates | chat template per dialect, and a fidelity gate that must print `GATE PASS` | [chat-templates.md](chat-templates.md) |
| Compaction | leveled; the record is sized by the room the window has; the summary turn sees a reduced span | [compaction.md](compaction.md) §7 |
| The gate | the boundary it measured, the never-write list, last-match-wins permission rules, an adjudicator with a corpus | [boundary-and-adjudication.md](boundary-and-adjudication.md) |
| The queue | a reviewer, a rebase onto the target's tip, the repository's own gate from `main`, then the landing | [gate.md](gate.md) |
| Subagents | real sessions: steerable, downgradable, in a worktree or a VM; 38 tools on the default seat | [subagents.md](subagents.md), [leticode.md](leticode.md) |
| Placements | a session or child on another machine; `hosts`, `fanout`, `scenario` | [remote-hosts.md](remote-hosts.md) |
| The head | conversation window, panes, click targets, `!cmd`, `!term` that survives detaching, `ctrl-e` | [tui-testing.md](tui-testing.md) |
| The room | one queue per tag, leases, a launcher that restarts the room, a pane | [room.md](room.md) |
| Plugins | wasm layers, a pane hook, hot load behind a shadow gate, built-in layers | [plugins-plan.md](plugins-plan.md) |
| Skills | pins in the prompt, a per-round offer, `/skills`, `task {skills}` | [skills.md](skills.md) |
| Similarity | an embeddings tier, off by default: internal or HTTP, cached, blended | [embeddings.md](embeddings.md) |
| Bridges | an MCP server for Claude Code, and a monitor that hears a flowy seat | [claude-code-bridge.md](claude-code-bridge.md), [flowy-monitor.md](flowy-monitor.md) |
| Instruments | `stats`, `fb`, host config and its changelog, `decisions`, `transcript` | [metrics.md](metrics.md), [host-config.md](host-config.md) |

## Half built

- **The GitHub reviewer seat.** The poll, the row states, the config, the drafts, the worker and the
  scope pane are in the tree (`crates/github`, `letibot attach github`): a review runs as a brief in
  that pull request's own session and is kept as a draft. What is left is that the seat is its own
  process rather than the daemon's, so the daemon feeds the pane no `github` key, a pull request of
  another repository in an org scope is refused until a mirror per repository exists, and the review is
  not yet R116's panel ([github-reviewer.md:39](github-reviewer.md), [github-reviewer.md:97](github-reviewer.md),
  [TODO.md:58](../TODO.md) R118, which depends on R116 at [TODO.md:73](../TODO.md)).
- **Placements on a real host.** `Placement::RemoteVm` is in the tree with tests of its shape, and the
  live run owes four facts; six packages placed on one host at once is still unproven; a firecode
  backend that runs on the remote host is not built ([TODO.md:166](../TODO.md),
  [TODO.md:312](../TODO.md) R99, [TODO.md:362](../TODO.md) R100, `crates/tools/src/remote.rs:1188`).
- **A host placement's last act.** Processes are swept on stop, and the branch is not fetched home by
  the stop path; a note written on a host lands there but is not read back into this session's prompt
  ([TODO.md:96](../TODO.md) R115, items 1 and 3).
- **The macOS boundary.** The policy half is built; the Seatbelt half is built on a branch and has
  never been proved on a Mac ([TODO.md:244](../TODO.md) R98).
- **Plugin script layers.** Wasm layers work; a layer that is source text with no toolchain stops on
  two decisions, the JavaScript engine and the JS-side contract
  ([plugins-plan.md:715](plugins-plan.md), [plugins-plan.md:742](plugins-plan.md),
  [TODO.md:170](../TODO.md)).
- **Compaction's map.** Eviction landed and nothing recalls what it evicted: there is no addressable
  map in the new base, no `recall` tool, and no standalone `prune`
  ([compaction.md:276-283](compaction.md)).
- **Remote hosts.** No per-host trust, no matrix runner, no subnet lease, no teardown except docker,
  no `remote_sync`, and `MaxSessions` is neither raised nor read ([remote-hosts.md:606-619](remote-hosts.md)).
- **Active memory.** A design with the mechanism it would reuse and nothing built: no producer writes
  a `SegmentMark`, there is no surfacing policy and no `recall` tool ([memory.md:89-103](memory.md)).
- **The one seat.** The plan to replace roles with one seat and a working automode is parked, because
  no locally hosted model passes the oracle corpus ([TODO.md:46](../TODO.md) R120).

## Known broken, known flaky

- **Three core-loop findings are deferred, not fixed.** A parked child's later answer reaches its
  record but not its parent's `task_result` until a restart; an interrupted turn drops a monitor's
  wake it was never told about; a snapshot does not say where its base starts, so a head's ordinals
  drift after a resume ([2026-11-03-harnessd.md:250](reviews/2026-11-03-harnessd.md),
  [:290](reviews/2026-11-03-harnessd.md), [:382](reviews/2026-11-03-harnessd.md), deferred list at
  [:628-636](reviews/2026-11-03-harnessd.md)).
- **A store busy wait can stall a session for thirty minutes and ignores interrupts**; a child can
  answer its parent and then run a `[meanwhile]` turn nobody hears; closing a root session drops held
  results. R91 leaves five items in all, and these three are the ones an operator meets
  ([TODO.md:593](../TODO.md)).
- **The queue can lose a push and leaves debris.** A commit pushed after the first review is never
  re-enqueued, and nothing reaps failed or refused children's worktrees; the filed measurement is 48
  registered trees, 62 directories and 240 GB of checkouts ([TODO.md:668](../TODO.md) R92, items 11
  and 15).
- **An inherited `LETIBOT_SOCKET` outranks the workspace's own socket**, so `--status` and `--stop`
  can act on another workspace's daemon while printing this one ([bootstrap.md:177](bootstrap.md)).
- **One test is a broken instrument.** `loop_closes` drives live inference on `127.0.0.1:8080`, passes
  alone in 11 to 32 s and times out under `cargo test --workspace` whenever the box is busy; it is
  still not gated or `#[ignore]`d ([TODO.md:4794](../TODO.md)).
- **Memory growth is instrumented, not explained.** A harnessd once reached 113 GB; the tested path
  grows nothing now, and the local-model route has never been measured ([TODO.md:945](../TODO.md) R66).
- **Limits, not defects:** on macOS the confined seats refuse to start ([README.md:54-56](../README.md)),
  and the eight install gaps B2 to B9 are open with their done-when written
  ([bootstrap.md:140-195](bootstrap.md)).

## The next five pieces of work, by value

1. **R116's verdict as the reviewer's schema** ([TODO.md:73](../TODO.md) R116). The GitHub seat at
   [TODO.md:58](../TODO.md) now turns a pull request into a review through that pull request's own
   session ([github-reviewer.md:39](github-reviewer.md)); what is left is the shared verdict schema, so
   that a Claude reviewer and a letibot reviewer are interchangeable in one panel.
2. **Close the three deferred core-loop findings** ([2026-11-03-harnessd.md:628-636](reviews/2026-11-03-harnessd.md)).
   Each already has a named smallest fix and each fails silently, so no head can show the operator
   that it happened.
3. **Reap worktrees and re-review pushes** ([TODO.md:668](../TODO.md) R92, items 11 and 15). Dead
   checkouts and a commit that is never reviewed are both met by the operator rather than by a test,
   and neither needs a decision from anyone.
4. **Run a placement on a real host and close its live facts** ([TODO.md:312](../TODO.md),
   [TODO.md:362](../TODO.md), [TODO.md:166](../TODO.md)). Every placement path has tests of its
   shape, and none has carried a commit out of a guest or held six packages at once, so the riskiest
   path in the tree is the one no real run has touched.
5. **Read the rest of bash** ([TODO.md:177](../TODO.md) R94). 6.2% of 161,099 recorded `bash`
   decisions still hold a construct the resolver cannot read, so the gate settles them by asking the
   oracle about text it does not understand.

## Keeping this page true

The file a line names is the authority; this page only points at it. A change that makes a line here
false updates the line in the same commit.
