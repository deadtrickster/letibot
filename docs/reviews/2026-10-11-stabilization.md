## The stabilization review of 2026-10-11, in full

Four read-only reviews of main at `435bb88`, run as subagents on the operator's ask (*"spawn
subagents to do a code review - goal is stabilization"*, and *"the merge/review queue is a
center-piece and it was never reliable so far"*). Kept verbatim so nothing a summary dropped is
lost; R61, R67, R68, R69 and R70 are filed from them. Line numbers are on `435bb88`.

### The head: mouse and key routing (crates/tui)

#### Terminal head (crates/tui) review: mouse and key routing

Everything below was found by reading the code. Nothing was run. `crates/ui` holds only the composer editor and has no mouse or layout routing, so all findings are in `crates/tui`.

**Overall shape:** mouse and keys share one entry point. `key_of` (`app/keys/input.rs:126`) turns a wheel step or a plain left press into a `Key`. That `Key` then goes down a hand-ordered list of about 25 `key_*` handlers in `App::key` (`app/keys/mod.rs:54-260`). The first handler to answer `Break` wins.

There is no single focus and no single record of where things were drawn:
- Each pane is its own `bool`.
- Draw priority (`ui/screen.rs` `main_area`), key priority (`keys/mod.rs`) and hint-bar priority (`ui/hint_bar.rs:15`) are three separate hand-written orders, and they disagree.
- Each clickable surface does its own hit-testing: some use a recorded map, some redo the layout arithmetic, some do none.

##### Findings, most severe first

1. **CONFIRMED – typing with the queue pane open runs queue verbs.** `app/keys/panes.rs:736` (`r`) and `:770` (`a`/`v`/`d`) have no empty-composer guard. The subagents pane's `o`/`p` do have one.
   - Scenario: `/queue` is open and the operator starts typing "deploy…". The `d` sends `queue rm <selected id>` and the `a` sends approve. Every letter is eaten.

2. **CONFIRMED – a click in the session picker selects the wrong session.** `panes.rs:~294-308` maps `y - (header+2)` straight to `picker_sel`. But rano's `SessionPicker::content` draws two lines per session: the row, then an id/workspace line. The footer lines also fall inside the `picker_rows_drawn - 2` bound.
   - Scenario: clicking "two" (y=5) selects "three", and clicking any id line selects the next session. Enter then switches to a session the operator did not click.
   - The test `a_click_picks_the_row_under_the_pointer…` (`app/tests/session.rs:1762`) encodes this: it clicks y=5 and expects index 2.
   - The picker also truncates with no scrolling, so a selection past the window is invisible.

3. **CONFIRMED – clicks on the mode/setting card are mis-mapped.** Three causes:
   - `ui/screen.rs:393-394` sets `mode_first_row = (top of the whole chrome) + 1`. The stopping, link, lock, compact-off and mode-confirm lines are pushed above the card (`:311`), so the click is off by that many rows.
   - `mode_rows_drawn` counts `mode_choices()` (the *mode* row) for every subject: `/verbosity`, `/diff` and `/models` too.
   - The rows' `why` text wraps under each value, so one line no longer equals one value.
   - Scenario: `/verbosity`, click "loud" → it selects a neighbouring profile, or nothing. With the daemon link line showing, every mode click is shifted.

4. **CONFIRMED – several panes can be open at once, and keys go to one the operator cannot see.** `toggle_jobs`, `toggle_subagents`, `toggle_todos`, `toggle_standing` and `/queue` flip only their own flag (`app/panes.rs:22,38`, `todos.rs:279`, `standing.rs:125`, `commands.rs:699`).
   - Draw order is: todos > queue > standing > subagents > jobs.
   - Key order is: subagents > todos > queue > jobs > standing.
   - Scenario: ctrl-t then ctrl-g. The todos pane is drawn, but ↑↓ and Enter go to the hidden subagents list. Enter *switches session* into a subagent row nobody saw.
   - The shared `pane_scroll` also makes `scroll_into_view` jump the visible pane.
   - Same class: `editor_chord` (ctrl-], `app/editor.rs:267`) opens rano over an open jobs pane. After ctrl-] back to the composer, the hint says "enter sends", but Enter is swallowed by the hidden jobs pane (`keys/mod.rs:~392` swallow list). Only `key_standing_pane` guards on `!editor_drawn()`.

5. **CONFIRMED – the diff popup blocks the permission card.** `key_diff_popup` (`keys/mod.rs:132`) runs before `key_decision` (`:196`) and returns `Break` for every key. Its own comment says cards stay answerable.
   - Scenario: click an edit row, and a permission decision arrives. The card is drawn in the chrome, and the hint bar shows "Deciding… a row number answers". But digits, ↑↓ and Enter scroll or vanish. All ctrl chords are dead too, while the hint bar still lists them, because `hint_bar` has no diff-popup branch.

6. **CONFIRMED – scrolling in the queue pane moves the hidden transcript.** The wheel/PgUp pane list in `app/keys/chords.rs:270` omits `queue_pane` and `queue_open`.
   - Scenario: wheel or PgUp in the queue pane or entry overlay scrolls the conversation underneath and parks it at "holding". After Esc the operator lands in scrollback.
   - `key_queue_review`'s PageUp/PageDown arm (`panes.rs:182`) is dead code, because chords run first.

