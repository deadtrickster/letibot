# One daemon per project, or one per machine

Written 2026-10-26 (branch `one-daemon-design`). The operator: *"hmm, so why don't we have a
single daemon?"*

Today there is **one `harnessd` per workspace** — the git toplevel you stand in, so every worktree
of the same repository gets its own process, its own socket, its own copy of a store connection
and its own opinions about the merge queue. A night of parallel packages across worktrees meant
about ten of them at once.

This document is the design question only: what a daemon owns (measured from the code), the four
shapes that could replace that, what each costs, and a recommendation with a migration path in
landable pieces. No code changed here.

**Short answer.** Go to one daemon per **repository** first (§4). It is a key change plus a
completion of work the tree has already done — a session's workspace, its mode, its consent, its
monitors and its jobs are already keyed by the session or the project root, not by the daemon — and
it removes exactly the duplication the operator paid for: N queue-lease candidates per repository,
N gatekeeper sessions per repository, N supervisors ticking the whole process table, N queue
watchers reading the same history. One daemon per **machine** is a second step, and it collides
with things that are genuinely per process: one vocabulary, one placement, one flowy seat, one
`leticode.toml`.

---

## 1. What a daemon owns today

### 1.1 The socket, the record, the lock, the log — and the lifecycle

`scripts/letibot` takes `git rev-parse --show-toplevel` as the workspace (`scripts/letibot:119`),
hashes it (`:274`) and names everything after those twelve hex characters:

```
KEY="$(printf '%s' "$WORKSPACE" | sha12)"      # scripts/letibot:274
SOCKET=…/$KEY.sock   RECORD=…/$KEY.json        # :275-276
LOG=~/logs/harnessd-$KEY.log                   # :283
```

The same hash is spelled a second time in Rust, deliberately and with a contract test
(`crates/letibot/src/launcher.rs:16-24` names four measured keys), a third time in the bridge
(`crates/bridge/src/daemon.rs:82-90`), and once keyed to the store as
`letibot_sessionlog::workspace_key` (`crates/sessionlog/src/lib.rs:190-197`).

Three things follow from that key being the *worktree*:

* **The lock is per folder.** `FolderClaim` is an `flock` on `<socket>.lock`
  (`crates/sessionlog/src/server.rs:88-130`), so "one daemon per folder" is enforced per folder and
  says nothing about two daemons serving one store.
* **A stop is a folder's stop** — `ClientFrame::Stop` ends the daemon and every session on it
  (`crates/sessionlog/src/protocol.rs:1815-1825`, `crates/sessionlog/src/server.rs:1695-1712`).
* **The daemon leaves on its own** when nothing has needed it for `idle_exit_after` (default one
  hour, `crates/harnessd/src/config.rs:784-793`, `crates/harnessd/src/daemon.rs:376-380`). So the
  ten daemons of a busy night are ten *busy* daemons: the ones with nothing to do are supposed to
  be gone, and the ones that are left are each holding a vocabulary, a store connection and a
  supervisor.

### 1.2 The registry and the sessions — and one thread each

`Daemon::serve` binds the socket and serves one in-process `Registry`
(`crates/harnessd/src/daemon.rs:199`). A `Sessions` holds the open harnesses, the registry, and
the per-session clocks (`crates/harnessd/src/sessions.rs:920-1017`). Since T2 each session is
served by its own thread — `Court` (`sessions.rs:844`), armed by `Daemon::run`
(`sessions.rs:1443`, `daemon.rs:272-278`).

A daemon therefore already holds **many** sessions comfortably: worker, subagents, the gatekeeper
and a dozen conversations in one folder are one process today.

### 1.3 The store connection, and the writer claim

**Every session opens its own `Store`** (`crates/harnessd/src/harness.rs:3281-3286`) — plus a
second connection for the corpus sink (`:3297-3305`) — and each session claims `session_writer`
so a second open of the same transcript is refused by name rather than half-written
(`crates/harnessd/src/writer.rs:130`, `crates/tokencore/src/store/writers.rs:61-190`, schema 32).
The queue service and the supervisor open their own too
(`crates/harnessd/src/mergequeue/serve.rs:2206-2211`, `crates/harnessd/src/supervise.rs:1003`).

The store's own module doc records what that costs at scale:
*"one `sessions.db` is shared by every daemon on the box (measured 2026-10-10: ~11 daemons and
dozens of connections on one 2.1 GB file)"* (`crates/tokencore/src/store/floor.rs:4`).

