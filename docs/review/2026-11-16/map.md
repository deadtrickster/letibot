# letibot architecture map

Snapshot: `origin/main` at `9e9312b06` (2026-11-16). Repo `~/Projects/letibot/letibot`, read from the detached worktree `~/Projects/letibot/wt-review-snap`. All `file:line` refs are at that commit, paths under `crates/` unless they start with `scripts/`, `tests/` or `claude-plugin/`. Since the previous snapshot `408353957`, batches 8-12 landed (about 60 branches) and a comment-only deslop pass that moved most line numbers.

Sizes count `.rs` lines including tests. The workspace has 836k lines under `crates/`, and `tests/` and `scripts/` add about 13k. Companion files are `seams.md` (every interface), `invariants.md` (rules and their enforcement) and `partition.md` (review panels).

## 1. What it is

A local coding-agent harness in two processes. `harnessd` owns sessions, the hash-chained ledger, the model connection and the permission gate. The TUI head attaches over a unix socket, so closing it mid-turn does not stop the turn. A box-wide room coordinator (`harnessd --room`) carries chat between seats. `letibot-mcp` is the Claude Code bridge, an MCP server that drives daemons, rooms and the merge queue. The shell launcher `scripts/letibot` (3.2k lines) decides placement, store and scope before any Rust runs, then execs `letibot-multi`, `harnessd` or `letibot-tui`.

Versions at this snapshot: head protocol 78 (`sessionlog/src/protocol.rs:15 PROTOCOL_VERSION`), store schema 51 (`tokencore/src/store.rs:207 SCHEMA_VERSION`), room protocol 12 (`room/src/wire.rs:15 ROOM_PROTOCOL_VERSION`), plugin contract 1.

## 2. Crate graph