7. **CONFIRMED (code path); effect PLAUSIBLE – the `!term` pane gets raw mouse reports.** rano enables `?1002`+`?1006` (button and drag tracking) by default. `pane_keys` (`keys/mod.rs:719`) forwards `raw_input()` verbatim, including SGR mouse sequences.
   - This contradicts `crates/vt/src/screen.rs:27`: "the head must not hand a program the operator's mouse".
   - Scenario: wheel over `!term bash` → the shell gets `ESC[<64;x;yM`, and drags give a stream of them, so junk is likely typed at the prompt.

8. **CONFIRMED – the wheel ignores where the pointer is.** `key_of` drops the wheel's x/y. Chords pick the target by priority:
   - a decision card with overflowing content takes the wheel anywhere on screen;
   - the ctrl-v window takes it anywhere;
   - the session picker and mode card swallow it and do nothing.
   - Scenario: hovering over the transcript and scrolling moves the card's evidence instead.

9. **CONFIRMED – any wheel or click cancels allow-all.** `key_mode_confirm` (`app/keys/cards.rs:25`) treats *any* key, including `WheelUp` and `Click`, as "no".
   - Scenario: `/mode allow-all`, then scroll to reread → "allow-all cancelled".

10. **CONFIRMED – which surfaces take clicks is inconsistent.**
    - Clickable: the session picker, mode card, todos pane, queue pane, the count labels, edit rows, and rano.
    - Not clickable: the subagents, jobs, standing and config panes, the decision card, the quit card. This is despite `subagents_stop_rows`, `standing_stop_rows` and the jobs row maps already existing.
    - The todos (`panes.rs:~505`), queue (`:~798`), picker and pick card handlers return `Break` on *every* click, hit or miss. So the subagents/jobs count labels (`keys/mod.rs:245`) stop working while those are open, yet work while the jobs or subagents pane is open.

11. **CONFIRMED – who owns Enter is decided differently per pane.**
    - The "pane owns Enter" list (`keys/mod.rs:~392`) leaves out `queue_pane`. With an empty queue, Enter with typed text sends the prompt. With a non-empty queue, the same Enter opens an entry.
    - Todos and the picker guard on an empty composer; config always takes Enter.

12. **CONFIRMED – help and hint text disagree with the bindings.**
    - Help (`ui/panes/help.rs:99`) says "a click on an edit or write row opens that one" in rano. It actually opens the diff popup (`keys/mod.rs:~144`).
    - The hint bar (rano `agent/hint_bar.rs:96`) says "ctrl-n notes". ctrl-n *retires every note* (`chords.rs:162`), and the standing-notes pane has no chord.
    - Help never mentions ctrl-g, ctrl-q, ctrl-o, `/queue`, `/jobs`, `/todos` or `/subagents`.
    - Help says "wheel scrolls the transcript", which finding 8 shows is not true.
    - The hint bar has no mode for `slash_out`, `sub_out`, the diff popup or the payload window. With `sub_out` open over the subagents pane, the bar shows the list's "enter (or o) opens a subagent". Enter actually re-peeks, and `o` falls through to switching session.

13. **CONFIRMED – `/sessions` and ctrl-s open the picker differently.** `/sessions` (`commands.rs:510`) sets `picker=true` but does not seed `picker_sel` to the current session and does not clear `pick`. ctrl-s (`chords.rs`) does both.
    - Scenario: `/verbosity` then `/sessions` → both cards are up, with a cursor left over from an earlier list.

14. **PLAUSIBLE – a click in the same read as a wheel uses mixed positions.** All keys from one ~100 ms read are handled before the next `screen()`.
    - `todo_stop_at_row` and `queue_stop_at_row` combine *updated* `pane_scroll` with *last frame's* stop rows, and `file_rows` stays in last frame's coordinates after `scroll_up`.
    - Scenario: trackpad momentum scrolling followed by a click selects a row about 3 off, or opens the wrong file's diff.

15. **PLAUSIBLE, low – `editor_drawn()` ignores the diff popup.** It (`app/editor.rs:198`) does not account for `diff_popup`, which `main_area` draws first. If both are ever open, `route()` would send mouse and keys to the hidden rano instead of the popup.

##### Structural recommendation

- **One surface stack.** Replace the pane booleans with a `Vec<Surface>` (or a single `focus: Surface`) whose top is drawn *and* receives keys. Opening a pane pushes it or replaces the current one, and Esc pops. Draw order, key order and hint mode then all read the same value, which removes findings 4, 5, 6, 11, 12 and 13 as a class.
- **Hit-test against what was last drawn.** Each frame, `main_area` and the chrome push `(Rect, SurfaceId, Vec<(row, StopId)>)` into a `last_layout` that already accounts for scroll. Mouse routing is then one function: find the topmost rect containing (x, y), convert to local coordinates, hand it to that surface. No surface recomputes header, card or line arithmetic. Keep x/y on wheel events so scrolling goes to the rect under the pointer. A click that hits nothing falls through instead of being swallowed. Forward mouse reports to `!term` only if the program asked for mouse mode, translated to the pane's own coordinates.
- **Per-surface keymap tables.** Each surface declares `&[(Key, guard, Action, hint_text)]`. Plain-letter bindings always carry the empty-composer guard. The hint bar and help are generated from the table of the focused surface. Slash commands and chords call the same `open_surface(..)` function, so `/sessions` and ctrl-s cannot drift apart.
- **Re-snapshot the layout between keys in a batch**, or drop clicks that follow a scroll-changing key within the same read.
- **Fix the tests that encode the bugs.** `tests/session.rs:1762` (the picker) currently asserts the wrong row.