### 1.4 The gatekeeper session — per workspace, not per box

Each daemon owns one review-queue session whose id is derived from its workspace:
`s-gatekeeper-<workspace key>` (`crates/harnessd/src/sessions/gatekeeper.rs:144-172`, computed at
`sessions.rs:1341`, opened by `ensure_gatekeeper` at `sessions.rs:3400`). It used to be the
constant `s-gatekeeper` for the whole box and two daemons appended to one transcript
(`sessions.rs:930-940`); the fix keyed it to the workspace.

The consequence for this design: **ten worktrees of one repository are ten gatekeeper sessions for
one queue.** The board is filled from the rows whose `owner` is this process's lease claim —
`gatekeeper::waiting(store, &me)` (`harness.rs:10419-10438`) — so only the daemon that holds the
repository's lease has rows on its board; the other nine are an opened session, a composed prompt
and an empty board.

### 1.5 The merge queue

The queue is keyed by **repository**, not by workspace: `repository_of_workspace`
(`crates/harnessd/src/mergequeue.rs:624-632`) names *"the checkout it is in (a worktree counts as
its repository)"*, and `repositories_of_workspace` (`mergequeue.rs:638-643`) is that one or each
repository of a folder that is one project of several (`git.rs:181-206`).

Per daemon, the queue is:

* a **lease per repository**, compare-and-swap in the store, heartbeat every 5 s, TTL 90 s
  (`crates/harnessd/src/mergequeue/lease.rs:14-22`, `mergequeue.rs:517-583`);
* a **worker thread per lease held** (`serve.rs:2206-2260`);
* a **watcher** that reads the store's `queue_transition` history every 2 s for moves other
  daemons made (`crates/harnessd/src/mergequeue/watch.rs:1-25`).

The lease claim is one per **process** — `host/pid/<started>`, carrying the binary's own build
name (`lease.rs:76-95`) — which is how a stale daemon holding a lease is told from a fresh one.

Every daemon seated anywhere under a repository is a candidate for that repository's lease —
`mergequeue::serves` (`mergequeue.rs:697-710`) — which is why a package worker's daemon needs
`--no-queue` (`mergequeue.rs:520-525`).

### 1.6 The supervisor — and it supervises the *store*, not the daemon

One supervisor thread per daemon (`crates/harnessd/src/cli.rs:1806`). It ticks every second
(`supervise.rs:63-69`) and each tick reads `Store::open_processes()` — **every open process record
in the shared store, with no owner filter** (`supervise.rs:275-300`,
`crates/tokencore/src/store/procs.rs:365-369`) — then probes each host once and writes what it
found.

So with N daemons on a box, a host with one running job gets N ssh round trips a second and N
processes write the same rows about it. Nothing dedupes that: the line each daemon prints is
silent unless something happened (`supervise.rs:1021-1025`), so the same answer from ten daemons
is ten lines when it does.

### 1.7 Monitors, providers, the oracle

Monitors are **owned by the session** (`crates/harnessd/src/pr_watch.rs:1-12`, the `armed` set at
`sessions.rs:943-947`), and stored by session id (`crates/tokencore/src/store/subagents.rs:113-123`).

The provider client and the model adjudicator are built **per session, at open**
(`harness.rs:5781`, `harness.rs:22943`), and rebuilt on a model switch (`harness.rs:8548`).

### 1.8 The permission gate, modes and consent — already per session

This is the least daemon-shaped thing in the daemon, and §2 returns to it. The durable mapping is
**project root → point**, in the store, with the operator's `consented_at` beside it
(`crates/harnessd/src/modes.rs:1-21`, `crates/tokencore/src/store/modes.rs:13-40`). It is resolved
at every session open, over the session's own workspace, longest-ancestor-wins
(`harness.rs:3505-3534`), and `--mode` is only the default for a root that has no row.

`modes.rs`'s own module doc is an argument for this document: the mapping used to be a
`modes.tsv` each daemon read at start and rewrote whole — *"there is a daemon per workspace, so one
daemon's write undid another's"*. The same note shows in the code: `ModeStore` keeps a per-process
cache and `refresh()` re-reads the table **at every session open and after every write**, *"so no
copy outlives the moment it is read"* (`crates/harnessd/src/modes.rs:100-102`).

### 1.9 What is genuinely keyed by the daemon

