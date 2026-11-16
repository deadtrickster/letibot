# The box daemon, one orchestration primitive, and job-first tool calls

One design for three leads of the 2026-11-16 review: lead 52 (per-box state lives in per-folder
daemons), lead 32 (`digest` is a hard-coded workflow) and lead 31 (only `bash` backgrounds
itself). Order and rows are in `docs/PLAN.md`, "After the stability bar". Rows are R372-R381 in
`TODO.md`. R360-R371 are `auto-broker-design`'s rows and R359 is `mcp-live`'s. letibot references are at `ce6952213` (`w4-base`), rano references at `a78a740`.
`docs/design/automode-and-broker.md` lives on branch `auto-broker-design` and is cited by section.

Nothing here is built. No code starts before the stability bar is met.

## 1. What is wrong, in one paragraph each

**The box.** There is one `harnessd` per git toplevel (`docs/one-daemon.md:3`). Each one polls the
provider's usage endpoint, keeps its own idea of which model is limited, starts its own MCP
servers, runs its own reaper, prunes the one `metrics.db`, renews the same remote leases and sets
the same slice property. The duplicated work costs requests and ssh round trips. The duplicated
state costs correctness: N readings of one window with N ages, a park one session knows and its
neighbour finds out by spending a request. `docs/one-daemon.md:231` already names the way out as
shape (d), the hybrid. The room coordinator is the one piece that already has that shape.

**Orchestration.** `digest` splits, starts a helper child per part and folds, inside one tool
call, holding the turn for up to 24 parts at 300 s each (`crates/tools/src/builtins/digest.rs:258`,
`:130`). It has no resume. Review panels, sweeps and rebases are run from outside by hand-written
workflows, and on 2026-11-16 that layer failed five ways (PLAN B16). Each failure is a rule that
no code enforces.

**The threshold.** A foreground `bash` past 60 s is moved to the background and its completion is
delivered (`crates/tools/src/builtins/bash.rs:1464`). Every other tool runs synchronously on the
turn's thread at `crates/tools/src/runtime.rs:2382` with only a cooperative 180 s bound
(`runtime.rs:203`).

The three meet in one place. A fan-out needs a budget per provider account and per host, and
only a process that sees every daemon can hold one. A fan-out is a job, and `digest` only stops
holding the turn if any call can become a job.

## 2. The state map

Every piece of state or loop that a per-folder daemon holds today and that is really per box or
per account. "Tier" is the phase that moves it (section 9).