### The daemon: session lifecycle (crates/harnessd, crates/sessionlog)

I reviewed the daemon on main at 435bb88 by reading the code only; nothing was run. Paths below are relative to `crates/` unless they start with `tools/`. I found 15 problems, worst first.

#### Findings

**1. CONFIRMED: the worker can open a second Harness on a live subagent.** `harnessd/src/sessions.rs` ~3074 (`dispatch`, `Prompt` arm) → `run_prompt` → `harness_as` (~1564).
- A child's hub shares one command queue between two readers: the child's thread (`Hub::take_own_work_until`, hub.rs:1659) and the daemon worker (`Hub::try_command`, hub.rs:1772, which pops anything).
- The `Interrupt` (~3286) and `Message` (~3620) arms hand the command back with `give_back_to_its_own_reader`. `Prompt` does not.
- So when a head prompts a subagent it is attached to, the worker can win the race. `harness_as` then builds a second `Harness` for that session id, because `open` never held it.
- Result: two writers on one transcript and store row. The second open also replaces the child's `watch_runs`, `set_prompt` and `notes` entries in the Registry and starts another deadline thread.

**2. CONFIRMED: a subagent's hub can be left in the Registry with no reader, so later wakes and messages are lost.** harness.rs:15350–15475 (`run_to_completion`). After `adopt`, three exits leave the hub registered with nobody reading it, and nothing ever closes it:
- the first turn fails (the `?` on `submit_as_a_normal_session`);
- a non-Host (VM) placement returns before `serve_child`;
- a `Stop` makes `serve_child` return (harness.rs ~13075).

Then `wake_route` (sessions.rs:748) answers `ItsOwnReader`, and `Sessions::wake` returns `HandedOn` into a void. The same happens to `Message` and `Interrupt` handed back to the hub. The parent's `task_message` looks delivered, and a head's prompt falls through to finding 1.

**3. CONFIRMED: two threads per finished Host subagent are never released, which matches R66's 7 → 19.**
- Thread one is the `subagent-*` thread, parked in `serve_child` until its hub closes. Nothing closes a child hub before daemon exit.
- Thread two is `tools/src/exec/host.rs:629`, the `Deadlines` watchdog (`letibot-deadline`). It holds its own `Arc<Deadlines>` and `run()` (host.rs:660) is an endless `loop` with no exit, so one per `HostProcesses` lives forever even after the host is dropped.
- Each parked child also keeps its whole `Harness` alive: transcript, ledger, `HostProcesses` with up to 8 MiB of capture per job, and a lazily opened SQLite connection in `JobRecorder` (jobwatch.rs:757).

**4. CONFIRMED: per-session tables only grow.**
- `Registry.inner.entries` (registry.rs:1001 `adopt`, 940 `create_under`) has no remove anywhere. Every subagent's `Hub` (log and view) lives for the daemon's life, and every `get` is a linear scan under the registry lock.
- `Registry.runs` (`watch_runs`, harness.rs:2602) captures `Arc<HostProcesses>` per session. So even a child whose thread exited keeps its job table and captures reachable until `close`.
- `Registry.prompts` and `Registry.notes` are per session and never removed.
- In `Sessions`, `open`, `slots`, `seated`, `fabric_seen`, `nags`, `bangs`, `armed` and `wall_gave_up` have no removal. Every root session ever resumed keeps a full Harness and a `monitor-wake-*` thread (sessions.rs:2840).
- In `HostProcesses`: `jobs` (host.rs:1628), `reaps` (1124), `promotions` (1129) and `Deadlines.fired` are push-only. Job lookup is a linear `iter()` (1740).

**5. CONFIRMED: the idle sweep is quadratic and runs over every open session on the worker.**
- `abandoned_calls` (harness.rs:20292) checks every asked call against the whole answered list (`answered.iter().any`): O(calls × results) over the full in-memory transcript.
- `Sessions::sweep_abandoned_calls` (sessions.rs:2367) runs it for every held session, 2 s after every command or wake.
- The tree had about 15k bash results, so that is on the order of 10⁸ string compares per sweep for one big session. That is the same shape of worker stall as the `Store::session` loop.

**6. CONFIRMED: the turn tails have diverged.**
- `Sessions::deliver_due_nags` (sessions.rs:2379) runs a turn on the idle arm but calls neither `arm_sweep` nor `arm_wake`. `sweep_at` was set to `None` just before (2367), so a call stranded by a nag turn is never swept. This is the wake bug again, on the third door.
- `arm_wake` is called by `run_prompt`, `run_a_message_from_a_session` and `run_continuation`. It is not called by `wake()` (2915), `deliver_due_nags`, `OperatorShellResult` (~3560) or `run_operator_shell_here`. A monitor first declared in one of those turns gets no waiter until the next prompt.
- Children use a second copy of the tail, `Harness::submit_spoken_as_a_normal_session` (harness.rs:8232), plus `serve_child`:
  - the `Wake` and `TimedOut` arms (~12955, ~12985) have no forced compaction when a wake or nag turn hits the wall;
  - children are never swept, because the sweep iterates only `Sessions.open`;
  - `take_new_title` is drained only by `Sessions::publish_title`, so the Registry title is never updated for a child;
  - the daemon's `wall_gave_up` latch and its wording do not exist on the child path.