| fact | where | why it is per process |
|---|---|---|
| the workspace **default** | `cli.rs:685` (the process cwd) | a daemon needs somewhere to stand before any session exists |
| the **vocabulary** and dialect renderer | `Sessions.parts`, `harness.rs:117-134`, cloned as `Arc`s per court (`sessions.rs:1566-1570`) | one GGUF load per process; measured at 0.6 s (`cli.rs:45-49`) |
| the **placement** (`--host`) | `cli.rs:1284-1301` — *"a fact of the PROCESS"*, written beside the socket | the protocol carries no backend (`SessionWiring`, `sessions.rs:2378-2385` has model, dialect, endpoint, workspace only) |
| the **flowy seat** | `sessions.rs:948-958` | *a seat is a persistent identity with one inbox reader*; `docs/flowy-monitor.md:196-205` |
| the gatekeeper **id** | `sessions.rs:1341` | derived once from the daemon's workspace |
| the **`leticode.toml`** | `cli.rs:1406` (`LeticodeConfig::discover(&cfg.workspace)`) | the project's models are read once, at start |
| the queue's **repository set** | `mergequeue.rs:576`, `serve.rs:2424-2435` | one reading of the workspace per daemon |
| **Parts-mode-store** | `harness.rs:120-125`, `modes.rs:100-102` | one cache per process, re-read at every session open because daemons share the rows |

### 1.10 The bridge's assumptions

`crates/bridge/src/daemon.rs:1-19` states the rule plainly: *"one daemon per workspace, a socket
named by the workspace's own hash … a session it starts must live in the daemon that folder
already has."* `leticode_start` takes a worktree, resolves it to its workspace and calls `ensure`
(`crates/bridge/src/tools.rs:110`), which starts or joins; `leticode_attach` resolves the daemon
the same way, and the name `gatekeeper` resolves through *that daemon's* workspace key
(`tools.rs:181-193`, `:523-525`).

**And one of its own sentences is false today.** `leticode_attach`'s schema says: *"Absolute path
to a folder of the checkout whose daemon holds the session — the daemon is found by the
workspace's hash, so a worktree of the same repository is the same daemon"*
(`tools.rs:651`). A worktree's git toplevel is the worktree, so it hashes differently and is a
different daemon. The sentence is true only for a subdirectory of the same worktree.

### 1.11 The launchers

`scripts/letibot` is where the per-folder rule lives: `WORKSPACE` from the git toplevel
(`:119`), the key, socket, record, store and log (`:274-283`), the liveness probe and the lock
(`:1072-1170`), and `--daemons` enumerating every record in the runtime directory (`:1618`).
`scripts/leticode` sets `LETIBOT_ROLE` and execs `letibot` (`scripts/leticode:34`, `:50`) — it inherits
the whole rule.

`letibot --sessions` and `--continue` are already **store queries, not socket queries** — they
return before `Parts::load`, before the health check and before the socket is bound
(`cli.rs:45-49`, `:34-35`) — and `--continue`'s scope rule is *exact workspace match beats a
descendant* (`cli.rs:2780-2820`).

---

## 2. What is already per session, and therefore cheap

The reason this change is small is that the store already treats a session as the unit that owns
things. `Store::transfer` says it in one list: a row belongs to a session when the session id is
its key — `session`, transcripts and items, todo, jobs and job contexts, monitors, `session_mode`,
the gate's decisions, citations — and *"the queue tables, `project_mode` and `daemon_life` are
keyed by a repository, a root or a process, none of which is a session"*
(`crates/tokencore/src/store/transfer.rs:8-18`, `TABLES` at `:75-97`).

The rest of the half that already works:

* **A session's workspace comes from its own row.** `Harness::open` replaces `cfg.workspace` with
  the stored `workspace_root` and says so, because *"a resumed session's tools are confined to the
  tree the conversation is about"* (`harness.rs:3366-3377`). A daemon seated anywhere can already
  hold a session about anywhere — it needs the row, not the socket.
* **Its mode and consent** come from `project_mode`/`session_mode` per open (`harness.rs:3505-3534`).
* **The config is cloned per session** — `Sessions.base`, *"so a second session is the same daemon
  and not a second configuration nobody typed"* (`sessions.rs:926-929`).
* **The wire already multiplexes**: `NewSession`, `ResumeSession`, `Switch`, `CloseSession`,
  `Peek` (`crates/sessionlog/src/protocol.rs:2161`, `:2193`, `:2214`, `:1838`, `:2229`). A head
  attaching to one socket can reach every session in that process, and the picker already draws
  them.
