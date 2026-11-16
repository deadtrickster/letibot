# letibot seams inventory

Snapshot `9e9312b06` on `origin/main` (2026-11-16), read from the detached worktree `~/Projects/letibot/wt-review-snap`. Refs are `file:line` at that commit, with paths under `crates/` unless they start with `scripts/`, `tests/`, `docs/` or `claude-plugin/`. The first snapshot was `408353957`. Batches 8-12 (about 60 branches, 90 non-deslop commits) and a comment-only deslop pass (44 commits) landed since.

**[BOTH-SINCE]** marks a seam where one commit since `408353957` changed both sides. **[NEW]** marks a seam added at this snapshot (S20-S23).

Each seam gives its sides, shape, compat rules, risks and tests. Each risk carried over from `408353957` is marked **fixed in <sha>**, **changed**, or **remains** (re-verified at `9e9312b06`, or "not re-traced"). "Risk" items are reading findings, not test failures. HYP means plausible from the code but not exercised.

Versions at this snapshot: head protocol 78 (was 75), store schema 51 (was 49, reader floor 37), room protocol 12 (oldest client 11, unchanged), plugin contract 1, warning census 223 (was 211), bridge tools 39 (was 36).

---

## S1. Session wire protocol (head and harnessd)

**Sides.**
- Server: `sessionlog/src/server.rs:700 serve_conn_rooted`, hub `sessionlog/src/hub.rs`, registry `sessionlog/src/registry.rs`. harnessd drives sessions through these.
- Shared client: `sessionlog/src/client.rs` (`attach :60`, `start_attach :111`, `prompt_from :201`, `pump :1055`).
- Clients:
  - tui: `tui/src/head.rs`, `tui/src/driver.rs`, `tui/src/app/events/hello.rs:53`
  - bridge: `bridge/src/head.rs`, `bridge/src/fleet.rs`
  - acp: `acp/src/session.rs:270,687`
  - letibot CLI: `letibot/src/github.rs:183`, `letibot/src/migrate.rs:1479 quiesce`, `letibot/src/roles.rs:117`
  - askpass helper (`sessionlog/src/peer.rs:10 ASKPASS_KIND`)
  - rano's hand copy: rano `src/letibot.rs` (line 26), which still says protocol 72

**Shape.**
- NDJSON, one serde object per `\n` line (`sessionlog/src/wire.rs:59-77`). No length prefix and **no line-length bound** (`sessionlog/src/wire.rs:62`, `read_line`). Batched writes send at 64 KiB (`sessionlog/src/wire.rs:112`).
- `ClientFrame` (`sessionlog/src/protocol.rs:617-1113`, `#[serde(tag="frame")]`). Among them: Attach, Prompt{client_request_id, expected_seq, text, attachments, context, from, delivery} (`sessionlog/src/protocol.rs:640`), DriverTodos (`:681`), WithdrawPrompts, Interrupt, Mode*, Slash, Answer*, List*, NewSession, ResumeSession, Term*, Pane*.
- `ServerFrame` (`sessionlog/src/protocol.rs:1139-1381`): Hello, Sessions, Settings, `Event(Envelope)`, Accepted, Rejected, Bye, Term*.
- `Envelope{session_id, seq, ts, #[flatten] SessionEvent}` (`sessionlog/src/event.rs:1775`). `SessionEvent` is tagged `event` (`sessionlog/src/event.rs:1089`).
- Handshake: `Attach{protocol_version, session_id, since_seq, kind, identity, caps}` must come first (`sessionlog/src/server.rs:712`). `Caps.features` is the forward-compat channel, one flag defined (`sessionlog/src/protocol.rs:310 FEATURE_QUESTION_ANSWERS`).

**Version.**
- `PROTOCOL_VERSION=78` (`sessionlog/src/protocol.rs:15`). History string in `sessionlog/src/protocol/tests.rs:162-175`:
  - 76 `SessionEvent::TurnEnded`, the prompt's end apart from a round's `TurnFinished` (8d4943a7a, R55). `TURN_ENDED_SINCE=76` (`sessionlog/src/protocol.rs:19`). An older head gets `TurnFinished{ends_turn}` through `sessionlog/src/compat.rs:13 OlderHead` (new file).
  - 77 `ClientFrame::DriverTodos` and the `Peer <handle>` todo author (8da965edd, R260).
  - 78 `TranscriptItem::ToolResult` `ran_ms` and `decision`, defaulted (5eecd00f1, R287; `transcript/src/lib.rs:199,203`).
- Policy in practice is unchanged: a new variant bumps, an added `#[serde(default)]` field does not. No `deny_unknown_fields`, no `#[serde(other)]` on the top enums.
- On mismatch the daemon still **accepts and only logs** (`sessionlog/src/server.rs:729-744`). The doc comment at `sessionlog/src/protocol.rs:12-14` still says "refuse a mismatch by name". The workspace `Cargo.toml:112` release comments say a bump makes head and daemon "refuse each other".
- The TUI seats read-only when the daemon is older (`tui/src/app/tests/session.rs:487`). The bridge records the skew (`bridge/src/head.rs:823`) and still prompts. `fleet::prompt_through` ignores the version (`bridge/src/fleet.rs:216`).
- An unreadable client frame makes the daemon send `Bye` and close (`sessionlog/src/server.rs:2146-2157`). An unreadable server frame makes the head skip the line (`sessionlog/src/client.rs:1067`).

**Risks.**
- Unbounded line buffering by any socket peer. **Remains** (`sessionlog/src/wire.rs:62`). The room wire caps at 1 MiB.
- Semantic changes under one number. **Remains, one more case.** `Prompt.from` (7ab2f03fd), `pending` (af0de414e) and `DAEMON_COMMANDS_KEY` (d80292d72) are unchanged. New: 6f538d24a (R290) added `Prompt.delivery: steer|queue` (`sessionlog/src/protocol.rs:609 PromptDelivery`) at 76 with no bump, so a queued prompt sent to an older 76 daemon is taken as steering, silently. 711bb1b27 added the `models.usage` settings key with no bump (keys are not versioned).
- rano's client is now 6 versions behind (72 vs 78) and tolerated silently. **Remains.**

**Tests.** Covered:
- `sessionlog/src/protocol/tests.rs` (`every_frame_is_accounted_for_at_this_version :7`, `every_frame_round_trips :598`, `a_todos_frame_round_trips_with_its_statuses :776`, old-shape decodes)
- `sessionlog/src/wire.rs:143-214`, `transcript/src/lib.rs:693` (old result row still reads)
- `sessionlog/tests/appended_row.rs:125,158`
- `tui/src/app/tests/session.rs:373,469,487`, `tui/tests/stop_force.rs`, `tui/tests/rano_editor_head.rs`
- `bridge/src/head/tests.rs:45 skew_and_unreadable_lines_reach_the_status`

Not covered: the daemon's `Bye` on an unreadable frame, an oversized line, two real builds against each other or golden frames, a bridge `from` prompt or a `delivery: queue` prompt reaching an older daemon.

**Since 408353957.** 8d4943a7a (76), 8da965edd (77), 5eecd00f1 (78), 6f538d24a (`delivery`, no bump), 711bb1b27 (usage key), 160eb8d2e (deslop).

## S2. Event stream and one-record rule (hub, transcript, warning destinations)

**Sides.**
- Producer: `turn/src/engine.rs:432 append_items`, which emits `TranscriptAppended` (`turn/src/engine.rs:487`). The hub's row door is `sessionlog/src/hub.rs:1314 append_row`, and `sessionlog/src/hub.rs:1320 append_row_at` (new, R312: a re-announced row keeps its stored time).
- Consumers: the TUI fold, bridge `Head::events`, the shadow feed (`harnessd/src/shadow/feed.rs`), plugin pane feeds.

**Shape.**
- `TranscriptItem` carries a `speaker`. `ToolResult` now also carries `ran_ms` and `decision` (S1, 78).
- Warning codes each have exactly one destination, Model, Status or Notes (`sessionlog/src/warning.rs:1151 TWIN`, `sessionlog/src/warning.rs:1269 destination`).
- Census: `TABLE` (`sessionlog/src/warning.rs:93`) is 223 codes, 98 routine, 16 refused, 109 failures, asserted at `sessionlog/src/warning.rs:1720` (was 211). New codes: `compact_failed`, `compaction_pending`, `driver_todos_refused`, `handed_on_twice`, `limit_hook`, `limit_hook_done`, `limit_hook_fallback`, `limit_hook_unsaved`, `limit_new_session`, `provider_refusal_body`, `slash_listing`, `wordfreq_unavailable` (e52000bba also folded the `mcp_server` rows into one).

**Compat.** A row without `speaker` deserialises as Operator (`#[default]`). A result row without `ran_ms`/`decision` reads as `None`.

**Risks.**
- The event is shown before it is stored. **Remains.** `submit_items_then` appends (emits) then calls `persist()` (`harnessd/src/harness/turns.rs:1711-1713`), and round results persist at `harnessd/src/harness/turns.rs:2611`. What R313 (313efe571) changed is delivery marking: `Harness::the_batch_stored` (`harnessd/src/harness.rs:1405`) runs in the `opened` callback after `persist()` and before `run_rounds` (`harnessd/src/harness/turns.rs:1714`), so delivery and room acks now follow the store, not the turn's end.
- The prompt journal writes on its own thread and drops `from`. **Remains** (`sessionlog/src/hub.rs:419 run_journal`, `sessionlog/src/hub.rs:458 JournaledPrompt` has no `from`, restored with `from: None` at `sessionlog/src/hub.rs:779`). The identity is kept, so a restored bridge prompt is a peer named `letibot-mcp` (`sessionlog/src/hub.rs:87 peer`), not `cc@<folder>`.
- 7d7b2a9a1 (parked-turn line) and 5a4e8d7a6 (quiet restart row) against "no operator-only rows": not re-traced. New display-only rows since: the shadow's `OfferKind::Shadow` grant row (R317), the limit hook's model switch/park line (6e31b27f7), `nags_paused`/`nags_resumed` (R311). Each needs the same check.

**Tests.** `every_code_has_exactly_one_destination` (`sessionlog/src/warning.rs:1555`), `the_table_is_98_routine_16_refused_and_109_failures` (`sessionlog/src/warning.rs:1715`), `tui/src/app/tests/onevent.rs:172 no_warning_is_a_row_of_the_conversation`, `harnessd/tests/resume.rs` (re-announced rows keep their time). No crash-between-show-and-store test. No journal round trip of `from`.

**Since 408353957.** 313efe571 (R313 marking), fccb8e110 (R312 row times), e52000bba (census 223), 5eecd00f1.

## S3. Store tables and migrations (tokencore store and every opener)