- The `Answer` arm (harness.rs ~13017) does `.is_ok()` and drops the error, so `turn_failed` carries no reason.

**7. CONFIRMED: `!` stops working for a session after one failed settle.** sessions.rs ~3525 (`OperatorShellResult`). If `settle_operator_shell` returns `Err`, the arm returns `Failed` before `clear_bang_in_flight`. `bangrun::State` then stays in flight, and every later `!` line is queued (`queue_bang_line`) and never runs. The error goes to stderr only.

**8. CONFIRMED: busy traffic starves the idle work.** registry.rs:183 (`Bell::next_any_until`) drains opens, then commands, then wakes, and checks the deadline only when all three are empty. Wakes are not de-duplicated (`ring_wake` pushes every time).
- Under sustained wakes (children settling, monitors, `HandedOn` relays) `WorkOrIdle::Idle` never fires. Nags, the sweep, `ensure_gatekeeper` and `serve_reviews` all wait.
- Each `Harness::wake` (harness.rs:8008) also reads the store: `serve_reviews` reads all reviews, and `queue_notices` and `handover_notices` each read all merge entries. A wake storm becomes a store-scan storm on the worker.

**9. CONFIRMED: `Registry::list()` scans the whole store on the worker.** registry.rs:1432 calls `SessionSource::list` → `Store::list_sessions`, a correlated-subquery scan over all sessions and their transcript items.
- Worker callers: `is_root` (sessions.rs:715) and the parent lookup (~1580), both on every open; `attach_session` (~926); `ensure_gatekeeper` (~2292); `child_todos` `resolve` and `my_children` during turns.
- Opening one session costs 2–3 full scans. The `StoreSessions.store` mutex is held for each scan, and every head's list frame (server.rs) takes the same mutex, so a picker refresh and a worker open serialize.
- `Registry::created_ms` and `wiring` already exist as cheap lookups. A `parent_of` lookup in the same style is missing.

**10. CONFIRMED: one store error stops the merge queue for the rest of the daemon's life.** mergequeue.rs:4115/4121 (`self.recover()?`, `self.step()?`). Any `StoreError` (for example SQLITE_BUSY against another connection) ends `run`, and spawn (4196) only prints to stderr. The queue stops serving and no hub is told.

**11. CONFIRMED: failures that never reach a hub.**
- `Harness::sweep_abandoned_calls` (harness.rs:5228): `let _ = self.append_results(...)`, and the hub block at 5222 is dead code.
- `JobRecorder` `put_job` (jobwatch.rs:776, 813): `let _`. This is the row restart-time `due_todo_rows` depends on.
- `flush_todos` failures (harness.rs:7044, 7056, 7704, 8122) are stderr only. At 8122 a fired condition that is not persisted fires again after a restart.
- `StoreSessions::merge_entries` and `merge_reviews` (sessions.rs:4247, 4266) print and return an empty list. The comment above them says this is "said out loud rather than answered as empty", but the pane shows an empty queue.
- `OperatorCall` failures, "not open" failures, and `run_operator_shell_here` fallback lines (sessions.rs ~3460, 3497, 3538) reach only `on_reply` → `eprintln` (cli.rs:1761).
- A child's `ChildCommand::Leave` (harness.rs ~13093): `!`, `/slash`, `/mode` or a todo edit sent to a subagent session is dropped with a stderr line, and the head attached to it sees nothing.
- daemon.rs: the `ensure_gatekeeper` error, sweep counts and nag counts are stderr only. The nag *failure* is published by `after_turn`.

**12. PLAUSIBLE: a gatekeeper session that fails to open is retried every 30 s and may leak each time.** sessions.rs:2252. When `open_as` fails after `registry.create`, every 30 s review pass retries through the `list()` branch. Each retry pays a store scan and publishes another `resume_failed`. If `Harness::open_with_registry` fails after building its `HostProcesses`, each retry may also leak a deadline thread.

**13. PLAUSIBLE: wakes for a root session that failed to open are lost.** A failed `Work::Open` leaves the hub registered and not held. Any later ring for it is routed `ItsOwnReader` and reported `HandedOn`, though nobody reads that hub.

**14. Lock ordering: no deadlock found.** Registry → Hub is always collect-then-release (`list`, `broadcast`, `close`, `next_work_until`), and the hub rings its bell outside its lock. The real contention is the `StoreSessions.store` mutex held across full SQL scans (finding 9). `Registry::close` runs `list()` (a scan) on the signal thread, which is harmless.

**15. Minor:** `seat_tool` inserts into `slots` before the open can fail, and `detach_seat` clears `seated` but not `slots`.

#### Structural changes that remove the most classes of bug