* **A migration already moves a session between daemons** (`letibot --migrate`, RH4): one session,
  out of one store and into another, verified twice. So "which daemon holds this session" is
  already allowed to change, and the store records `session.lives_on`.

---

## 3. The options

### (a) Status quo — one daemon per worktree

**What changes:** nothing.

**What it costs, measured from the code:** ten worktree daemons of one repository are ten
candidates for one queue lease (§1.5); ten gatekeeper sessions for one queue (§1.4); ten
supervisors probing every host every second and writing the same rows (§1.6); ten watchers reading
`queue_transition` every two seconds (§1.5); ten vocabulary loads and ten sets of threads; ten
logs; and a stale binary on one folder that cannot read the store while its neighbours can
(`crates/tokencore/src/store/floor.rs`, `crates/harnessd/src/store_floor.rs`).

**What it buys:** the blast radius is one folder; `letibot --stop` in one folder touches nothing
else; a daemon's flags (`--glm`, `--bash`, `--host`, `--no-queue`, `--mode`) are that folder's;
and two projects' turns never contend for one process.

### (b) One daemon per repository — every worktree of one repo shares it

**What changes:**

* The socket key becomes `repository_key(workspace)` — the canonical main checkout
  (`mergequeue::repository_of_workspace`, `mergequeue.rs:630-632`, `git.rs:237-239`) — falling back
  to the folder key where there is no single repository (`repos_of_workspace` returns nothing for
  a folder that merely contains repositories, `git.rs:195-197`).
* The gatekeeper id becomes `s-gatekeeper-<repository key>`: **one queue session per repository**,
  which is what the queue was always keyed by (§1.5).
* One lease candidate per repository; one supervisor and one watcher per repository; one
  `Parts`, one `LeticodeConfig`, one placement for the whole repository.
* `session.workspace_root` does the rest — already implemented (`harness.rs:3366-3377`).

**What it costs:** the daemon's flags become the repository's flags, so a package that wants a
different seat, model or `--no-queue` cannot have its own daemon — it has to ask the shim or be
told no. `--stop` now ends every worktree's sessions of that repository. A worktree's directory
being pruned no longer takes its daemon with it (an improvement, but a behaviour change). Old
`s-gatekeeper-<worktree key>` rows stay in the store, unreachable, unless a migration deletes
them.

### (c) One daemon per machine — every project, one process

**What changes:** everything in (b), plus making per-process facts per-session:

* **The vocabulary.** `Sessions<'a>` borrows one `Parts` for the process (`sessions.rs:922-925`)
  and a court clones it as `Arc`s (`:1566-1570`). Two projects that want different GGUFs
  (`--glm` and a `--vocab`) need per-session `Parts`, which means a real copy of a vocabulary per
  project — the sharing that makes (c) attractive is exactly what breaks.
* **The placement.** `--host` is *"a fact of the PROCESS"* (`cli.rs:1284-1290`) and is written to
  `<socket>.placement` beside the socket. One daemon per machine means one placement for every
  project, or a placement field on the session and a real change to `SessionWiring` and to the
  bridge's refusal (`bridge/daemon.rs:453-470`).
* **The flowy seat.** One seat per daemon, and a token is minted per project
  (`docs/flowy-monitor.md:196-205`). One machine daemon needs the *tenant* shape that document
  names as the extension — seat + project, each with its own reader, claim and spool — because two
  projects in one process must not `say` as one another's seat.
* **`leticode.toml`** discovery must become a per-session read (`cli.rs:1406`).
* **The blast radius is the box.** One `Stop` frame (`protocol.rs:1815`), one panic in one
  session's thread, one leaked fd toward the limit, and every project goes.
* **Upgrades become one act** — which is the prize: no stale binaries, and `store_floor`'s red
  warning stops being the way the operator learns they are running an old build.

**What it costs beyond that:** the daemon never idles out while any project works, so nothing is
ever reclaimed; and `--mode`/`--bash`/`--web-fetch`/`--glm` stop being daemon flags and there is no
per-session channel for them today (`Switch` changes the session, not the seat).

### (d) The hybrid — one supervisor/queue/store process per machine, thin per-folder session hosts

**What changes:** the work that duplicates across daemons moves into one process — the queue (lease,
workers, watcher), the supervisor, the store's leases — and each folder keeps a session host that
holds only its sessions, its tools and its heads.