**Sides.**
- Definition: `tokencore/src/store.rs` (DDL, `migrate :1848`, `migrate_from :1927`), `tokencore/src/store/split.rs` (placement `:56 PROJECT_TABLES`, `:88 SHARED_TABLES`, `StoreSet::open :425`), `tokencore/src/store/floor.rs`, `tokencore/src/store/queues.rs`, `tokencore/src/store/owners.rs`, `tokencore/src/store/grants.rs` (new, `session_grant`), `tokencore/src/store/transfer.rs`.
- Routing: `Store::at(table)` (`tokencore/src/store.rs:1768`) answers the connection that holds the table on a set; writes route through `table_in` (`tokencore/src/store.rs:1580`).
- Openers:
  - harnessd `harnessd/src/config.rs:3618 StoreAt` (open `:3671`, `refuse_a_split_boxes_legacy :3701`)
  - `bridge/src/daemon.rs:245 store_in`, `bridge/src/store.rs:76-91`, `bridge/src/holds.rs:31`
  - `letibot/src/gc.rs:1080-1132`, `letibot/src/migrate.rs:664,871,896` (these migrate on open)
  - `tools/src/lifecycle/reap.rs:423`, `tools/src/lifecycle/autoarchive.rs:248` (`open_reader`)
  - `harnessd/src/corpus.rs:206`

**Shape.**
- Tables per file are listed in map.md 5.1. `session_grant` is a project table (`tokencore/src/store/split.rs:82`).
- Both files carry `schema_version` and `schema_reader_floor`.
- Cross-file foreign keys are stripped by string-replace (`tokencore/src/store/split.rs:308-309`).
- Every write connection sets WAL, `synchronous=FULL` and `foreign_keys=ON` (`tokencore/src/store.rs:1608-1609`), and writes with `BEGIN IMMEDIATE`. The per-statement busy wait is now 5 s (`tokencore/src/store.rs:1817 BUSY_TIMEOUT`, was 30 s, dbe4c3b9c), inside a 90 s retry ladder (`tokencore/src/store/busy.rs:12 BUSY_RETRY_TOTAL`).

**Version.**
- `SCHEMA_VERSION=51` (`tokencore/src/store.rs:207`). v50 `session_grant` (R314, `tokencore/src/store.rs:2617`), v51 `session.nags_paused_at/by` (R311) and `session_grant.granted_by` (R317) (`tokencore/src/store.rs:2622`). Both arms Additive (`tokencore/src/store/floor.rs:88-89`), so the floor stays 37 (Breaks at 23, 24, 37).
- A newer store at or under the floor opens with a `NewerStore` notice, above it `SchemaTooNew` (`tokencore/src/store/floor.rs:203-224 open_newer`).
- `CORPUS_VERSION=1` on adjudication rows (`tokencore/src/store.rs:221`).

**Risks.**
- Migration runs with no transaction and no lock, every daemon migrates `shared.db` on open, and arms are check-then-ALTER. **Remains** (`tokencore/src/store.rs:1866-1872`). v49 and v51 check before ALTER, v50 is `CREATE IF NOT EXISTS`.
- The v44 and v47 arms create tables in the wrong file. **Remains, not re-traced**: they still run on `self.conn()` (`tokencore/src/store.rs:2534,2586`), and so does the new v50 arm (`tokencore/src/store.rs:2620`).
- `StoreSet` holds two connections per file. **Remains** (`tokencore/src/store/split.rs:441-459`, a `Writer` plus a `Store::open` per file).
- `open_sharing` holds a process-wide lock across the migration (R230). **Changed in ac83835eb**: the lock is now per file (`tokencore/src/store.rs:1698-1706`), so one file's open no longer holds up others. `StoreSet::open` still holds the process-wide `sets()` lock across `create`, which opens and migrates both files (`tokencore/src/store/split.rs:432-437`).
- Project-table reads on the shared connection. **Fixed in 9489dbac3 / 313efe571 (R313)**: `job_context`, `subagent_run`, `process`, `session_mode`, `session_marks`, citations, recall and transfer read through `at(table)`. `import_session` on a set runs on the project file with `shared.db` attached (`tokencore/src/store/transfer.rs:312 import_tx`); the two files commit one after the other, not atomically.
- 11b57abdc changed the `stable_prefix` id hash with no schema bump. **Remains** as a fact of the record.
- R229 (a test migrated the live `shared.db`). Not re-traced here (see invariants).

**Tests.** Covered:
- `tokencore/src/store/tests.rs`: unwind-and-reopen per version, `every_migration_arm_declares_whether_it_breaks_a_reader :2940`, floor
- `tokencore/src/store/split_coverage_tests.rs:253 every_public_method_on_a_project_table_is_exercised_on_a_split_set`, `:309 every_project_method_runs_on_a_split_set` (new, R313; a source scan plus a run)
- `tokencore/src/store/split_tests.rs`, `tokencore/tests/{store_census, store_split_carve, store_split_corpus, store_split_migrate, store_split_paths}.rs`, `bridge/tests/split_store.rs`

Not covered: two processes migrating at once, a crash mid-migration, stray tables, a crash between the two commits of `import_tx`.

**Since 408353957.** 47894496a (v50), acb4c65cb (v51 nags), de0b555e3 (v51 `granted_by`), ac83835eb (R230), 313efe571 and 9489dbac3 (R313), 3ea7ba818, dbe4c3b9c (busy), ce86daadd/f16742cff (deslop).

## S4. Room wire, coordinator and clients (cursors, DMs)

**Sides.**
- Coordinator: `room/src/server.rs`, `room/src/chat.rs`, `room/src/store.rs`. Hosted by `harnessd/src/room/{coordinator.rs, supervise.rs, mod.rs}`.
- Clients:
  - session daemons: `room/src/client.rs`, `harnessd/src/room/link.rs`, `harnessd/src/room/wake.rs`
  - the bridge's own client: `bridge/src/rooms.rs:800-1305`
  - the TUI room pane

**Shape.**
- JSON lines capped at 1 MiB (`room/src/server.rs:68 MAX_FRAME_BYTES`).
- `Hello{protocol, speaker, room, seat, token, share, family}` (`room/src/wire.rs:150`).
- `Request::Say` and `Dm` (`room/src/wire.rs:175,196`), `MayPrompt` (`room/src/wire.rs:261`). `SayKind` is at `room/src/wire.rs:68`. Text is capped at 4000 (`room/src/chat.rs:25 MAX_TEXT`).
- `Reach` (`room/src/chat.rs:128`, decided at `room/src/chat.rs:388 reach`): Operator, Dm, then fyi/blocker/names-you, else Passing.
- Cursors are `Chat.cursors` (`room/src/chat.rs:1289`), persisted as `Record::Cursor` in `<socket>.chat.state` (`room/src/chat.rs:1383`).
- Room text enters a session as one `User{Speaker::Agent}` row; the ack now follows the row's store, not the turn (R313, `Harness::the_batch_stored`, S2).
- Relayed queue permission asks do **not** cross the room. They are in-process: `sessionlog/src/pending_asks.rs:123 PendingAsks`, posted as cards on the daemon's main session (`harnessd/src/queue_asks.rs`).

**Version.**
- `ROOM_PROTOCOL_VERSION=12`, `OLDEST_CLIENT=11` (`room/src/wire.rs:15,19`), unchanged. `protocol_refusal` (`room/src/wire.rs:24`) answers `Bye{why}`. Only comment changes in `room/src` since 408353957 (fef5871a0).

**Risks.**
- `Request::MayPrompt` under 12 (5e721bbad): an older coordinator closes the connection. **Remains**.
- Hello `share` and `family` under 12 (cfae85130): a new bridge on an older coordinator loses family mail silently. **Remains**.
- The room JSONL store has no version, skips unreadable lines and compacts from memory (`room/src/store.rs:88-126`, `compact :373`). **Remains** (HYP for the drop).
- The bridge copy of `reach` maps every fyi to `passing` (`bridge/src/rooms.rs:400-414`), while the room crate addresses an fyi that names you (`room/src/chat.rs:396`). **Remains**.
- The bridge re-derives the socket path and `host_key_of` (`bridge/src/rooms.rs:270,341` vs `room/src/lease.rs:321`). **Changed**: still a copy, now pinned by `the_host_key_matches_the_room_crate` (`bridge/src/rooms.rs:2285`).
- Reply matching is by arrival order: any non-event frame goes to the one waiting slot (`bridge/src/rooms.rs:1277`), and a closing reader drops the slot whatever its generation (`bridge/src/rooms.rs:1294-1301`), so it can end a newer connection's wait. **Remains**.
- Suffix recursion has no bound: a `name_in_use` refusal retries `open()` with `suffixed(handle, pid)` (`bridge/src/rooms.rs:818-825`). **Remains**.

**Tests.** Covered:
- `room/tests/{wire,socket,dm,queue_seat,chat,ask,injection}.rs`
- `harnessd/tests/room_{bell_turn,two_sessions}.rs`
- bridge against the real `Server`: `bridge/tests/{room_share, room_nick, prompt_unheld}.rs`

Not covered: `bridge/tests/room_tools.rs` still uses a fake coordinator (`bridge/tests/room_tools.rs:15 Fake`); no socket test of a v10, v11 or v13 client; no bridge `Bye` test; no test that bridge `reach` agrees with room `Reach`.

**Since 408353957.** fef5871a0, 6614b2d47, 0abb150a3 (deslop only), 313efe571 (ack timing).

## S5. Cross-project consent and queue seats

**Sides.**
- Coordinator: `room/src/server.rs:1735 queue_seat_hears`, `:1757 queue_seat_gate`, `:1790 operator_said`, `:1800 may_prompt`.
- Bridge: `bridge/src/tools.rs:838 send_unheld` (sends `may_prompt` with `because`), `bridge/src/fleet.rs:186 prompt_once`.
- Target daemon: plain `ClientFrame::Prompt`.

**Shape.** A `because` field carries the seq of an operator message the sender can see.

**Risks.**
- A cooperation check, not a security one: the project in Hello is self-declared (`room/src/server.rs:404 project_of_hello`), and any visible operator message qualifies. **Remains**.
- The target daemon checks nothing. **Remains**.
- `prompt_once` can time out and still deliver later (the thread keeps running, `bridge/src/fleet.rs:200-212`), so a retry can deliver twice. **Remains**.

**Tests.** `room/tests/queue_seat.rs`, `bridge/tests/prompt_unheld.rs`.

**Since 408353957.** None that change it.

## S6. Bridge MCP tools to daemon

**Sides.**
- Claude Code to `letibot-mcp`: `bridge/src/mcp.rs` (MCP `2025-06-18` at `bridge/src/mcp.rs:17`, `claude/channel` at `:133`, permission relay `:134`).
- Bridge to daemon: head socket (S1). Bridge to room: S4. Bridge to the store: fleet, holds, seat todos.
- New: bridge to the merge queue, `bridge/src/queue_desk.rs` (2eff65ee1, e66038adb), which links `letibot-harnessd`. New mode `letibot-mcp wait-queue <repo> <row>` (`bridge/src/main.rs:115`).

