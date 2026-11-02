# One daemon per workspace, and the case for one per repository

There is one `harnessd` per workspace: the git toplevel you stand in, so every worktree of a
repository gets its own process, its own socket, its own store handle and its own opinion about the
merge queue. A night of parallel packages across worktrees meant about ten daemons at once.

This is the design question: what a daemon owns, the four shapes that could replace it, what each
costs, and a recommendation with a migration in separately landable steps.

**The recommendation.** One daemon per repository first (shape b), then measure, and treat the
hybrid (shape d) rather than the box's single daemon (shape c) as the next shape if the numbers
still point at duplicated work. Shape b is a key change plus the completion of work already done: a
session's workspace, mode, consent, monitors and jobs are keyed by the session or the project root,
not by the daemon. It removes exactly the duplication that costs - N queue-lease candidates per
repository, N gatekeeper sessions, N supervisors ticking the whole process table, N watchers over
one history - and it keeps the blast radius at the unit a night of work is measured in.

## What a daemon owns

### The socket, the record, the lock and the lifecycle

`scripts/letibot` takes `git rev-parse --show-toplevel` as the workspace, hashes it, and names the
socket, the record and the log after those twelve hex characters. The same hash is spelled again in
Rust with a contract test over four measured keys (`crates/letibot/src/launcher.rs`), a third time
in the bridge (`crates/bridge/src/daemon.rs`), and keyed to the store as
`letibot_sessionlog::workspace_key`.

Three things follow from the key being the worktree:

- The lock is per folder. `FolderClaim` is an `flock` on `<socket>.lock`
  (`crates/sessionlog/src/server.rs`), so "one daemon per folder" is enforced per folder and says
  nothing about two daemons serving one store.
- A stop is a folder's stop: `ClientFrame::Stop` ends the daemon and every session on it.
- The daemon leaves on its own when nothing has needed it for `idle_exit_after`, one hour by
  default (`crates/harnessd/src/config.rs`, `daemon.rs`). The ten daemons of a busy night are ten
  busy daemons; the ones with nothing to do are meant to be gone.

### Sessions, one thread each

`Daemon::serve` binds the socket and serves one in-process `Registry`. `Sessions` holds the open
harnesses and the per-session clocks, and since T2 each session is served by its own thread
(`sessions::Court`, armed by `Daemon::run`). A daemon already holds many sessions comfortably: a
worker, its subagents, the gatekeeper and a dozen conversations are one process today.

### The store: one handle per session, one connection per path per process

Every session opens its own `Store`, plus a second for the corpus sink, and claims
`session_writer` so a second open of one transcript is refused by name rather than half-written
(`crates/harnessd/src/writer.rs`, `crates/tokencore/src/store/writers.rs`). The queue service and
the supervisor open their own.

Those are handles, not connections. `Store` is an `Arc<Shared>` holding one
`Mutex<ManuallyDrop<Connection>>` per path (`crates/tokencore/src/store.rs`), and sqlite3 keeps the
file's descriptor until the process's LAST connection to that file closes - so the descriptor count
is bounded by the pool, not by the session count. Two rules come with it:

- A `Store::connection()` guard must not be alive across another `Store` method call. That is a
  self-deadlock and the compiler will not say so.
- A test whose point was a second connection now sees one, and measures nothing.

A newer store is read, not refused. `crates/tokencore/src/store/floor.rs` declares, for every arm
of `migrate_from`, whether it breaks a younger reader or is additive; the floor of a store at
version V is the newest breaking arm at or below V, and a binary whose own schema is at or above
that floor opens it. So a stale daemon keeps working across an additive migration, and says so on
the status line. The floor is not the version, and three places in the tree had pinned the version
before this existed.

### The gatekeeper session, per workspace

Each daemon owns one review-queue session whose id is derived from its workspace,
`s-gatekeeper-<workspace key>` (`crates/harnessd/src/sessions/gatekeeper.rs`). It used to be the
constant `s-gatekeeper` for the whole box, and two daemons appended to one transcript; the fix keyed
it to the workspace. Every daemon opens it at startup with nobody attached, which is why a head may
attach to a session it never started.

The board is filled from the rows whose `owner` is this process's lease claim
(`Harness::fill_queue_board` over `gatekeeper::waiting(store, &me)`), so of ten worktrees of one
repository only the daemon holding that repository's lease has rows on its board: the other nine are
an opened session, a composed prompt and an empty board.