| # | State | Today | Really | Evidence | Moves to boxd | Folder daemon keeps | Tier |
|---|---|---|---|---|---|---|---|
| 1 | Usage polling | one poller per endpoint per process, every 120 s, registry is a process static, no file, no lock | per account | `crates/provider/src/usage_poll.rs:15`, `:318`, `:352`; started per session at `crates/harnessd/src/harness/usage.rs:123` | the poller and the reading | the per-session threshold notices in the transcript (`harness/usage.rs:94`), the `models.usage` row it serves heads (`harness/usage.rs:212`) | B |
| 2 | Limit and park knowledge | per session, by design. Herd handling is 2 s of jitter | the fact "model M on account A is refused until T" is per account. The park of a turn is per session | `crates/harnessd/src/harness/limit.rs:214`, `:11`, `:57`; stored in the session row at `limit.rs:525` | an account ledger of limits, advisory | `LimitState`, the stopped turn, the way back, the fallback choice (`harness/turns.rs:3646`), the wake clock (`crates/harnessd/src/sessions.rs:2815`) | B |
| 3 | Provider budget | none. A fan-out reads no window before launching (lead 37) | per account | `TODO.md` R336, "Lead 37 is not built" | reservations against the live window | the estimate, from its own canary | B |
| 4 | The `/models` listing | fetched once per daemon, process static | per account | `crates/provider/src/registry.rs:604` | one fetch per account | the card | B |
| 5 | `providers.toml` writes | read-modify-write through one fixed tmp name, no lock | per user | `crates/provider/src/keys.rs:743`, `:804` | the only writer | reads, through `keys::resolve` (`keys.rs:203`) | B |
| 6 | MCP servers | one pool per process keyed by the spec list, stdio servers are children of the daemon | per box for servers that hold no project state | `crates/harnessd/src/mcp.rs:1`, `:165`; `crates/harnessd/src/harness/open.rs:2200`; `crates/mcp/src/wire.rs:117` | servers the operator marks `scope = "box"` | folder-scope servers, the catalog a session sees | E |
| 7 | Disk reaping | one daemon per box by `flock` on `reaper.lock`, every 600 s. With no daemon up, nothing reaps | per box | `crates/harnessd/src/reap_pass.rs:8`, `:270`, `:283`, `:32`; `docs/lifecycle.md:18` | the whole disk half | nothing | C |
| 8 | Idle reaping | every daemon, every 60 s. A daemon cannot reap itself | per box (it reads the store and the kernel, never a daemon) | `reap_pass.rs:28`; review panel P5, F2 | the idle half too | rotation of its own log | C |
| 9 | VM reaping, port-block release, remote lease renewal | each daemon, on its own clock | per box | `crates/harnessd/src/vm_reap.rs:24`; `crates/harnessd/src/leases.rs:14`; `crates/harnessd/src/supervise.rs:136` | all three | supervision of its own process records (`supervise.rs:14`) | C |
| 10 | `metrics.db` prune and size cap | every daemon's writer thread runs both | per box | `crates/metrics/src/store.rs:697`; `crates/harnessd/src/instruments.rs:351` | prune and cap | its own writes | C |
| 11 | `letibot.slice` `MemoryHigh` | set by each scoped daemon at start | per box | `crates/harnessd/src/cli.rs:475` | set once | nothing | C |
| 12 | Build placement | by hand: target dirs named in briefs, `/build/gate.lock` in a script | per box | `TODO.md` R294; `scripts/gate-at.sh:12` | placement, admission, the gate class | running the build in its own seat | D |
| 13 | Host budget | ssh bound of 8 per host per process, a ControlMaster per daemon pid, review cap per daemon | per host, across the box | `crates/tools/src/rpc.rs:183`; `crates/tools/src/remote.rs:772`; `crates/harnessd/src/mergequeue/review/hosts.rs:66` | host slots by class | its ssh masters | D |
| 14 | Daemon registry | records globbed from the runtime dir, plus `daemon_life` beats. A reader attaches to every socket | per box | `crates/bridge/src/fleet.rs:1`; `docs/store-split.md:74` | the live list: folder, pid, build, protocol | its record file and its beat | A |
| 15 | Room coordinator | already one per box, its own lease and socket | per box | `crates/room/src/lease.rs:1`; `crates/harnessd/src/config.rs:641` | becomes a service of the same process | a link per top-level session (`crates/harnessd/src/room/link.rs:1`) | A |
| 16 | CLI broker, egress proxy, CA | designed as part of each daemon | per box: one CA, one policy, one credential seat spawner | `automode-and-broker.md` sections 5 and 6 | all three | the card, the gate verdict, the call id | F |
| 17 | VM boot slots | lock files shared by every daemon | per box, and already correct | `crates/tools/src/firecode/boot.rs:90` | nothing | as is | - |
| 18 | Port blocks, queue leases, session writers | rows in `shared.db` under `BEGIN IMMEDIATE` or compare-and-swap | per box, and already correct | `crates/tools/src/lease.rs:28`; `crates/harnessd/src/mergequeue/lease.rs:13`; `crates/tokencore/src/store/split.rs:96` | nothing | as is | - |

Looked at and left where they are:

- **The merge queue.** Keyed by repository, leased in `shared.db`, one worker per lease held
  (`crates/harnessd/src/mergequeue/serve.rs:1`, `:3760`). The gate runs the repository's own code
  and reviewers are `task` children of a session. Both belong in a seat, so the queue stays with
  the folder daemons. boxd only hands the gate its slot (row 12).
- **Sessions, transcripts, heads, grants, the supervisor's process table.** Per project by the
  store split (`docs/store-split.md:35`).
- **The vocabulary and the embedder.** Per process loads (`docs/one-daemon.md:139`,
  `crates/harnessd/src/config.rs:657`). Sharing them is a memory question, not a correctness
  one. Not in this plan.
- **The guard endpoint.** A box property a daemon reads and calls over HTTP
  (`crates/provider/src/keys.rs:54`). Nothing to own.

## 3. The box daemon

### Shape

One trusted process per box and user, outside every seat. It is the room coordinator's process:
`harnessd --room` grows into `harnessd --box` and serves two sockets, the room's and the box's.

Why the same process: the coordinator already has everything a box daemon needs and the plan
would otherwise build twice. One coordinator per box by a lease with a beat and a pid check
(`crates/room/src/lease.rs:12`, `:163`). A launcher that ensures it before a session opens
(`scripts/letibot:2082`, `crates/bridge/src/daemon.rs:659`). Socket activation under systemd and
launchd (`crates/harnessd/src/room/activation.rs:1`, `:16`). No idle exit
(`crates/harnessd/src/room/coordinator.rs:74`). Clients that redial with backoff
(`crates/room/src/client.rs:196`). The cost is blast radius: a broker bug restarts the room. Rooms
keep their state in the store beside the socket and clients redial in 250 ms to 5 s, so that
restart is a gap, not a loss. This is open question 1.

The box socket is `$XDG_RUNTIME_DIR/letibot/box-<host key>.sock`, mode `0600`,
`LETIBOT_BOX_SOCKET` to override, as the room has `LETIBOT_ROOM_SOCKET`. Tests never touch the
operator's.

### Who may connect

The peer's pid is a kernel fact (`crates/sessionlog/src/peer.rs:143`), classified as
`docs/socket.md` classifies a daemon's peers:

- a folder daemon: the peer holds a `FolderClaim` (`crates/sessionlog/src/server.rs:71`) and says
  its key in `hello`. boxd checks the claim's lock holder is that pid.
- a launcher or CLI of the operator: a peer in no letibot job cgroup. Read-only verbs and
  `stop`.
- a tool call: a peer inside any daemon's job cgroup. Refused by name on the box socket, always.
  A seat reaches boxd only through the broker and proxy sockets bound into its view, which are
  per session and speak their own protocols.

Two invariants keep the trust small:

1. **boxd never runs agent-authored input.** It grants placements, it does not run builds. It
   pools MCP servers the operator's own config names, never a project's. The one thing it execs
   is a credentialed CLI in a credential seat, with argv the gate has judged (broker design,
   section 5).
2. **boxd holds soft state only.** Everything it knows is rebuilt from its clients or from a
   provider within one poll. Nothing durable depends on it having been up.

### Wire

Its own number, `BOX_PROTOCOL_VERSION`, starting at 1, with the room's rules
(`crates/room/src/wire.rs:16`, `:21`, `:64`): a client outside `OLDEST_CLIENT..=VERSION` gets
`Bye` with the sentence that names the restart. One addition, because boxd is several services:
`welcome` carries `services: {accounts: 1, housekeeping: 1, ...}`, each with its own small
version. A daemon uses a service only when boxd announces a version it speaks. Otherwise that
service is "away" and the daemon degrades for that service alone.

Frames, line-delimited JSON:

- `hello {key, folder, pid, build, protocol}` then `welcome {services, instance}`. `instance` is
  boxd's start time. A changed instance tells a daemon to re-say its state.
- accounts: `usage.watch {account}` then pushed `usage {account, windows, read_ms}`.
  `limit.report {account, model, until_ms, source}` and pushed `limit {account, model,
  until_ms}`. `budget.reserve {account, tokens, ttl_s, who}` answers `granted | split {parts} |
  refused {why, resets_ms}`. `budget.release`. `models.list {account}`. `key.store {provider,
  key}`.
- placement: `place.acquire {folder, class, cmd}` answers `placed {slot, env, target_dir} |
  queued {position}`. `place.release {slot}`. `host.acquire {host, class}` and `host.release`.
- pool: `mcp.open {server}`, `mcp.call {server, tool, args, call}`, `mcp.result`, `mcp.cancel`.
- registry: `daemons`, `status`.
- seats (phase F): `seat.open {session, cgroup}` answers the broker and proxy socket paths.
  `ask {session, card}` from boxd, `answer` from the daemon.

`account` is the endpoint plus a fingerprint of the resolved key, never the key. Two keys on one
provider are two accounts, which keeps `limit.rs:214`'s rule true: one key's refusal says nothing
about another's.

Every acquire is a lease on the connection. When a daemon's connection drops, its reservations
and slots are released after a grace of 60 s, the figure `leases.rs:14` already uses.

### Failure: boxd down or restarted

The rule: **a folder daemon with no boxd behaves as it does today, minus the duplicated polling,
and says so once.** No session stops because boxd stopped.

| Service | boxd away | boxd restarted |
|---|---|---|
| Usage | the last reading stays, with its age shown. No local poller. The on-refusal read stays local (`harness/usage.rs:47`, 5 s) | daemons re-send `usage.watch`. First reading within one poll |
| Limits | a session learns a limit from its own refusal, as now | daemons re-report the limits their sessions hold |
| Budget | a fan-out checks its own daemon's last reading and says "box daemon away: checked against this daemon only" | reservations are re-sent by the runs that hold them |
| Housekeeping | nothing reaps. `letibot gc` by hand still works (`docs/lifecycle.md:819`) | the next pass runs. The period is kept in a file, as `reaper.lock` keeps it now |
| Placement | a build runs unplaced after a 5 s wait, with a notice | slots are re-acquired by the builds still running |
| MCP box pool | `mcp_call` answers "not run: the pool is away" | servers restart. A call in flight settles "may have run" |
| Broker and proxy | fail closed. The shim says letibot's broker is not running. A proxied seat has no egress | in-flight brokered calls end with a named error. boxd drains them first on an asked restart |
| Room | as today (`crates/harnessd/src/harness/open.rs:2471`) | as today (`crates/harnessd/src/room/link.rs:398`) |

Usage has no local fallback poller on purpose. A second poller is the same rule in a second
place, which is the review's own finding about this tree. A stale figure with its age is honest
and boxd is back within seconds under activation.

Presence is not capability. A daemon does not treat an open box connection as a working boxd.
Each pushed reading carries `read_ms`, and a reading older than three polls is shown as stale
whether or not the socket is up.

### Upgrade and skew