1. **One session lifecycle with an explicit close.** `Registry::remove(id)` takes away the entry, `runs`, `prompts` and `notes`. `Sessions::close(id)` drops the Harness and its map entries. Every exit of a child thread closes and removes its hub through a single guard. Add an idle-child reap policy, or park a child as store-only so a later prompt reopens it. This fixes findings 2, 3, 4 and 13.
2. **One owner per session, enforced in the hub.** A hub carries an owner tag (worker or own-thread). `try_command` refuses hubs it does not own, so the worker cannot pop a child's commands, and `harness_as` refuses a session that is live under another owner. Fixes 1.
3. **One turn tail for every door, shared by the daemon and children.** Move the before/after sequence (pre-wall compact, title, `arm_wake`, `arm_sweep`, failure publish, refused-compaction, continuation loop, nag rearm, bang-state cleanup on any exit) into a single function both `Sessions` and `serve_child` call. Fixes 6 and 7 and stops the drift.
4. **Get store and git work off the worker, and make the scans cheap.** Give `Registry` a `parent_of` and `title_of` from its own entries. Make `Harness::wake`'s store reads incremental (keyed by row or version) instead of full-table. Make `abandoned_calls` use a `HashSet` over items since the last sweep. Coalesce wakes per session in the bell. Give the idle clock a turn that cannot be starved. Fixes 5, 8 and 9.
5. **A single "report" sink instead of `eprintln!` and `let _`.** One `report(session, code, detail)` that writes stderr and the hub (a daemon-level broadcast when there is no session). Make the merge queue and job recorder retry or announce rather than die or drop. Fixes 10 and 11.

### The merge/review queue: why it is unreliable

#### Why the merge queue is unreliable: findings for R61

##### (a) What actually happens, from the store, git and the log

**Store** (`merge_queue`: 22 rows; `merge_review`: 21 rows). Commit e7798d9 cleared the table once, so the history before 10-10 is gone. The table also keeps only the latest evidence, so how long a row spent in each state cannot be reconstructed.

- **Landed: 9.** Only **1** of them went through the queue's own gate (`agent/thinking-merge`, 23:27 on 10-10, gate about 6.7 min). The other 8 say *"landed without the queue's gate"*: the queue had given them back to the project, and the project session merged them itself.
- **Failed: 10.**
  - 9 are pg-noop entries. Each had 3 reviewer attempts, and all failed with `store schema is version 23; this build knows 21` (a binary older than the store). They sat 24–43 h before failing.
  - 1 is an empty branch (`needs_human`).
- **Stale: 1.** `group-the-queue-messages` was accepted at 22:41, then marked stale at 09:15 the next morning ("gate job died with the daemon"). A later-accepted branch of the same priority landed in between.
- **Waiting: 2.**
  - `a-pass-reads-one-row` has empty evidence and no review row, and has sat about 11 h. No daemon for `/home/dead/Projects/letibot` is running, so nothing owns it.
  - `queue-spin` has a **reject** verdict (empty branch) but is still `waiting`. The refusal hit a daemon that was shutting down. Its branch now has commits, and the same work already reached main by hand as c12d044.

**Reviews:** 17 of 21 never ran.
- 9 failed on the schema mismatch.
- 8 failed because the firecode VM refused to copy a 72.8 GiB workspace (8 GiB ceiling).
- 4 got a verdict: 2 accept, 1 reject, 1 needs_human.
- 148 `gatekeeper` sessions were spawned for these 21 rows.

**Main:** 227 first-parent commits since 10-06, 36 of them since the queue's first landing on 10-10. The queue's gate landed 1 of those.

**Log** (`~/logs/harnessd.log`):
- `merge queue: the queue could not be served: sqlite: database is locked` appears 3 times. Each one is the queue thread ending for good.
- 41 refusals, 9 conflicts, 5 gate failures.
- "landed <same sha>" repeated 7 and 10 times in a row: these were reset rows recognised as already on main, not merges.

**Processes:** 9 `harnessd` daemons share one `sessions.db`, all running deleted binaries of **different builds** (started 09-21 through 10-11). The 10-07 (Leonid) and 10-09 01:28/01:38 (rano, firecode) builds have a merge queue with **no claim and no workspace filter** (both arrived in 837bd59, 10-09 20:37).

**Git history:** 39 commits touch the three files. They cluster into recurring bug classes:
- **Reviewer can't start or is placed wrong** (VM, copy source, fixture): e70245c, a07523a, 8912a8f, 4ebdd66.
- **Which daemon owns an entry:** 837bd59, b1adf2a, 5e53434, 7bc3f46.
- **Verdict delivery:** a1974c0, c05b9c9, 08c6742, 3d36cea, 233939d.
- **Stuck rows and manual escape hatches:** 4c53014, 8178c4e, 424c212, e9eb358.
- **Landing correctness:** a40a9f6 (two branches of real work were deleted), 2659cca, c22d07d, 9f71b36.
- **Performance:** c12d044.
- Separately, 5 merges on main say "landed by hand on the operator's instruction".

##### (b) Findings, ranked by severity

1. **One store, many daemons, mixed builds, no single owner.** `cli.rs:1080` spawns a queue in every daemon. Old builds (e.g. `46642a3:mergequeue.rs` `step`) take any accepted waiting entry with no `claim_merge_entry` and no `serves`, then fast-forward `self.repo`, which is *their own* workspace. *Scenario:* the rano daemon, alive since 10-09, takes a letibot entry, rebases it and runs a gate, then `merge --ff-only` fails in the wrong repo and the row goes `failed`, or it races the real owner. The schema check stops old binaries from *opening* the store, but not ones already running. **CONFIRMED** (process list and old code); actual interference is PLAUSIBLE.

2. **Any store error kills the queue thread for good.** `mergequeue.rs:4121` does `match self.step()?`, so `run` returns on the first error and `spawn` prints one line. *Scenario:* `database is locked` (3 times in the log) or an unknown state word (`store.rs:1436`, e.g. `vetoed` read by an older build) leaves a daemon running with no queue. The pane still draws the rows as if they were being served. **CONFIRMED.**