### The merge queue is keyed by repository, not by workspace

`repository_of_workspace` names the checkout a workspace is in, a worktree counting as its
repository; `repositories_of_workspace` is that one, or each repository of a folder that is one
project of several.

Per daemon the queue is a lease per repository (compare-and-swap in the store, heartbeat every 5 s,
TTL 90 s), a worker thread per lease held, and a watcher that reads `queue_transition` every 2 s for
moves other daemons made. The claim is one per process, `host/pid/<started>`, which is how a stale
daemon holding a lease is told from a fresh one. Every daemon seated anywhere under a repository is
a candidate for its lease, which is why a package worker's daemon needs `--no-queue`.

### The supervisor, which supervises the store

One supervisor thread per daemon, ticking every second. Each tick reads `Store::open_processes()` -
every open process record in the shared store, with no owner filter - probes each host once, and
writes what it found. With N daemons on a box, a host with one running job gets N ssh round trips a
second and N processes writing the same rows about it.

### Monitors, providers, the gate

Monitors are owned by the session (`crates/harnessd/src/pr_watch.rs`, the `armed` set in
`sessions.rs`) and stored by session id. The provider client and the model adjudicator are built per
session at open and rebuilt on a model switch.

Modes and consent are per project root, in the store, with the operator's `consented_at` beside it
(`crates/harnessd/src/modes.rs`). They are resolved at every session open over the session's own
workspace, longest ancestor wins, and `--mode` is only the default for a root with no row. This is
the least daemon-shaped thing in the daemon: `modes.tsv` used to be a file each daemon read at start
and rewrote whole, so one daemon's write undid another's, and `ModeStore` now re-reads at every
session open and after every write so no copy outlives the moment it is read.

### What is genuinely per process

| fact | why it is per process |
|---|---|
| the workspace **default** | a daemon needs somewhere to stand before any session exists |
| the **vocabulary** and dialect renderer | one GGUF load per process, cloned as `Arc`s per court |
| the **placement** (`--host`) | the protocol carries no backend: `SessionWiring` has model, dialect, endpoint and workspace only; the placement is written beside the socket |
| the **flowy seat** | a seat is a persistent identity with one inbox reader (`docs/flowy-monitor.md`) |
| the gatekeeper **id** | derived once from the daemon's workspace |
| the **`leticode.toml`** | the project's models are read once, at start |
| the queue's **repository set** | one reading of the workspace |
| the **mode cache** | one per process, re-read at every open because daemons share the rows |

### The bridge and the launchers

`crates/bridge/src/daemon.rs` states the rule plainly: one daemon per workspace, a socket named by
the workspace's own hash, and a session the bridge starts must live in the daemon that folder
already has. `leticode_start` resolves a worktree to its workspace and starts or joins;
`leticode_attach` resolves the same way, and the name `gatekeeper` resolves through that daemon's
workspace key.

One of the bridge's own sentences is false today. `leticode_attach`'s schema says a worktree of the
same repository is the same daemon; a worktree's git toplevel is the worktree, so it hashes
differently and is a different daemon. The sentence is true only for a subdirectory of the same
worktree.

`scripts/letibot` is where the per-folder rule lives, and `scripts/leticode` sets `LETIBOT_ROLE` and
execs it, inheriting the whole rule. `letibot --sessions` and `--continue` are store queries rather
than socket queries - they return before the vocabulary loads and before the socket is bound - and
`--continue`'s scope rule is that an exact workspace match beats a descendant.

## What is already per session, and therefore cheap

The store already treats a session as the unit that owns things. `Store::transfer` says so in one
list: a row belongs to a session when the session id is its key - transcripts and items, todos,
jobs and job contexts, monitors, `session_mode`, the gate's decisions, citations - while the queue
tables, `project_mode` and `daemon_life` are keyed by a repository, a root or a process, none of
which is a session.

The rest of the half that already works:

- A session's workspace comes from its own row: `Harness::open` replaces `cfg.workspace` with the
  stored `workspace_root`, because a resumed session's tools are confined to the tree the
  conversation is about. A daemon seated anywhere can hold a session about anywhere.
- Its mode and consent come from `project_mode` and `session_mode` at every open.
- The config is cloned per session off `Sessions.base`, so a second session is the same daemon and
  not a second configuration nobody typed.