**What it costs:** a second lifetime and a new protocol between the two. The queue's doors already
reach into a daemon's registry (`QueueService::ring_project` calling `registry.create`, `mergequeue.rs:392-417`),
and a session closing tears down its processes through `supervise::teardown_session`
(`sessions.rs:2808`, `:3617`) — both of those become cross-process calls. It buys the blast radius
of (a) with the single-owner work of (c), and it does not require the vocabulary, placement or seat
arguments above to be re-decided, because each folder still has its own process.

### The comparison

| | (a) per worktree | (b) per repository | (c) per machine | (d) hybrid |
|---|---|---|---|---|
| daemon key | worktree path (`scripts/letibot:119,274`) | repository key, else folder | user/machine | as (a), plus one supervisor process |
| workspace identity | the daemon's | **per session**, from the store row | per session, from the store row | the host's; per session in the store |
| queue lease | N candidates per repository | **1 per repository** | 1 per repository | 1 per repository, single claimant |
| gatekeeper | one per worktree (§1.4) | **one per repository** | one per project/repository | one per project |
| modes + consent | per project root (already) | per project root (already) | per project root (already) | per project root (already) |
| vocabulary / placement / seat | per daemon, correct | per repository, correct | **must become per session** | per host, correct |
| supervisor + watcher | N × whole store, 1 Hz / 2 s | 1 per repository | 1 for the box | 1 for the box |
| blast radius | one folder | one repository | **everything** | one folder |
| upgrade | per folder; stale binaries possible | per repository | one restart moves all | two processes to upgrade |
| memory | N × (vocab + threads + connections) | 1 per repository | 1 for the box (if the vocab is shared) | N hosts + 1 stub |
| bridge / launcher change | none | key, `--daemons`, `--stop`, and the false sentence at `tools.rs:651` | all of (b), plus placement/seat/config per session | new ipc; the keys stay |
| prior rulings | today's | matches the queue's own key (`mergequeue.rs:13`) and the bridge's stated rule | needs tenants (`docs/flowy-monitor.md:204`) | — |

---

## 4. Recommendation

**Land (b): one daemon per repository.** Then measure, and treat (d) — not (c) — as the next
shape if the numbers still point at duplicated work.

Why (b) and not (c):

1. **It removes the duplication the operator actually paid for.** Ten worktrees of one repository
   is ten candidates for one lease, ten gatekeepers for one queue, ten supervisors over one process
   table, ten watchers over one history — all of it per *repository* work done *per worktree*.
2. **It is the shape the code already believes in.** The queue is keyed by repository; the
   bridge's own prose says a worktree of the same repository is the same daemon; `--continue`'s
   scope rule is already a workspace-subtree rule.
3. **Most of the per-session work is done** (§2). What is left is the key, the gatekeeper id and
   the handful of daemon-level readers in §1.9.
4. **It keeps the blast radius at the unit the operator works in.** A night of packages is
   packages of one repository; a crash that takes the repository's daemon is a crash that would
   have taken those sessions wherever it happened.
5. **It is a strict subset of (c).** Everything (b) requires — workspace per session, gatekeeper
   per repository — (c) requires too. Nothing in (b) has to be undone to get to (c) or (d).

### The migration, in separately landable steps

Each step is useful on its own and leaves the tree green.

1. **Measure first (§5).** No code. Two weeks of the numbers below, so the claim is paid for.
2. **Name the key once.** Add `daemon_key(workspace) -> String` — `repository_key` when there is
   one, else the folder hash — beside `workspace_key` (`crates/sessionlog/src/lib.rs:190`), and
   make `scripts/letibot`, `crates/letibot/src/launcher.rs` and `crates/bridge/src/daemon.rs`
   compute it. Ship it behind `LETIBOT_DAEMON=repo|folder`, defaulting to `folder`, with the
   launcher's contract test extended to the new key (`launcher.rs:12-24`).
3. **Fix the false sentence** at `bridge/tools.rs:651` — one line, and it is the cheapest way to
   find out whether the bridge depends on the wrong reading. If `leticode_attach` across two
   worktrees of one repository currently resolves to a daemon that does not hold the session, there
   is a defect to fix before the migration, not after it.
4. **Gatekeeper per repository.** Id from `daemon_key`, and a start-up note naming any
   `s-gatekeeper-<folder key>` rows this daemon is not going to serve, so the operator's
   `--sessions` does not keep offering queues that nothing fills.