3. **An entry with no running daemon is silently orphaned.** `served_by` (`mergequeue.rs:714`) only lets the daemon whose workspace equals the root session's workspace serve an entry. *Scenario:* `a-pass-reads-one-row` has sat `waiting` for 11 h with empty evidence. The pane counts it "in review", although no reviewer was ever asked. **CONFIRMED.**

4. **Nothing in the row moves can enforce a state change.** `put_merge_entry` (`store.rs:3608`) overwrites the whole row with no expected-state check. `move_to` writes from a snapshot taken at the start of the pass. `MergeQueueDaemon::recover` (`mergequeue.rs:3522`) turns *every* `Taken` row in the store into `Stale`, not only this daemon's. *Scenario:* daemon B starts while daemon A is gating entry X, so X shows `stale`. A then writes `Landed`. Or a review-pass write of `Waiting` from a stale snapshot undoes another actor's `Taken`, and X is claimed twice. The `group-the-queue-messages` timeline fits this. **CONFIRMED** (code); the incident itself is PLAUSIBLE.

5. **The verdict is not tied to a commit.** `merge_review` and `merge_queue` store `base_sha` only, never the branch head that was reviewed. `review_gate` (`mergequeue.rs:1400`) accepts on the word alone. *Scenario:* the reviewer accepts tip T, the child commits T′, and the queue lands T′ without review. Conversely, `queue-spin` was rejected as "no change", got commits afterwards, and its reject still stands. **CONFIRMED.**

6. **The gate has no time limit and cannot be stopped.** `run_captured_env` (`mergequeue.rs:516`) uses `Command::output()`: no timeout, no process group, output held in memory. `step` (`:3874`) runs the gate on the queue thread and never checks `stop`. `cli.rs:1813` detaches the thread on exit. *Scenario:* a hung step (an interactive `act`, a test waiting on the model server) keeps the row `taken` with no end. On restart the row goes `stale`, the orphaned `cargo test` keeps running, and it holds the worktree's `target` lock. **CONFIRMED** (code).

7. **Landing happens in the operator's own checkout, after the install step.** `fast_forward_main` (`:385`) runs `git -C <repo> merge --ff-only`, which moves whatever branch is checked out and touches the live working tree. It then records `rev-parse main` as the landed sha. The gate's `install-gate-build.sh` step installs the binary *before* the fast-forward and the push.
   - If main moved during the ~7 min gate, the fast-forward fails and the entry is parked `Failed` instead of rebased again. With 36 hand commits in this window, that is likely.
   - If the push fails after the fast-forward, local main has the branch but the row says `Failed`.
   - Either way the installed binary is code that is not on main.

   **CONFIRMED** (code).

8. **Red main is never detected.** Nothing ever runs the gate on main itself. *Scenario:* main goes red, so every entry's gate fails and is blamed on the branch, and each one parks `Failed` one after another. **CONFIRMED.**

9. **The review queue is one global session served by every daemon.** `gatekeeper.rs:162` sets `SESSION = "s-gatekeeper"` for the whole store. `daemon.rs:216` calls `ensure_gatekeeper` in every daemon. `serve_reviews` (`harness.rs:7465`) filters by `session_id == me`, not by workspace, and `is_open_for` is per-process. *Scenario:* two new-build daemons each open their own copy of `s-gatekeeper` and each spawn a reviewer for the same entry, both writing one row and one transcript. **PLAUSIBLE** (only one new-build daemon is live right now).

10. **Reviewers have no deadline and their retries never stop.** `review_outcome` (`harness.rs:1129`) treats `Running` as "still waiting" forever. A child that vanished (`Unknown`) writes nothing, so the next pass spawns another, and that attempt is never counted. *Scenario:* 148 reviewer sessions for 21 rows; a stuck reviewer holds its entry indefinitely. **CONFIRMED** (code and counts).

11. **The fallback became the normal path.** Infrastructure failures (VM placement, wrong build) use up 3 attempts in about 5 min. `hand_over` then parks the row `Failed` and gives it to the project session, which merges by hand without the gate. *Scenario:* 8 of the 9 letibot landings went this way. **CONFIRMED.**

12. **The pane cannot show why an entry is stuck.** `sessions.rs:4225` returns an empty queue when the read fails, which looks like an empty queue. The age shown is time since creation (`tui/.../queue.rs:152`), not time in the current state. No row says which daemon owns it or whether that daemon (or its queue thread) is alive. Live updates come only from the attached daemon's own moves. Evidence is a single field that gets overwritten, with no transition history. **CONFIRMED.**

##### (c) Five requirements R61 has to meet

1. **Exactly one owner per queue, with a live heartbeat.** A lease row (owner pid, build, heartbeat time) that is claimed with a compare-and-swap. Every row shows its owner and how old the heartbeat is. A daemon whose build does not match the store does not serve any queue. Recovery only touches rows whose lease it held, or whose lease has expired.
2. **Every state change is a conditional move with history.** `UPDATE … WHERE state = expected AND owner = me`. Each transition is appended to an event table (from, to, at, by, why), so time in each state and the reason a row is stuck can be read directly. No whole-row upserts.
3. **The verdict is bound to a commit.** The review records the head SHA it judged. The merge queue lands only that SHA; if the branch moved, it goes back to review. The landing runs in a dedicated worktree (or ref update and push) with a compare-and-swap on main's SHA. If main moved, rebase and gate again rather than fail. Install only after the push succeeds.
4. **Every external step has a time limit, can be killed, and has its errors handled.** The gate and each reviewer get a deadline and run in a process group that is killed on timeout or shutdown. Timeout and infrastructure failure are outcomes of their own, separate from "branch is bad". Store errors are retried with backoff, never end the thread, and a dead thread is visible in the pane.
5. **Red main is first-class, and infrastructure failure never silently routes around the queue.** Gate main's tip whenever it changes; while red, land nothing and put main's tip first in review. A reviewer that cannot start should leave the entry visibly blocked on infrastructure at high severity, not hand it to a model session to merge without the gate.