- Install restarts boxd first. It holds soft state, so a restart costs one poll. Without an asked
  restart, boxd notices its own binary was replaced the way a daemon does
  (`crates/harnessd/src/exe_stamp.rs:30`) and exits when no brokered call is in flight.
  Activation starts the new one on the next connect.
- boxd serves older daemons down to `OLDEST_CLIENT`. A daemon newer than boxd uses the services
  boxd announces and treats the rest as away. So neither order of upgrade stops work.
- The registry makes the stale-build notice (I71) a box fact: boxd knows each daemon's `build`
  and says which folders run an old one, in `letibot --status` and to the heads of those folders.
- There are now four wire numbers: head 79 (`crates/sessionlog/src/protocol.rs:15`), room 13,
  box 1, and rano's own copy at 72 (`src/letibot.rs:26` in rano). `scripts/wire-skew.sh` gains
  the box pair: old daemon against new boxd, new daemon against old boxd.
- A remote box (lubuntu, a VM host) runs its own boxd. Two boxes on one account are still two
  pollers. One reading for the fleet is the hub question of `docs/cross-host-identity.md:55` and
  is not answered here.

## 4. The services, one by one

**Accounts (rows 1-5).** One poller per account. `usage_poll`'s registry and gate move into boxd
unchanged. The daemon side of `usage_poll::watch` becomes a subscription. The limit ledger is
advisory: a session about to send to an account the ledger marks limited parks without spending
the request, and sessions parked on one account are woken staggered by boxd instead of by 2 s of
jitter. The session's own refusal stays the truth and the ledger never un-parks a session.
Budget is section 6. `key.store` gives `providers.toml` one writer.

**Housekeeping (rows 7-11).** Both reaper halves run in boxd. The reaper already reads the store
and the kernel and never a daemon (`crates/tools/src/lifecycle/reap.rs:9`), so the move is where
the thread lives. It deletes the `reaper.lock` election and its flaky test (lead 44), closes
"with no live daemon nothing reaps", makes the unshipped `letibot-gc.timer` (decision 12)
unnecessary, and ends panel P5's F2: a daemon that could not idle-close itself is closed by a
process that is not it.

**Placement (rows 12-13).** R294 as a boxd service, with one change to its shape. R294 says the
service runs the build. Here it only places it: the answer is a slot, a target dir and an env
(`CARGO_TARGET_DIR`, the cache server), and the daemon runs the command in the session's seat.
boxd reads `/proc/pressure/io` and per-disk load, caps concurrent builds and link steps, and
queues past the cap. A gate is the class `gate` with one slot per box, which replaces
`/build/gate.lock`. Host slots are the same mechanism keyed by host: one `suite` slot per host,
and a host the operator marks `ask` in `hosts.toml` places nothing until told.

**MCP pool (row 6).** A server is box-pooled only when the operator's own `leticode.toml` says
`scope = "box"`. The default stays `folder`, in the daemon, as now. Reasons: a stdio server may
hold a cwd or a project index, and a project file must not be able to make boxd run a command.
boxd multiplexes JSON-RPC ids. The daemon sees a proxy catalog.

**Broker and proxy (row 16).** This amends `automode-and-broker.md` in three sentences. "The
host broker is part of the daemon" becomes "of the box daemon". "A host-side proxy per daemon"
becomes one per box, still one listener per session. "The CA is made per daemon start" becomes
per boxd start. Everything else stands. What the move buys: one CA and one egress table for the
box, package daemons that live an hour do not each mint a CA, and a credential seat is spawned by
one process. What it costs: the card. The broker walks the peer's parents to the job root
(broker design, "Who is asking"). boxd reads the peer's cgroup, finds the owning daemon in the
registry, and sends `ask {session, card}`. The daemon maps the cgroup to its call id, shows the
card through its existing path and answers. The gate verdict stays in the daemon, which has the
session's trail. boxd executes verdicts and does not make them.

## 5. One orchestration primitive: the flow

### What it is

A `flow` is a job that plans items, starts a child per item, checks each child, folds the
answers, and settles once. It is the generalisation of two things the tree has: `digest`'s fold
over `TaskRunner` (`digest.rs:198`), and background `fanout`'s job of jobs, an `Aggregate` held
in the session's job table (`crates/tools/src/exec/host.rs:591`,
`crates/tools/src/builtins/fanout_bg.rs:65`).

It runs in the folder daemon, in the session that asked. The call returns
`Invocation::backgrounded` (`runtime.rs:137`). The existing sink watches the handle
(`crates/harnessd/src/jobwatch.rs:1944`) and the settlement takes the path every job takes:
`JobSettled` to heads, a completion in the queue, a store row, the bell (`jobwatch.rs:1732`,
`:1301`), then `delivery::decide` (`crates/harnessd/src/delivery.rs:239`). `job_output` reads its
journal and `job_kill` stops every child. One new word: `BackgroundKind::Flow`
(`jobwatch.rs:48`), with a `[flow] ` settlement row beside `[job] ` and `[task] `
(`crates/harnessd/src/harness/deliveries.rs:296`, `:383`).