5. **The daemon-level readers in §1.9.** `Sessions::wiring` (`sessions.rs:2383`) and
   `LeticodeConfig` (`cli.rs:1406`) are the two that must follow the session once one daemon spans
   worktrees; `mergequeue::repositories_of_workspace` already answers for a worktree, so the queue
   needs nothing.
6. **`--daemons`, `--status`, `--stop` grouped by repository** (`scripts/letibot:1618`,
   `:1072-1170`): say which repository a daemon serves, and let `--stop` name a repository rather
   than only a folder.
7. **Flip the default** to `repo`, keep `folder` as the escape hatch for a package that needs its
   own seat or `--no-queue`, and revisit (d) with the same instrument from step 1.

Step 2 and step 3 are days of work. Step 4 is the one that needs a decision about old rows. Step 7
is the only irreversible-feeling one, and it is a one-word default.

---

## 5. What to measure first

The instrument exists for each of these; none of them is a new daemon.

1. **How many daemons, and of what.** `letibot --daemons` (`scripts/letibot:1618`) is a list of
   the records in the runtime directory. Sample it every five minutes for a night and group by
   repository — the answer is either "ten worktrees of one repository", which is (b)'s case, or
   "ten projects", which is (c)'s.
2. **Memory per daemon.** `memwatch` prints `rss N MiB · heap live N MiB · … · N threads` every 60 s
   (`crates/harnessd/src/memwatch.rs:184-195`), one line per daemon, in the per-folder log. It is
   untimestamped (`docs/metrics.md:128`), so read it as a distribution per daemon, not a series.
3. **The duplicated supervision.** Count `supervisor:` lines and the ssh calls each host sees per
   second (`crates/harnessd/src/supervise.rs:1016-1045` prints only when something happened; the
   host's own auth log is the other half). Today's expectation is N daemons × 1 Hz.
4. **How often a stale daemon actually broke something.** Count `store:` refusals naming a schema
   version (`crates/harnessd/src/store_floor.rs:19-30`) and `store_newer` warnings on the wire,
   against the number of daemon starts (`docs/metrics.md:99` counts 385 starts in one log).
   This is the number that decides how much (c)'s single-upgrade is worth.
5. **Time lost to per-folder lifecycle.** `letibot --stop` in a folder while another folder's
   session was running, and how often the operator restarted a daemon only to change a project's
   posture — the thing `DECISIONS.md:295-301` already ruled should never be necessary.

---

## 6. Open questions for the operator

1. **What is a "project" for the key?** A git repository (worktrees share a daemon) or a folder of
   repositories (`~/Projects/stroppy-io` is one project of several repos, and
   `repositories_of_workspace` already treats it as many)? One daemon per folder-of-repos is a
   fourth key, and the queue would serve each of its repositories from it.
2. **Should `--stop` still be per folder?** Under (b) one `Stop` ends every worktree's sessions of
   that repository. Is that wanted, or does `--stop` need a scope word (this worktree / this
   repository / everything)?
3. **Do package workers still need their own daemon** — their own seat, model, `--no-queue` — or
   may they join the repository's daemon and take its flags? Today's `--no-queue` exists precisely
   because a package worker *is* a session daemon of the repository by the queue's rule
   (`mergequeue.rs:520-525`).
4. **Modes and consent per worktree, or per repository?** They are keyed by project root today, so
   two worktrees of one repository are two points and two consents. Under (b) that stays true —
   is it what you want, or should the row be the repository?
5. **What should `--continue` mean** in a daemon that spans worktrees: the newest conversation of
   the worktree you are standing in, or of the repository? Today's rule is *exact workspace match
   beats a descendant* (`cli.rs:2800-2820`).
6. **Should the bridge get a way to say "this session, not this daemon"?** It only has
   `leticode_start`/`attach`/`stop` against a daemon, and under (b) a package's stop touches a
   daemon shared with the operator's own conversations in another worktree.
7. **Is one flowy seat per repository right?** A token is minted per project; two worktrees of one
   repository would be one seat under (b). Confirm, because `docs/flowy-monitor.md:196-205` chose
   one seat per daemon on that argument.
8. **Do you want the box's single daemon (c) eventually**, accepting that one bad session ends
   every project and that a vocabulary must be shared? If yes, (d) is the safer route there and
   (b) is the first half of it either way.