### The tests: layout, counts, coverage, flakiness

#### Letibot test review (main @ 435bb88, read-only, nothing edited)

I counted tests with grep for `#[test]` / `#[tokio::test]`. I did not use `cargo --list`, to avoid a full workspace compile. No property-test or rstest macros exist, so one attribute is one test.

##### 1. Inventory

| crate | unit (src/) | integration (tests/) |
|---|---|---|
| tools | 944 | 146 |
| tui | 663 | 20 |
| harnessd | 467 | 160 |
| sessionlog | 146 | 78 |
| turn | 113 | 33 |
| tokencore | 80 | 0 |
| provider | 54 | 4 |
| letibot | 50 | 4 |
| ui 47, code 39, flowy 38/16, vt 36, dialect-qwen 30, transcript 22, webfetch 22, llama 0/20, dialect-glm 10/12, others under 10 | | |
| **Total** | **~2,780** | **~505** |

That is about 3,285 test functions and about 14,000 `assert*!` lines.

Test code is about 154k of about 349k Rust lines (44%):
- 73.9k lines in inline `mod tests { }` blocks
- 39.5k lines in out-of-line test modules
- 40.5k lines in `tests/*.rs`

**Top 10 files by inline-test lines** (test lines / file lines / tests):
1. `harnessd/src/mergequeue.rs`: 6,070 / 11,281 / 91 (54% test)
2. `harnessd/src/harness.rs`: 4,390 / 20,765 / 101 (`mod tests` at :16534)
3. `tokencore/src/store.rs`: 3,798 / 8,211 / 56
4. `tools/src/intent.rs`: 3,697 / 9,091 / 113
5. `tools/src/adjudicate.rs`: 3,573 / 7,583 / 72
6. `tools/src/builtins/todo.rs`: 3,312 / 5,879 / 68 (56%)
7. `letibot/src/launcher.rs`: 2,115 / 2,312 / 43 (91% test)
8. `harnessd/src/config.rs`: 1,743 / 4,749 / 47
9. `harnessd/src/sessions.rs`: 1,470 / 5,802 / 33
10. `tools/src/authorise.rs`: 1,306 / 3,691 / 41

`tui/src/app.rs` no longer exists. It became `app/mod.rs` (2,006 lines), and its tests are already split out: `tui/src/app/tests/` is 27 files, 38,395 lines. The largest are `transcript.rs` (7,789), `scroll.rs` (3,660) and `todos.rs` (2,525).

##### 2. What Rust convention says, and a rule for this repo

- **Unit tests** go in `#[cfg(test)] mod tests` inside the module. They can see private items, and that is the idiomatic default.
- **Integration tests** go in `tests/*.rs`. They see only the public API, and each file is its own binary, so compile and link cost grows with the number of files.
- **Middle path:** `#[cfg(test)] mod tests;` plus a sibling `foo/tests.rs`, or `#[path = "foo_tests.rs"]`. Private access stays the same; only the file location changes. std and rustc use this widely. It is not "moving tests out" in the integration-test sense.

The repo already uses the middle path in three places:
- `harnessd/src/standing_notes.rs:579` → `standing_notes/tests.rs`
- `harnessd/src/notes_offer.rs:526` → `notes_offer/tests.rs`
- `tui/src/app/mod.rs:2006` → `app/tests/{mod,…}.rs`, split by topic

`#[path]` is only used to share `turn/tests/support/canned.rs` between integration binaries.

**Recommended rule:** when a file's inline test block passes about 800 lines, or about 40% of the file, it moves to `<module>/tests.rs`. When that file passes about 2,000 lines, split it by topic the way `app/tests/` is. Add a new `tests/*.rs` file only for real public-API or end-to-end checks.

**Split first:**
1. mergequeue.rs (6k test lines, 33 recent commits)
2. harness.rs (4.4k; 156 commits since 09-25, the hottest file, so the most diff noise)
3. todo.rs
4. adjudicate.rs
5. intent.rs
6. store.rs
7. launcher.rs (91% test)

These are pure moves, so the test count should be identical before and after. Check it with `cargo test -p X --lib -- --list | wc -l`.

##### 3. Comparison with leticl

leticl uses **FiveAM** (`tests/package.lisp`). Its tests are 786 `def-test` forms in a single 28,413-line `tests/tests.lisp`, plus 7 in `tests/replay.lisp`, for **793 tests**. Those contain 4,793 `is`/`signals`/`finishes` forms (only 2 use `for-all`). The "7k+" is FiveAM's executed-checks count: `TODO.md:71` says "HEAD is green (41d8935, 7043 checks)". The 313 `deftest` hits are vendored alexandria/anaphora code.

So the numbers measure different things:
- **leticl:** about 793 tests, about 7,000 assertions run (roughly 9 per test).
- **letibot:** about 3,285 tests, about 14,000 assertion sites. The run-time count would be higher, because many sit in loops (for example the C0 table in `tui/src/app/keys/input.rs`).