### A script is data

A flow script is a TOML file of stages. No embedded language. Each stage has four parts:

- `split`: where items come from. Built in: transcript rows by byte budget, a file glob, a list
  of commits or branches, an explicit list, the outputs of the previous stage.
- `map`: the child for one item: role, mode, provider and model, the prompt template, the
  deadline.
- `fold`: `chain` (sequential, each child gets the answer so far, which is `digest`), `reduce`
  (one child reads every output), or `table` (no model, one row per item).
- `verify`: checks before an output is used. Always: the child's actual model, mode and exit
  against what was asked (`AGENTS.md:170`). Optional: a command, or a verifier child.

Scripts ship in `flows/` and a project may add its own. The four first scripts decide whether
data is enough. If one of them cannot be written as stages, that is the evidence for a language,
and not before (open question 5).

### The rules, built in

Each rule is from PLAN B16 or the hands-off template, and each is enforced at plan time or
recorded in the journal. None is advice in a brief.

1. **A worktree per child.** A child whose mode can write gets its own worktree through
   `start_worktree` (`crates/harnessd/src/harness/children.rs:2668`). A script that maps a
   write-capable child with no worktree is refused before anything starts. Read-only children
   share the workspace.
2. **Waits on real exits.** The flow waits on `TaskRunner::collect` and `wait_job`, the condvar
   and the process exit (`jobwatch.rs:1820`, `:1671`). There is no count and no sleep used as a
   clock. A child killed at its deadline is recorded `killed` and its output is not used.
3. **A canary first.** Item one runs alone through map, verify and its fold step. The flow checks
   real wall time, a non-empty output and the model and mode it actually got. Only then does the
   rest start. The canary's measured tokens and time become the estimate for the budget.
4. **No silent caps.** Every limit is in the plan row the call returns: items, width, deadline.
   Width is a queue and never truncates. A limit that bites refuses at plan time or is said in
   the settlement in `digest`'s words, "NOT EVERYTHING" (`digest.rs:450`). The settlement table
   is cut at a stated line count with the rest one `job_output` away, as
   `fanout_bg.rs:24` does.
5. **A backup before resume.** See below.
6. **A budget per provider and per host.** Section 6.
7. **The record.** For each child the journal holds what was asked and what ran. `SubagentRun`
   (`crates/tokencore/src/store/subagents.rs:136`) gains the served model, the mode and the exit,
   which no record holds today. A mismatch fails verify and the row says which field.
8. **Children end with the flow.** `job_kill` on the handle ends every child. A session that
   closes ends its flows. This is lead 29's rule inside one daemon.

### Journal, resume, restart

A flow has a directory, `<data dir>/flows/<run id>/`: `plan.json`, an append-only
`journal.ndjson` (planned, started, settled, verified, folded) and `out/<item>` per child.

The resume cache key of an item is a hash of its stage definition, its content, the asked model
and the prompt template. `flow {resume: <run id>}`:

1. copies `journal.ndjson` and `out/` to `backup-<time>/`, first, always;
2. recomputes every key and reports before starting: "14 of 20 items are cached, 6 will run: the
   prompt template changed";
3. runs the misses, canary first if the canary is among them.

A hit needs a settled and verified journal entry with the same key. An output with no verify
entry is a miss.

Across a daemon restart a flow is resumed from its journal by the same path, under
`restore_within`, as a mid-task child is (`crates/harnessd/src/restart.rs:16`). Its children are
owned by the flow. Today a `helper` child is never resumed and never delivered
(`crates/harnessd/src/subagent_record.rs:445`), and a background `fanout` loses its handle
(`fanout_bg.rs:3`). A flow child is a third kind: settled children are read from `out/`, running
ones are restarted by the flow, and none is handed to the model as news.

### The scripts

- **`digest`**, first. `split` is the transcript search in 24 000-byte parts, `map` is the part
  reader seated read-only (`digest.rs:54`, `:187`), `fold` is `chain`, and the 24-part ceiling
  is in the plan row. The `digest` tool stays as the door: it starts the flow and waits on it in
  the foreground. Past the threshold it is a job like any other (section 7). A digest
  interrupted by a restart resumes at the part it reached.
- **Review panel**, second (`~/Projects/hands-off/templates/adversarial-review.md`). Two stages
  for two rounds. Positions map over providers, read-only. Round two takes round one's outputs.
  The fold is `table`, and the positions are kept verbatim in `out/`, which is the record the
  template asks for.
- **Sweep.** Map over files or rows, a worktree per child, verify by a command, fold `table`.
- **Rebase.** Map over branches, a worktree per child, verify by the touched crates' tests, fold
  `table` of branch, tip and verdict.

`fanout` over hosts keeps its own tool. Its `Aggregate` and the flow's are one type by phase I.

## 6. The budget (lead 37)

Before a flow starts children beyond the canary it reserves, per account and per host.