- The wire already multiplexes: `NewSession`, `ResumeSession`, `Switch`, `CloseSession`, `Peek`. A
  head on one socket can reach every session in that process, and the picker draws them.
- `letibot --migrate` already moves a session between daemons, and the store records
  `session.lives_on`, so "which daemon holds this session" is allowed to change.

Two wire facts a head meets on day one. A bare attach - empty session id - lands on the session the
daemon's default points at, and the default follows whoever switched last, because `Switch` calls
`Registry::set_default`. And a fresh attach is handed a snapshot with an empty backlog
(`Hub::attach` with `since_seq = 0` replays nothing), so a warning published before a head attached
is never delivered to it; a head that must not miss one asks for the session's rows.

## The four shapes

**(a) One daemon per worktree, today.** Nothing changes. It costs ten candidates for one queue
lease, ten gatekeeper sessions for one queue, ten supervisors probing every host every second and
writing the same rows, ten watchers reading one history, ten vocabulary loads and ten logs, and a
stale binary on one folder that cannot read the store while its neighbours can. It buys a blast
radius of one folder, `--stop` that touches nothing else, flags that are that folder's, and two
projects whose turns never contend for one process.

**(b) One daemon per repository, every worktree of one repository sharing it.** The socket key
becomes the repository key, falling back to the folder key where there is no single repository; the
gatekeeper id follows; one lease candidate, one supervisor, one watcher, one vocabulary, one
`leticode.toml`, one placement per repository; `session.workspace_root` does the rest. It costs the
daemon's flags becoming the repository's, so a package that wants its own seat, model or
`--no-queue` has to be told no; `--stop` ends every worktree's sessions of that repository; a
pruned worktree no longer takes its daemon with it; and old `s-gatekeeper-<worktree key>` rows stay
in the store, unreachable, unless a migration deletes them.

**(c) One daemon per machine.** Everything in (b), plus turning the per-process facts of the table
above into per-session ones. The vocabulary is the one that bites: two projects that want different
GGUFs need a real copy per project, so the sharing that makes (c) attractive is what the vocabulary
breaks. The blast radius becomes the box - one `Stop`, one panic, one leaked descriptor and every
project goes - and the prize is that upgrades become one act, with no stale binaries and no
`store_floor` warning. The daemon also never idles out while any project works, so nothing is ever
reclaimed; and `--mode`, `--bash`, `--web-fetch` and `--glm` stop being daemon flags with no
per-session channel for them today.

**(d) The hybrid: one supervisor, queue and store process per machine, thin per-folder session
hosts.** The duplicated work moves into one process and each folder keeps only its sessions, tools
and heads. It costs a second lifetime and a new protocol: the queue's doors already reach into a
daemon's registry, and a session closing tears down its processes through
`supervise::teardown_session`, so both become cross-process calls. It buys (a)'s blast radius with
(c)'s single owner, and needs none of the vocabulary, placement or seat arguments re-decided.

| | (a) per worktree | (b) per repository | (c) per machine | (d) hybrid |
|---|---|---|---|---|
| daemon key | worktree path | repository key, else folder | user/machine | as (a), plus one supervisor process |
| workspace identity | the daemon's | per session, from the store row | per session | the host's; per session in the store |
| queue lease | N candidates per repository | 1 per repository | 1 per repository | 1 per repository, one claimant |
| gatekeeper | one per worktree | one per repository | one per project | one per project |
| vocabulary, placement, seat | per daemon, correct | per repository, correct | must become per session | per host, correct |
| supervisor and watcher | N x whole store, 1 Hz / 2 s | one per repository | one for the box | one for the box |
| blast radius | one folder | one repository | everything | one folder |
| upgrade | per folder, stale binaries possible | per repository | one restart moves all | two processes |
| memory | N x (vocabulary + threads + connections) | one per repository | one for the box, if the vocabulary is shared | N hosts + one stub |
| bridge and launcher | none | key, `--daemons`, `--stop`, the false sentence | all of (b), plus per-session placement and config | new ipc, keys stay |

## Why (b), and not (c)

1. It removes the duplication actually paid for: ten worktrees of one repository is ten candidates
   for one lease, ten gatekeepers for one queue, ten supervisors over one process table, ten
   watchers over one history - per-repository work done per worktree.
2. It is the shape the code already believes in: the queue is keyed by repository, the bridge's own
   prose says a worktree of the same repository is the same daemon, and `--continue`'s scope rule is
   already a workspace-subtree rule.