Leaves first. "Used by" lists normal (non-dev) dependents. No crate was added or renamed since `408353957`. Two edges changed: `bridge` now depends on `harnessd` (default features off, for the queue's own enqueue), and `mcp` dropped `http` for ureq with rustls.

| crate (dir) | purpose | lines | used by |
|---|---|---|---|
| transcript | item model (User/Assistant/Tool rows), `Speaker`, media, citation fence | 2.8k | 17 crates (nearly all) |
| vt | terminal screen model, bytes onto a grid | 2.8k | tools, tui, ui |
| metrics | event table, the one way to send an event | 2.8k | 9 crates |
| activation | socket activation of helpers (systemd/launchd user units), `bounded.rs` | 1.5k | letibot, bridge, harnessd |
| config-ref | static reference of every config key in every file | 1.7k | letibot, provider, tui (+dev in many) |
| http | blocking HTTP/1.1 without TLS, the one retry rule (jitter, no-network budget), limit classification | 2.4k | embeddings, flowy, harnessd, provider, tui, turn |
| dialect | prompt-template trait | 0.5k | dialect-glm/qwen, harnessd, tokencore, turn |
| dialect-glm, dialect-qwen | templates per model family (+ `letibot-render*` bins) | 2.3k, 2.4k | letibot, harnessd, turn |
| plugin-sdk (pkg `letibot-plugin`) | guest SDK: hook kinds incl. `limit_decide`, `HOST_VERBS`, pane contract | 7.1k | bridge, harnessd, plugins, sessionlog, tools, tui |
| plugins | wasm layer host: manifests, trust pins, stack (no engine) | 7.1k | bridge, builtins, harnessd, plugins-rano, tui |
| plugins-rano | the engine, rano's wasmtime | 0.3k | bridge, harnessd, tui |
| builtins | built-in layers compiled to wasm32 by `build.rs` | 0.5k | bridge, harnessd, tui |
| backend | `Backend` trait, `TurnRequest`, `BackendError` | 0.3k | harnessd, provider, turn |
| tokencore | vocabulary, ledger, **the store** (schema, migrations, split routing, grants), `apparatus` | 24.9k | letibot, bridge, harnessd, llama, tools, turn |
| llama | GGUF vocabulary over llama.cpp FFI (the only crate that links it) | 1.3k | harnessd, sessionlog, turn |
| provider | cloud doors (ureq+rustls), presets, keys, registry, usage poll and warnings, effort scale | 19.5k | harnessd, leticode-config |
| embeddings | embedding endpoint client, internal model, cache, wordfreq list | 5.2k | letibot, harnessd, tools |
| turn | agent loop: engine, steering (`Pending::absorb`), compaction | 16.8k | harnessd, sessionlog |
| code | code intelligence (rano syntax) | 4.5k | harnessd, tools |
| tools | tool union: builtins, exec+confinement, adjudication, grant ledger, view, lifecycle/reaper, remote, firecode | 198.0k | 9 crates |
| webfetch, websearch | curl reader-mode fetch, Brave search | 1.5k, 0.6k | harnessd |
| mcp (pkg `letibot-mcp-client`) | MCP client: stdio, http and https; trusted `.mcp.json`; restart with backoff; `tools/list_changed` (dd76c1bb3) | 1.5k | harnessd |
| sessionlog | hub, registry, **wire protocol**, events, pending asks, socket server/client, peer gate | 36.6k | letibot, acp, bridge, harnessd, tui |
| room | room coordinator server, wire (protocol 12), chat log, cursors | 28.6k | letibot, bridge, harnessd |
| flowy | fabric seat as a monitor (external flowy node) | 5.9k | harnessd |
| github | GitHub reviewer seat | 3.9k | letibot, harnessd |
| claude, opencode | importers of those conversation formats | 3.8k, 3.6k | harnessd |
| leticode-config | `leticode.toml` loader, instruction files, `.mcp.json` reader | 6.5k | letibot, harnessd, tui |
| harnessd | the daemon: harness, sessions, mergequeue, shadow, grants, queue asks, plugins host, room glue, cli (bins `harnessd`, `letibot-m1`, `letibot-migrate-split`) | 244.3k | letibot, bridge |
| ui | editor and drawing pieces | 2.1k | tui |
| tui | the head (`src/app/` is the brain, 116k) | 145.0k | letibot |
| bridge | `letibot-mcp`: Claude Code MCP tools (39), room link, fleet reader, queue desk, channel push | 31.8k | letibot, acp |
| acp | ACP stdio agent for editors | 2.8k | none (bin) |
| letibot | `letibot-multi` multicall: roles by argv0, verbs (doctor, gc incl. `--targets`, holds, session, migrate, rooms, shadow...) | 12.9k | none (bin) |

The external dependency **rano** (git rev `c07e8ea`, pinned separately in 6 manifests, `tui/Cargo.toml:51`) provides styles, the `agent::*` widget set, `term`, markdown, syntax, the wasm engine, and its own copy of the letibot socket client.

The biggest modules are `harnessd/src/harness` (45k), `harnessd/src/mergequeue` (35k), `harnessd/tests` (57k), `tools/src/builtins` (60k), `tools/src/exec` (32k), `tools/src/lifecycle` (11k), `harnessd/src/shadow` (7.5k) and `tui/src/app` (116k).

## 3. Main flows

### 3.1 Operator prompt to rendered row

1. The composer runs `App::submit` (`tui/src/app/composer.rs:11 submit`). The order is skew lock, paste guard, `!term`, `!send`, `!cmd`, `/verb`, else `Action::Prompt` (`tui/src/app/composer.rs:245`). Tab mid-turn sends on the queue lane (R290).
2. The driver sends it (`tui/src/driver.rs:641`), and `HeadClient::prompt_from` writes `ClientFrame::Prompt` (`sessionlog/src/client.rs:201 prompt_from`). `from: None` means the operator. `delivery` is `steer` or `queue` (`sessionlog/src/protocol.rs:608 PromptDelivery`): a running turn takes only steers, a queued prompt opens its own turn after it.
3. On the socket, `serve_conn_rooted` (`sessionlog/src/server.rs:700 serve_conn_rooted`) reads `Peer::of` through SO_PEERCRED (`sessionlog/src/server.rs:708`). The first frame must be `Attach`. `peer.refuses(frame)` runs per frame (`sessionlog/src/server.rs:797`, `sessionlog/src/peer.rs:36 refuses`). A prompt then goes to `hub.submit(CommandKind::Prompt{from, delivery})` (`sessionlog/src/server.rs:1738`).
4. The hub queues it (`sessionlog/src/hub.rs:1715 submit`). `QueuedCommand::peer()` is at `sessionlog/src/hub.rs:87 peer`. The restart journal `JournaledPrompt` (`sessionlog/src/hub.rs:458 JournaledPrompt`) is written on its own thread (`sessionlog/src/hub.rs:755 journal_prompts`).
5. The daemon dispatches it: `Sessions::dispatch` (`harnessd/src/sessions.rs:3970 dispatch`), then `run_prompt` (`harnessd/src/sessions.rs:2497 run_prompt`), then `Harness::submit_with_context_from` (`harness/turns.rs:230 submit_with_context_from`). A peer becomes `Speaker::Peer`. The operator path runs `submit_said` (`harness/turns.rs:302 submit_said`), which writes the auth trail and the `User{speaker}` row, then `run_rounds` (`harness/turns.rs:1821 run_rounds`) and `rounds` (`harness/turns.rs:2057 rounds`).
6. A prompt that arrives mid-turn on the steer lane is steering: `HubSteering` (`harness/hub_io.rs:101 HubSteering`, source impl `harness/hub_io.rs:183`), then `Pending::absorb` (`turn/src/steering.rs:280 absorb`).
7. A provider round builds a `retry::Ladder` (`harness/turns.rs:2949`), then calls `engine.run_turn_messages_with_tail` (`turn/src/engine.rs:1124 run_turn_messages_with_tail`), then `backend.complete`. The llama-server path is `run_turn_steered_with_tail` (`turn/src/engine.rs:539 run_turn_steered_with_tail`).
8. Retry and limits: `Policy::class` (`http/src/retry.rs:155 class`) checks the GLM business code first, then money words, then `TRY_AGAIN` statuses (`http/src/retry.rs:12 TRY_AGAIN`). Retries jitter, have a no-network budget, and wait interruptibly (dbe4c3b9c). When the ladder is exhausted, `after_the_ladder` (`harness/limit.rs:466 after_the_ladder`) first asks the limit hook (3.9), then waits, falls back to `[fallback]`, or parks until reset. A park reads the usage poll's reading (3.10).
9. Tools: `run_the_rounds_calls` (`harness/turns.rs:2637 run_the_rounds_calls`), then `ToolRuntime::run` (`tools/src/runtime.rs:2151 run`). It applies grant revocations to the view first, runs the plugin `before` hook, then the **gate, only when the effective access is not unattended** (`tools/src/runtime.rs:2264`, `Access::is_unattended` = Read or Session, `tools/src/schema.rs:129 is_unattended`), then the TOCTOU recheck, then `tool.invoke`. The gate call is `tools/src/runtime.rs:2306`.
10. Results: `settle_call` (`harness/turns.rs:2704 settle_call`), then `append_items` (`turn/src/engine.rs:432 append_items`), which emits `TranscriptAppended` to the hub (`turn/src/engine.rs:487`) **before** `Harness::persist()` (`harness/turns.rs:1887`). Tool result rows carry `ran_ms` and `decision` (R287, protocol 78). The store is the tokencore store, and the single writer is claimed at `harnessd/src/writer.rs:62 claim`.
11. Events go out as `ServerFrame::Event(Envelope{session_id, seq, event})` (`sessionlog/src/event.rs:1775 Envelope`). A resume re-announces rows with their stored time (`sessionlog/src/hub.rs:1320 append_row_at`, R312). TUI tests live in `crates/tui/src/app/tests/`.
12. Compaction is `harness/wall.rs:405 compact_if_at_the_wall` and `harness/wall.rs:419 compact_at_the_wall` plus `turn/src/compaction.rs`. The head sees pending, running and done phases, and the summary turn's cost reaches the meter (76b439a89, e39e989ec). Summary items are `Speaker::Agent`.

### 3.2 Claude Code bridge and daemon

- The `letibot-mcp` binary (`bridge/src/main.rs`) has four modes: an MCP stdio server, `wait`, `room-wait` and `wait-queue` (`bridge/src/main.rs:115`). JSON-RPC is hand-rolled at MCP rev `2025-06-18` (`bridge/src/mcp.rs:17 PROTOCOL_VERSION`). It offers the `claude/channel` experimental capability (`bridge/src/mcp.rs:133`). On `initialized` it starts a permission watcher, the room stream, and session, queue and seat pushes (`bridge/src/mcp.rs:85-112`).
- It has 39 native tools (`bridge/src/tools.rs:2062 call`, count held by `bridge/src/tools.rs:2388 the_thirty_nine_tools_are_listed_with_schemas`), listed in seams.md S6. New since the last snapshot: `leticode_todo` (driving a session's todo board, R260) and `leticode_queue_verb` (`bridge/src/queue_desk.rs:142 VERB_TOOL`), which enqueues and reads rows through harnessd's own checks. Head tools attach to `<rundir>/<sha256(ws)[..12]>.sock` as kind `remote` (`bridge/src/daemon.rs:18 HEAD_KIND`), identity `letibot-mcp` (`bridge/src/daemon.rs:13 IDENTITY`).
- `leticode_start` (`bridge/src/tools.rs:366 start`) can create a package worktree. It then calls `daemon::ensure_with` (`bridge/src/daemon.rs:580 ensure_with`), which spawns `harnessd ... --no-queue` (`bridge/src/daemon.rs:324`), opens or creates the session, and sends the brief as `Prompt{from: cc@<folder>}`.
- `leticode_send` (`bridge/src/tools.rs:808 send`) goes one of three ways: held (`Head::prompt`), gone (`bridge/src/tools.rs:908 reopen_gone`, which reopens by id), or unheld (`bridge/src/tools.rs:838 send_unheld`: room `may_prompt`, then `fleet::prompt_once` straight to the target daemon, `bridge/src/fleet.rs:186 prompt_once`).
- The fleet reader (`bridge/src/fleet.rs:34 reader`) reads the shared store plus one attach per daemon. The TUI uses it for "packages in other daemons".
- The Claude plugin (`claude-plugin/`) runs `letibot-mcp` plus a `room-wait` monitor loop, and `hooks/register.tsx` draws panes.

### 3.3 Rooms

- The coordinator is `letibot --room`, which runs `harnessd --room` (`harnessd/src/room/coordinator.rs:21 configure`, supervisor `harnessd/src/room/supervise.rs`). The socket is `$LETIBOT_ROOM_SOCKET`, else `<data>/letibot/room-<host_key>.sock` (`harnessd/src/room/mod.rs:38 socket_path`). Beside it are `.token` (operator token, 0600), `.lease`, `.chat` and `.chat.state`.
- The wire is JSON lines capped at 1 MiB (`room/src/server.rs:68 MAX_FRAME_BYTES`). It carries `ROOM_PROTOCOL_VERSION=12` with `OLDEST_CLIENT=11` (`room/src/wire.rs:15,19`). The Hello door order is protocol, then token, then name, then principal, then project (`room/src/server.rs:735-780`).
- Messages are `Say`/`Dm` with kinds post, announce, fyi, ask, blocker, decision. One global `seq` covers rooms (`host`, `project/<key>`) and dms (`room/src/chat.rs`). `Reach` is at `room/src/chat.rs:128 Reach`. Text is capped at 4000 (`room/src/chat.rs:25 MAX_TEXT`).
- Cursors are per principal name (`room/src/chat.rs:1289 cursors`), persisted as `Record::Cursor` lines in `<socket>.chat.state` (`room/src/chat.rs:1383`). The bridge acks only what was settled (R259, `bridge/src/rooms.rs:1071 settle`, `bridge/src/rooms.rs:1167 ack`).
- In a session, the room link (`harnessd/src/room/link.rs:54 SessionRoom`) wakes the session. The text enters as one `User{Speaker::Agent}` row at turn start (`harness/turns.rs:655`). The room ack now happens when the turn's carrying row is stored, before its rounds (R313, `harnessd/src/room/wake.rs:300 ack_through`).
- Queue seats (`s-gatekeeper-<key>`) hear only dm/ask (`room/src/server.rs:1735 queue_seat_hears`). A cross-project prompt needs `because` = an operator message seq (`room/src/server.rs:1800 may_prompt`, `room/src/server.rs:1790 operator_said`).

### 3.4 Merge queue (review, gate, land)

- The module is `harnessd/src/mergequeue.rs` plus `serve.rs`, `gate.rs`, `git.rs`, `review.rs`, `capacity.rs`, `gate_host.rs` and `gate_firecode.rs`. Store rows and states live in `tokencore/src/store/queues.rs` (`ReviewState :170`, `MergeState :239`, `HealthState :307`). The queue lease is 90 s with a 5 s heartbeat (`mergequeue/lease.rs:13 LEASE_TTL_MS`, `mergequeue/lease.rs:17 HEARTBEAT_EVERY`).
- Review is separate from merging (operator ruling 2026-10-11). Reviewer seats start per `mergequeue/review.rs:773 start_reviewer`, with a 1 h deadline and 3 attempts (`mergequeue/serve.rs:26 REVIEW_DEADLINE_MS`, `mergequeue/serve.rs:34 MAX_REVIEW_ATTEMPTS`), at most 2 reviewing at once and 4 cores per reviewer (`mergequeue/capacity.rs:11 CORES_PER_REVIEWER`). A reviewer's exec follows the operator's mode inside its clone (5e1ed9a22).
- Landing runs `land_next` (`mergequeue/serve.rs:2149 land_next`), then `land` (`mergequeue/serve.rs:2335 land`). That step checks the approved sha via `change_id`, rebases (`mergequeue/git.rs:334 rebase_in`), reads the gate from the TARGET tip's `AGENTS.md` (`mergequeue/gate.rs:2056 declared_gate`, called at `mergequeue/serve.rs:2450`) and starts the gate (`mergequeue/serve.rs:770 start_gate`). `finish_land` (`mergequeue/serve.rs:2843 finish_land`) then does a CAS on the local `refs/heads/<t>` (`mergequeue/git.rs:516 update_ref_cas`), `follow_checkouts` (`mergequeue/git.rs:574 follow_checkouts`), pushes without force (`mergequeue/git.rs:690 push`), and runs `install-gate-build` (`mergequeue/gate.rs:66 INSTALL_MARK`) only after a successful push.
- A red train is bisected: the first half gates alone, then the second halves by the first's verdict (`mergequeue/serve.rs:64`, `mergequeue/serve.rs:2252`, 2eff65ee1). Trains are `land_train` (`mergequeue/serve.rs:2529 land_train`).
- The gate deadline is 30 min **per gate run** (`mergequeue/gate.rs:21 GATE_DEADLINE`). The landing worktree is under `<data>/landing` (`mergequeue/git.rs:187 landing_root`).
- Red main runs `target_went_red` (`mergequeue/serve.rs:1958 target_went_red`). That enqueues an urgent target review, calls `halt_landings` (`mergequeue/serve.rs:2072 halt_landings`), and lets only fix rows land. `halts_reviews` is `mergequeue/serve.rs:55 halts_reviews`.
- Tree removal asks `lifecycle::in_use::why_kept` first (R307, `mergequeue/serve.rs:1456`, `mergequeue/serve.rs:1301`), after the queue's own-registry check `mergequeue.rs:416 session_is_live`.
- Local scripts with their own gates: `scripts/gate-at.sh`, `scripts/rrun --gate` and `scripts/install-gate-build.sh`.

### 3.5 Lifecycle (reaper, leases, restart, jobs)

- The reaper thread runs every 60 s (`harnessd/src/reap_pass.rs:28 SWEEP`). `renew_held` (`harnessd/src/reap_pass.rs:152 renew_held`) renews the lease for a head (unless the package landed), a turn (`harnessd/src/reap_pass.rs:290 LeaseBeat`) or a job. Then `lifecycle::pass` (`tools/src/lifecycle/reap.rs:472 pass`) decides. `close_due` (`tools/src/lifecycle/reap.rs:146 close_due`) tests in this order: lease held, parent closed, hard TTL, start grace, daemon gone, then busy or served-elsewhere (`tools/src/lifecycle/reap.rs:181`), then idle. One daemon per box runs the disk half (targets, remote hosts) every `[gc] disk_every`, default 10 min (`harnessd/src/reap_pass.rs:32 DISK_EVERY`, 3af2084b3).
- Owners, holdings and `reap_event` live in `shared.db` (`tokencore/src/store/owners.rs`).
- A daemon stop by the reaper goes through `end_daemon` (`tools/src/lifecycle/reap.rs:1151 end_daemon`). When other owners' jobs exist, it writes a restart marker (`<socket>.restart`, `tools/src/lifecycle.rs:814 restart_marker`). The daemon reads it on SIGTERM (`harnessd/src/daemon.rs:122`, `harnessd/src/daemon.rs:97 stop_on_signal`) and detaches jobs (`harnessd/src/kept_jobs.rs:20 record_detached`). The sequence is SIGTERM, 5 s, then SIGKILL.
- Restart handover is in `harnessd/src/restart.rs`: beat 30 s (`harnessd/src/restart.rs:41 BEAT`), restore within 1 h (`harnessd/src/restart.rs:44 RESTORE_WITHIN`), owed-restart detection (`harnessd/src/restart.rs:818 promises_a_restart`), and reseat onto a stale prompt (`harness/wall.rs`). `letibot --restart` prints each step (R312).
- Job re-delivery (R313). Every delivery door marks what its batch carried as delivered in the `opened` callback of `submit_items_then` (`harness/turns.rs:1697 submit_items_then`), after `persist()` and before `run_rounds`: `Harness::the_batch_stored` (`harnessd/src/harness.rs:1405 the_batch_stored`), `Deliveries::stored` (`harness/deliveries.rs:1051 stored`), `JobWatchers::stored` (`harnessd/src/jobwatch.rs:815 stored`). A restart notice's ended jobs wait in `restart_owed` and are marked once its row is stored (`harness/turns.rs:903 restart_owed_stored`). Restore is `harness/turns.rs:920 restore_after_restart` and `harness/turns.rs:812 deliver_settlements_left_undelivered`. `Harness::wake` opens no turn once the registry is stopping (`harness/turns.rs:440`, again before the submit at `harness/turns.rs:668`).
- Cleanup in use (R307). `tools/src/lifecycle/in_use.rs:13 why_kept` keeps a directory when a session whose workspace is inside it has a `session_writer` held by a live pid here or on another host, or any readable `/proc/<pid>/cwd` is inside it. Asked by the queue's settle and release, `reap_worktrees` and lifecycle's work settle.
- Own view (R315). `tools/src/lifecycle.rs:265 own_view` is one value for a session's spill, `/tmp`, `owner_env` build dirs and a read-only rule for its children's spills, granted through `tools/src/exec/confine.rs:851 owning`.
- Auto-archive is `tools/src/lifecycle/autoarchive.rs` plus `harnessd/src/reap_pass.rs:136 live_hold`.
- gc and holds: `letibot/src/gc.rs`, `letibot/src/targets.rs` (`gc --targets` for `/build/target-*`), `tools/src/lifecycle/plan.rs`, `bridge/src/holds.rs`. Logs rotate at 32 MiB x3 (`tools/src/lifecycle/logs.rs:23`). Spills are in `tools/src/spill.rs` (R268, older layouts read). Per-job IO and the disk floor are in `tools/src/exec/diskio.rs`. The remote pass removes settled owners' homes (e30635163).
- Activation: the launcher wraps the daemon in `systemd-run --user --scope` after a background probe (`scripts/letibot:230 scope_probe_start`, `scripts/letibot:317 scope_run`, `scripts/letibot:337 detach`). A late answer goes to `harnessd/src/late_scope.rs:12 ENV` (`LETIBOT_SCOPE_LATE`). Memory delegation is `tools/src/exec/scope.rs:1040 delegate_memory`.
- Subagents are capped per tree and per daemon (`[subagents] max_running` 8, `daemon_max_running` 32, `when_full`), and a running objective is refused twice (`harness/admission.rs:106 try_admit`, c923859a9).

### 3.6 The shadow (R243, R251, R257, R261, R300, R317; R316 planned)

- The code is `harnessd/src/shadow.rs` plus `shadow/*`, with glue in `harness/shadow_seat.rs`. `open_for` (`harnessd/src/shadow.rs:144 open_for`) opens one only for top-level sessions. The record is a stored `SessionOrigin::Shadow` session with its own toolless prefix.
- There are three jobs (`harnessd/src/shadow.rs:56 deadline`): `turn_end` (30 s, owed-reminder and note capture, acts; note proposals are settled by `/note accept|decline`), `during` (20 s, drift, skill picks, recall; acts through R261 skill picks) and `permission` (2.5 s). The permission job observes settled cards through `ShadowSink` (`shadow/permission.rs:85 ShadowSink`), puts one line on relayed cards (`shadow/permission.rs:457 line_of`), and, since R317, answers one class of relayed ask (below).
- Rows are fed with authors (`shadow/words.rs:9 Author`). The conversation is persistent per job with a 64 KB budget (`shadow/conversation.rs:11 BUDGET`) and is now resumed from its own record after a restart (`shadow/resume.rs`, a489bc75a). `[shadow] mode = "stateless"` takes the older path.
- Verdicts are JSON with `deny_unknown_fields` (`shadow/verdict.rs:171`). Quotes must be substrings of shown, authorised spans (`shadow/verdict.rs:241 verify`, `shadow/verdict.rs:72 authored_span`). A permission verdict can only be allow or ask (`shadow/verdict.rs:464`). A denylist sits on allow (`shadow/guard.rs:29 allow_passes`).
- R317, relayed reads. `PendingAsks::note_before` (`sessionlog/src/pending_asks.rs:255 note_before`) asks the shadow before a relayed card is posted, inside the 2.5 s deadline. `shadow/permission.rs:263 scope` admits only a review-queue seat's read-only view widening whose path passes `shadow/guard.rs:138 read_passes`. Only an `allow` citing a listed permission rule acts (`shadow/permission.rs:272 acting_rule`). It settles as `grant_ro` by `shadow:<judge>`, is kept in `session_grant` (3.7), and shows one row on the main session. `/shadow revoke` and `/permissions revoke` are one path (`harnessd/src/grants.rs:85 revoke`). Two disagreements trip the breaker until restart (`shadow/permission.rs:170 BREAKER_AT`). Switch: `[shadow] answer_reads` in providers.toml, on with the permission job (`shadow/config.rs:22`).
- R316, the shadow as the automode oracle. Built: `ShadowOracle` (`harnessd/src/shadow/oracle.rs`) behind `letibot_tools::ModelAdjudicator`, its authority from the stages the operator promoted (`stage_scope`), screened per call by `stage_of` (the tier, the reading, a secret flow, the reach) and by `read_passes`/`allow_passes`. The stages are `[shadow] answers = ["reads", "workspace_writes", "exec"]` (`shadow/config.rs::Stage`), with `answer_reads = true` still meaning `["reads"]`. `[gatekeeper] via = endpoint|shadow|both` (`harnessd/src/guard_via.rs`, the default `both` when both are configured) asks the local endpoint first and the shadow when the endpoint does not answer at all. The breaker, the rows and `/shadow arm` are R317's, unchanged; every answer is a row naming the model and the stage.
- The restart "owed" call reads the last turn_end verdict (`harness/shadow_seat.rs:422 shadow_on_the_last_answer`, used at `harness/turns.rs:1485`).

### 3.7 Grants kept in the store (R314)

- Table `session_grant` (schema 50, `granted_by` at 51), a project table in a split set (`tokencore/src/store/grants.rs:10`). One row per `(session_id, scope, kind, key)`. `scope` is `session` or `queue`, `kind` is `view`, `allow` or `memory`. `Store::put_grant`, `grants`, `revoke_grant`, `clear_grants` at `tokencore/src/store/grants.rs:53`, `:75`, `:115`, `:120`.
- `tools/src/grant_ledger.rs:236 GrantLedger` is the gate's view of it, with a process-wide generation (`tools/src/grant_ledger.rs:17 GENERATION`) so every gate reloads after a keep or a revoke, and `queue_of` (`tools/src/grant_ledger.rs:30 queue_of`). `AdjudicatedGate::with_grant_ledger` (`tools/src/adjudicate.rs:1630 with_grant_ledger`) reads the session's scope and, for a queue seat, the queue's, refreshing on a generation move (`tools/src/adjudicate.rs:1654 refresh_grants`). An answer on a relayed card from a queue seat (`tools/src/adjudicate.rs:991 last_relayed`) is kept at the queue's scope and covers every current and future reviewer, never the main session.
- At open, `kept_views` (`harnessd/src/grants.rs:192 kept_views`) binds kept view grants before the first command. A revoked path is unbound at each session's next call (`tools/src/runtime.rs:828 view_revocations`).
- `/permissions` (alias `/grants`, `harnessd/src/slash.rs:350`) lists kept grants of the session and its queue, and `/permissions revoke ID` removes one live (`harnessd/src/grants.rs:215 permissions`).

### 3.8 Relayed queue asks (R300)

- Review-queue sessions ask on boards nobody watches. `harnessd/src/queue_asks.rs` turns each open ask into a question card on the daemon's main session naming the asker by role (`reviewer of <work> (<queue>)`, `harnessd/src/queue_asks.rs:164`), relays the answer to the asker's row (`harnessd/src/queue_asks.rs:181 relay`), and reposts after a restart from the durable board.
- Permission cards from queue seats are relayed the same way (`harnessd/src/answers.rs:536 relayed`). They drop the box-wide rung (`harnessd/src/answers.rs:476 without_durable`), relabel session rungs to queue scope (`harnessd/src/answers.rs:495 queue_scoped`), and carry the shadow's verdict as one line. A view read can be answered by the shadow (3.6).

### 3.9 Limit hook and `/fork` (R233)

- A rate-limit decision goes first to a `limit_decide` plugin layer when one answers (`plugin-sdk/src/limit.rs:11 HOOK`, host `harness/limit_hook.rs:105 limit_hook`, called from `harness/limit.rs:472`). The layer alone decides: `wait`, `switch`, `new_session`, ask, or fall back to the host rules. Episodes and the layer's state are kept per model (`harness/limit_hook.rs:36 load`).
- `new_session` forks the conversation into a new session and continues the turn there (`harness/limit_hook.rs:334 limit_new_session`). The same mechanism is the operator's `/fork N`: a new session that is this conversation up to message N (`harness/fork.rs:61 fork_at`, `harnessd/src/slash.rs:342`). `/fork` and `/note` are host verbs that a plugin may not shadow (`plugin-sdk/src/command.rs:58 HOST_VERBS`).
- The head keeps a limit's model switch, park and return above the composer until dismissed (6e31b27f7).

### 3.10 Usage poller (R232, R310)

- `provider/src/usage_poll.rs` polls the provider's usage endpoint in the background every 2 min (`provider/src/usage_poll.rs:15 EVERY`, `provider/src/usage_poll.rs:288 watch`), one request per endpoint at a time, with the latest key. A failed round re-tells the kept reading with its time, and the row gains its age once older than the period (`provider/src/usage_poll.rs:174 age`). A panicked poll is restarted.
- The harness side is `harness/usage.rs` (`harness/usage.rs:102 watch_usage`, row `harness/usage.rs:75 usage_row`). Every reading redraws the status row. Thresholds are said once per window (`provider/src/usage_warn.rs`, `[provider.usage_warn]`).

### 3.11 Mode ceilings and nags

- A child's point is held under its parent's (R306): `Harness::follow_parent` hands the parent's live point cell to a spawned child (`harnessd/src/harness.rs:846 follow_parent`), `within_parent` refuses a wider point by name (`harnessd/src/harness.rs:813 within_parent`, used by `set_mode_consented` at `harnessd/src/harness.rs:743` and `mode_past_parent` at `harnessd/src/harness.rs:826`), and `narrow_to_parent` (`harnessd/src/harness.rs:853 narrow_to_parent`) narrows a child at its next turn or call batch. A child reopened after a restart has no live parent cell.
- Nags are on by default and a pause lasts one daemon's life (R311): `restore_nag_pause` (`harness/nags.rs:626 restore_nag_pause`) drops a pause an earlier daemon wrote, against `harnessd/src/nag_pause.rs:102 daemon_life_started_ms`. Who paused is `NagBy` (`harnessd/src/nag_pause.rs:30 from_identity`), stored in `session.nags_paused_at/by` (schema 51).

### 3.12 Plugins (wasm hooks)

- The guest SDK is `plugin-sdk`. Hook kinds are listed in seams.md S8, now including `limit_decide`. The host is `plugins` (manifest `CONTRACT=1` at `plugins/src/lib.rs:84 CONTRACT`, `MOVED` at `plugins/src/lib.rs:100 MOVED`, trust pins in `trust.rs`, stack in `stack.rs`).
- The engine is rano's wasmtime via `plugins-rano/src/lib.rs`. Each call gets a fresh instance, 50M fuel and 64 MiB, with no wall clock and **no host imports** (`plugins-rano/src/lib.rs:43`, `Capabilities::default()`). Guests act only through JSON answers and "looks", which the daemon executes (`harnessd/src/plugins/tool.rs:483 run_look`, `harnessd/src/plugins/tool.rs:538 exec_look`).
- Built-in layers are built by `builtins/build.rs` from `plugins/builtin/*`: cards, chat, claude-transcript, compact, github, irc, push, queue, review, room, sessions, steer, todo. Project layers come from `.letibot/plugins`.
- Command files apply frontmatter `allowed-tools` and `model` to their turn, with dir/name namespaces (R283, 3107cc7a6). Policy skills (`kind: policy`) are delivered as rules, never offered.

### 3.13 Remote hosts and firecode placement

- `hosts.toml` (`tools/src/remote.rs:169 load`, override `LETIBOT_HOSTS` at `tools/src/remote.rs:157`). Keys are `~/.config/letibot/keys/<host>`, with letibot's own `known_hosts`. `~/.ssh` is never read.
- The ssh flags (`tools/src/remote.rs:699 flags`) set `-F /dev/null`, `StrictHostKeyChecking=yes` (`tools/src/remote.rs:711`), `IdentitiesOnly`, `BatchMode` and a ControlMaster per daemon pid (`tools/src/remote.rs:773 control_path_for`). The remote home is `<root>/.letibot-home/<short>` (`tools/src/remote.rs:1304 HOME_DIR`).
- Remote lifecycle: `tools/src/lifecycle/remote.rs:173 host_pass` runs **from the home box over ssh** (script `tools/src/lifecycle/remote.rs:127 reap_script`). There is no host-local reaper.
- Firecode: `tools/src/firecode.rs` (`tools/src/firecode.rs:416 FirecodeSpec`, `tools/src/firecode.rs:568 up`, `tools/src/firecode.rs:1059 down`, copy-out cap `tools/src/firecode.rs:1735 COPY_OUT_CAP`). VM boots queue for a box-wide slot with a load-scaled deadline (`tools/src/firecode/boot.rs`, R234). Cache is `$XDG_CACHE_HOME/letibot/firecode`, and the rescue bundle is `tools/src/firecode/keep.rs`.
- Placement: `firecode_route` (`harness/open.rs:6020 firecode_route`) and reviewer placement Vm, Host or NoVm (`harnessd/src/sessions/gatekeeper.rs:45 ReviewerPlacement`). The queue runs its gate on a host (`gate_host.rs`) or in a VM (`gate_firecode.rs`).
- `scripts/rrun` has its own ssh flags and awk parsing of `hosts.toml`.

## 4. Trust boundaries

| boundary | where | shape |
|---|---|---|
| Socket peer gate | `sessionlog/src/peer.rs:26 of`, `sessionlog/src/peer.rs:36 refuses`, cgroup test `sessionlog/src/peer.rs:248-270`. Socket is 0600, set by chmod after bind (`sessionlog/src/server.rs:542`) | Who counts as the operator: a peer that is neither a descendant of the daemon nor in a `letibot.<daemon>.` cgroup. Inside-tree peers may attach only as `askpass` |
| Speaker | `transcript/src/lib.rs:49 Speaker` `{Operator(default), Agent, Peer{from}}`. Gate side `tools/src/authorise.rs:19` (only Operator authorises) | `from` is asserted by the client. The identity `letibot-mcp` implies a peer (`sessionlog/src/hub.rs:87 peer`) |
| Permission gate | `AdjudicatedGate::admit` (`tools/src/adjudicate.rs:3179 admit`) | Order: breaker, NotRun, Blocked, never-write, `permission.json` (`tools/src/adjudicate.rs:3325`; an allow no longer settles AlwaysAsk, `tools/src/adjudicate.rs:3362`, R305), mode point (`tools/src/adjudicate.rs:3427`, `tools/src/mode.rs:486 admits_unasked`), reviewer arm, standing and kept grants (`tools/src/adjudicate.rs:3486`), then ask (`tools/src/adjudicate.rs:3549`). Tiers at `tools/src/adjudicate.rs:63`. `ALWAYS_ASK` at `tools/src/intent.rs:2409 ALWAYS_ASK`. Always-allow writes the exact call (`tools/src/permission.rs:411 exact_rule_for`), and a wildcard covering a write or exec flag asks (`tools/src/permission.rs:506 write_or_exec_flag`) |
| Kept grants | `session_grant`, `tools/src/grant_ledger.rs:236 GrantLedger`, `harnessd/src/grants.rs` | A grant outlives a restart. A queue-scope grant covers every reviewer of that queue. `granted_by` is `human:` or `shadow:<judge>`. Revocable by `/permissions revoke` |
| Model adjudicator (oracle) | `tools/src/authorise.rs:564 OracleScope`, `tools/src/authorise.rs:1010 AuthorisationOracle`, `[gatekeeper]` in providers.toml (`provider/src/keys.rs:155 gatekeeper`) | intents, `max_scope`. It sees the operator utterances of the auth trail. The shadow is not an oracle yet (R316) |
| Shadow read answers | `shadow/permission.rs:263 scope`, `shadow/guard.rs:138 read_passes` | Queue seats' view reads only, rule-cited, never secrets, `~/.config/letibot` or `keys`. Never denies |
| Sandbox | `tools/src/exec/confine.rs` (bwrap on Linux, `seatbelt.rs` on macOS). Env cleared to `KEEP_ENV` (`tools/src/exec/confine.rs:772 KEEP_ENV`). Seats `tools/src/exec/seat.rs:131 seat_for_level` (Wide, or Tight = project + grants + own view). Secret stores `tools/src/secrets.rs:37 STORES`. File fence `tools/src/exec/fence.rs:16 FileFence` | Egress follows the mode, not the command (`tools/src/exec/seat.rs:160 network_rule`) |
| Boundary config | `[boundary]` in `leticode.toml`, `tools/src/exec/boundary.rs`, perimeter `tools/src/exec/perimeter.rs` | env refusals (`_API_KEY`, `GH_TOKEN`, `LD_PRELOAD`), grants_ro/hide. Project files only narrow |
| Push credential | `tools/src/exec/seat.rs:46 github_env` | `GH_TOKEN` + a credential helper for the top-level seat only. Children cannot push |
| Keys | provider keys come from `provider/src/keys.rs:204 resolve` (flag, env, `providers.toml`, opencode `auth.json`), written 0600. ssh identities are `keys/<host>` | `NEVER_WRITE` plus masks keep `.config/letibot` out of seats |
| Remote ssh | `tools/src/remote.rs:699 flags` | strict host keys, own known_hosts, no agent |
| Plugin fuel | rano `src/plugin.rs`, trust pins `plugins/src/trust.rs` | Fuel and memory are capped per call. Unpinned layers still load and answer most hooks (`stack.rs`). Pinned layers exec declared programs ungated (`harnessd/src/plugins/conditions.rs`) |
| MCP servers | `mcp/src/wire.rs:74 spawn` (stdio with the daemon env, unconfined), http/https (`mcp/src/wire.rs:261`). MCP calls are preapproved per catalogued tool (`tools/src/adjudicate.rs:3887`) | `[mcp.<name>]` only in the user `leticode.toml`. A checkout's `.mcp.json` starts only when the user file lists it in `[mcp_projects] trusted` (`harness/open.rs:2120`). Project `[mcp]` is named and not started |
| HTTP head | `harnessd/src/httphead.rs`, `harnessd/src/cli.rs:1999` | opt-in `/v1/chat/completions`, no auth, no tools |
| webfetch | `webfetch/src/lib.rs:464`, private set `webfetch/src/lib.rs:811` | curl with pinned `--resolve`, refuses private addresses, same-origin redirects |

## 5. Persistent formats

### 5.1 The store (tokencore)

- The data dir is `$XDG_DATA_HOME/letibot`, else `~/.local/share/letibot`, overridden by `LETIBOT_STORE`. The launcher ignores XDG.
- `shared.db` is written by every daemon. It holds `session` (now with `nags_paused_at/by`), `session_writer`, `review_queue`, `merge_queue`, `queue_lease`, `queue_transition`, `queue_told`, `target_health`, `claude_review`, `queue_mute`, `host_lease`, `daemon_life`, `project_mode`, `mode_import`, `seat_todo`, `seat_job`, `owner`, `holding` and `reap_event` (`tokencore/src/store/split.rs:88 SHARED_TABLES`).
- `projects/<12hex>.db` is written only by that folder's daemon. It holds `transcript`, `transcript_item`, `stable_prefix`, `todo`, `citation`, `session_marks`, `session_mode`, `session_monitor`, `job`, `job_context`, `process`, `queued_prompt`, `subagent_run`, `adjudication`, `recall_unit` and `session_grant` (`tokencore/src/store/split.rs:56 PROJECT_TABLES`).
- Split routing (R313). Project-table reads go through `Store::at(table)` (`tokencore/src/store.rs:1768 at`), writes through `table_in` (`tokencore/src/store.rs:1580 table_in`). `import_session` runs on the project file with `shared.db` attached (`tokencore/src/store/transfer.rs:312 import_tx`). `tokencore/src/store/split_coverage_tests.rs` runs every public `Store` method touching a project table on a split set and fails on any it finds but does not call. Opening one file no longer holds up opens of others (R230, ac83835eb).
- `sessions.db` is the legacy single file. While a box is being carved it is the shared file (`tokencore/src/store/split.rs:25 LEGACY_FILE`, `tokencore/src/store/split.rs:30 CARVED_TABLE`).
- `SCHEMA_VERSION=51` (`tokencore/src/store.rs:207`). 50 adds `session_grant`, 51 adds `session.nags_paused_at/by` and `session_grant.granted_by`, both additive (`tokencore/src/store/floor.rs:39 READER_ARMS`). Reader floor 37.
- Pragmas are WAL, `synchronous=FULL` (`tokencore/src/store.rs:1608`) and a 30 s busy handler. Triggers make `transcript_item` append-only (`tokencore/src/store.rs:526-545`).
- The ledger chain is `h_k=SHA256(h_{k-1}||tokens)` (`tokencore/src/ledger.rs`).
- Other SQLite files: embeddings cache (`embeddings/src/cache.rs:81 open_in`) and `metrics.db` (`LETIBOT_METRICS_DB`).

### 5.2 JSONL and flat files

- Room items: `<socket>.items` and `<socket>.chat` (4 MB segments). Room state: `.chat.state` with cursor, seen, guest and ask records. None of these files carry a version.
- Per workspace, under `.letibot/`: `delivery-log.jsonl`, `notes-offers.jsonl`, `skills-offers.jsonl`, `recall-offers.jsonl`, `done-offers.jsonl`, `offer-declines.jsonl`, `context-claims.jsonl`, `etalon.jsonl`, `host-changelog.jsonl`, `notes/*.jsonl`, `shadow/<job>.md`, `skills/`, `commands/` and `plugins/`.
- Data dir: `wordfreq/` (the 20k-word list skill offers read, fetched once by `[wordfreq] mode = "internal"`, `embeddings/src/wordfreq.rs`; absent is the `wordfreq_unavailable` failure), `etalon.jsonl`, `spills/`, `landing/`.
- Bridge files: the push outbox per folder and session, nicks under `<data>/letibot/nicks/`, and waiter marks under `<data>/letibot/waiters/`.
- Daemon logs: `~/logs/harnessd-<key>.log` (`LETIBOT_LOG`, `LETIBOT_DAEMON_LOGS`). Spills: `<data>/spills/` and `<owner_root>/tmp/{spill,session}`.
- Lifecycle markers: `<socket>.restart` (restart marker), the late-scope answer file, and `runs/<run>/copy_out` (firecode).

### 5.3 Config files (`~/.config/letibot/`, reference in `crates/config-ref`)

| file | holds |
|---|---|
| `leticode.toml` | model, fallback, provider (incl. `usage_warn`), roles, exec, output, mcp, `mcp_projects`, memory, notes, browser, github, boundary, perimeter, skills, nag, room, queue, ui, sessions, `subagents`, warnings, keys, verbosity, project, embeddings, `wordfreq`, bridge, gc, docker, firecode |
| `providers.toml` | default, fallback, nudge, shadow (incl. `answer_reads`), `shadow.<job>`, presets, provider doors, models, prices, gatekeeper, memory, metrics, brave |
| `hosts.toml` | fleet hosts |
| `permission.json` | "always allow" rules (exact calls since R305), seeded from `config/permission.json` |
| `plugin-trust.json` | plugin pins |
| other | `head.toml`, `prompts.toml`, `theme.toml`, `modes.tsv` (legacy), `keys/`, `known_hosts`, `roles/`, `commands/`, `skills/`, `plugins/` |

The writers splice a generated reference header (`config-ref` `header`/`splice`). Kept grants are in the store, not in a config file.