**Per account.** `tokens = canary tokens x remaining items`, per provider the script names. The
flow sends `budget.reserve`. boxd compares against the live window: what is left in the reading,
less other reservations, less the operator's share (`[budget] keep`, default 30% of every window
for interactive sessions). The answers:

- `granted`: start.
- `split`: the script named more than one provider for the stage, or allows waves. The flow
  starts what fits and queues the rest for the next window, or moves items to the other
  provider. Either is in the plan row before it happens.
- `refused`: nothing starts beyond the canary, and the result names the window, the figure and
  when it resets.

The 2026-11-16 case: 20 positions at about 2M tokens each on one 5 h window. The canary measures
2M, the reservation asks for 38M, and boxd refuses or splits. rano does not park.

An account with no usage endpoint has no window to read. The flow says so in the plan row and
runs at a width the script states, which is a stated cap and not a silent one.

**Per host.** A child placed on a host takes `host.acquire {host, class}`. One `suite` per host.
A host marked `ask` refuses until the operator answers.

**Without boxd.** The flow checks its own daemon's last reading, with no knowledge of other
daemons' reservations, and the plan row says so. This is the form that ships in phase H, before
boxd exists. Proving test, from B16: `a_fan_out_over_a_providers_live_window_refuses_or_splits`.

## 7. Past the threshold, any call is a job (lead 31)

### The rule

**A call that can outlive the threshold is a job from its first instant. The foreground is a wait
on it. Past the threshold, or the moment the operator types, the wait ends and the job goes on.**

This is what `bash` does: the command is a job of the process host from the start, the foreground
loop waits on it, and promotion only stops the wait (`bash.rs:1417`, `:1464`). The design moves
that loop out of `bash` into the runtime.

### The mechanism

`Tool` (`runtime.rs:607`) gains one method with a default:

```rust
fn start(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Started {
    Started::Done(self.invoke(ctx, args))
}
// Started::Running(Arc<dyn Aggregate>) for a tool whose work can go on without the turn.
```

`ToolRuntime::run` calls `start`. On `Running` it holds the aggregate in the session's job table
and runs the one foreground loop: wait in ticks, forward progress, honour an interrupt, and at
`[exec] foreground_seconds` return `Invocation::backgrounded` with `Backgrounding::Promoted`
and mark the completion awaited (`bash.rs:1038`). A tool that returns `Done` is untouched, which
is every short tool.

A running `invoke` cannot be moved by the runtime, because a call borrows the session's sink and
backend (`runtime.rs:188`). So the rule is opt-in by structure: a tool is promotable when its work
is split from its wait. That is the honest limit of "every tool", and the table says which.

Three things change for everything:

- **The operator typing is a threshold of zero.** `digest` already stops between parts when the
  operator types (`digest.rs:389`). The runtime does it for every running call, and the work
  goes on as a job.
- **One knob.** `[exec] auto_background` and `foreground_seconds`
  (`crates/leticode-config/src/lib.rs:933`) govern every tool.
- **An explicit timeout is the model's ask.** As with `bash`'s `timeout_ms`
  (`bash.rs:469`), a call that names its own deadline holds until then, up to the tool's
  maximum. The operator typing still ends the hold.

### The tools

| Tool | Class | What happens at the threshold | Care |
|---|---|---|---|
| `bash` | job already | as now. Its loop moves to the runtime | the tty and password-ask parts stay in `bash` |
| `task`, `task_start`, `fanout {background}` | always background | nothing | none |
| `digest`, `flow`, blocking `fanout` | `Running` | the flow goes on, the settlement is the answer | the answer is the size of an answer by design, so it may ride in the settlement row |
| `job_wait`, `task_result` with a timeout, queue and room waits | a wait, not work | the wait returns "still running, its completion is delivered" and marks it awaited (`crates/tools/src/builtins/jobs.rs:876`, `task.rs:2198`) | a wait never becomes a job, there is already one |
| `web_fetch`, `web_search` | `Running`, on a worker thread | the fetch goes on. 30 s per hop and 5 hops is 150 s worst case (`crates/webfetch/src/lib.rs:202`) | the body is untrusted. The settlement row carries the handle and the facts, never the body. The body is read with `job_output`, as a tool result |
| `mcp_call` | `Running` | the JSON-RPC request stays pending and is not cancelled | not idempotent. The timeout today sends `notifications/cancelled` and answers "may have run" (`crates/mcp/src/lib.rs:23`, `:170`). Promoted, the server's `timeout_seconds` is the job's deadline, not the turn's hold. A pending call lost to a restart settles "may have run" and is never re-sent |
| a permission card, an operator ask | a wait on a person | nothing | not work. Bounded by the card's own rules |
| `edit`, `write`, `todo`, `read`, `grep`, `glob`, pooled reads | `Done` | never promoted. The cooperative 180 s bound stays (`runtime.rs:404`) | a call that changes session state must finish in its turn |