**Tools.** 39 native tools, asserted at `bridge/src/tools.rs:2390` and `bridge/src/mcp.rs:336`. Name list `bridge/src/tools.rs:1945 names`, dispatch `bridge/src/tools.rs:2062 call`.

| tool | def | dispatch | what it calls |
|---|---|---|---|
| `leticode_start` | `bridge/src/tools.rs:1521` | `:2067` | `daemon::ensure_with` (spawns `harnessd --no-queue`), new or open session, brief as `Prompt{from}` |
| `leticode_attach` | `bridge/src/tools.rs:1547` | `:2085` | Hello, watch-only by default |
| `leticode_send` | `bridge/src/tools.rs:1571` | `:2091` | held, gone (`reopen_gone :908`) or unheld (`send_unheld :838`) |
| `leticode_todo` (new) | `bridge/src/tools.rs:1589` | `:2096` | `ClientFrame::DriverTodos` (S1, 77) |
| `leticode_status` | `bridge/src/tools.rs:1616` | `:2100` | Settings, `protocol_skew` |
| `leticode_events`, `leticode_asks` | `bridge/src/tools.rs:1639,1660` | `:2101,2124` | the head's local ring |
| `leticode_answer` | `bridge/src/tools.rs:1674` | `:2125` | `ClientFrame::Answer` |
| `leticode_diff` | `bridge/src/tools.rs:1692` | `:2132` | git in the worktree |
| `leticode_transcript` | `bridge/src/tools.rs:1707` | `:2107` | peek and rows |
| `leticode_mode` | `bridge/src/tools.rs:1728` | `:2114` | `mode_confirm`, 120 s (`bridge/src/tools.rs:31 MODE_PATIENCE`) |
| `leticode_model` | `bridge/src/tools.rs:1747` | `:2119` | `/model` slash |
| `leticode_stop` | `bridge/src/tools.rs:1765` | `:2136` | interrupt, close, detach |
| `leticode_archive` | `bridge/src/tools.rs:1780` | `:2137` | archive |
| `leticode_collect` | `bridge/src/tools.rs:1797` | `:2142` | `tools/src/remote.rs:4293 collect` |
| `leticode_list` | `bridge/src/tools.rs:1816` | `:2149` | local map plus fleet |
| `holds`, `gc_plan` | `bridge/src/tools.rs:1824,1840` | `:2150,2151` | `bridge/src/holds.rs` |
| `queue` (new) | `bridge/src/queue_desk.rs:18 definition` | `bridge/src/tools.rs:2152` | `bridge/src/queue_desk.rs:61 call`: enqueue and read a row |
| `leticode_queue_verb` (new) | `bridge/src/queue_desk.rs:149 verb_definition` | `bridge/src/tools.rs:2153` | `bridge/src/queue_desk.rs:172 verb_call`: approve, restart, veto, rm, reset, clean, regate |
| `todo_write`, `todos`, `todo_answer` | `bridge/src/tools.rs:1870,1872,1879` | `:2154-2156` | `bridge/src/seat.rs` |
| `seat_job_start`, `seat_jobs`, `seat_job_kill` | `bridge/src/tools.rs:1895,1918,1925` | `:2157,2172,2173` | `bridge/src/seat_jobs.rs` |
| `rooms` plus 12 `room_*` | `bridge/src/rooms.rs:1351 TOOLS`, defs `:1375-1559` | `bridge/src/rooms.rs:1815 dispatch` | room frames |
| plugin `tool_` layers | `bridge/src/plugins.rs:97 tools` | | plugin calls, not counted in the 39 |

**Speaker.** `speaks_as = (identity == IDENTITY).then(rooms::handle)` (`bridge/src/head.rs:1691`), sent at `bridge/src/head.rs:1957`. The daemon side stores `Speaker::Peer{from}` with the text signed (`harnessd/src/harness/turns.rs:258-262`). `Speaker::peer("")` gives `unknown` (`transcript/src/lib.rs:100`).

**Risks.**
- Skew is reported and the bridge prompts anyway (S1). **Remains**.
- `room-wait` has contradictory descriptions. **Remains**: the usage text says the inbox cursor is moved (`bridge/src/main.rs:49`), the skill says stream and wait share one cursor (`claude-plugin/skills/room-waiter/SKILL.md:24`), and the code keeps its own mark file and never acks (`bridge/src/rooms.rs:1988 wait`, `wait_marked`).
- The waiter of a suffixed bridge watches the base name. Not re-traced.
- Stale "ten" counts. **Remains**: `bridge/src/tools.rs:1` "The ten verbs" and `bridge/src/tools.rs:1515` "The ten tools" against 39.
- The shipped plugin polls a personal path. **Remains** (`claude-plugin/hooks/register.tsx:27 JOBS_CMD`).
- A bridge-started main-checkout daemon passes `--no-queue`, so the queue stalls (HYP). **Remains**: `--no-queue` is unconditional (`bridge/src/daemon.rs:324`).
- The owner `repo` field has two writers. Not re-traced; the bridge still records it (`bridge/src/tools.rs:538`).
- New: the bridge now links `letibot-harnessd` for `queue_desk`, so queue verbs run with the daemon's own code in the bridge process, beside the daemon's.

**Tests.**
- `bridge/tests/end_to_end.rs` plus `mode_consent`, `reopen`, `transcript`, `wait_*`, `no_spin`, `vm_close`, `daemon_record`, and new `driver_todo.rs`, `queue_enqueue.rs`, `split_store.rs`
- `head_reap`, `permission_relay` and `placement` use a hand-rolled listener

**Since 408353957.** 01840fe24 (39), 8da965edd (`leticode_todo`), 2eff65ee1 and e66038adb (queue tools, `wait-queue`), acedd8610 and 999f652e3 (relayed cards), 8b6ade4f2, 0abb150a3 (deslop).

## S7. Merge queue, gate and git

**Sides.**
- Queue: `harnessd/src/mergequeue/{serve.rs, review.rs, verbs.rs}`, store `tokencore/src/store/queues.rs` (`ReviewState :170`, `MergeState :239`, `HealthState :307`).
- Gate: `harnessd/src/mergequeue/gate.rs` (`GATE_DEADLINE :21`, `INSTALL_MARK :66`, `StepEnv::apply :474`, `declared_gate :2056`, `run_gate :1134`).
- Remote runners: `harnessd/src/mergequeue/gate_host.rs`, `harnessd/src/mergequeue/gate_firecode.rs:147 runner_for`.
- Git: `harnessd/src/mergequeue/git.rs` (landing root `:187`, rebase `:334 rebase_in`, CAS `:515 update_ref_cas`, follow `:574 follow_checkouts`, push `:690`, `retire_if_empty :846`, `reap_worktrees :917`), `tools/src/git.rs:322 remove_worktree`.
- In use: `tools/src/lifecycle/in_use.rs:13 why_kept` (new, R307).
- The repo's `AGENTS.md` gate block.

**Shape.**
- States as before. The queue lease is 90 s with a 5 s beat (`harnessd/src/mergequeue/lease.rs:13,17`). Review deadline 1 h, 3 attempts (`harnessd/src/mergequeue/serve.rs:26,34`).
- New: a red train is bisected. Rows carry a `[half TAG]` in their evidence so the train cannot re-form whole (`harnessd/src/mergequeue/serve.rs:66 half_of`, `red_train :3300`; 2eff65ee1).
- New: review-queue seats' exec follows the operator's mode inside the clone (5e1ed9a22). Their permission asks are relayed as cards on the main session, named by role, with the box-wide rung dropped (acedd8610), and carry the main session's shadow line (999f652e3). An `allow_session` or view answer on a relayed card is kept at queue scope (R314, S-new grant seam).

**Compat.** The gate is read from the target tip, so a branch cannot change its own gate (`harnessd/src/mergequeue/serve.rs:2335 land`).

**Risks.**
- The deadline covers the whole run while the docs said "a step". **Fixed in 5e602864e** (docs): `AGENTS.md:65` and `docs/gate.md:17` now say one thirty-minute deadline for the whole run, matching `harnessd/src/mergequeue/serve.rs:785`.
- The queue never fetches, and the CAS is on the local ref, so after a push to origin the row ends `landed_local` with no install. **Remains** (no `fetch` in `serve.rs`, `harnessd/src/mergequeue/git.rs:515`).
- With `gate_host` or `gate_in=firecode` the install step runs off-box (HYP). **Remains**, not re-traced.
- Three gates. **Remains**: `scripts/gate-at.sh:61` still judges by `ok -lt 140`, `scripts/rrun` hard-codes its steps (`scripts/rrun:108-111`).
- The gate env does not isolate HOME or XDG. **Remains** (`harnessd/src/mergequeue/gate.rs:474-480` sets target, TMPDIR and jobs only).
- `settle_tree` asks only the queue daemon's registry, so it can remove a live package's clean worktree. **Fixed in a88adbb82 (R307)**: `settle_tree` (`harnessd/src/mergequeue/serve.rs:1436`) asks `why_kept` (`:1456`) after its own registry, and so do the release after a landing (`:1301`) and `reap_worktrees`. Left: `retire_if_empty` (`harnessd/src/mergequeue/git.rs:846`) is not asked, and another user's process cwd is not seen.

**Tests.** Covered:
- `harnessd/src/mergequeue/serve/tests.rs` (`a_terminal_rows_tree_another_daemon_or_a_process_holds_is_left_alone :4468`, `a_red_train_lands_nothing_and_goes_back_in_line_as_two_halves :5533`, `a_bad_branch_in_a_train_is_found_by_bisection_and_the_rest_land :5586`), `gate/tests.rs`, `git/tests.rs`, `review/*tests.rs`, `gate_host/tests.rs`
- `harnessd/src/harness/tests/reviewer_mode.rs` (new), `harnessd/src/answers/escalate_tests.rs` (relayed cards)
- `harnessd/tests/{gate_host, gate_memory, queue_census, queue_verbs_from_a_head, r66_queue}.rs`, `bridge/tests/queue_enqueue.rs`

Not covered: install with a remote runner, landing after origin moved, `retire_if_empty` on a live tree, `gate-at.sh` behaviour.

**Since 408353957.** 2eff65ee1 (bisect, queue tool), a88adbb82 (R307), 5e1ed9a22, acedd8610, 999f652e3, e66038adb, e6cafa02f, 5e602864e (docs), 6db95af0d (deslop).

## S8. Plugin hook ABI (plugin-sdk, plugins host and rano engine)

**Sides.**
- Guest SDK `plugin-sdk/src/*`, guest ABI crate `plugins/builtin/abi/src/lib.rs`.
- Host `plugins/src/{lib.rs, manifest.rs, stack.rs, trust.rs, admit.rs}` (`CONTRACT :84`, `MOVED :100`, `HOOK_PREFIXES :118` in `plugins/src/lib.rs`).
- Daemon dispatch `harnessd/src/plugins/{hook.rs, tool.rs, look.rs, conditions.rs, install.rs}`.
- Engine `plugins-rano/src/lib.rs`, rano `src/plugin.rs` (`HOOK_FUEL :11`, `HOOK_MEMORY :14`).
- Other hosts: `bridge/src/plugins.rs`, the tui.

