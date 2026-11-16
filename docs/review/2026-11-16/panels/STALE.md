# Panel STALE - stale facts

- Panel: STALE, defined in `/data/review-prep/partition.md`, section "Added by the operator (2026-11-16): stale facts panel".
- Scope: every number and state a reader is shown as current - the head's header, status row and resident lines (ctx, usage %, model, mode, nags, queue counts, waiting-on); every pane (todos, queue, jobs, subagents, sessions); the bridge's tools (`todos`, `leticode_status`, `leticode_asks`); the seat boards; the nag text; state restored from the store at open (pauses, waits, modes, pins, limits, grants). Seeds: R298, R310, R309, R301, the qwen-next model name, R311, R308, R314/R317, R306.
- Snapshot: `9e9312b06` (worktree `/home/dead/Projects/letibot/wt-review-snap`). All reports are from reading code. No test was run by any seat or by the synthesis.
- Positions: 1 against (every cached fact without an age is a defect), 2 for (most copies are refreshed by reliable events; show real reproductions only), 3 against both (the head should hold no derived state).

| round | position 1 (against) | position 2 (for) | position 3 (against both) |
|---|---|---|---|
| 1 | GLM (glm-coding preset default) | Claude fable | Kimi (kimi-code-global preset default) |
| 2 | Claude opus | DeepSeek (deepseek-flash; the one-shot ignored `--model`) | Claude sonnet |

The Claude round-2 positions (opus, sonnet) were argued before the DeepSeek, GLM and Kimi round-1 reports existed. They answer fable's round-1 report only, and both say "round 1 produced no report" for their own seat. The DeepSeek round-2 position was argued after GLM and Kimi round 1. No position answered the GLM or Kimi round-1 findings in round 2 except DeepSeek.

Supplementary evidence, not positions: `/data/review-ext/as-deepseek/STALE-r1-p1-glm.txt` and `STALE-r1-p3-kimi.txt`. Both were answered by DeepSeek in a mislabelled earlier pass. A point found only there is credited "DeepSeek (supplementary)".

This file replaces the earlier STALE record, in which R1 P1, R1 P3 and R2 P2 had failed.

## Round 1