The settlement rule that comes out of `web_fetch` is general: **a settlement row is a user-role
row, so it carries the job's own facts and a bounded report, and payload from outside the box is
only ever read through a tool result.** Otherwise a fetched page arrives in the conversation with
the wrong provenance, which is B3's finding by another door.

Restart: a promoted job that is not a host process (a fetch thread, a pending MCP call) does not
survive the daemon. Its `running` row (`jobwatch.rs:1508`) is settled at reopen as "lost with the
daemon, may have run". Jobs are reminded, never re-run (`restart.rs:18`). A flow is the
exception, because its journal makes a re-run safe.

One stale comment to fix on the way: `crates/tools/src/exec/monitor.rs:25` says the promote
threshold is 15 s. It is 60 (`bash.rs:550`).

## 8. rano, the head and the renderer

Three different things carry the name:

- **rano's panel.** rano has its own client for the head wire, by design not letibot's crate
  (`src/letibot.rs:2`). One link per folder (`src/main.rs:562`), found by shelling to
  `letibot --where --ensure` (`src/letibot.rs:1415`). It parses the turn events and drops every
  frame and event it does not know (`src/letibot.rs:984`, `:1222`).
- **rano's widgets.** `rano::agent::*` are view models with no state (`src/agent/mod.rs:4`).
  The letibot head fills them (`crates/tui/src/ui/panes/jobs.rs:6`).
- **The pin.** letibot pins rano by git rev in six manifests (`crates/tui/Cargo.toml:51` and
  five more). `docs/rano-integration.md:4` names an older rev and is stale.

The design choice that keeps this small: **the head wire does not change shape, and a head keeps
one socket.** Folder daemons keep their sockets and sessions. Box data reaches a head through its
folder daemon, the way room chat is relayed (`crates/sessionlog/src/server.rs:1118`,
`crates/harnessd/src/room/operator.rs:14`). The usage row stays the `models.usage` setting
(`crates/sessionlog/src/protocol.rs:75`). Parks stay `Warning` codes
(`crates/tui/src/app/limit_notice.rs:9`).

What a refactor touches, in order of need:

1. **rano panel, nothing forced.** Unknown frames are dropped, so phases A-I do not break it.
   The comparison R357 already owes (`PROTOCOL_VERSION` 72 against the daemon's `Hello`) should
   land before any head-wire bump from this plan, so a skew is said and not drawn.
2. **`--where --json`** gains `box: {live, protocol}`. rano may say "the box daemon is away" in
   the panel's status line. Optional.
3. **Widgets.** The header's usage takes an age (`src/agent/header.rs:3`). The jobs row takes a
   kind and, for a flow, a child count and progress (`src/agent/jobs.rs:3`). A new `agent::box`
   pane: accounts with window and age, limits, reservations, placements and queue, pools,
   daemons with build. `agent::queue` has no user today (`src/agent/mod.rs:65`). It is used or
   deleted.
4. **The head.** `[flow] ` joins the settlement groups
   (`crates/tui/src/ui/transcript/job_card.rs:284`). `JobSettled` and the jobs frame carry the
   new kind (`crates/tui/src/app/events/children.rs:75`,
   `crates/tui/src/app/events/frames.rs:156`). A `/box` pane is filled by relayed ops. The fleet
   reader stops attaching to every folder's socket and asks the registry
   (`crates/bridge/src/fleet.rs:1`). A new word in a closed enum moves `PROTOCOL_VERSION`
   (`docs/socket.md:98`), once, for the flow kind.
5. **The panel showing settlements live.** Today a `[job]` row reaches rano's panel only on
   replay, as a note (`src/panel.rs:490`). Showing a flow settle live is new parsing in rano's
   client. Wanted, not needed.

Every rano change is a branch plus a DM to `cc@rano`, then a rev bump in the six manifests. The
letibot side lands behind the bump, never before it. `crates/tui/tests/rano_editor_head.rs:16`
is the drift check and gains the flow kind.

The larger rano refactor the operator expects is not forced by this design. It becomes possible:
with one registry and one account view, the panel can stop being one link to one folder and
become a view of the box. That is a rano design for `cc@rano`, after phase J shows what the box
pane wants.

## 9. Phases

Each is one TODO row and lands through one full gate. "Alone" means it ships with nothing else
from this list.

| Phase | Row | What | Needs | Alone |
|---|---|---|---|---|
| G | R372 | Job-first tool calls: `Tool::start`, the one foreground loop, the operator-typing rule, the settlement payload rule | - | yes |
| H | R373 | The flow: journal, resume, canary, verify, the local budget. `digest` as its first script | G | yes |
| A | R374 | boxd skeleton in the coordinator's process: box socket, wire, peer classes, registry, `--status`, wire-skew pair | - | yes |
| B | R375 | Accounts: usage, limit ledger, `/models`, `providers.toml` writer, budget reservations | A | yes. H's budget becomes box-wide when both are in |
| C | R376 | Housekeeping: both reaper halves, VM reap, lease release, remote renewals, metrics prune, slice | A | yes |
| D | R377 | Placement: R294 as a boxd service, the gate class, host slots | A | yes |
| E | R378 | MCP box pool, opt-in per server | A, G for a promoted call | yes |
| F | R379 | Broker, proxy and CA hosted by boxd. Amends R364-R366 | A, and those rows | no, it is those rows' host |
| I | R380 | Scripts: review panel, sweep, rebase. A bridge door. `fanout` on the flow's aggregate | H, B | yes |
| J | R381 | Head and rano: `/box` pane, flow rows, usage age, registry-backed fleet, R357's comparison | B, H | per item |

Order: **G, H, A, B, C, then D, E, F, I, J.**

- G and H first. They need no new process, they are what the operator hits daily (`digest`
  holding a turn), and PLAN already names lead 32 as the first post-stability feature.
- A before the broker's R364 is built. The broker design puts its host in the folder daemon.
  Building it there and moving it is the work twice. A is small and settles the host.
- B is lead 52's headline and lead 37's second half. C deletes an election and a flaky test.
- D, E, F and I are independent of each other. J trails whatever it draws.

Each phase removes what it replaces in the same landing: B deletes the per-daemon poller start,
C deletes `reaper.lock`, D deletes `gate.lock` from `scripts/gate-at.sh`. No phase leaves two
owners behind a flag for longer than one release.

## 10. Risks

1. **One process to lose.** boxd down takes usage freshness, reaping, placement and, after F,
   every brokered call and every proxied seat. Mitigation: soft state, activation restart, the
   degrade table, a drain before an asked restart. The broker's fail-closed is the price of the
   broker, not of boxd, but boxd makes it box-wide.
2. **Trust concentration.** The process that holds the CA and spawns credential seats also talks
   to every daemon. The two invariants of section 3 are the defence, and each needs a test: a
   tool-call peer is refused on the box socket, and a project file cannot name a box-scope
   server.
3. **The room shares the blast radius** (open question 1).
4. **Skew grows.** Four wire numbers and a pinned rev. The per-service versions keep a skew to
   one service, and wire-skew gains a pair, but the matrix is real.
5. **A cache hit that should have been a miss.** A resumed flow uses a stale output. The key
   covers stage, content, model and template, and a hit needs a verify entry. The backup makes a
   wrong resume recoverable.
6. **An estimate that is wrong.** The canary's tokens may not stand for the rest. The
   reservation is re-checked as children settle, and a flow over its reservation stops starting
   children and says so.
7. **A late result confuses the model.** A call it expected inline arrives as a settlement. The
   promoted result says so in `bash`'s words, and the completion is marked awaited so the turn
   is woken. `mcp_call` is the sharp one, because the model may retry a call that is still
   running. The promoted result must say "do not call it again".
8. **Tests that read the box.** The review found tests reading live state (leads 34, 44). Every
   boxd test runs against `LETIBOT_BOX_SOCKET` in a temp dir, and a test that finds the
   operator's boxd is a failure.
9. **`shared.db` stays contended.** The queue path is still written by every daemon
   (`docs/store-split.md:336`). This design does not fix that and must not be read as if it did.

## 11. Open questions, with recommendations

1. **One process for the room and the box, or two?** Recommend one, `harnessd --box`, serving
   both sockets. It reuses the lease, the launcher and activation, and the operator asked for
   fewer daemons. Revisit if phase F shows broker restarts disturbing rooms.
2. **Does the broker move to boxd, or stay in each folder daemon with only the CA and policy in
   boxd?** Recommend it moves. A package daemon that lives an hour should not hold a credential
   seat spawner, and the firecode vsock wants one host endpoint. It needs the operator's answer
   before R364 starts.
3. **Do provider keys leave the folder daemons?** That means model traffic through boxd.
   Recommend no. A daemon is a trusted host process, and a proxy for every model round puts
   every session behind one process. Only writes centralise.
4. **Usage with boxd away: stale with an age, or a local fallback poller?** Recommend stale with
   an age.
5. **Flow scripts: TOML stages or a language?** Recommend TOML, and let the four first scripts
   be the test.
6. **The operator's share of a window.** Recommend `[budget] keep = "30%"`, per account, in the
   operator's own config. The figure is a guess. Set it from a week of readings after B.
7. **Does `digest` keep returning inline under the threshold?** Recommend yes. A three-part
   digest finishes in the foreground and reads as now.
8. **MCP default scope.** Recommend `folder`. `box` is opt-in per server, in the operator's
   file only.
9. **Does the queue ever move to boxd,** as shape (d) in `docs/one-daemon.md` drew it? Recommend
   no for this plan. Measure lease churn after C and D first. The gate must stay in a seat
   whatever the answer.
10. **A box pane in rano itself,** not only in the letibot head? For `cc@rano`. Recommend
    deciding after J.