**Hook kinds** (plugin-sdk):
- `on_tool_call` `plugin-sdk/src/tool_call.rs:10`, `on_tool_result` `plugin-sdk/src/tool_result.rs:8`
- `condition_<n>` `plugin-sdk/src/condition.rs:9`
- `notice_<kind>` `plugin-sdk/src/notice.rs:8` (23 kinds, `KINDS :66`, was 21)
- `digest_<n>` `plugin-sdk/src/digest.rs:6`, `link_<n>` `plugin-sdk/src/link.rs:6`
- `nag_todo` `plugin-sdk/src/nag.rs:19`
- `command_<verb>` `plugin-sdk/src/command.rs:6`, `complete_<n>` `plugin-sdk/src/complete.rs:6`
- `adjudicate_floor` `plugin-sdk/src/adjudicate.rs:12`
- `lifecycle_*` `plugin-sdk/src/lifecycle.rs:6`
- `steer_turn` `plugin-sdk/src/steer.rs:9`
- `compact_summary` `plugin-sdk/src/compact.rs:10`
- `review_brief`, `review_verdict` `plugin-sdk/src/review.rs:11,14`
- `brief_worktree` `plugin-sdk/src/brief.rs:9`
- `push_channel` `plugin-sdk/src/push.rs:14`
- `card_*` `plugin-sdk/src/card/mod.rs:11`
- `pane_<n>`, 9 feeds (`plugin-sdk/src/pane.rs:76 ALL`), `SESSION_FEED_ROWS=250` (`plugin-sdk/src/pane.rs:18`)
- **new** `limit_decide` `plugin-sdk/src/limit.rs:11` (1568dbcd1, R233). A layer answering it decides a rate or usage limit in place of `on_limit`, `wait_if_within` and the fallback lists. Answers include `new_session`, which forks the conversation and continues the turn there (60d76a919). Host side `harnessd/src/plugins/hook.rs:289 limit`, caller `harnessd/src/harness/limit_hook.rs:105 limit_hook`, episode state in `limit-hook.json` (`harnessd/src/harness/limit_hook.rs:14`).

**Host verbs.** `HOST_VERBS` (`plugin-sdk/src/command.rs:58`) is the set a layer may not take. Added since 408353957: `shadow`, `effort`, `fork`, `note`, `permissions`, `grants` (5cb028063 for `/fork` and `/note`; the others with R277, R314, R317).

**Version.**
- SDK `V=1` (`plugin-sdk/src/lib.rs:8`), host `CONTRACT=1`. Unchanged: `limit_` is an added prefix, not a version.
- `MOVED`: `pane_sessions` is at v3.
- The SDK test still duplicates the prefix list (`plugin-sdk/src/lib.rs:57-78`), now with `limit_`. `on_` and `provider_` are still host prefixes with no SDK module.

**Trust and limits.**
- 50M fuel and 64 MiB per call, a fresh instance per call, no wall-clock deadline.
- No host imports: `Capabilities::default()` (`plugins-rano/src/lib.rs:43`).
- Fuel is now counted per thread (`plugins-rano/src/lib.rs:185-191`, 8af025ce0 for its test).

**Risks.**
- Unpinned layers still load and answer `steer_turn`, `compact_summary`, notices and panes (`plugins/src/stack.rs:52`). **Remains, and widened**: `answers()` does not check trust (`harnessd/src/plugins/hook.rs:16`), so an unpinned layer's `limit_decide` also decides waits, model switches and `new_session` forks (HYP, not exercised).
- Pinned layers exec declared programs with no gate (`harnessd/src/plugins/conditions.rs:190-195`). **Remains**.
- The manifest `Http(host)` capability widens the access class to Network (`harnessd/src/plugins/tool.rs:145`) and has no implementation. **Remains**.
- Command files: frontmatter `allowed-tools` and `model` were parsed and not applied. **Fixed in 3107cc7a6 (R283)**: they apply to the command's turn (see S19).

**Tests.** `plugins/src/stack/tests.rs`, `plugins/src/admit.rs:70`, `plugins-rano/src/lib.rs:229-295` (4 tests), `builtins/tests/shared_target.rs`, `tui/src/app/tests/command_layer.rs:277` (head verbs vs `HOST_VERBS`). Not covered: an unpinned layer's steer, compact or `limit_decide` answer.

**Since 408353957.** 1568dbcd1, 60d76a919, e8e5a2a99, d34e39374 (limit hook), 5cb028063, 3107cc7a6, 8af025ce0, 6ec28bb9a and a33af26b3 (deslop).

## S9. Provider HTTP doors