3. Most of the per-session work is done. What is left is the key, the gatekeeper id, and the handful
   of daemon-level readers in the table above.
4. It keeps the blast radius at the unit the operator works in. A night of packages is packages of
   one repository.
5. It is a strict subset of (c): nothing in (b) has to be undone to reach (c) or (d).

## The migration, in separately landable steps

Each step is useful on its own and leaves the tree green.

1. **Measure first.** No code. Enough of the numbers below that the claim is paid for.
2. **Name the key once.** Add `daemon_key(workspace) -> String` - the repository key when there is
   one, else the folder hash - beside `workspace_key`, and make `scripts/letibot`,
   `crates/letibot/src/launcher.rs` and `crates/bridge/src/daemon.rs` compute it. Ship it behind
   `LETIBOT_DAEMON=repo|folder`, defaulting to `folder`, with the launcher's contract test extended
   to the new key.
3. **Fix the false sentence** in `crates/bridge/src/tools.rs`. One line, and the cheapest way to
   find out whether the bridge depends on the wrong reading.
4. **Gatekeeper per repository.** Id from `daemon_key`, plus a start-up note naming any
   `s-gatekeeper-<folder key>` rows this daemon will not serve, so `--sessions` stops offering
   queues that nothing fills. This is the step that needs a decision about old rows.
5. **The daemon-level readers.** `Sessions::wiring` and `LeticodeConfig` are the two that must
   follow the session once one daemon spans worktrees; the queue needs nothing, since
   `repositories_of_workspace` already answers for a worktree.
6. **`--daemons`, `--status` and `--stop` grouped by repository**, so `--stop` can name a repository
   rather than only a folder.
7. **Flip the default** to `repo`, keep `folder` as the escape hatch for a package that needs its
   own seat or `--no-queue`, and revisit (d) with the same instrument.

Steps 2 and 3 are days of work. Step 7 is the only irreversible-feeling one and it is a one-word
default.

## What to measure first

The instrument exists for each of these; none of them is a new daemon.

1. **How many daemons, and of what.** `letibot --daemons` lists the records in the runtime
   directory. Sample it every five minutes for a night and group by repository: the answer is
   either "ten worktrees of one repository", which is (b)'s case, or "ten projects", which is
   (c)'s.
2. **Memory per daemon.** `memwatch` prints rss, heap live and thread count every 60 s, one line per
   daemon in the per-folder log. Read it as a distribution per daemon, not a series.
3. **The duplicated supervision.** Count `supervisor:` lines and the ssh calls each host sees per
   second. Today's expectation is N daemons x 1 Hz.
4. **How often a stale daemon actually broke something.** Count `store:` refusals naming a schema
   version and `store_newer` warnings on the wire, against the number of daemon starts. This is the
   number that decides how much (c)'s single upgrade is worth.
5. **Time lost to per-folder lifecycle.** `letibot --stop` in one folder while another folder's
   session was running, and how often the operator restarted a daemon only to change a project's
   posture.

## Open questions for the operator

1. **What is a "project" for the key?** A git repository, so worktrees share a daemon, or a folder
   of repositories, which `repositories_of_workspace` already treats as many? One daemon per
   folder-of-repos is a fourth key, and the queue would serve each of its repositories from it.
2. **Should `--stop` still be per folder?** Under (b) one `Stop` ends every worktree's sessions of
   that repository. Is that wanted, or does `--stop` need a scope word (this worktree, this
   repository, everything)?
3. **Do package workers still need their own daemon** - their own seat, model, `--no-queue` - or may
   they join the repository's daemon and take its flags?
4. **Modes and consent per worktree, or per repository?** They are keyed by project root today, so
   two worktrees of one repository are two points and two consents. Under (b) that stays true; is
   that what you want, or should the row be the repository?
5. **What should `--continue` mean** in a daemon that spans worktrees: the newest conversation of the
   worktree you are standing in, or of the repository?
6. **Should the bridge get a way to say "this session, not this daemon"?** Under (b) a package's
   stop touches a daemon shared with the operator's conversations in another worktree.
7. **Is one flowy seat per repository right?** A token is minted per project, and
   `docs/flowy-monitor.md` chose one seat per daemon on that argument.
8. **Do you want the box's single daemon (c) eventually**, accepting that one bad session ends every
   project and that a vocabulary must be shared? If yes, (d) is the safer route and (b) is its first
   half either way.