### Position 1 - GLM
[model: glm-coding (the preset's default model)]
THESIS (position 1). The head is a cache farm: every fact it shows is a copy of a daemon fact, and the copies that lack a read-time or an age are defects by construction. At 9e9312b06 the recently-fixed facts carry ages; the rest do not, and two of the worst stale facts are daemon-side, where no redraw ever reaches them.

FINDINGS

1. HIGH — Usage reading prefers `usages` and never reads `limits[]` when both are present. `provider/src/usage_poll.rs:50-52`: `kimi_limits` runs only `if out.is_empty()`. Kimi answers both; the seeded `5h 47%` vs 60% is this. The wrong number feeds the status row (`tui/src/ui/turn_status.rs:64`), the model card, threshold notices, and the park ladder's reset (`harnessd/src/harness/usage.rs:47` `spent_reset_ms`). Failure: the operator plans around 47%, the provider refuses at 60%, and a park waits on a window that already reset. Fix: always merge `limits[]`; when both name one window, take the greater `used`.

2. MEDIUM-HIGH — The usage row's age and reset clock are frozen at publish. `harnessd/src/harness/usage.rs:200-201` computes `label` + `age` once; `sessionlog/src/registry.rs:2062-2076` stores the string. While the poller lives, a failed round re-tells with a fresh age; when it ends (watcher `still` false, door left, endpoint dropped — the re-arm is only `publish_settings`, HYP), the row keeps "5h 47% · resets 12:00 (2m ago)" forever, past the reset, served to every later `Settings` answer. Fix: carry `at_ms` on the row and age it at draw, or withdraw the row when its door's poller ends.

3. HIGH — The shadow's breaker resets while its grants survive. `harnessd/src/shadow/permission.rs:174-202`: `Answering` (breaker, granted shapes, asked cards) is process memory; `harnessd/src/grants.rs:192-211` `kept_views` rebinds durable `session_grant` rows at open, and `/permissions` (`grants.rs:215`) lists them with no note. Failure: two disagreements trip the breaker; a routine restart (reaper, restart marker) hands the shadow a fresh budget of 2 over the same shapes. Fix: persist the disagreement count beside the grants or drop shadow grants at restart.

4. MEDIUM — A reopened child's ceiling is its own opening point. `harnessd/src/harness/open.rs:669-673` reads a session's stored point only when `parent_session_id.is_none()`; `open.rs:2212-2217` sets `parent_ceiling = cfg.mode`. After a restart a child never sees its parent's current point; `within_parent` (`harness.rs:813`) then enforces a stale ceiling and the settings row presents "parent's point at spawn" as current. Fix: read the parent's stored point at reopen and clamp.

5. MEDIUM — The waiting-on line has no refresh of its own. `harnessd/src/harness.rs:1544-1549` — "Not a timer"; `activity.rs:114` `delivery: hub.work_pending()`. R308's "waiting on: delivery" while the worker spun is this class: any transition without a call site is stale until the next move, and the row carries no time. Fix: recompute on the idle pass, or stamp the reading.

6. LOW — The header's model falls back to `wiring.model`, the daemon config's `prompt_model_name()` (`tui/src/ui/header.rs:86-94`, `sessions.rs:1982-1988`), defaulting to `qwen-3.8-flash-next` (`config.rs:1696`), drawn unmarked when no settings row arrived.

CONCESSIONS. R298 (compaction re-bases ctx, `tui/src/app/events/warnings.rs:119-126`), R309 (one stuck predicate, `tui/src/ui/panes/queue.rs:418,426`), R301 (`resolve_job_waits`, `tools/src/builtins/todo.rs:718`), R311 (a pause dies with its daemon), and the queue snapshot's `queues_got_ms` with aged rows (`events/frames.rs:215`) all hold. Position 2 will cite these as proof the event fabric suffices; but each was bolted on after an operator was misled — per fact, never as a rule. Position 3's render-only cure misses findings 1-4, which are daemon-side: the head's copy is not what rots.


### Position 2 - Claude fable
PANEL STALE, round 1, reviewer 2 (position 2: for). All paths under /home/dead/Projects/letibot/wt-review-snap/crates at 9e9312b06. No test run; everything is from reading code.

THESIS
The event paths mostly hold. Of nine seeds, four hold as built, one was never a stale fact, and four really go stale. I add two new ones. Each is a missing arm of a few lines, not an architecture problem.

FINDINGS

1. HIGH - R298 holds only for a head attached live. `sessionlog/src/view.rs:731-744` (TranscriptForked) keeps `turn`, so the snapshot still carries the pre-compaction `Finished{usage}`. `tui/src/app/events/mod.rs:241-247` copies it into `self.usage`. The snapshot's warning loop (`mod.rs:373-391`) never applies the `compacted` correction, which exists only on the live arm (`events/warnings.rs:112-126`).
Scenario: `/compact` on an idle session, then switch away and back, reattach, or resync (same path, `mod.rs:95-97`). Header shows the old ctx until the next turn ends. HYP: a compaction republish overrunning the head queue triggers that resync by itself.
Change: fix the daemon's view at the fork (drop or rewrite the turn's usage). The store row is already rewritten there (`harnessd/src/harness/wall.rs:1950-1973`).

2. MEDIUM - bridge `leticode_status.model` goes stale after a switch. The bridge head's Settings arm reads only `idle_exit` (`bridge/src/head.rs:947-952`). `model` is written only at TurnStarted (`:1116`), the snapshot's turn (`:1003`) and ProviderTrouble (`:1342`). `set_model` promises "`leticode_status` names the model it is on" (`bridge/src/tools.rs:1173`).
Scenario: `leticode_model` then `leticode_status` before a turn returns the old model or null. An orchestrator decides on it.
Change: read the `model` settings row in that arm.

3. MEDIUM - R301 is job-only. `tools/src/builtins/todo.rs:728-731` retains every non-Job need. The pane prints child, pid and monitor needs verbatim (`tui/src/ui/panes/todos.rs:360-367`, `:489-496`). Daemon readers judge them live (`todo.rs:1464-1481`), so the nag is right and the pane is not. Monitors live in the process (`harnessd/src/harness/nags.rs:139-146`).
Scenario: a row waits on `monitor ci`, the daemon restarts, the pane shows the wait indefinitely. HYP: pane not rendered.
Change: generalise `resolve_job_waits` to the three kinds.

4. MEDIUM - R306 left, confirmed. A reopened child's ceiling is a fresh cell of its own point (`harnessd/src/harness/open.rs:2214-2217`). Only a spawn hands over the live cell (`harness.rs:846-849`).
Scenario: restart, then narrow the parent with `/mode`. The child keeps its wider point.
Change: resolve the parent's cell from the registry at open.

5. LOW - R317 breaker. The count is an in-process atomic (`harnessd/src/shadow/permission.rs:176,188`), "observe-only until restart" (`:233`). After a restart the shadow answers again, and grants from the tripped judge are still applied.
Change: store the trip beside the grants.

HOLDS (verified)
- R310: every round tells, a failed one with its time (`provider/src/usage_poll.rs:370-390`), the row is replaced per reading (`harnessd/src/harness/usage.rs:126-140`), and a session leaving the door drops the row at republish (`harness.rs:1146`). The `usages` vs `limits[]` gap was not seen live. Not counted.
- R311: `nags.rs:626-654`.
- R309: one predicate (`tui/src/ui/panes/queue.rs:418-437`), re-read on QueueMoved (`tui/src/app/events/todos.rs:36-43`) and on attach (`tui/src/app/session.rs:801-818`).
- R301 for jobs: `nags.rs:198-225`.
- R308 was not stale. Activity is re-derived on every touching event (`sessionlog/src/hub.rs:1144-1146`, `:2014`; `harnessd/src/sessions.rs:551`). "delivery" was true. The worker was wrong.
- Header model in the TUI: both copies stamped (`tui/src/ui/header.rs:76-84`, `events/frames.rs:301`, `events/turn.rs:105`), and every switch republishes (`harnessd/src/harness/models.rs:377,604,796,871`). HYP: an unopened session has no rows and draws `wiring.model`. This is the likely qwen-next case, not traced.

AGAINST THE OTHER POSITIONS
- Position 1: an age on every field is noise where an event already fires on change (activity, settings, todos, queue). Age belongs only on polled facts, and usage has it.
- Position 3: in finding 1 the stale copy is in the daemon's own view. A stateless head would draw the same wrong number. `load` is already the single replace path.

CONCESSIONS
- The usage age is text baked at tell time, with 2-minute resolution.
- There is no general mechanism. Findings 1 to 3 are the same class found one at a time, which supports I48 as an audit list.
- Seat boards, the jobs and subagents panes, and the `/config` grants row were not traced.

### Position 3 - Kimi
[model: kimi-code-global (the preset's default model)]
Thesis: no — “the head holds no derived state” is not feasible and would not fix the stale facts that matter.

The frame loop cannot synchronously re-read daemon state. Panes are drawn by wasm layer calls (`tui/src/ui/panes/plugin.rs:1007-1015`), the editor/scroll/selection are local, and a socket delay must not blank the screen. Replacing one event-fed copy with a per-frame round trip would add latency and fail the same way on a missed frame. Keep caches; give each a source, watermark and age, and revalidate restored authority. The defects below are not head caches.

Findings:

1. `/clear` leaves the header's context at the old conversation — high.
   `harnessd/src/harness/clear.rs:86-98` says “conversation cleared: … — N tokens, down from M”, but `tui/src/app/events/warnings.rs:108-125` only updates usage when `notice_card::compaction_after` parses the sentence, and that parser (`tui/src/ui/transcript/notice_card.rs:222-231`) accepts only `compacted`/`reseated` followed by a bare number. `conversation cleared: s-x#t1 → s-x#t2` fails `parse::<u64>()`.
   Scenario: 928k context, `/clear`; the transcript is fresh but the header still shows 928k with no age until another turn. The operator concludes the clear failed and the model is told nothing.
   Change: publish the same structured `CompactionReport` on `cleared` and set `usage.prompt_tokens = tokens_after` (or unknown), never parse prose.

2. A compaction's new base is parsed from prose although the structured value is in the same event — medium.
   `sessionlog/src/event.rs:900-901` carries `tokens_after`; `tui/src/app/events/warnings.rs:119-125` ignores it and parses `detail`. A wording/desync change makes the header silently stale. It also discards `estimated`, so an estimated after-count is drawn as exact (`notice_card.rs:208`).
   Change: read `compaction.tokens_after` and its estimate/measurement flag; make a prose change not affect the header.

3. The shadow breaker resets at restart while its grants persist — high.
   `harnessd/src/harness/open.rs:3607-3608` constructs `Answering::new(...)`, whose disagreement counter starts at 0 (`shadow/permission.rs:184-188`), while grants live in `session_grant` (`tokencore/src/store/grants.rs:1-3,75`) and queue grants cover all future reviewers (`grant_ledger.rs:40-41`, `harnessd/src/grants.rs:183-188`).
   Scenario: a judge grants a bad path; the operator revokes, the second disagreement trips the breaker (`permission.rs:231-238`); a daemon restart turns answering back on with the old grants still bound. The safety state is older than the authority it is supposed to bound.
   Change: persist breaker/disagreement state per box and give shadow grants an epoch; on open, require shadow grants to be revalidated or drop them with a row. Queue grants need a TTL or reviewer/worktree binding.

4. A reopened child's parent ceiling is its own restored point — high, HYP end-to-end.
   `open.rs:2212-2217` builds `parent_ceiling` from `cfg.mode`; only a live spawn replaces it with the parent's cell (`children.rs:4114`, `harness.rs:846-848`). After restart `within_parent` (`harness.rs:813-821`) compares against that snapshot, not the parent's current point.
   Scenario: parent narrows writes→read-only, daemon restarts, child reopens at its old writes point and `/mode` widens back to it.
   Change: resolve the parent's current stored point at open, or mark the link stale and refuse widening until the parent is live.

Concessions: most UI state is event-fed and reliable — activity is republished on attach (`sessionlog/src/hub.rs:581-583`), queue snapshots age correctly (`tui/src/app/queue_pane.rs:65-68`), and R309/R310 fixes hold. Position 1 is right that silent caches are defects; position 2 is right that events suffice for conversation state. The line is authority and lifecycle state, not rendering.


## Round 2

### Position 1 - Claude opus
Position 1 (against), round 2. Round 1 produced no report, so this case is new. All findings come from reading code at 9e9312b06. No test was run.

Rebuttals
- P2 finding 1 (R298 on snapshot): conceded and adopted. `sessionlog/src/view.rs:731-745` leaves `turn` alone at the fork. `tui/src/app/events/mod.rs:241-247` copies the old `Finished{usage}`. The warning loop (`mod.rs:373-391`) handles `turn_failed`, boot and `MODEL_SET` but not `compacted`. The correction exists only live (`events/warnings.rs:112-126`). P2's self-resync trigger stays HYP.
- P2 finding 2 (bridge model): conceded. Settings reads only `idle_exit` (`bridge/src/head.rs:947-952`). `model` is written at `:1003`, `:1116` and `:1342`, never from `model_set`.
- P2 "four hold as built": not contested item by item. But a fact with no age is a defect even when its event is reliable, because the reader cannot tell fresh from frozen. `queues_got_ms` (`frames.rs:215`) is the only age stamp I found.
- P3: no report. Against its premise, the daemon's own view is stale at the fork (finding 1), so "render the daemon's view" alone does not fix R298.

Final findings
1. HIGH - `sessionlog/src/view.rs:731`. The daemon view keeps the pre-fork turn usage. Change: clear or rewrite `turn.usage` at `TranscriptForked`.
2. MEDIUM - `bridge/src/head.rs:947`. `leticode_status.model` goes stale after `set_model`. Change: take `MODEL_SET` the way the TUI does (`mod.rs:386`).
3. MEDIUM - `tui/src/app/events/frames.rs:243`. Seat boards are refreshed only on the `Todos` reply: at attach, resync and pane open (`surface.rs:376`). There is no live event. A pane left open shows a frozen board, and `SeatBoard` has no timestamp (`event.rs:172-176`). Change: add a read time and draw the age.
4. LOW - `frames.rs:289-292`. `asks_setting` is a copy from attach time that nothing refreshes. Change: stamp it, or drop it once the board is read.

### Position 2 - DeepSeek
[model: deepseek/deepseek-flash (the default; the one-shot ignored --model)]
Thesis: round-1 p2 holds; I raise its F1 and cut one rival's.

REBUT P1 (r2): your F1/F2 conceded in their snapshot/bridge shape, but your fix is insufficient. Clearing `turn.usage` daemon-side (view.rs:731) fixes only the snapshot arm. The live arm is stale too, with no bound: `/clear` runs no summary turn (clear.rs:26-29), its warning detail is transcript ids (clear.rs:88-105), `compaction_after` demands `<u64> → <u64> ` (notice_card.rs:224-246) → `None`, and the head never touches usage on `TranscriptForked` (transcript.rs:104-111). Your r1 `/clear` finding was right; dropping it lost the worse half. Rebut r2-F4 (`asks_setting`): self-healing in one round trip — todos.rs:108,229; frames.rs:293-297 say only "still being read".

REBUT P3: a daemon `Status` view is not needed for the model case — the `Settings` frame already carries `model` (config.rs:2381; registry.rs:1843) and the bridge discards it (head.rs:947-952). And a per-frame stateless head loses a fact: "the gate tail has not moved" needs the previous frame (queue.rs:42-70). Agree on ctx: put the after-size on the wire (CompactionReport.tokens_after is ledger tokens, event.rs:897-901) and take it at the fork. Age belongs on polled facts only — the one drawn age is the queue's (queue.rs:35), from `queues_got_ms`.

FINAL LIST
1. HIGH view.rs:731-745 + tui/app/events/mod.rs:243,373-391, warnings.rs:109-126: ctx pre-fork on snapshot; never on `/clear`. Change: carry `context_after` (shown unit) on the fork; head takes it in both arms; delete the prose parse.
2. MEDIUM bridge/src/head.rs:947-952: `leticode_status.model` stale after a switch. Change: read `MODEL_KEY` in that arm.
3. MEDIUM tui/app/events/frames.rs:236-243: seat boards only on a `Todos` reply; no clock, no push (server.rs:1040), `SeatBoard` has no time. Change: slow-clock read, stamp.
4. MEDIUM tools/src/builtins/todo.rs:728-743: only `Job` waits resolve; child/pid/monitor survive a restart. Change: resolve all three.
5. LOW harnessd/src/harness/open.rs:2214-2217 and shadow/permission.rs:176-188: restored ceiling, breaker lost. Change: I48 check at open.


### Position 3 - Claude sonnet
PANEL STALE r2, position 3 (against both: head holds no derived state). Read-only; no test run.

Revision from round 1: none to defend (no report ran). Case built fresh.

REBUT P2 (fable) f1: CONCEDE and use it as proof. `tui/src/app/events/mod.rs:241-247` copies the snapshot's pre-compaction `Finished{usage}` into `self.usage`. The snapshot loop (`mod.rs:373-391`) only filters turn_failed, boot steps and MODEL_SET. The `compacted` correction (`events/warnings.rs:112-126`) is live-arm only. `sessionlog/src/view.rs:731-744` (TranscriptForked) leaves `turn` alone. So one fact (ctx) has two writers that disagree: a live-only patch over a snapshot copy. Position 2's "fix a missing arm" is the pattern that produced R298, R308, R309 and R301 in turn.

REBUT P1: "name each cached fact" is right but is a list of seats. App has about 1190 lines of fields (`tui/src/app/mod.rs:56` to its close). An age on each does not remove the writer pair.

DEFEND: a pure render-the-daemon-view head. Feasible for 3 classes, not all:
 - Derived numbers (ctx, usage %, model, mode, queue/stuck counts, todos waiting on jobs, waiting-on): the daemon computes once and pushes one `Status` view. The head draws it. This removes the snapshot/live split, since a snapshot is just the latest view.
 - Not feasible: scroll, selection, composer draft, local prompt echo (`unconfirmed`), animation. The head keeps UI state, never facts.
 - Cost: the daemon must own the compaction/usage/model arithmetic. Bridge (`bridge/src/head.rs:947-952`, model set only at `:1116`, `:1003`, `:1342`) reads the same view, so P2 f2 disappears. Migration is large, so do it one fact at a time, ctx first.

FINAL LIST
1. HIGH `tui/src/app/events/mod.rs:241-247` + `warnings.rs:112-126`. Snapshot ctx is wrong after /compact followed by reattach or resync. Change: daemon rewrites the view's turn usage at the fork (`view.rs:731`), and the head drops its live-only patch.
2. MEDIUM `bridge/src/head.rs:947-952`. `leticode_status.model` stale after a switch. Change: take the model from the Settings arm.
3. DESIGN, rule I48. One daemon-pushed `Status` view per fact class, with a read time and an age drawn once it is older than its period.

## Synthesis

Paths are under `crates/` at `9e9312b06`. "Checked" means the synthesis read the cited lines. HYP means nobody reproduced it.

### Seeds at 9e9312b06

| seed | holds? | evidence |
|---|---|---|
| R298 header ctx after compaction | live `/compact` only. Fails on snapshot load (F1) and on `/clear`, `/reseat`, re-ingest (F2) | fable, opus, sonnet, DeepSeek; Kimi; DeepSeek (supplementary) |
| R310 usage 47% vs 60% | poll and age hold. `usages` vs `limits[]` left (F8). Age baked as text (F9) | GLM vs fable |
| R309 queue stuck | holds, one predicate | GLM, fable, Kimi |
| R301 todo waits on ended jobs | holds for jobs. Child, pid, monitor waits left (F5) | fable, DeepSeek |
| qwen-next model until first turn | likely `wiring.model` fallback for a session with no settings rows (F12). Not traced | GLM, fable |
| R311 nag pause restored | holds, a pause ends with its daemon | GLM, fable, DeepSeek (supplementary) |
| R308 waiting on: delivery | not a stale fact per fable (the reading was true). GLM keeps it as a class (F11) | disputed |
| R314/R317 grants vs breaker | left. Grants persist, breaker resets (F7) | GLM, fable, Kimi, DeepSeek |
| R306 child ceiling after restart | left (F6) | GLM, fable, Kimi, DeepSeek |

### Agreed findings

- F1 HIGH. Snapshot ctx after a fork. `sessionlog/src/view.rs:731-745` keeps `turn` at `TranscriptForked`; `tui/src/app/events/mod.rs:241-247` copies the pre-fork `Finished{usage}`; the snapshot warning loop (`mod.rs:373-391`) has no `compacted` arm. Raised by fable, conceded by opus and sonnet, kept by DeepSeek. Checked.
- F2 HIGH. Live ctx after `/clear`, `/reseat` and a re-ingest. The head takes the new base only from `notice_card::compaction_after` (`tui/src/app/events/warnings.rs:117-126`, `tui/src/ui/transcript/notice_card.rs:224-246`). `/clear` says `conversation cleared: <id> → <id>` (`harnessd/src/harness/clear.rs:86-98`), `/reseat` says `this conversation now speaks the prompt...` (`harnessd/src/sessions.rs:4121-4136`), a re-ingest says `re-seated: ...` (`sessions.rs:5259-5264`). None parses. `/clear` runs no turn, so the stale ctx has no bound. Kimi (clear), DeepSeek r2 (clear), DeepSeek (supplementary) (reseat, re-ingest). Checked.
- F4 MEDIUM. `leticode_status.model` stale after a switch. Bridge Settings arm reads only `idle_exit` (`bridge/src/head.rs:947-952`). fable, opus, DeepSeek, sonnet. Checked.
- F5 MEDIUM. `resolve_job_waits` resolves only `TodoNeed::Job` (`tools/src/builtins/todo.rs:726-734`). fable, DeepSeek. Checked.
- F6 / F7. Every position that read them agrees the restored ceiling and the reset breaker are real. Severity is disputed (D4).
- F10 MEDIUM. Seat boards are written only by a `Todos` reply (`tui/src/app/events/frames.rs:234-243`), with no clock, no push and no read time. opus, DeepSeek, DeepSeek (supplementary). Checked.

### Disagreements

D1. Where ages go.
- Every cached fact without an age is a defect: GLM, opus ("the reader cannot tell fresh from frozen"; `queues_got_ms` is the only stamp).
- Age only on polled facts. An age where an event fires on change is noise: fable, DeepSeek r2.
- Per-field stamps are "365 new ways to be wrong": Kimi (as DeepSeek (supplementary)). Sonnet: an age does not remove the writer pair.
- Proposed decision: I48 as written, applied by class. Polled and pull-only copies (usage, seat boards, session list, asks) carry a read time and draw their age past their period. Event-fed copies carry no drawn age. They get a resync on a lost frame instead (F14).

D2. Stateless head.
- Feasible for derived scalars, one daemon `Status` view, migrate ctx first: sonnet.
- Not feasible: the frame loop cannot round-trip to the daemon per frame, panes are wasm layer calls (`tui/src/ui/panes/plugin.rs:1007-1015`), UI state is local: Kimi. A per-frame stateless head loses "the gate tail has not moved", which needs the previous frame (`queue.rs:42-70`): DeepSeek r2.
- It would not fix the worst cases. F1's stale copy is the daemon's own view (fable, opus). F6, F7, F8 are daemon-side (GLM).
- Proposed decision: no stateless head. One daemon-pushed facts frame (mode, model and pending, ctx and usage with read time, activity, counts, pause), replaced whole, carried in `Hello`. Migrate fact by fact, ctx first. Both sonnet and DeepSeek (supplementary) land here.

D3. Where the ctx fix goes.
- Daemon rewrites the view's turn usage at the fork; head drops its live-only patch: fable, opus, sonnet.
- Carry a shown-unit `context_after` on the fork event, take it in both arms, delete the prose parse. The daemon-side fix alone leaves `/clear` stale live: DeepSeek r2, Kimi. `CompactionReport.tokens_after` is ledger tokens, not the shown unit (`sessionlog/src/event.rs:892-901`): DeepSeek.
- Proposed decision: both. The structured field is the source. The view rewrite makes the snapshot agree. F1, F2, F3 close in one change.

D4. Severity of F6 (ceiling) and F7 (breaker).
- F7: HIGH by GLM and Kimi (safety state younger than the authority it bounds; reaper restarts are routine). LOW by fable and DeepSeek r2.
- F6: HIGH (HYP end-to-end) by Kimi, MEDIUM by GLM and fable, LOW by DeepSeek r2.
- Proposed decision: F7 HIGH, because a routine restart re-arms a tripped judge over grants it made. F6 MEDIUM until a restart-then-narrow reproduction shows the child widening; HIGH if it does.

D5. Usage `usages` vs `limits[]` (F8).
- GLM: HIGH, `kimi_limits` runs only `if out.is_empty()` (`provider/src/usage_poll.rs:50-52`), and the wrong number also drives the park ladder's reset.
- fable: the gap was not seen live, not counted.
- Code checked: the guard is as GLM says. Whether Kimi returns both shapes in one body is HYP.
- Proposed decision: count it MEDIUM. Capture one real Kimi body into a test. Merge both shapes and take the greater `used` per window.

D6. R308 "waiting on: delivery" (F11).
- fable: activity is re-derived on every touching event (`sessionlog/src/hub.rs:1144-1146`). "delivery" was true and the worker was wrong.
- GLM: `publish_activity` is "Not a timer" (`harnessd/src/harness.rs:1544-1549`). Any transition with no call site stays stale, and the line carries no time.
- Proposed decision: close R308 as not stale. Keep F11 LOW as an audit item under I48.

D7. `asks_setting` (opus F4).
- opus: an attach-time copy nothing refreshes.
- DeepSeek r2: self-healing in one round trip (`frames.rs:293-297` queues a board read; `todos.rs:108`).
- Code checked: the board read is queued. Proposed decision: drop. Only the window before the board reply remains.

### What each model saw

- GLM: the `usages`/`limits[]` guard (F8); the usage row's age frozen as text once the poller ends (F9); waiting-on has no timer (F11); `qwen-3.8-flash-next` as the config default behind the header fallback (F12).
- Claude fable: the snapshot arm of R298 (F1); the bridge model (F4); non-job waits (F5); R308 was never stale.
- Kimi: `/clear` leaves ctx (F2); the prose parse discards `estimated` (F3); queue grants need a TTL or binding (F7); why a per-frame stateless head is not feasible.
- Claude opus: seat boards have no refresh and no stamp (F10); `asks_setting` (dropped, D7).
- Claude sonnet: one fact with two writers that disagree as the class behind R298/R301/R308/R309; a `Status` view migration, ctx first (F17).
- DeepSeek: a stateless head loses frame-to-frame facts (queue gate tail); the Settings frame already carries `model`; the `asks_setting` rebuttal.
- DeepSeek (supplementary): `/reseat` and re-ingest sentences also fail the parse (F2); the hub's 256-frame side queue evicts facts behind terminal paint (F14); the ctrl-p held frame freezes header and status (F15); the header's session count is pull-only (F16); a facts document instead of a stateless head.

### Order of work

1. F2, F1, F3 in one change: shown-unit `context_after` on every fork event (compact, clear, reseat, re-ingest), daemon view rewritten at `TranscriptForked`, head takes the field in both arms, prose parse deleted. Test each fork kind on live and snapshot arms.
2. F4: read `MODEL_KEY` in the bridge Settings arm. Few lines.
3. F7: persist the breaker trip beside the grants, or drop shadow grants at open with a row.
4. F6: resolve the parent's stored point at child reopen, or refuse widening until the parent is live. Reproduce first (D4).
5. F5: resolve child, pid and monitor waits at open.
6. F10, F16: pane clock or push for seat boards and the session list, with read time drawn.
7. F8, F9: merge `limits[]`, then carry `at_ms` on the usage row and age it at draw.
8. F14: latest-value slots for fact frames in the hub, or a resync when a fact frame is evicted.
9. F11, F12, F15: low items.
10. F17: the facts frame under I48, one fact per landing.