The comparable letibot figure is the ~14k assertions, not 3.3k, so letibot is not behind. The gap is granularity: letibot writes many narrow, named behavioural tests, and leticl writes fewer, fatter ones. Neither count says anything about coverage, which depends on where the tests sit (section 4). If you want a like-for-like number, measure line coverage with `cargo llvm-cov`.

##### 4. Coverage gaps for stabilisation

Since 2026-09-25 there were 738 commits. 56 of the 299 that touched the TUI, merge queue or daemon added no test, though most of the big ones were the 10-08 TUI module-split refactors.

- **TUI input dispatch.** `app/keys/mod.rs` (1,332 lines, 15 commits), `keys/panes.rs` (896 lines, 25 per-pane `key_*` handlers) and `app/events/mod.rs` (1,260 lines) have no tests of their own. They are only exercised through `app/tests/*`. No test checks which pane gets the keyboard when several overlays or panes are open: precedence is the order of the `key_*` calls in `key()`. "focus" appears in only 7 test files, 12 of the hits in `editor.rs`.
- **Mouse.**
  - The decoder is well pinned: `keys/input.rs:264-277` covers wheel, left press, release and modifier rejection.
  - Click handlers sit at `keys/mod.rs:144,244`, `panes.rs:294,505,690,798` and `cards.rs:498`. Tests exist for the picker (`session.rs:1762`), the count labels (`screen.rs:384-636`), todos (`todos.rs:293-332`), settings, the editor and the queue strip (`queue.rs:1049`).
  - **No click test found** for queue rows (`panes.rs:798`). `cards.rs:498` (`key_pick`) is only reached indirectly via settings.
  - The real driver path is untested: `tui/src/driver.rs` (1,210 lines, 19 commits) has 4 tests, all about child processes. Mouse enable/disable and the stdin-to-`Key` loop have none.
  - Render-side hit maps have no tests of their own: `ui/screen.rs` (847 lines, 27 commits), `ui/panes/queue.rs` (880 lines) and `ui/transcript/window.rs` (1,152 lines).
- **Merge queue.** Best covered of the three: 91 tests, a real-repo daemon test (`the_thread_serves_the_queue_and_stops`, :8615) and a no-double-spawn-on-restart test (:9411). The risk is file size, not test absence.
- **Daemon worker loop.**
  - `harnessd/src/daemon.rs` `Daemon::run` (:162-335, signal-fd, 7 commits) has 0 unit tests. Only 8 integration files call `Daemon::serve`.
  - `cli.rs` (3,081 lines, 33 commits incl. today's memory-cap scope) has 13 tests, all for argument and query helpers.
  - In `sessionlog`, `server.rs` (2,010 lines, 23 commits) has 3 tests and `client.rs` has 2.
  - `turn/src/engine.rs` (2,074 lines) has 1 inline test.

##### 5. Flaky-test smells

- **Env vars changed without a lock** (edition 2024, so these are in `unsafe` blocks):
  - `tools/src/builtins/task.rs:2101,2131` sets `FIRECODE_BIN`, which `tools/src/firecode.rs:73` reads from any concurrent test in the tools lib binary.
  - `turn/src/serving.rs:587` sets `LETIBOT_COMPLETION_URL`.
  - `letibot/src/roles.rs:332-341` sets `LETIBOT_SOCKET`/`LETIBOT_SESSION`.
  - `websearch/src/lib.rs:298` sets `BRAVE_*`.
  - `tokencore/src/apparatus.rs:189-223` toggles `LETIBOT_REQUIRE_APPARATUS` and `LETIBOT_VOCAB_GGUF`.
  - By contrast, `confine.rs`, `host.rs` and several `tests/*.rs` do hold a `static LOCK: Mutex<()>`.
- **Silent skips that the require-switch cannot catch.** `tools/tests/confine.rs` (13 early `return;` across 23 tests), `notscoped.rs:55`, `memory_cap.rs:49` and `src/exec/scope.rs` skip with `eprintln!` + `return`. libtest hides that output on a pass, and they do not go through `apparatus::present`, so `LETIBOT_REQUIRE_APPARATUS=1` cannot turn a missing cgroup v2 into a failure. On a runner these show green. `apparatus.rs:5-8` itself records 7 cgroup tests among 84 runner failures.
- **Wall-clock upper bounds:** `task.rs:1815` (`< 1s`), `answers.rs:2256` (`< 1s`), `answers.rs:1677` (`< budget/2`), `answers.rs:1723,1908` (`< 5s`). The 1s ones will trip on a loaded box or during a parallel `cargo test`.
- **Sleeps:** 70 `sleep` calls under test dirs, many 10-20 ms polls. `tools/tests/deadline.rs:179,235` arms a 400 ms deadline and asserts `after >= 400ms`; it is safe as a lower bound but slow. `tui/tests/stop_daemon.rs` and `reconnect.rs` poll at 10 ms. Most `recv_timeout(200ms)` uses are inside 10-30 s deadline loops, which is fine.
- **Shared global state:**
  - `tui/tests/frame_allocations.rs:43` uses a global `ALLOCS` counter. It is safe only while the binary holds one test, which it does today; a second test would make it flaky.
  - `tools/src/builtins/pattern.rs:142` is a process-wide `CACHE`.
  - Sockets are named by `SystemTime` nanoseconds plus pid (`harnessd/tests/operator_shell.rs:102`), which is acceptable.