**Sides.**
- Engine: `turn/src/engine.rs:1124 run_turn_messages_with_tail`, then `backend/src/lib.rs:78 TurnRequest`.
- Doors in `provider/src/` (ureq and rustls): `openai.rs:398`, `anthropic.rs:542`, `responses.rs:1120`, `gemini.rs:526` (each door's `complete`), `kimi.rs`, `registry.rs`, `usage.rs`, `presets.rs`. New: `effort.rs` (R277), `usage_poll.rs`, `usage_warn.rs`.
- `http/src/{lib.rs, retry.rs, reset.rs}`: blocking with no TLS. `TRY_AGAIN` `http/src/retry.rs:12`, `Policy::class` `http/src/retry.rs:155`, `Policy::trouble` `:208`.
- External APIs as before: deepseek, GLM, grok, anthropic, kimi, openai, gemini, local `llama-server :8080`, Brave (`websearch/src/lib.rs:12 ENDPOINT`), `curl` (`webfetch/src/lib.rs:438`).

**Shape.**
- `BackendError` (`backend/src/lib.rs:166`). `Trouble{RateLimit, Quota, Timeout, ...}` (`http/src/retry.rs:63`).
- New in retry (dbe4c3b9c): a jittered wait seeded per ladder (`http/src/retry.rs:411`), a no-network budget (`http/src/retry.rs:110 offline_for`), interruptible store waits, refusal bodies kept (`provider_refusal_body`).
- New: one effort scale across both CLIs and every door, quirks as config keys (`provider/src/effort.rs:8 Effort`, 66fdd5646, 132875320, R277).
- New: the usage poller. `usage_poll::EVERY` is 120 s (`provider/src/usage_poll.rs:15`). `watch` (`provider/src/usage_poll.rs:288`) keeps the latest key, a drop guard clears `running` after a panic (`provider/src/usage_poll.rs:260`), a failed round re-tells the kept reading with its age (`provider/src/usage_poll.rs:174 age`). Harness side `harnessd/src/harness/usage.rs:102 watch_usage`, row `:75 usage_row`. A limit park reads the poll's reset (f562547fb). See the new usage-poll seam.
- Keys come from `provider/src/keys.rs:204 resolve`, config file `provider/src/keys.rs:194 config_file`.

**Risks.**
- `[provider.x] url = http://` is accepted, so the key goes in the clear. **Remains** (`provider/src/registry.rs:326`).
- The HTTP head serves `/v1/chat/completions` with no auth. **Remains** (`harnessd/src/cli.rs:1999`).
- Limit classification is spread across http and provider. **Remains and widened**: harnessd's limit hook reads `Policy::trouble` too (`harnessd/src/harness/limit_hook.rs:107`), and the poll feeds parks.
- The default `on_limit` is Wait in code and "switch" in the docs. **Resolved**: `limit_policy` (`leticode-config/src/lib.rs:3251`) gives `switch` with a fallback list, else `wait`, which is what `docs/config/leticode.toml.md:36` says. `Config`'s no-file default is Wait (`harnessd/src/config.rs:1733`), consistent with no fallback.
- New: Kimi's body carries the 5h window twice (`usages` and `limits[]`), and `windows()` reads `limits[]` only when `usages` is absent (R310 left). Not seen live.

**Tests.** `provider` has 247 `#[test]` and `http` 34 (`grep -r '#\[test\]'`). `harnessd/tests/{limits, model_outage, provider_trouble, provider_choice}.rs`, `http/src/retry.rs:1034,1066` (jitter, no-network), `usage_poll::tests` (R310). Live tests hit production GLM (`LETIBOT_COMPLETION_URL`).

**Since 408353957.** dbe4c3b9c, 66fdd5646, 132875320, 711bb1b27, bb6c32a21, f562547fb, ca998ddee, c083ef021 and b01fb116d (deslop).

## S10. TUI and rano renderer

**Sides.**
- `tui` (131 files use `rano::`), `ui` (`ui/src/ansi.rs:72` makes `rano::render::Line`), `code` (`code/src/lib.rs:12` syntax), `harnessd` (7 files), `bridge` (1), `plugins-rano`.
- rano at rev `c07e8ea`, unchanged, pinned separately in `tui/Cargo.toml:51`, `ui/Cargo.toml:52`, `code/Cargo.toml:31`, `bridge/Cargo.toml:67`, `harnessd/Cargo.toml:80` and `plugins-rano/Cargo.toml:18`.

**Shape.** As before: `rano::style::{Role,Palette}`, `rano::agent::*`, `rano::render`, `rano::term` (`Terminal` at `tui/src/head.rs:31`), `rano::width`, `rano::markdown::IncrementalMarkdown`, `rano::theme`.

**Compat.** A rano change is a branch, a DM to `cc@rano`, and a rev bump in 6 manifests.

**Risks.**
- rano's `src/letibot.rs` copy of the socket client is at protocol 72 (rano `src/letibot.rs`, line 26), now 6 behind. **Remains**.
- The Cargo.toml comments still say "absolute path" (`ui/Cargo.toml:32`, `code/Cargo.toml:28`) while the dependency is a git rev. **Remains**.
- `rano/src/agent/` (32 modules) is letibot's widget set in another repo. **Remains**.

**Tests.** `tui/tests/rano_editor_head.rs` (a real daemon). Screen tests in `tui/src/app/tests/` re-bless with `LETIBOT_BLESS=1`.

**Since 408353957.** No rev bump. ea8f0402a, c083f7136 and peers (deslop), 79cfb06f7 (thinking marker), 6c94d7c47 (verb table).

## S11. Shadow and harness [BOTH-SINCE]

**Sides.**
- Harness glue: `harnessd/src/harness/shadow_seat.rs` (`relay_annotator :17`, `tend_shadow :88`, `shadow_during_round :125`, `submit_turn_end :296`, `shadow_skill_picks :382`, `shadow_on_the_last_answer :422`). Callers: `harnessd/src/harness/turns.rs:1485` (restart owed), `:2194` (skill picks), `:2436` (turn end).
- Gate sink: `harnessd/src/shadow/permission.rs:85 ShadowSink` (`observe :96`), installed at `harnessd/src/harness/open.rs:526-531`.
- Relayed cards: `harnessd/src/shadow/permission.rs:288 Relay` (an `Annotator`, `sessionlog/src/pending_asks.rs:94`), installed on the queue's `PendingAsks` at `harnessd/src/sessions.rs:3006 set_annotator`. See S20.
- Shadow: `harnessd/src/shadow.rs` (`Job :30`, `deadline :56`, `open_for :144`, `on_outcome :185`), `shadow/{feed, conversation, resume, verdict, guard, words, record, replay, judge, runner, during, turn_end}.rs`.
- Note proposals: `/note accept|decline [N]` (`harnessd/src/slash.rs:118`, `harnessd/src/harness/note_accept.rs`), 616c753f4.
- Provider: `TurnRequest::json`, thinking off.

**Shape.**
- Feed rows carry an `Author` (`harnessd/src/shadow/words.rs:9 Author`): Operator, Peer, Harness, Assistant, Tool. `rows_of` is `harnessd/src/shadow/feed.rs:45`.
- The persistent conversation is no longer memory-only. It resumes from its record (`harnessd/src/shadow/conversation.rs:122 resume`, `harnessd/src/shadow/resume.rs:70 parse`, `:102 matches` checks the kept turn against the log chain, `:132 kept_of`), a489bc75a. Budget 64 KB (`harnessd/src/shadow/conversation.rs:11 BUDGET`).
- Verdicts: `deny_unknown_fields` (`harnessd/src/shadow/verdict.rs:171`). Quotes at least `MIN_QUOTE` (`harnessd/src/shadow/verdict.rs:12`) and inside authorised spans (`verify :241`, `authored_span :72`). A permission verdict is allow or ask only (`:459-463`). Under `strict` an allow's rule must be a listed rule (`:479-488`). The denylist on allow is `harnessd/src/shadow/guard.rs:29 allow_passes`.
- Config: `[shadow] enabled`, `mode` (persistent or stateless, `harnessd/src/shadow/config.rs:52`), `fresh_every`, `answer_reads` (`answers_reads :84`, on with the permission job unless false), `[shadow.<job>]` models in `providers.toml` (`provider/src/keys.rs:581 ShadowLines`).
- The permission job is no longer observe-only. It answers a review-queue seat's relayed read (R317, 913e532d6), and puts one verdict line on every relayed card (999f652e3).

**Risks.**
- Stateless mode: the during-offer author rule (`harnessd/src/shadow/verdict.rs:413-429`) and the `none`-needs-evidence rule (`:345`) are still strict-only, and only the persistent conversation sets `strict: true` (`harnessd/src/shadow/conversation.rs:241`). R261 skill picks act in both modes. **Remains.**
- `authored_span` (`harnessd/src/shadow/verdict.rs:72`) accepts a quote if any occurrence of it lies in an authorised span, so tool text the model echoes becomes quotable. **Remains.**
- `Author::Operator` still includes command-file bodies (S19) and flowy/acp prompts. **Remains.**
- New: the resumed conversation is extra context from disk. `Kept::matches` binds it to the log chain, but a record from an older build (`stateless` then persistent) resumes without a fresh-every comparison until the next Nth question (HYP).
- New: `harnessd/src/sessions.rs:3005` still says the annotator is "observe-only". It acts since 913e532d6. Stale comment.

**Tests.** `harnessd/src/shadow/tests.rs` (1.5k lines), `harnessd/src/shadow/persistent_tests.rs` (0.9k, resume cases `:812-861`), `harnessd/src/answers/escalate_tests.rs` (R317 cases), `harnessd/src/harness/tests/shadow_turn_end.rs`, `shadow_during.rs`, `prefix_on_open.rs`. Not covered: the stateless during-offer author rule.

**Since 408353957.** a489bc75a (resume, replay both modes), 616c753f4 (note proposals), 999f652e3 (shadow line on relayed cards), 913e532d6 (answers relayed reads), de0b555e3 (shadow grants in `session_grant`), 1cba39a02 (a changed skill pin or note reseats an automatic session).

## S12. Config keys (config-ref and every parser) [BOTH-SINCE]

**Reference.** `crates/config-ref`:
- `File/Table/Key` at `config-ref/src/lib.rs:20,36,47`
- files are `config-ref/src/files/mod.rs:23 ALL`: leticode.toml, providers.toml, head.toml, hosts.toml, prompts.toml, theme.toml, permission.json, plugin-trust.json, modes.tsv
- renderers `config-ref/src/lib.rs:118 text`, `:140 overview`, `:164 markdown` (into `docs/config/*.md`), `:201 header`, `:230 splice`
- CLI `letibot-multi --config-reference [--markdown] [FILE]` and `--config-header FILE [--as NAME]` (`letibot/src/main.rs:332,334`)

**Keys added since 408353957** (`git diff 408353957 9e9312b06 -- crates/config-ref/src/files`):
- leticode.toml: `[provider.usage_warn]` and `[provider.<name>.usage_warn]` (5h, weekly, monthly thresholds), `[mcp_projects] trusted`, `[mcp.<name>] url` now http or https, `[skills]` policy `command`, `[queue] gate_train` (a red stack is bisected), `[subagents] max_running`, `daemon_max_running`, `when_full`, `[wordfreq] mode`, `download`, `[gc] disk_every`, `targets_cap`.
- providers.toml: `[shadow] answer_reads`; door compat keys `effort_field`, `effort_levels`, `max_tokens_field`, `reasoning_echo`, `reasoning_required`, `tool_schema`, `json_object`, `cache_key`, `client_gate`, `thinking_locked` (R277, 66fdd5646, 132875320); `effort` on the off..max scale.
- `[provider]` note says a `limit_decide` layer decides in place of `on_limit` (S23).

**Parsers.**
- `leticode-config/src/lib.rs:2787 load` (separate readers for `[bridge]` `bridge/src/settings.rs:60 load`, `[gc]` `tools/src/lifecycle/config.rs:66 load`, `[docker]`, `[embeddings]`, `[queue]` `harnessd/src/mergequeue/settings.rs`). `[wordfreq]` is accepted unread by leticode-config (`leticode-config/src/lib.rs:3544`) and read by harnessd with `[embeddings]` (19a2b2cba).
- `provider/src/keys.rs:850 parse_file`, `provider/src/registry.rs`
- `tools/src/remote.rs:156 hosts_file`, `:190 parse` (hosts.toml)
- `tools/src/permission.rs:684 load_file`
- `tui/src/prefs.rs:251 load`
- `harnessd/src/config.rs:1228 path` (prompts)
- `plugins/src/trust.rs:371`
- `harnessd/src/modes.rs:40 legacy_path`
- **Shell parsers**: awk over providers.toml in `scripts/letibot:1102,1126`, awk over hosts.toml in `scripts/rrun:24`

**Guards.**
- `config-ref/src/tests.rs:58 docs_match_the_tables`, plus probes in `config-ref/src/probe.rs` (`config-ref/src/probe.rs:62 literals`, `config-ref/src/probe.rs:179 refused_examples`, `config-ref/src/probe.rs:198 unlisted_accepted`).
- Forward-only checks for permission.json, plugin-trust.json and `[brave]`.
- Nothing covers the shell parsers.

**Risks.**
- The config dir is resolved independently in at least 7 places. `provider/src/keys.rs:194 config_file`, `harnessd/src/modes.rs:41` and `tools/src/permission.rs:673` still take an empty `XDG_CONFIG_HOME` as a cwd-relative base. **Remains.**
- `[provider]`, `[fallback]` and `[memory]` exist in both files with different meanings. **Remains.**
- `location:` strings are prose. **Remains** (not re-audited key by key).
- New: `[mcp_projects] trusted` lets a directory prefix start every checkout's `.mcp.json` below it (S18).

**Since 408353957.** 66fdd5646, 132875320 (provider compat keys), bb6c32a21 (usage_warn), dd76c1bb3 (mcp_projects, https), c923859a9 (`[subagents]`), aabe84db8 and 19a2b2cba (`[wordfreq]`), 3af2084b3 (`disk_every`), f95103baa (`targets_cap`), 913e532d6 (`answer_reads`), e4a34db70 (policy skills), 2eff65ee1 (`gate_train` bisect).

## S13. Environment variables

The full table is `/data/review-prep/notes/E-config-env-cli.md` (written at 408353957). A grep of `crates scripts tests` finds 139 `LETIBOT_*` names (136 at 408353957). New: `LETIBOT_RESTART_STEPS`, `LETIBOT_VM_BOOT_SLOTS`, `LETIBOT_WORDFREQ_SOURCE` (test opt-in), `LETIBOT_ASK_UNDUMPABLE`. Gone: `LETIBOT_LAUNCHER_REF`. The load-bearing ones:

| var | readers | writers | note |
|---|---|---|---|
| `LETIBOT_STORE` | `scripts/letibot:374`, `bridge/src/daemon.rs:90 default_store`, `letibot/src/gc.rs:1070`, `letibot/src/doctor.rs:142`, `letibot/src/migrate.rs:1808` | activation `activation/src/render.rs:126` (test) | harnessd takes only `--store`. A stale value naming `sessions.db` is now folded to its directory (`scripts/letibot:375-389`) |
| `LETIBOT_SOCKET`, `LETIBOT_SESSION`, `LETIBOT_COMMAND` | `scripts/letibot:372`, `bridge/src/daemon.rs:219`, `tools/src/exec/confine.rs:703`, `letibot/src/roles.rs:85-87` | `harnessd/src/harness/open.rs:1367-1373` sets them for every spawned command | Agent-run tests inherit the live socket. **Remains** |
| `SUDO_ASKPASS`, `LETIBOT_ASKPASS` | `tools/src/builtins/host_config.rs:308`, `harnessd/src/sudo.rs:39` | `harnessd/src/harness/open.rs:1379-1382` | askpass routes over the socket |
| `HOME`, `XDG_{CONFIG,DATA,RUNTIME,CACHE,STATE}_HOME` | 7 config resolvers. The launcher ignores `XDG_DATA_HOME` except for the room (`scripts/letibot:374,1948`). Runtime: launcher `:355`, `sessionlog/src/server.rs:37` | `scripts/gate-at.sh` | drift (S12). Nine daemon-spawning tests now get a private `XDG_RUNTIME_DIR` (f3fddd6b4) |
| `LETIBOT_PROVIDERS` | `scripts/letibot:1095` only | - | the comment at `scripts/letibot:1093` still says it points both readers. No Rust reads it. **Remains** |
| `LETIBOT_PERMISSION`, `LETIBOT_PLUGIN_TRUST`, `LETIBOT_HOSTS`, `LETIBOT_GC_SETTINGS`, `LETIBOT_NOTES_REPO`, `LETIBOT_LOGS_ROOT`, `LETIBOT_LANDING_ROOT` | `harnessd/src/config.rs:1648`, `plugins/src/trust.rs:371`, `tools/src/remote.rs:157`, `tools/src/lifecycle/config.rs:67`, `harnessd/src/config.rs:3748`, `harnessd/src/config.rs:1890`, `harnessd/src/mergequeue/git.rs:183` | tests | test doors and overrides |
| `LETIBOT_SCOPE_LATE` | `harnessd/src/late_scope.rs:12 ENV` | launcher `scripts/letibot:331` | shell-to-Rust file contract |
| `LETIBOT_RESTART_STEPS` | `scripts/letibot:1862` (unset at `:1863`) | `scripts/letibot:2465,2474` | new, R312. Carries the step mode to the `--stop` and `--continue` that `--restart` runs |
| `LETIBOT_VM_BOOT_SLOTS` | `tools/src/firecode/boot.rs:96` | - | new, d74c82e1f. Default `DEFAULT_SLOTS` 2 (`tools/src/firecode/boot.rs:17`) |
| `LETIBOT_ROOM_SOCKET`, `LETIBOT_ROOM_NAME` | `harnessd/src/room/mod.rs:22`, `bridge/src/rooms.rs:16,342`, `tools/src/exec/confine.rs:673` | `tests/launcher.sh` | |
| `LETIBOT_ACTIVATION` | `activation/src/lib.rs:22` | unit text `activation/src/render.rs:157,205` | |
| `LETIBOT_ROLE`, `LETIBOT_BASH`, `LETIBOT_INVOKED_AS` | `scripts/letibot` | `scripts/leticode:21-23` | |
| `GH_TOKEN`, `LETIBOT_GH` | `tools/src/exec/perimeter.rs:1011`, `github/src/gh.rs:41` | `tools/src/exec/seat.rs:46 github_env` (top-level seat) | masked in job output |
| `LETIBOT_REQUIRE_APPARATUS`, `LETIBOT_REQUIRE_MODEL`, `LETIBOT_COMPLETION_URL`, `LETIBOT_CHECK_REAL_STORE` | `tokencore/src/apparatus.rs:55,109`, `turn/src/serving.rs:39,49` | the AGENTS.md gate | turn a skip into a failure, or opt in to the real store |
| `LETIBOT_LLAMA_DIR`, `LETIBOT_LLAMA_LIB` | every `build.rs`, `llama/build-support.rs:22` | `install.sh` | baked into the rpath |
| `CUDA_VISIBLE_DEVICES` | - | `scripts/letibot:401 NOGPU`, `bridge/src/daemon.rs:406`, `letibot/src/rooms.rs:165` | daemon runs without a GPU |

## S14. CLI verbs (launcher, letibot-multi and harnessd flags) [BOTH-SINCE]

**Sides.**
- `scripts/letibot` (3173 lines, was 3720 before the deslop). It resolves WORKSPACE `:123`, RUNDIR `:354-362`, KEY (sha12) `:371`, SOCKET `:372`, STORE `:374-389`, LOG `:396`, and the providers awk `:1102` before any exec.
- `scripts/leticode` (35 lines: exports its role, then execs `letibot`).
- `letibot-multi` (`letibot/src/main.rs`): `REGISTRY :111` for argv0 roles. Verbs are an if-chain at `letibot/src/main.rs:291-339`: stats, holds, gc (now `--targets`, f95103baa), hold, release, where, boundary, notes, shadow, browser, rooms, fb, migrate, attach, session, doctor, `--config-*`.
- harnessd flags in `harnessd/src/cli.rs` (`--list-sessions :577`, `--calibrate :669`, `--room :745`, `--prompt :955`).
- `scripts/rrun`.

**Routing.**
- the bare form runs `detach` (`scripts/letibot:337`) for harnessd, then `exec "$HEAD" --socket` (`scripts/letibot:3118`)
- a one-shot is `harnessd ... --prompt` (`scripts/letibot:3006`)
- `session VERB` goes to `letibot-multi session --store` (`scripts/letibot:2265`). The old `--sessions`, `--rename`, `--delete`, `--archive`, `--unarchive` flags stay one release as aliases with a note (`scripts/letibot:2240-2249`)
- `stats | boundary | fb | holds | gc` go to `letibot-multi` (`scripts/letibot:2737`)
- `doctor` and `--config-*` go to `letibot-multi` (`scripts/letibot:437-443`)
- `--restart` prints one `restart:` line per step (`scripts/letibot:1859-1893`, R312, fccb8e110)

**Risks.**
- The multicall verbs `hold`, `release` and bare `where` still have no launcher arm (`scripts/letibot:2737` lists only stats, boundary, fb, holds, gc). A bare word ends flag parsing, so `letibot hold X` is a billed one-shot prompt. `--where` has an arm (`scripts/letibot:637`). **Remains.**
- The verbs live outside `REGISTRY`, with no check against the launcher. **Remains.**
- Socket key parity between shell and Rust is held only by `tests/launcher.sh:601`. **Remains.**
- The restart row does not say which launcher or seat restarted the daemon (R312 left). The stop reason reaches only the old daemon's interrupt.

**Tests.** `tests/launcher.sh` (1714 lines, run by `tools/tests/shell_suites.rs:13`), including "--restart says its steps in order"; `letibot/tests/{dispatch, session_verbs, where, no_hang}.rs`, `harnessd/src/cli/tests.rs`, `scripts/check-dist-names.sh --built`.

**Since 408353957.** 747b98cc1 (session verbs), fccb8e110 (restart steps), f95103baa (`gc --targets`), 27895afc2 (launcher deslop).

## S15. Activation and systemd boundary

**Sides.**
- Launcher scope: `scripts/letibot:212-336`. The probe `scope_probe_start :230` runs `systemd-run --user --scope --quiet --collect --expand-environment=no true` in the background (`:243`) and the answer is read bounded (`probe_bounded :301`). `scope_run :317` builds `systemd-run --user --scope --unit <stem>-<KEY>-$$ --slice ... -p Delegate=yes -p OOMPolicy=continue`, plus `RuntimeMaxSec` when `LETIBOT_SCOPE_RUNTIME_MAX` is set and accepted.
- Daemon: `harnessd/src/late_scope.rs:12 ENV`, `:53 watch`. Memory delegation is `tools/src/exec/scope.rs:1040 delegate_memory`.
- Helper units: `activation/src/bounded.rs` (`QUERY :9`, `output :31`, `text :72`), `activation/src/control.rs:205,269`, `activation/src/render.rs`.
- Gate sibling scopes: `harnessd/src/mergequeue/gate.rs:1209`.
- Per-job IO: `tools/src/exec/diskio.rs`.

**Shape.** The late-scope file is a TSV line. Unit and plist text sets `LETIBOT_ACTIVATION`.

**Risks.**
- After a successful probe, the real `systemd-run --scope` in `detach` (`scripts/letibot:346-347`) has no bound. **Remains.**
- On success, `bounded::output` joins its pipe-drain threads (`activation/src/bounded.rs:55-56`), so a grandchild holding the pipe blocks it. **Remains.**
- The `systemd-run` sites in jobs and the gate were not checked for bounds. **Remains.**
- Without a scope the daemon is "UNCAPPED". Since 5cc2bced2 a bash session says so once, and again only when the reason changes (R302).

**Tests.** `activation/src/bounded.rs:75-98`, `letibot/tests/no_hang.rs`, `tests/launcher.sh`, `harnessd/src/late_scope.rs:85 the_answer_says_what_systemd_said_and_what_was_done`.

**Since 408353957.** 5cc2bced2 (uncapped note once), deslop only otherwise.

## S16. Reaper, jobs, restart marker and delivery [BOTH-SINCE]

**Sides.**
- Writer: `tools/src/lifecycle/reap.rs:1151 end_daemon` writes the marker at `:1192` (`tools/src/lifecycle.rs:814 restart_marker`).
- Reader: `harnessd/src/daemon.rs:291`, then `stop_on_signal`, then `harnessd/src/kept_jobs.rs`.
- Lease renewal: `harnessd/src/reap_pass.rs:152 renew_held` (a landed package's head does not renew, `:163`), `LeaseBeat :290`, then `tokencore/src/store/owners.rs:593 reopen_owner`.
- Decision: `tools/src/lifecycle/reap.rs:146 close_due`.
- Disk half: one daemon per box at `[gc] disk_every` (`harnessd/src/reap_pass.rs:216 DiskTurn`, lock `:276 take_lock`), 3af2084b3. The idle half runs in every daemon.
- Job delivery across a restart (R313): `Harness::the_batch_stored` (`harnessd/src/harness.rs:1405`), `Deliveries::stored` (`harnessd/src/harness/deliveries.rs:1051`), `JobWatchers::stored` (`harnessd/src/jobwatch.rs:815`), `restart_owed` (`harnessd/src/harness.rs:422`), `Harness::wake` refusing once stopping (`harnessd/src/harness/turns.rs:437-440`), `restore_after_restart :920`, `deliver_settlements_left_undelivered :812`.
- Split-store routing: `Store::at` (`tokencore/src/store.rs:1768`), used by `job_context` (`tokencore/src/store/subagents.rs:511`) and the other project tables, and `import_tx` (`tokencore/src/store/transfer.rs:312`), 9489dbac3, 313efe571.
- Join token: carries the daemon's pid (cfc86cfa2, `tools/src/exec/host.rs:2134`).

**Shape.** `owner(id)` with state, lease_ms, hard_ttl_ms and created_ms. `holding(owner_id, kind, host, handle)`. `reap_event`. A job's `delivered` is now written in the `opened` callback of `submit_items_then`, after `persist()` and before `run_rounds`.

**Risks.**
- Hard TTL counts from `created_ms` after a reopen: `reopen` keeps `was.created_ms` (`tools/src/lifecycle.rs:459-460`), and `close_due` compares `now_ms - o.created_ms` for every kind but `Session` (`tools/src/lifecycle/reap.rs:160-163`). A Package reopened after its TTL is closed at the next pass. **Remains.**
- A daemon never reaps its own pid (`tools/src/lifecycle/reap.rs:1175`), so its own owner may stay `Closing` (HYP). **Remains.**
- `DAEMON_GRACE` 5 s (`tools/src/lifecycle/reap.rs:52`) then SIGKILL can interrupt a detach (HYP). **Remains.**
- Delivery marked after the turn (R313 cause 1). **Fixed in 313efe571.**
- Restore missed project tables on a split store (R313 cause 2). **Fixed in 9489dbac3**, guarded by `tokencore/src/store/split_coverage_tests.rs`.
- New: `Harness::wake` checks `is_stopping` twice (`harnessd/src/harness/turns.rs:440,668`), but the launcher's restart interrupts the turn before the daemon gets its signal (R313 left, `scripts/letibot:2465`), so a wake between the interrupt and SIGTERM can still open a turn the stop then cuts.

**Tests.** `tools/src/lifecycle/reap/tests.rs:132-701`, `harnessd/tests/session_lifecycle.rs:877,973`, `harnessd/src/harness/tests/kept_jobs.rs`, `harnessd/tests/restart_restore.rs` (five R313 cases on a split store), `tokencore/src/store/split_coverage_tests.rs`.

**Since 408353957.** 313efe571, 9489dbac3, 3ea7ba818 (R313); 3af2084b3 (disk half); cfc86cfa2 (join token); db71b2f4e (a job's start stamped before the fork); a88adbb82 (cleanup asks `in_use`, see invariants).

## S17. Remote hosts, firecode and ssh

**Sides.**
- `tools/src/remote.rs` (`hosts_file :156`, `parse :190`, `flags :699`, `HOME_DIR :1304`), `tools/src/lifecycle/remote.rs` (`reap_script :127`, `host_pass :173`), `tools/src/firecode.rs` (`FirecodeSpec :416`, `up :568`, `down :1059`), `tools/src/firecode/boot.rs` (box-wide boot slots and a load-scaled up deadline, `up_deadline :75`, d74c82e1f), `harnessd/src/mergequeue/gate_host.rs`.
- The remote pass now removes a settled owner's home, ledgers and fanout dirs (`remove_script`, e30635163).
- `scripts/rrun` has its own ssh flags (`scripts/rrun:22-29 ssh_for`) and runs on the host as a `systemd-run --user` service.
- External: firecode `bin/firecode`, `~/.cache/letibot/firecode/<sid>-<ts>`.

**Risks.**
- `rrun` takes the key from `keys/<host>` and the user from `RRUN_USER` or `$USER`, never from hosts.toml (`scripts/rrun:23-28`). **Remains.**
- There is still no autonomous host-local reaper. The "host-local reaper" in `tools/src/lifecycle/remote.rs:4` is `reap_script`, run from the home box over ssh by `host_pass`. **Remains.**
- Copy-out silently lost work after a storage move (operator memory). Partial in code: `down` waits for the copy-out record (`tools/src/firecode.rs:1072-1112`). **Remains** as process.
- `scripts/gate-at.sh:40-42` links the real `hosts.toml`, `keys` and `known_hosts` into the gate's test HOME. **Remains.**

**Tests.** `tools/src/remote/tests.rs`, `tools/tests/remote_{confine,hosts,sessions,sweep,sweep_queue,sweep_sentence,vm}.rs`, `harnessd/tests/remote_{close,host,settle}.rs`, `tools/tests/firecode_live.rs`, `tools/src/firecode/tests.rs`, `tests/rrun.sh`.

**Since 408353957.** e30635163, d74c82e1f.

## S18. MCP client servers [BOTH-SINCE]

**Sides.**
- Client: `mcp/src/lib.rs` (`PROTOCOL_VERSION "2025-06-18" :20`, `DEFAULT_TIMEOUT` 60 s `:23`, `backoff :60`, `Entry :255` with `tools: RwLock` and `link: Mutex<Link>`, `up :277` reconnects, `call :311`, `Catalog::start :336`), `mcp/src/wire.rs` (stdio `spawn :73`, timeout `:217`, `TOOLS_CHANGED :39`).
- Pool and config: `harnessd/src/mcp.rs` (`servers :16` joins the user `[mcp]` and a trusted checkout's `.mcp.json`, `specs :81`, `pool :139`). Trust: `leticode-config/src/lib.rs:1273 mcp_trusts`.
- Doors: `tools/src/builtins/external/mcp.rs` (`McpCatalog :56`, `SEARCH`/`CALL :178-179`, `mount :381`).
- Gate: the call is `Access::Network` (`tools/src/builtins/external/mcp.rs:304`), preapproved per catalogued tool (`tools/src/adjudicate.rs:3887-3888`).
- Warnings: one `mcp_server` code (`harnessd/src/harness/open.rs:2121-2131`, e52000bba).

**Risks.**
- A timeout sends no `notifications/cancelled` and does not mark the server dead: `WireError::Timeout` becomes `Transport` and only `Gone` sets `dead` (`mcp/src/lib.rs:157-164`). **Remains.**
- The per-server `Mutex<Link>` is held across the whole call (`mcp/src/lib.rs:312`), so one slow call blocks every session in the pool on that server. Listings no longer wait (`RwLock`). **Changed, partly remains.**
- No reconnect and no tool refresh. **Fixed in dd76c1bb3**: restart with backoff (`up :277`), `tools/list_changed` re-listing (`refresh :267`).
- Stdio servers run unconfined with the daemon env (`mcp/src/wire.rs:79-90`). **Remains**, and widened: a checkout under `[mcp_projects] trusted` now starts its own `.mcp.json` servers. Trust is a directory prefix (`mcp_trusts`), so every repo cloned under a trusted directory runs its servers (HYP).
- Every call is classed Network. **Changed**: still `Access::Network`, but `permission.json` can now preapprove a catalogued tool by name (`{"mcp_call": {"fs__read_*": "allow"}}`, `tools/src/adjudicate.rs:3887`), so a glob over server tools admits unseen calls with no card.
- The description lint is only on the test-only `mount()` path. **Remains** (not re-checked).

**Tests.** `mcp/tests/fake_server.rs` (8 tests, `tools_are_listed_searched_and_called :60`, `a_call_not_preapproved_goes_to_the_gate_and_does_not_run :261`), `mcp/tests/http_server.rs` (5), `mcp/src/lib.rs` (3, backoff), `harnessd/src/mcp.rs` (3, trust). Not covered: timeout, hang or contention.

**Since 408353957.** dd76c1bb3 (mcp, harnessd, leticode-config, config-ref, http), e52000bba (one `mcp_server` row).

## S19. Command files, skills and offers

**Sides.**
- `harnessd/src/commands.rs` (`dirs :42`, `find :72`, `tool_rules :327`, `ToolRules :352`), which reads `.letibot/commands`, `.claude/commands` and `<config>/commands`, with dir/name namespaces.
- `harnessd/src/sessions.rs:1141 command_file`: the model (`turn_model`) and `allowed-tools` (`set_turn_tools`) apply to the one turn (`:1188-1208`), then the body goes through `run_prompt` with `peer: None` (`:1211`), as the operator.
- Protocol `DAEMON_COMMANDS_KEY` (S1), tui composer.
- `harnessd/src/skills_offer.rs`, `Harness::load_skill` (`harnessd/src/harness.rs:1539`, an operator utterance). Policy skills (`kind: policy`) are delivered as rules at the first matching bash call, never offered (e4a34db70). Offer words are checked against the `[wordfreq]` list (2133a2e0f, aabe84db8).

**Risks.**
- Frontmatter `allowed-tools` and `model` were parsed and not applied. **Fixed in 3107cc7a6** (R283): a call outside the list is refused, the model is switched for the turn and switched back.
- Repo-supplied text becomes operator authority for the auth trail and the shadow: `run_prompt(.., None)` at `harnessd/src/sessions.rs:1211`. **Remains.**
- `/skills NAME` loads as an operator utterance. **Remains.**

**Tests.** `harnessd/tests/command_files.rs`, `tui/src/app/tests/composer.rs`.

**Since 408353957.** 3107cc7a6, e4a34db70, 2133a2e0f, aabe84db8.

## S20. Shadow and gate (oracle seam) [NEW]

**Sides.**
- Oracle seam: `tools/src/authorise.rs:1010 AuthorisationOracle`, driven by `ModelAdjudicator` (`:1693`). `ModelBrief :224`, `OracleScope :564` (`narrowest :585`). A trail that was not collected returns `Unavailable` before the oracle is asked (`:1842-1850`, `TrailProvenance::NotCollected :92`).
- The one implementation is `HttpOracle` (`harnessd/src/oracle.rs:27`, `impl AuthorisationOracle :337`). `Prereq::Oracle` is met only when `cfg.oracle` is set (`harnessd/src/harness/open.rs:2860-2861`), and `requires_given` always asks for it (`tools/src/mode.rs:508,522`).
- The shadow's acting path today is not the oracle. It is an `Annotator` on the relay: `PendingAsks::note_before` (`sessionlog/src/pending_asks.rs:255`) asks `Relay::may_answer` then `Relay::note` (`harnessd/src/shadow/permission.rs:407`, `:403`), which calls `judge` (`:327`).
- Scope: `harnessd/src/shadow/permission.rs:263 scope` (a review-queue seat by role, `queue_kind :250`, a read-only view widening, `answers::view_read` at `harnessd/src/answers.rs:432`), path past `harnessd/src/shadow/guard.rs:138 read_passes`.
- Acting rule: `harnessd/src/shadow/permission.rs:272 acting_rule`, over `rules_of :476` (every allow rule of `permission.json`, as `"<permission> <pattern>"`).
- Breaker: `BREAKER_AT` 2 (`harnessd/src/shadow/permission.rs:170`), `disagree :226`. Disagreements are a revoke (`revoked :444`) or an operator No to a shape the shadow granted (`answered :414`). In-process, resets at restart.
- Switch: `[shadow] answer_reads` (`harnessd/src/shadow/config.rs:84`). The gate side: `ask_to_see` (`tools/src/adjudicate.rs:2678`), `last_relayed` (`:991`).

**Shape.**
- An acting answer is `Note::Answer{option_id: "grant_ro", by: "shadow:<judge>"}`, one display-only row on the main session and one record row with the verdict, rule, evidence and grant.
- Everything else is `Note::Line`, the card's shadow line (999f652e3).

**Plan versus code (R316).**
- Planned: `ShadowOracle` implementing `AuthorisationOracle` in `harnessd/src/shadow/`, the shadow as the `Oracle` prerequisite, `[shadow] scope = reads | writes | exec_known | all`, stage promotion on replays 7 days apart, the Relay path retired. None of these exist at 9e9312b06 (`grep ShadowOracle` finds nothing).
- The gap step 1 hit: `ask_to_see` builds its request with `AuthorisationTrail::default()` (`tools/src/adjudicate.rs:2787`), so a view card cannot reach any oracle. R316 stage 1b closes it.

**Risks.**
- `acting_rule` accepts a rule if it is at least 8 bytes and a substring of any allow rule (`harnessd/src/shadow/permission.rs:280-281`). Nothing ties the rule to reads or to the path. Any 8-byte fragment of `bash git log*` qualifies, so the model's judgement is the only real gate (verified by reading).
- `read_passes` refuses secret stores, `keys` and `~/.config/letibot`, but not `~/bin/fleet` (the fleet private key, I17) or `/proc/<pid>/environ` (HYP). A reviewer can be granted either with no card.
- The breaker lives in the process. Grants it issued survive a restart in `session_grant` while its disagreement count resets (R317 left).
- A sibling waiting on the same path (`Claim::Shares`) is granted too (R317 left).
- The shadow's answer is gated by the main session's `cfg.permission` (`harnessd/src/harness/shadow_seat.rs:29`), not the reviewer's.

**Tests.** `harnessd/src/answers/escalate_tests.rs` (R317 seven cases), `harnessd/src/shadow/tests.rs` (`read_passes`, `answer_reads`, `only_a_listed_rule_on_an_allow_verdict_acts`). Not covered: a fragment rule unrelated to reads, `~/bin/fleet`, `/proc`.

**Since 408353957.** 999f652e3, acedd8610, 913e532d6, de0b555e3, 7148e2c88 (R316 plan).

## S21. Grant ledger and gate [NEW]

**Sides.**
- Store: `tokencore/src/store/grants.rs` (`session_grant` DDL `:10`, `GrantRow :27`, `put_grant :53`, `grants :75`, `grants_by :83`, `revoke_grant :115`, `clear_grants :120`, `forget_session_grants :128`). Schema 50 adds the table (additive, a project table in a split set), 51 adds `granted_by`.
- Ledger: `tools/src/grant_ledger.rs` (`GENERATION :17`, `bump :20`, `queue_of :30`, `Scope :37`, `Kept :76`, `Stored :215`, `GrantLedger :236`).
- Daemon side: `harnessd/src/grants.rs` (`StoreLedger::new :18`, `keep :28`, `revoke :85`, `shadow :121`, `scopes_for :183`, `kept_views :192`, `permissions :215`).
- Gate: `AdjudicatedGate::with_grant_ledger` (`tools/src/adjudicate.rs:1630`), `refresh_grants :1654`, `view_revocations :2879`; `ViewGrants::revoke` (`tools/src/view.rs:237`), `revoked_since :250`.
- Relay labelling: `harnessd/src/answers.rs` (`take_relayed :70`, `queue_scoped :495`, `queue_label :520`).
- Heads: `/permissions` (alias `/grants`) and `/permissions revoke ID`; `/shadow revoke [N]`. Both go through `grants::revoke`.

**Shape.** One row per `(session_id, scope, kind, key)`. `scope` is `session` or `queue` (the gatekeeper's id, `queue_of`). `kind` is `view`, `allow` or `memory`. A queue row lives until revoked. A mode change clears the session's `allow` and `memory` rows.

**Risks.**
- `GENERATION` is a process-wide atomic (`tools/src/grant_ledger.rs:17`). A revoke in another daemon process bumps only that process. A gate in this process re-reads the store only after a local keep or revoke (`tools/src/adjudicate.rs:1659-1662`), so a grant revoked from another daemon stays live here until something local moves the generation or the session reopens (verified by reading; HYP that two daemons share one scope in practice).
- A ledger that cannot be read leaves the copy as it was (`tools/src/adjudicate.rs:1669`), silently.
- A revoked view path is unbound at a running session's next call. Commands already running keep their namespace (R314 left).
- A child's view grants are kept under the child's id, so a parent does not re-read them after a restart (R314 left).
- The `/config` pane does not list grants (R314 left). A stale-facts case.
- `queue_of` trusts the id prefix `s-gatekeeper` (`tools/src/grant_ledger.rs:30-33`). Ids are minted by the daemon, so this holds while no other path names sessions.

**Tests.** `harnessd` `a_view_grant_survives_a_daemon_restart`, `a_queue_grant_admits_the_next_reviewer_and_survives_a_restart`, `a_queue_grant_does_not_widen_the_main_session`, `revoking_a_grant_removes_it_from_the_store_and_a_running_session`, `a_session_answer_on_a_relayed_reviewer_card_covers_the_queue_until_revoked`; `tools` `every_kind_reads_back_as_written`, `the_queue_is_the_gatekeepers_tree_and_nothing_else`, `view::tests::a_revoke_leaves_the_record_and_every_cursor_reads_it_once`; `tokencore` grant tests. Not covered: a revoke from a second process.

**Since 408353957.** 47894496a (R314), de0b555e3 (one revoke).

## S22. Usage poll and head [NEW]

**Sides.**
- Poll: `provider/src/usage_poll.rs` (`EVERY` 120 s `:15`, `windows :30`, `age :174`, `fetch_body :206`, `Tell :230`, drop guard `Stopped :260`, `watch :288`, `reading :323`, `spent_reset_ms :332`, `read :337` with one request per endpoint at a time through `GATES`).
- Thresholds: `provider/src/usage_warn.rs` (`shown :89`, `crossings :111` once per window, `notice :133`).
- Harness: `harnessd/src/harness/usage.rs` (`with_the_plans_reset :10`, `usage_row :75`, `flush_usage_notices :85`, `watch_usage :102`, `usage_setting :188`). A limit park reads the poll's reading (`harnessd/src/harness/turns.rs:3070`, f562547fb).
- Head: the `models.usage` settings row (`MODEL_USAGE_KEY`, 711bb1b27), carried in the `Settings` frame. The TUI replaces its settings on every frame and colours the figure by level (bb6c32a21).

**Shape.** The row's value is `label + age`, computed in the daemon at tell time (`harnessd/src/harness/usage.rs:200-201`). `choices` carries the door and the level.

**Risks.**
- The age is text baked in when the reading is told. The head never recomputes it. If the poll stops telling (no watcher left, the thread gone after its drop guard, a daemon detached from the endpoint), the row keeps its last string with no age growing (HYP).
- Kimi carries the 5h window twice (`usages.limit_5h.used_ratio` and `limits[]`), and `windows` reads `limits[]` only when `usages` gives nothing (`provider/src/usage_poll.rs:51-52`). Deduplication keeps windows with equal `used` and reset (`:56-59`). If the two disagree, the less exhausted may be shown (R310 left).
- `read` holds the endpoint's gate across a blocking HTTP request (`provider/src/usage_poll.rs:338-344`), so a limit park waits behind a slow poll round.
- The `source` string says "polled every few minutes" (`harnessd/src/harness/usage.rs:203`) while `EVERY` is 2 min. Prose, not a fact source.

**Tests.** `usage_poll::tests::a_failed_round_tells_the_kept_reading_with_its_time`, `a_reading_older_than_the_period_says_its_age`; `harnessd/src/harness/usage.rs` (`each_threshold_is_said_once_per_window :287`, `each_reading_redraws_the_row_without_a_crossing :317`), `harnessd/src/harness/tests/usage_notice.rs`.

**Since 408353957.** 711bb1b27, bb6c32a21, f562547fb, ca998ddee (R310).

## S23. Limit hook and plugins [NEW]

**Sides.**
- Guest: `plugin-sdk/src/limit.rs` (`HOOK = "limit_decide" :11`, `Input :77`, `Do :132` = Wait, Switch, NewSession, AskUser, Call, Stop; `Out::parse :177`).
- Host dispatch: `harnessd/src/plugins/hook.rs:289 limit`, following `call` answers down the chain.
- Harness: `harnessd/src/harness/limit_hook.rs` (`limit_hook :105`, `limit_hook_fell_back :218`, `limit_input :234`, `limit_ask :289`, `limit_new_session :334`), called first from `after_the_ladder` (`harnessd/src/harness/limit.rs:472`). Episodes persist per model (`load :36`, `save_limit_episodes :54`).
- Fork: `harnessd/src/harness/fork.rs:91 fork_into_new_session`, shared by `/fork N` (`harnessd/src/slash.rs:342`, 11601e36e) and `new_session` (60d76a919, R233). The store copy is `Store::fork_session` (`tokencore/src/store.rs:2871-2882`).
- Host verbs: `HOST_VERBS` (`plugin-sdk/src/command.rs:58`) gained `shadow`, `effort`, `fork`, `note`, `permissions`, `grants` (5cb028063 and later).
- Warnings: `limit_hook`, `limit_hook_done`, `limit_hook_fallback`, `limit_hook_unsaved`, `limit_new_session` (`sessionlog/src/warning.rs:217-329`, e8e5a2a99). The census is 223 (`sessionlog/src/warning.rs:1720`).
- Head: a limit's model switch, park and return stay above the composer until dismissed (6e31b27f7).

**Shape.** The new session's continuation prompt is sent as `from: Some(<old id>)` through `submit_daemon` with identity `agent:<old id>` (`harnessd/src/harness/limit_hook.rs:362-372`), so it is a peer line, not the operator's.

**Risks.**
- `fork_session` writes `parent_session_id: None` and origin `operator` for every fork (`tokencore/src/store.rs:2880-2882`). A limit hook firing in a child session forks it into a top-level operator-origin session, outside its parent's ceiling (R306) and its tree's caps. `Input` passes `parent` to the layer (`harnessd/src/harness/limit_hook.rs:279`) but nothing refuses (HYP: whether children carry a plugin host that answers `limit_decide`).
- The fork does not carry `session_grant` rows, which are keyed by the old id. A forked session asks again for what was granted.
- Unpinned layers still answer non-gate hooks (`plugins/src/stack.rs:52-53`), and `limit_decide` is one. A project layer from `.letibot/plugins` can switch the conversation to another configured door or fork it (HYP: candidates are limited to configured targets).
- `/fork N` copies rows only. Modes, pins and kept grants of the source are not copied (by design, but a stale-facts case for the head showing the fork).

**Tests.** `harnessd/src/harness/limit_hook.rs:414`, the limit hook cases in `harnessd/tests/limits.rs`, `harnessd/src/slash.rs:2018`, `harnessd/src/harness/fork.rs` tests.

**Since 408353957.** 1568dbcd1, 60d76a919, 11601e36e, e8e5a2a99, d34e39374, 5cb028063, 6e31b27f7.

---

## Version bumps and both-sides commits since 408353957

134 commits landed (`git log 408353957..9e9312b06`): batches 8-12 and a comment-only deslop pass (about 45 `deslop:` commits).

Version bumps:
- Head protocol 75 to 78 (`sessionlog/src/protocol.rs:15`): 8d4943a7a (76, `TurnEnded` vs `TurnFinished`, R55), 8da965edd (77, `DriverTodos`, R260), 5eecd00f1 (78, `ToolResult` `ran_ms` and `decision`, R287).
- Store schema 49 to 51 (`tokencore/src/store.rs:207`): 47894496a (50, `session_grant`), acb4c65cb (51, `nags_paused_at`/`nags_paused_by`, `granted_by`).
- Room protocol unchanged at 12, `OLDEST_CLIENT` 11 (`room/src/wire.rs:15,19`). No commit touched `room/src/wire.rs` outside the deslop.
- Warning census 223 (`sessionlog/src/warning.rs:1720`, e52000bba).

Wire changes under one number (no bump): 6f538d24a adds `Prompt.delivery` (`PromptDelivery` Steer/Queue, `#[serde(default)]`), so a new head's `queue` prompt to an older daemon steers instead, silently, the same pattern as `from` in 7ab2f03fd. e39e989ec (`cost_micros_usd`), acedd8610 (card `role`), 711bb1b27 (`MODEL_USAGE_KEY`) are additive.

Heaviest both-sides commits (crates touched, deslop and `todo:` excluded): 5eecd00f1 (13), dd76c1bb3 (6), 913e532d6 (6), 8da965edd (6), bb6c32a21 (5), 8d4943a7a (5), then 47894496a, de0b555e3, 1568dbcd1, acb4c65cb, 999f652e3, acedd8610, 66fdd5646, dbe4c3b9c, c923859a9, fccb8e110 (4 each).
