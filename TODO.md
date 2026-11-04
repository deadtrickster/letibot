# TODO

Work queue, open work only. The INDEX below has one row per open entry. Settled work is in
`TODO-settled.md` (each entry there has a `settled:` line naming what settled it), review records are
in `docs/reviews/`. Nothing closed belongs in this file.

How to read it: find the entry in the INDEX, then read its **Status** line (one sentence, then
`left:`). Entries are in three groups, in this order: Code work, Needs the operator, Parked. Entries
marked "in flight on a branch" are being changed by other branches and keep their old text until those land.

## If you are an agent picking work from this file

1. **Take one item from Code work. Nothing else.** Needs the operator and Parked cannot land without
   a human; starting one produces work that cannot land.
2. **Re-run the item's `still open?` check first.** Statuses go stale, and items have been found
   already implemented while filed as open.
3. **Verify with the item's `done when`, not by reading your own diff.** If a check needs a live
   model, see what is serving on 127.0.0.1:8080 first; several test files require it and will
   otherwise pass vacuously.
4. **If you find it already done**, move it to `TODO-settled.md` with a header line
   `settled: <commit or file>` and stop. That is a complete, valuable outcome.
5. **If it turns out to need a decision**, move it to Needs the operator with the question stated.
   Do not guess the answer.
6. **Do not edit an entry marked "in flight on a branch".**

## Repo facts you will need

    cargo check --workspace
    cargo test -p <crate>            # crates/: backend bridge builtins claude code dialect{,-glm,-qwen}
                                     #   embeddings flowy github harnessd http letibot leticode-config
                                     #   llama metrics opencode plugin-sdk plugins provider room
                                     #   sessionlog tokencore tools transcript tui turn ui vt webfetch websearch
    python3 tests/fidelity/run_gate.py    # template fidelity gate, must print GATE PASS
    scripts/clippy-gate.sh                # the gate's clippy step

Tests needing a live model server on 127.0.0.1:8080: `sessionlog/{live_e2e,late_head,
sessions,resume_frames}`, `harnessd/{loop_closes,wired}`, `turn/{live_qwen,
engine_decisions}`, `tools/{exec,background,confine}`.

---

---

# Code work

Things an agent can do in this repo, ordered by value: stability and UX first.

---

## R93 - What the model is told: tics, leaked numbers, contradictions, schema mismatches

**Status:** open, filed 2026-10-12. Items 1, 4, 5 and 6 are done (R93b, 2026-11-04), A1 to A7 are in. left: item 3 (B1), item 7 (the F findings the named ones do not cover), item 8 (a fabric memory body no tool reads, and the fabric block's time changing per open); `restart_restore`'s integration tests cannot open a harness on a box without `/sys/fs/cgroup`, so the restart wording has no end-to-end read there.

From real data (`sessions.db`, fork copies removed: 4,347 injected items) and the code. The full
report has the exact text and a replacement for every item — work from it. **Every lettered item
in that report (A1–A7, B1–B13, C1–C14, D1–D4, E1–E16, F1–F58, G1–G10) is an entry of R93 in its
own right**, addressed as R93.<id>; the list below groups them by what to fix first:

1. **The `[context check]` quotes the model's own sentence back, ✓ marks and all** (`context_claim.rs:460-466, :517-523`). The "my context N%" tic began two minutes after the first numbered check in s-1789919514688401228 (108 of 113 live checks are there); the latest one still quotes "Hmm — **my *context* ✓** ✓ ✓" (14×). Stop quoting. Also A2 (`repeated` reaches the model via `harness` warnings), A3 stale doc comment, A4 (`context_fill` `harness.rs:12341` ignores `--no-auto-compact`), A5/A6 (reply/reasoning variants), A7 (`digest.rs:192-197` invites context sizing). [model A1–A7]
   - **A1–A7 are all in** — the item is done. A1/A5: `notice` and `reasoning_notice` quote nothing and say *if context was the only reason you stopped*; A2: the `harness` tool's `warnings` view withholds the repeated-claim detail by code, so the quote and the numbers stay the operator's (`builtins/harness_view.rs`'s `WITHHELD`, test `the_repeated_context_claim_is_withheld_from_the_model`); A3/A4/A7 in `9031b890` and `7d876be2`; A6: a claim found in the reasoning is said only where a nudge continues the turn, so it is not the last row of a finished turn (`harness/turns.rs::run_rounds`, test `a_reasoning_claim_on_a_turn_that_ends_is_not_said`).
2. **The memory/box note is on 93% of bash results** (3,979/4,256 today; subagents ~95%) and peak/cap on every job — resource sizing the model should not size by. Say it only on a kill or near the cap. `bash.rs:140-168, :1077-1081`; `exec/memory.rs:236-243`; `exec/host.rs:221-235`; `jobs.rs:378, :713-717`; `jobwatch.rs:1269`; `mergequeue/gate.rs:252-257`. [model F1–F8, C4]
3. **The todo nag**: 45% name a row already in progress (793/1,778); 45% got no tool call back, only restated status (113× "Not done: three boxes open…"); "(N more open)" in the compared text resets the R77 backoff; criterion rows nagged every window (124× one row); `nothing_can_start` fires on job/young-child roots `todo_write` promised were silent (`todo.rs:2381-2416`, `:1813`); `attach_now` appended with no plan or no `todo_write` seated (`todo.rs:1355-1360`); 60–70-word advice on every nag. 393 identical nags at 66 s in one session were the pre-R77 metronome, fixed by ebc2af7. [model B1–B13]
   - **B4, B7, B8 and B13 are in**; B2/B3/B5/B6/B9–B12 landed earlier (`todo_write`, the idle clock). B4: a row whose own words are a running job's description is work in flight, so the check is silent instead of offering the wait it is already doing (`todo.rs::unfinished_plan_for`; tests `a_row_a_running_job_reads_like_is_not_nagged`, `the_offer_still_names_a_running_job_the_row_does_not_read_like`). B7: a need on a row the operator struck off is repaired by re-sending the row in `todos`, not by re-wording it, which adds a row and leaves the old one blocked (`blocked_line`). B8: the unknown-argument refusal names every door `wait_on` has — job, child, the operator's answer, `detach` (`tests/r93.rs::the_argument_list_does_not_call_wait_on_job_or_subagent_only`). B13: the tool's description is four paragraphs and one line per field, about 1,100 words before, guarded by `the_description_is_one_short_paragraph_per_field`. **Left here: B1's first clause** — a row already InProgress is still asked about at the next window; the report's fix is to leave it until it has been idle for the cap interval.
4. **Completion texts contradict themselves and arrive after the model already read the result** (21% of `[task]`, ≤6% of `[job]`): "don't call `task_result` … read it with `task_result`", placeholder "…" for a known id, the command echoed in full, `[meanwhile]` heads explaining the holding policy (and leaking the 10-min cap), `[monitor]` announcing the model's own retire as a firing, plural bugs. `harness.rs:1064-1148, :1260, :990-1004, :8657-8660, :13552-13554, :14865`; `delivery.rs:356-368`; `deliverable.rs:97-113`. [model C1–C14]
5. **"You keep overthinking"** leaks `reasoning_only`, says "Reason after" to models that cannot, and D2 is factually wrong (`harness.rs:11367-11387`; `compaction.rs:243/253`); the length-cap and progress-watcher texts are long (`length.rs:139-141`, `progress.rs:246-268`). [model D1–D4]
6. **Compaction/reseat/restart/stale notes**: `continue_after_wall` names the "context wall" and can fire with nothing compacted (`harness.rs:9037-9040`, `turn_tail.rs:177`); **literal 14-space gaps** in the cut note (`harness.rs:10520`); transcript ids, R27 and KV-cache internals (`event.rs:1061-1080`); reseat notes stacked 2–3× and `[reseat]` asks for a one-line restatement and re-plants `/reseat` (`harness.rs:2252-2283, :10581-10584`); the stale notice is 120 words of operator text (`:2205-2209, :2483-2514`); `[restart]` contradictions, nested tags, `pkill` hint missing the required `pattern`, `wait_on` named as a tool (`restart.rs:266-303, :353-354, :439-440, :480-482`; `subagent_record.rs:393-411`); `[cleared]` leaks ids (`harness/clear.rs:171-178`). [model E1–E16]
7. **Tool descriptions and results contradict the code**: `wake_sentence` (F12) vs R79 and six other texts; `bash` says a foreground command is moved to the background at `timeout_ms` but it is killed (F14, subreviewer's claim, not re-read); `job_wait` says it blocks but returns at once (F16–F18); `task`'s schema lacks `model` (F21) and `task_start` lacks `wake` (F22); `monitor`'s description contradicts its schema (F28); `retire` renders "it had retired by the model" (F29); two list tools (`todo` vs `todo_write`) and the intent check names the unseated one (F30); merge-gate text tells the model to commit AGENTS.md to main (F31); **literal `{raw}`** in `notes.rs:374` (F42); **~18-space gap** in `gate.rs:218` (F10); duplicated clause in the predicate note, 436× (F27); the snapshot-budget note 5,611× (F38); `[repaired]` notes every call (F56); the intent-check threat on `todo`, 163×, for a check off by default (F58); and the rest F2–F59 in the report. [model F1–F58]
8. **The base prompt** (`config.rs:987-1024 DEFAULT_SYSTEM`) says **"You have read-only tools."** to seats that have `edit`/`write` (G2); forbids tools for world questions even with `web_search` seated (G3); names tools many seats lack (G4); children inherit the parent's own-session and fabric blocks (G5); the fabric block names a `flowy get` action and a `skill load` form that do not exist (G6, G7); "Reasoning goes in a row" (G8); the gatekeeper's "reason as long as you like" vs the overthinking nag (G9); dead role tool names (G10). And it has no rule on waiting, jobs, todos, context or asking the operator, so nothing settles the contradictions above (G1). [model G1–G10]
   - **Items 4, 5 and 6 are done** (R93b, 2026-11-04). Every one had a test asserting on the text the model is handed:
     - C5 `deliveries.rs::stopped_closing`: a handed-up settlement ends `(Started by `s-a`, which has stopped; its output is gone.)` - and, for a child, `…; its answer is still here: `task_result` task="…".` One clause per starter in the batch. The head's `job_card::CLOSINGS` and `blocks::PROMISES` learned it as a set (`HANDED_UP`) rather than by position, and `job_notice_theirs.txt` is re-blessed. Tests `a_settlement_carries_the_jobs_name_and_an_unnamed_one_is_unchanged`, `a_handed_up_settlement_names_its_owner_and_sends_nobody_to_a_shut_door`, one_line_walls `an_orphan_childs_completion_…` (`a8a95a06`).
     - C6 `a_held_batch_never_denies_the_plan_line_under_it`: a held batch under `[meanwhile] Finished while you were working:` names the row that waited on it and the head never says the plan was not waiting - the clause that denied it went with C7's head (`2d4ff5ad`).
     - C9 (already fixed in `9031b890`) now has its test and its own function: `job_window::due_notice` builds `[todo] Due (its condition is met):` and names no count, so no number can disagree with the lines under it; `a_firing_names_its_rows_and_no_count` (`3b2f58ac`).
     - C10 `fate_line`'s `Unknown` is `` `j121`: unknown job (lost in a restart, or a wrong handle); its result is unavailable. `` - it named no place to read (`289b8f5a`).
     - C11 a parent's message is `[parent] {text}`: the child has one parent and the session id was the daemon's (`d5909da6`). The head has a `[parent]` card reader and keeps the two old openings for stored rows.
     - C12 `harness::said_failure`: a child that hit the wall hands its parent `ran out of room; resume it with task_message`, not 41 rounds and 980,112 tokens (`70a04c26`).
     - C13 `deliverable::with_the_line` puts the dirty-tree line AFTER the answer, so the parent's first-line bullet is the answer (`ec660f6c`).
     - C14 `mergequeue::git::daemon_leftover` ignores every file directly under `.letibot/`, not only `*.jsonl`, and still treats a SUBDIRECTORY there (`plugins`, `notes`) as the child's work (`0b5104ae`).
     - D3 `turn::length::batch_failed_notice`: `Tool call {name} was cut off and not run. Send it again complete (split a large write into several edits).` The head's `length_cap` reader accepts both openings (`639ec530`).
     - D4 `ProgressDetector::nudge` is two sentences, 22 words: `Your last {n} rounds returned nothing new. Answer if you can; otherwise try a different query and say what you're looking for. One more empty round ends the turn.` (`2f64e095`). The stop's own `evidence` keeps the tally — that one is the operator's.
     - E2 was already right in the tree (` The summary is incomplete (cut off); treat anything missing as unknown and check with `transcript`. `) and had no test: `a_cut_off_summary_is_admitted_in_one_sentence` pins it and asserts no double space survives (`fecd0a6a`).
     - E3/E4/E5 the compaction note reads `Earlier conversation is replaced by this summary (full history: `transcript` tool).{ The most recent messages follow verbatim.}{ The verbatim part starts mid-exchange; its first message answers something not shown.}` - no transcript id, no item counts, no "proposed no tool calls". The head's `system_notice` and `diff_popup` learned the new opening (`3613fdc4`).
     - E6 a `local_model` tail reason is left out of the model's row: it is a rule id and a cache rule (`51572a93`).
     - E9 the model gets its own stale-prompt sentence - `[stale prompt] Your tool list is out of date: … Updating it is the person's call: don't ask. Mention a missing tool only when you need it.` - and the head keeps the long one that names the session and the clock. `Registry::set_stale_prefix` now takes both, set and cleared together, so the two cannot disagree (`a5d2d2b7`).
     - E10 `[cleared]` names no session id and counts nothing: `[cleared] The operator cleared the conversation; assume nothing from before …` (`aaf77344`).
     - E11 `subagent_record::child_notice` no longer names the time or the build sha, and `build()` went with the sentence that used it (`16e1d8da`, `adaa7816`).
     - E12/E13/E16 `restart::header` leads with what happened and puts the clock in one bracket - `[restart] The daemon restarted (down 4m12s). Restored:` / `(stop time unknown)`, `nothing_running` ends `. Nothing was running.`; the parent's child lines are bullets (`- subagent `x`: resumed.`) with no second `[restart]` tag, and the firecode abandon reason names the tree it copied; and `notice()` appends `Nothing above is running now.` whenever a job line is there, which a restored restart also owes. The head's `restart_card` reads the new bracket and still reads every old opening, and the ten `restart_*` fixtures are re-blessed (`33046d7a`).
   - **What is still open here**: `restart_restore`'s integration tests cannot open a harness on this box (`/sys/fs/cgroup` is absent, so `HostBackend::confined` refuses), so the restart wording is pinned by the unit tests and `card_fixtures` only; the model's `[restart]` sentence has no end-to-end read here.
   - **G1–G10 are in main** (`Config::compose_for_seat`, `config.rs:2389`, called from `Harness::open_with_registry` when `Config::seat_prompt` is set; `SECTION_NAMES`, `config.rs:1126`; `default_section`, `:1192`) — the branch they were built on is gone. The base prompt is composed at open from `SeatFacts` — the seated tool names after role and downgrade, the backend's real scratch path, subagent, linked worktree. Sections (`config::SECTION_NAMES`: identity, language, tree, find_and_read, edit_files, scratch, notes, working, tone) say only what is true for the seat; a section with nothing true is left out, overridden or not. New `working` section: the R79 delivery sentence, `ask_user_question` (blocks) vs a `todo_write` `wait_on` operator question (the plan goes on), the harness manages context (no numbers), stop/continue (root) or "your last reply is your parent's answer" (child), the queue rule. Children compose for their own seat (no own-session paragraph, no fabric block). Fabric block renders `skill` / `flowy get` (CLI through `bash`) only when the seat has them; "Reasoning goes in a row" dropped; gatekeeper and notes-keeper "reason as long as you like" → "explain what you found; keep to the evidence"; standing-notes index line has no token number; dead `researcher()`/`reviewer()` roles removed. `prompts.toml` keys renamed: `read_only_tools` → `tree`, `scratch_path` folded into `scratch`, `working` added. The scratch pid is masked in `prefix_fingerprint` (`harness::instructions_of`) so a restart does not call every session stale. Left: no tool reads a fabric memory body (only the CLI); the fabric block's "Read from the node at <time>" still changes per open, so seated roots read as stale after a restart (pre-existing).

---

## R74 - What the 2026-10-11 landings left open

**Status:** audited 2026-11 (nine fixed, seven already done, six not a session's to settle, 37 since settled), then worked (4, 11, 12, 16, 23, 25 and 33 closed). left: item 20 (the rest of the report sink's sites - the sink is built and 18 of them are routed), item 39 (a live gate on lubuntu1/2), item 40 (the inline blocks over 800 lines).

Everything merged on 2026-10-11 (main `cf47e65`: R61, R64, R65, R66 instrumentation, R67, R68, R69,
R71, R72 + follow-up, R73, the no-llama build), and the gaps each landing reported or a check
found. One place to read them; each line names the item that owns it, whose own "left" list has
the detail.

**Audited 2026-11: where each item stands.** The list below is the record as it was written; this
block is the answer, item by item, after a pass that re-read every one against the tree and fixed the
ones that were code. `FIXED` names the code; `DONE` is an item the tree had already closed without
the list being updated; `OPERATOR` and `DESIGN` are not a session's to settle; `CHORE` is work with
nothing to design.

- 1 **FIXED** — `mergequeue::review::deliver_refusal` writes the undelivered refusal down
  (`refusal:<head>:<row>` in the store's `queue_told`) and `redeliver_refusals` hands it to the first
  session that opens, once, for a day, while the verdict stands. `Served.redelivered`.
- 2 **OPERATOR** — `RepoQueue::health` gates `[queue] target` every pass, including a daemon start
  with nothing queued (~7 minutes). Skipping it until a landing needs it trades a stale *main is
  green* line for that cost; half the waste is already gone (`regate_at_start` re-gates only a red,
  blocked or timed-out tip).
- 3 **FIXED** — `Store::review_rows_in` and `ReviewState::QUEUED`; the three wake paths read the live
  rows instead of the table and filtering in Rust.
- 4 **DONE** — the door was already there and nothing said so: a head sends `ClientFrame::Slash`, the
  daemon parses `Slash::Queue`/`QueueIn` (`crates/harnessd/src/slash.rs`) and runs it against the
  session's repository (`sessions.rs`), and `verbs::approve` has had a caller since
  `Harness::approve_review`. Two tests hold it now: the typed lines
  (`the_queue_verbs_a_person_types_go_to_the_daemon`) and the whole path through a real daemon and a
  real repository (`queue_verbs_from_a_head.rs`, where each row verb reaches the lookup and `regate`
  reaches the resolver).
- 5 **DONE** — `sessionlog::gatekeeper_session(workspace)`: one queue session per workspace's
  daemon, not one per box.
- 6 **FIXED** — `mergequeue::snapshot` keeps each LIVE row's own last `RECENT_MOVES_PER_ROW` moves
  beside the queue-wide window, so a busy queue no longer pushes a row's history out.
- 7 **FIXED** — `tools::gatekeeper::{wake, Reviewer, NoReviewer}` deleted; nothing stood on them.
- 8 **NOT CODE** — a `perf` measurement on an idle worker.
- 9 **OPERATOR** — three rulings (`[queue]` in `leticode.toml` exists): withdraw vs delete for
  `/queue rm` and `clean`; `red_target`'s default; `git branch -D` after landing.
- 10 **DESIGN** — the mechanism is built (`tools::spill::Spiller`) and wired to every call, but
  `SpillPolicy::Unset` → `NoBudget`, so nothing is capped by default. A default budget derived from
  the window is a policy the operator has not made.
- 11 **FIXED** (`8b73102b`) — `Harness::echoes_reasoning` is the one answer, read by the request
  builder's own predicate and threaded through `roll_resident`, `sent_since`, `sent_between` and
  `sent_scale`, so a tool-carrying round counts the reasoning it echoes and a no-tools one still does
  not. `a_round_that_carries_tools_counts_the_reasoning_it_echoes`.
- 12 **FIXED** (`d6d87e5f`) — `Harness::carried_pair` puts the new base through the last measured
  ratio and stores it, so a restart before any round reads provider units rather than ledger bytes;
  a verbatim fork still keeps its measurement, and a local session still stores nothing.
  `fork_carries_the_ratio.rs`.
- 13 **NOT CODE** — `e834316`'s six tests are all in the tree; what is left is the operator's live
  tmux replay of the real session.
- 14 **DESIGN** — 16 of 79 calibration excuses still score under the floor and there is no second
  tier. A second scorer is a slice of its own.
- 15 **NOT CODE** — the floor is a literal with a measured provenance; re-tuning needs the operator's
  `.letibot/context-claims.jsonl`, which no checkout has.
- 16 **DONE** (`2341d5f0`) — `an_excuse_in_a_round_that_calls_a_tool_rides_the_step_boundary`
  plays the round that says the sentence and then calls a tool: the correction is in the transcript
  once, in the harness's own voice, and the round after the boundary is the reply.
- 17 **DONE**, with a residue — all three decisions are in (park at 10 min, reopen on a message or a
  head, a message earns the plan-check back, failures are `TurnFailed`). `wake_failed` and
  `todo_check_failed` survive in `sessionlog::warning`'s table with no emitter anywhere.
- 18 **OPERATOR** — roots are never closed automatically, and an idle root holding a monitor pins the
  daemon until it stops (`court_refusal`'s monitor arm). Not a defect in the close path; a policy.
- 19 **FIXED** — `RECORD_LIMIT` and `bound_records` in `exec/host.rs`: `reaps`, `promotions` and
  `Deadlines.fired` drop the oldest instead of growing for the life of the daemon.
- 20 **PARTLY** — the sink exists: `Hub::warn(code, detail)` and `Hub::warn_with(.., compaction)` in
  `crates/sessionlog/src/hub.rs`, one construction of the variant, with a test. `Harness::warn` and
  `Sessions::warn` go through it, and all of `harness/wall.rs`'s twelve sites and six of
  `harness/turns.rs`'s — 18 of about 114 in `harnessd/src`. The rest still build
  `SessionEvent::Warning` inline, so the tree has two idioms until each is routed; the sites are
  listed by `grep -rn 'SessionEvent::Warning {' crates/harnessd/src`.
- 21 **DONE** — the gatekeeper retry exists and cannot storm: one attempt per 30 s pass, one warning
  per distinct reason.
- 22 **DONE** on `r60-term` — `letibot_vt::Screen::mouse` and the pane's own translation.
- 23 **FIXED** (`9ba596c7`) — the six cards are one list (`app::Card`, `App::card_up`) that the
  keyboard's order and the bottom bar both read, and `App::focused_surface` answers `None` while one
  is up: a card in the chrome owns every key, so the pane under it is drawn and not focused.
  `a_card_in_the_chrome_leaves_no_surface_focused`.
- 24 **FIXED** (`a4384ed5`) — the flake was one fixture directory shared by two tests, one deleted by
  the other's `Drop`; one directory per call now. Residual: `settle` gives up after 10 s of ticks.
- 25 **DONE** (`928604a4`) — `memory_card.rs` puts a real head over the socket in front of
  `AdjudicatedGate::grant_memory`: the ask is built from `/proc/meminfo` (`apparatus::applies` on a
  platform that has no such file), the card's three options are asserted, a once grant covers its
  call and nothing after it, and a session grant covers the next ask with no second card.
- 26 **DESIGN** — no box-wide ledger: two sessions can each be granted half of what is free at the
  moment of the ask. The single-daemon half (subtract the caps live jobs were promised) is three
  lines in `bash::memory_for`; the cross-daemon half needs the `flock` the proposal describes.
- 27 **NOT CODE** — a runner's `~/.actrc`; `act` appears nowhere in the tree and the fleet gate is
  R62's unfinished done-when.
- 28 **NOT CODE (deliberate)** — linear over the whole transcript on purpose, per its own doc. A
  cheaper index (`asked`/`answered`/`scanned` beside the session) is written out in the audit if it
  ever matters.
- 29 **NOT CODE** — the slow polls are documented as left on purpose and the reasons still hold.
- 30 **DONE** on `r60-term` — `TermPane::held` keeps a report the read ended inside.
- 31 **FIXED** — a real child's hub now refuses a `Compact` and the `child_door_refused` warning is
  read back off it (`tests/child_nag.rs`), which is the half the pure-function test could not show.
- 32 **NOT CODE** — settled in R67's own entry: a review mark names the head the reviewer judged and
  a landing mark the rebased commit, by design.
- 33 **CLOSED, one hole unresolved** (`e4707f8e`) — the four `bridge` fixtures no longer clear
  `LETIBOT_REQUIRE_APPARATUS` from their own environment, and `scripts/check-test-attrs.sh` (the
  gate's second step) refuses a file that clears it or `LETIBOT_REQUIRE_MODEL`, so a fixture cannot
  turn a binary's skips green. The audit's third hole — "a key-less box's `children.rs` test" — was
  looked for in every `children.rs` in the tree and no test there skips on a key; it is recorded here
  as not found rather than as fixed.
- 34 **FIXED** — the two wall-clock assertions in `tests/stop_force.rs` are gone (a span that began
  before the fixture attached, compared against the app's own deadline); the ordering claims stay and
  `reconnect`'s clocks were already injected.
- 35 **FIXED** — a file-level mutex around the one process-wide variable in `tests/confine.rs`, held
  for the whole body, which is what makes its SAFETY note true.
- 36 **FIXED** — every live target is `#[ignore]`d (`*_live.rs` ×7, plus `sessionlog/tests/live_e2e.rs`,
  `turn/tests/live_qwen.rs`, `flowy/tests/live.rs`), so a workspace run cannot dial the box; a person
  runs one by name with `--ignored`. `sudo_ask_live`'s re-exec spawn says `--ignored` with it.
- 37 **DONE** — the workspace clippy step was run on the result (`sh scripts/clippy-gate.sh
  --offline --all-targets -- -D warnings -A clippy::result_large_err -A clippy::type_complexity -A
  clippy::large_enum_variant -A clippy::too_many_arguments`), which is the step the audit said had
  never been run on a landed tree.
- 38 **DONE** — R63: `sessions::Court`, one thread per session, armed by `Daemon::run`;
  `tests/thread_per_session.rs` is its done-when.
- 39 **PARTLY** — R62's seam is built and tested against a fake ssh (`GateRunner`/`HostGate`,
  `mergequeue/gate_host.rs`); its done-when — a landing gated on lubuntu1/2 with the row naming the
  host — is a live run, and the fleet's `~/.actrc` is item 27's.
- 40 **CHORE** — R70 is partly done (`harness.rs`, `adjudicate.rs`, `todo.rs` and `sessions/` moved
  their blocks out). Nine inline blocks are still over ~800 lines; `harnessd/src/plugins.rs` is the
  largest at 3 320 lines and the one to move first. Not attempted: moving a 2 469-line block means
  reproducing every line of it, which is a job for a session that can cut and paste, not for one
  whose only write is a retyped file.
- 41 **DONE** — R60's three: scrollback (`vt::Screen::HISTORY_ROWS`/`view`), panes numbered on the
  wire, and a confined pane (`wrap_terminal`, no `--new-session`), each with a test.

**R61 — the queues**
1. A rejection whose root session is not open in any daemon is only raised (`refusal_undelivered`)
   — no model hears it later. Not a regression (the old door did the same).
   — OPEN: still only raised — `refusal_undelivered` is published and no model is told later (`crates/harnessd/src/mergequeue/review.rs:897`).
2. Every daemon start gates its repositories' target tips — ~7 minutes of gate even with nothing
   queued.
   — ✓ fixed in 362113e4, test a_judged_tip_is_gated_again_on_request_and_when_a_daemon_takes_the_queue
3. Each wake reads the whole `review_queue` table (`queue_notices`, `handover_notices` in
   `Harness::wake`) — should read only open rows.
   — OPEN: `store.review_rows(None)` still reads every row (`crates/harnessd/src/harness.rs:2924`, `:3074`), and the query has no state filter (`crates/tokencore/src/store/queues.rs:797`).
4. No `/queue regate`: a timed-out or blocked target holds all landings until its tip changes or a
   daemon restarts.
   — ✓ fixed in 362113e4, test a_judged_tip_is_gated_again_on_request_and_when_a_daemon_takes_the_queue
5. One `s-gatekeeper` review session per box.
   — ✓ fixed in 0eb8958a, test two_workspaces_get_two_review_sessions_and_one_workspace_keeps_its_own
6. The pane's per-row history covers only the last 60 moves overall.
   — OPEN: still 60 — `RECENT_MOVES` (`crates/harnessd/src/mergequeue.rs:154`), read at `:283`.
7. The tools-crate `Reviewer` trait and `wake` are unused and can go.
   — OPEN: both are still there — `crates/tools/src/gatekeeper.rs:261` (`wake`) and `:290` (`Reviewer`), with no implementor outside that crate's own tests.
8. The "worker ~0% when idle" done-when was not measured with `perf`.
   — OPEN: not code — no `perf` run of an idle worker is recorded anywhere in the tree.
9. Per-project config is `[queue]` in `leticode.toml` (`target`, `merge_via_session`,
   `red_target`) — three rulings deferred by the operator: withdraw vs delete for `/queue rm` and
   `clean`; `red_target` default `fix`; `git branch -D` after landing.
   — OPEN: the three rulings are still the operator's to make; the keys are read at `crates/harnessd/src/mergequeue/settings.rs`.

**R72 — compaction counting**
10. A single tool result larger than the whole window is still appended in full; the next round
    refuses to send it, but compaction then folds a history already over the window. Capping tool
    results is a larger design.
    — OPEN: `append_items_reporting` (`crates/turn/src/engine.rs:445`) still appends every item whole, and nothing caps a tool result's size.
11. Excluding reasoning rows from the provider-sized count is an assumption: a provider that counts
    the current tool chain's reasoning makes the count one round short until the next measurement.
    — OPEN: still an assumption — `sent_since` (`crates/harnessd/src/harness/wall.rs:1937`) sizes the prompt with the reasoning rows left out.
12. A restart after a compaction but before any round falls back to the unscaled ledger count
    (measured harmless at 22,496 against a 37,500 wall).
    — OPEN: `recovered_scale` (`crates/harnessd/src/harness/wall.rs:1735`) still answers the stored pair only when the stored ledger is not longer, so a restart after a fork comes back unscaled.
13. The follow-up (`e834316`) was not re-checked live in tmux; the real leticl session was never
    replayed against the fix.
    — OPEN: not code — the follow-up has never been replayed against a real leticl session.

**R71 — the context-excuse check**
14. 16 of 79 calibration excuses score under the floor (a clause deep in a long sentence, or no
    word saying whose context) — a second, embeddings tier.
    — OPEN: no embeddings tier — `crates/harnessd/src/context_claim.rs:48` still records the 16 of 79 it misses.
15. The floor (0.25) is in-sample; re-tune it from `.letibot/context-claims.jsonl`.
    — OPEN: `FLOOR` is still 0.25 (`crates/harnessd/src/context_claim.rs:64`), not re-tuned from `.letibot/context-claims.jsonl`.
16. No harness-level test of the tool-calling branch (the step-boundary queue).
    — OPEN: the harness tests all play a round with no tool call (`crates/harnessd/tests/context_claim.rs:3`), so the tool-calling branch is still untested at harness level.

**R68 — daemon lifecycle**
17. The agent's decisions to confirm: idle children PARK after 10 minutes (`child_park_after`) and
    reopen from the store on a message or a head; a message to a held session now earns the
    plan-check back; children's failures are `TurnFailed` instead of `wake_failed` /
    `todo_check_failed`.
    — OPEN: still the agent's to confirm — `child_park_after` is 600 s at `crates/harnessd/src/config.rs:1458`.
18. Root sessions are never closed automatically — a resumed root keeps its harness and its
    monitor-wake thread until the daemon stops.
    — OPEN: roots are still never closed — `park_idle_children` (`crates/harnessd/src/sessions.rs:2994`) parks children only.
19. `reaps`, `promotions` and `Deadlines.fired` still only grow (small records).
    — OPEN: still only grow — `Deadlines.fired` is a `Vec` pushed and never drained (`crates/tools/src/exec/host.rs:463`), and `promotions` likewise (`:1116`).
20. The single `report(session, code, detail)` sink is not built.
    — OPEN: no such sink — there is no `report(session, code, detail)` anywhere under `crates/harnessd/src`.
21. R68.7 (the gatekeeper retry storm) is probably moot after R61; not checked.
    — OPEN: still not checked.

**R69 — the head's input**
22. **DONE on `r60-term`**: `letibot_vt::Screen::mouse` records the program's mouse request and
    the pane forwards a report, translated to the pane, only when it asked — see R60.
    — ✓ fixed in 4ef25b13, test the_programs_mouse_request_is_recorded_not_dropped
23. Six modal cards (allow-all confirmation, new-todo, password, `!term close`, prompt, key ask)
    are not on the surface stack and have no key tables.
    — OPEN: the cards are still first in `App::key` (`crates/tui/src/app/keys/mod.rs:200`) and not on the surface stack; `key_key_ask` is `crates/tui/src/app/keys/cards.rs:262`.
24. `editor::the_editor_pane_wears_the_heads_frame_not_nanos` failed once at a fixture `unwrap`
    and passed on every rerun — a flake to find.
    — OPEN: not code — the flake was never reproduced.

**R65 — memory caps**
25. The operator card for an ask above the ceiling has never been answered by a person in a head.
    — OPEN: not code — the card is `crates/tools/src/adjudicate.rs:2401`, and no person has answered one in a head.
26. Two sessions can each be granted 50% — the box-wide ledger proposal is open.
    — OPEN: no box-wide ledger — the rule is per ask (`crates/tools/src/exec/memory.rs:245`), so two sessions can each be granted 50%.
27. Each fleet runner needs `~/.actrc` pinning the `ubuntu-latest` image, or `act` asks
    interactively.
    — OPEN: nothing in the tree pins it — `.github/workflows/ci.yml` is the only runner definition, and `actrc` appears nowhere but this entry.

**R67 — the contained fixes**
28. Item 14: the abandoned-call sweep is linear but still reads the whole transcript each pass (a
    late answer must count).
    — OPEN: `abandoned_calls` (`crates/harnessd/src/harness/turns.rs:3345`) still walks the whole item list every pass.
29. Item 24: slow test polls left on purpose.
    — OPEN: not code — the polls were left slow on purpose.
30. **DONE on `r60-term`**: the start of a report a read ends inside is held for the next read
    (`TermPane::held`) — see R60.
    — ✓ fixed in 26913ddd, test a_mouse_report_split_across_two_reads_is_put_back_together
31. Item 15: the subagent "command dropped" warning is tested only as a pure function.
    — OPEN: still only absence assertions — the warning is published at `crates/harnessd/src/harness/children.rs:960`, and the tests check it is absent (`crates/harnessd/tests/child_nag.rs:602`, `crates/harnessd/tests/clear_context.rs:474`).
32. Item 21: half of it was not a defect. The status marks cite pre-rebase commit hashes.
    — OPEN: not code — the marks still cite pre-rebase hashes.

**Tests and the gate**
33. Under `LETIBOT_REQUIRE_APPARATUS=1` a box without a working sandbox (bwrap, user namespaces)
    now FAILS the gate where it silently passed.
    — ✓ fixed in 18f57e00, test present_skips_when_absent_and_fails_when_the_run_demands_the_apparatus
34. TUI timing tests flake under heavy parallel load (two failures once while five agents
    compiled; clean on reruns) — likely `reconnect`/`stop_daemon`.
    — OPEN: not code — no cause found.
35. `tools/tests/confine.rs` sets `LETIBOT_TEST_FAKE_TOKEN` with no lock (nothing else reads it).
    — OPEN: still unlocked — `crates/tools/tests/confine.rs:889`.
36. `cargo test -p letibot-harnessd` / `-p letibot-sessionlog` also run the `*_live` files, which
    AGENTS.md says not to run broadly (they skipped — nothing listened on 8080).
    — OPEN: the `*_live` files are still in the package's test directory (`crates/harnessd/tests/compact_live.rs:1`), so a `-p` run still reaches them.
37. The workspace-wide `cargo clippy --all-targets` gate step has not been run on the result.
    — ✓ settled: it is the merge gate's own step now — `sh scripts/clippy-gate.sh --all-targets --
    -D warnings …` (`agents.md:98`), which touches every member's root first so a warm target cannot
    answer for the lints (`3feb2606`).

**Open work, in order**
38. R63 — a thread per root session (unblocked by R68).
    — ✓ fixed in f68c90b0, test a_held_turn_in_one_session_does_not_delay_another
39. R62 — the gate on lubuntu1/2; R61's `GateRunner` is the seam.
    — ✓ fixed in 5d3b2f05, test the_tree_and_the_repository_arrive_and_the_steps_run_there
40. R70 — move the largest inline test blocks into sibling files (unblocked now the big edits have
    landed).
    — OPEN: only part done — `crates/harnessd/src/plugins.rs:2226` still carries a 2,469-line inline test block (`TODO-settled.md:1500`).
41. R60 — the three things `!term` did not do: **built on `r60-term`**, see R60.
    — ✓ fixed in 26913ddd, test a_second_pane_runs_beside_the_first_and_each_is_named_by_its_number

---

## R95 - A variable an earlier command assigned is unread in the next one

**Status:** the design note (section 4j), the resolver, the card and a placement-true seed are built (2026-11-03). left: an `export` in one `bash` call still does not reach the next - the model's command is a one-shot spawn and `SessionEnv::observe` has no production caller. Closing it makes the session's shell stateful, which section 4j.5 says needs a deny-list for names that change what a program does without appearing in any text.

About 500 recorded commands (`$p`, `$d`, `$pr`, `$CARGO_TARGET_DIR`, …) use a variable the
session set in an EARLIER `bash` call, and G3 resolves only a variable assigned earlier in the
SAME command, so these stay unread (`docs/boundary-and-adjudication.md` §4i.4).

**The operator's decision (D14):** the session's environment is tracked, and a value the session
set may make a command readable for adjudication. Observed, not inferred: the persistent shell's
trailer (`ShellSession`, which already reports cwd) reports the environment after every call.
`PATH` is recorded but not used for resolution until R96 is ruled.

**Done when**: a design note in `docs/boundary-and-adjudication.md` (where it is observed, its
lifetime across a daemon restart, a migrated session, a confined and a remote shell, and how the
card shows it); the resolver reads the tracked environment; the corpus replay shows the
cross-command bucket gone without any command becoming unread; the card names a variable the
session set when it decided the reading.

**And R96, `$PATH`, decided 2026-10-27 (D15): resolved too, built with this.** 527 commands
on lab2x1 and 1,347 on the mac. When `PATH` is resolved, each stage's program is resolved to the
file it will run, and the stage, shape and card name that file when it differs from what the
bare name finds today, so a grant given for one binary cannot settle a stage that runs another.

**Still open?** grep the corpus replay's unread list for variables with no assignment in the
same command.

**What is built (2026-11-03).** §4j is the design note; the resolver reads the tracked environment
(`intent::env`, `parameters::value_at`), and the card names the value and its source for both of the
sources this tree has — the pairs a spawn is given, and a shell's report. The seed is now the
environment this box's spawn really gives, on this box only: a remote host's and a guest's spawn
export the CALL's own pairs into their own shell, so `surroundings_for` seeds nothing there and a
command using one stays unread rather than being placed on a machine it is not on
(`a_placement_that_is_not_this_box_seeds_no_environment`). The corpus counterfactual is built
(`Report::env_fixed`) and needs `~/letibot-corpus/<host>.json`, which this box has not got.

**What is left, precisely.** Nothing observes a `bash` call's own environment, so an `export` in
one call is still not in the next: the model's command is a one-shot `/bin/sh -c` that exits with
whatever it exported, and `ShellSession` — the only thing in the tree whose trailer reports an
environment — is opened by its own tests and no production code. A trailer appended to each call
would report it; carrying that report into the next spawn's pairs is what makes the session's
shell stateful, and §4j.5 refuses that without a deny-list for the names that change what a
program does without appearing in any text (`LD_PRELOAD`, `BASH_ENV`, `PATH` itself). That is the
operator's call, not this item's: with it, R95's remaining sentence is one trailer and one pair
merge; without it, the seed is the whole of what the gate can honestly resolve.

---

## R104 - A ⌘-chord reaches the head as its plain letter, because rano drops the super bit

**Status:** open, filed 2026-10-29, rano's. The chord reaches the head as a chord (46f8409b, rano f4c624e). left: rano's `legacy_bytes` still turns a `CSI ... u` into the legacy spelling and that is the door a `!term` pane's program reads, so cmd-C still types a `c` into a pane - a decision, not a line (rechecked at rano v0.10.1).

MEASURED 2026-10-29 on this box, rano pinned at `bf3fa48`: the operator selects with the mouse
and types ⌘C. The text is copied (the terminal's own copy, and this head's drag release), **and a
`c` lands in the composer** — because the chord never arrives as a chord. Ghostty sends it as
`CSI 99;9u` (modifier 9 = 1 + 8, and bit 8 is super — this head does ask for the kitty keyboard),
and:

* rano's `Mods::from_bits` is `Mods(b & 7)` (`src/term/event.rs:27-29`, and there is no super
  constant at all), so the bit is discarded before any letibot code sees it; — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter
* `kitty_key` then re-encodes the character through `kitty_legacy`, which carries alt and ctrl
  only (`src/term/decode.rs:402-421`, `482-512`), so `CSI 99;9u` decodes to `Key::Char('c')` with
  empty mods; — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter
* and the one door to the raw bytes is lossy the same way — `Terminal::raw_input` answers
  `legacy_bytes` under the kitty keyboard (`src/term/terminal.rs:504-514`). — OPEN: rano's `legacy_bytes` still turns a `CSI … u` into the legacy spelling (`crates/tui/src/app/keys/input.rs:571` pins it) and that is the door a `!term` pane's program reads (`crates/tui/src/head.rs:1041`), so ⌘C still types a `c` into a pane — a decision, not a line. Rechecked at rano v0.10.1 (4e16345f): the super bit is still carried and the head's chord test passes on it, and `Terminal::raw_input` still answers `legacy_bytes`, so this bullet stays open.

**The fix is two lines in rano**, not in this tree: keep the bit in `Mods::from_bits`, and in
`kitty_key` emit `Char(ch)` **with its mods** when a bit the legacy spelling cannot carry is set.
Verified against a scratch copy of the pinned rev: both `CSI 99;9u` and `CSI 99;13u` then decode
to nothing, every existing decode test still passes, and **letibot needs no change** — `key()`'s
last character arm already answers `None` for a modifier it does not bind. — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter; the head's own bindings landed in 8eaa4985, so "letibot needs no change" was not quite so.

**And ⌘⌃C is worse than the report**: with the super bit masked it decodes to `CtrlC`, which
interrupts the turn instead of copying. — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter (both bits are kept and the head answers nothing)

`crates/tui/src/app/tests/keys.rs` carries a characterization test pinning today's decode
(`a_cmd_chord_arrives_as_its_plain_character_because_rano_drops_the_super_bit`), written to fail
loudly with instructions the day rano keeps the bit, and
`a_selection_does_not_swallow_what_is_typed` refuses the tempting wrong fix (a head that goes deaf
under a selection). — ✓ fixed in 8eaa4985, test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter (the characterization test and its file are gone; a_selection_does_not_swallow_what_is_typed is at `crates/tui/src/app/tests/editor.rs:1136`)
**Still open?** `printf '\033[99;9u'` into the key decoder, or press ⌘C over a selection after a
rano bump — a `c` in the composer means it is still rano's. — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter — the decoder is fed exactly those bytes and no `c` comes out

---

## R55 - A round's end and a turn's end are the same event on the wire

**Status:** the field is built - `SessionEvent::TurnFinished` carries `ends_turn` (`crates/sessionlog/src/event.rs:12`, test `crates/harnessd/tests/turn_end_marker.rs`) and this head's row holds across rounds (`a_prompt_in_flight_holds_the_row_up_between_its_rounds`). left: `Hub::turn_in_flight` (`crates/sessionlog/src/hub.rs:1632`) still reads the round and not the field; a round with no calls followed by a nudge still says `true`; unclear: leticl's `:turn-finished` arm reading `ends_turn` is in another repo, check there.

The operator, watching leticl's composer edge during a multi-round turn: *"sometimes 'responding'
timers flickers to Responded and then back, without time reset. not harmful but annoying"*, and then
*"so it looks like the harnessd must be explicit here and add another real end marker"*.

The timer does not reset because you measured the turn once: `harnessd/src/harness.rs:4339`
`begin_turn_clock` — *"Called once per prompt, never per round"* — sets `turn_began_ms` and the
engine's copy, and every round's `TurnStarted` carries that same stamp. Good, and it is the half that
works. The status word toggles because the **ending** is not marked at all: `engine.rs:1318-1320`

```rust
let finish_reason = match done.finish {
    Finish::Stop | Finish::ToolCalls | Finish::Other(_) => FinishReason::Eos,
```

so a round that ended to call tools publishes a `TurnFinished { finish_reason: Eos }` that is
byte-identical to a real ending — which your own comment at `harness.rs:5800` already says out loud:
*"Per round, the same way the heads' own `TurnFinished` updates theirs."* A head therefore believes
the turn is over in the window between a round's finish and its calls being proposed (nothing is
generating and no call is unfinished yet), writes its `Responded in …` report, and the proposal flips
it back. On leticl's screen: `Responding → Responded → Responding` on every round boundary, with the
duration untouched — measured on the operator's own head, `Responded in 4m09s at 21:37` drawn while
the turn was still running.

**The ask: mark the real end.** The minimal shape, and the operator's own framing — *another real
end marker*: a `final: bool` on the existing `TurnFinished` (or a `round: u32` beside it if naming the
round is wanted). The metrics stay per round, where the heads read them; only the ending gains a name.
An absent field should mean **final**, i.e. today's behaviour, so an old head against a new daemon
keeps working and a new head against a recent daemon flickers but never lies.

**Both heads then answer it with one predicate each**: while a turn is in flight the row says
`Responding`, including that window; the report is written only on `final`, and cleared at the next
`TurnStarted` as it is today. leticl's half is one line in its `:turn-finished` arm — it is ready to
land the day the field exists, and its suite builds the event by hand, so it is testable before this
ships.

**What letibot found when it read the three sites — and why the shape is an EVENT, not a field.**
All three citations hold verbatim: `begin_turn_clock`'s *"Called once per prompt, never per round"*,
the `Finish::Stop | Finish::ToolCalls | Finish::Other(_) => Eos` fold, and — the one leticl could not
read — `harness.rs:5800`, which is **support rather than a contradiction**: it says *"Per round, the
same way the heads' own `TurnFinished` updates theirs"* in the middle of explaining why the SESSION
row's prompt size is per round, i.e. it records this as deliberate.

**The engine cannot set `final`, and that is the whole of it.** `Finish::ToolCalls` establishes only
that a continuation MAY follow; whether one does is decided one layer up, in this daemon's round loop
— `room_for_next_turn` and then `run_continuation` (`harnessd/src/sessions.rs`) — and the convergence
point that already knows a prompt is over is the one where `end_turn_clock()` is called
(`sessions.rs:1090`, whose own comment is *"Every turn ends here"*). A `final: false` written at the
engine's fold would therefore be a **guess**, and on the paths where the guess is wrong — the room
check says no, an interrupt lands while the calls are still running, `Finish::ToolCalls` on a turn
that then fails — the head is left saying *Responding* with nothing running at all. That trades the
flicker for a **stuck row**, and a stuck row is the worse defect: R13 exists because a reader who
cannot tell working from wedged is the confusion this row is for.

So the marker is published where the decision is made, and that means **a new variant — `TurnEnded`.**
Re-publishing the round's `TurnFinished` at the convergence point is the alternative and it
double-counts `usage` in any head that accumulates the turn's cost. A field would also have to carry
the right default (`absent` = final, i.e. not `#[serde(default)]`'s `false` for a positive name), which
is the smaller of the two problems.

**CORRECTED 2026-10-03, an hour after the paragraph above: the variant does NOT need the protocol
bump, because this tree already has the mechanism.** [`PROTOCOL_VERSION`]'s version-27 note is that a
new VARIANT forces a bump *because a head has no arm for it* — *"`serde` has no catch-all on this
enum — deliberately, so a head cannot silently skip a fact it does not understand"* — and `Caps`
carries, in its own words, *"**free-form feature names, for forward compatibility**"*
(`protocol.rs`: `features: Vec<String>`, currently unused and evidently waiting for its first
customer). The hub **already gates delivery per head on those caps** — `hub.rs:931` filters by
`can_decide` when routing a decision and `hub.rs:1425` bounds a head by `caps.queue` — so:

  · the head declares a feature name in its `Caps` on attach (this head would declare it, and leticl's
    half is one string where its attach already is);
  · the daemon sends `TurnEnded` **only to a head that declared it**, so no head ever receives a frame
    it cannot read and the reason the bump existed is dissolved rather than dodged;
  · and `Hello` **echoes the features the daemon honours** (a new field with `#[serde(default)]`, for
    which the rule is genuinely *no bump*), because otherwise a head cannot tell *this daemon ignores
    my feature* from *no turn has ended yet* — and guessing that right is the difference between the
    flicker and a row stuck on for ever.

So the bump is not needed after all, and the operator's deliberate pin of leticl to `v0.2.0` (protocol
27) is left intact: an old head gets today's behaviour, a new head gets the marker, and neither ever
sees a frame it has no arm for. If the operator would rather have the coarse signpost — one number
that says the two halves are skewed — then bump and the argument above is the fallback rather than the
mistake; say which in the commit, since the two routes cost leticl different things (a declared
feature, or a re-pin).

**MEASURED 2026-10-03, before any code: the gate the paragraph above rests on does not exist.** The
whole no-bump argument turns on *“the daemon sends `TurnEnded` **only to a head that declared it**”*,
and this tree has no way to send a frame to one head. `crates/sessionlog/src/hub.rs`'s entire public
surface offers exactly two routes to a head: `publish` (an `Envelope` in the log, read by every head in
`seq` order through `next_batch`) and `submit` (a `ServerFrame` **returned to the caller**, which writes
it back to the one head that asked). `deciding_heads()` is the nearest thing to a per-head gate and is
not one — it is a *query* the adjudicator makes before posting — and `Caps.features` today is
**advisory**: `FEATURE_QUESTION_ANSWERS`’ own doc says a head that does not advertise it *“can still be
sent a question”* and will simply not answer. Nothing in the tree filters a frame by a declared
feature.

So the three routes, with what each actually costs:

 · **A per-head directed frame.** New machinery in the connection layer: either the daemon holds every
   head’s writer, or an outbound queue per head that the accept loop drains. It dissolves the bump
   exactly as the paragraph above intends, and it touches the most delicate layer in the daemon — the
   one where a head that is not written to does not hear that its turn ended.
 · **A `SessionEvent::TurnEnded` and a bump to 28.** The simplest thing that works: one variant, every
   head reads it in `seq` order, a late head replays it correctly, and an old head is refused at the
   handshake rather than handed a frame it has no arm for. The cost is leticl’s — a re-pin, and the
   operator deliberately pinned leticl to 27 — which is a *sentence on the other head*, not a code
   change here.
 · **Zero-bump by reuse, and rejected twice for two different reasons.** Re-publishing the round’s
   `TurnFinished` at the convergence point double-charges every head that accumulates `usage`, and an
   old head over-billing is worse than a flicker. And a `Warning { code: "turn_ended" }` is an arm
   every head already has — so it works, and **a head that does not know the code DRAWS it**: one dim
   row per turn boundary, in the conversation the `model_slow_first_byte` ruling has just finished
   moving weather *out* of.

**Which route is the operator’s to pick**, which the paragraph above already says in the same words
(*“say which in the commit, since the two routes cost leticl different things (a declared feature, or a
re-pin)”*). **The fail-first half needs no ruling and is unaffected by the choice**: this head’s row and
the test that currently pins the flicker.

**And the convergence point is now verified rather than quoted** — both routes depend on it, and one of
its edges fails in exactly the way both are trying to prevent. `end_turn_clock` has **one** caller:
`sessions.rs:1090`, inside `after_turn`. `run_prompt` ends by calling `after_turn` unconditionally
(line 974 passes the `out` **Result**, so a failure is not a bypass), `submit` is the only way in, and
the wall-continuation loop (`1057`–`1085`, `WALL_CONTINUES`) runs **before** the clock stops — so a
marker published there lands after every continuation rather than before it. Every ending therefore
reaches it: an `eos` finish, a `length` cut, a failure, an interrupt, and each wall continuation.

**The one caveat, and it is where the publish must not go.** `end_turn_clock()` sits inside
`if let Some(h) = self.open.get_mut(session_id)` — so a session that left `open` between the round loop
and this tail skips it. That *should* mean *no heads left to tell*; the marker's publisher belongs
**outside** that guard anyway, because a head still attached to a session the daemon has closed would
otherwise sit on a stuck row, which is the failure mode this whole row exists to avoid.

**What THIS head does today, so the operator knows what to look for here.** letibot has no past tense
at all — `Responding` is the only word the row has (`app.rs:13718`), so it cannot flicker to
*Responded* — and it **drops the row** for the same window instead: the last call of a round finishing
and the next round's first delta, because `turn_busy()` is `generating || a call unfinished`
(`app.rs:8185`) and every call of the finished round is `Finished` by then. **And this tree currently
asserts that as correct**: `the_status_row_survives_a_tool_call` pins *"the call lands and nothing
else is outstanding, so the work is over and the row stands down"*. The fix therefore REWRITES that
assertion rather than adding beside it, and that is the fail-first evidence — available before any
daemon change, since it is one predicate and one test. The backstop that makes this *annoying rather
than harmful* on this head is `a_turn_that_has_gone_quiet_says_so_rather_than_spinning`: forty silent
seconds and the row says something again.

**still open?** `grep -n "final" crates/sessionlog/src/event.rs` around the `TurnFinished` variant
shows no such field, and `crates/turn/src/engine.rs:1320` still folds `Finish::ToolCalls` into `Eos`.
And the gate the no-bump route needs is *absent rather than unfinished*: `grep -n "pub fn "
crates/sessionlog/src/hub.rs` lists no directed send, so that is the piece to build if route one is
chosen.

**done when** the daemon publishes the end at the convergence point (where `end_turn_clock` is
called) by whichever route the operator picks above — a filtered `TurnEnded`, or a bumped version
whose `Hello` echo says what is honoured — so that **no head is ever handed a frame it has no arm for**;
and a two-round stream on each head shows the status row up across every round boundary and down only at
the marked end — asserted, not watched: a test per head, and on this head that test replaces the one
that currently pins the flicker.

**UPDATE 2026-10-26 — the fact this row is about is on the wire, and this head's half of the row
is done.** `PROTOCOL_VERSION` 52 publishes `SessionEvent::ActivityChanged` — quiet, or busy with
`turn`, `calls`, `delivery`, the jobs and children whose settlement will wake the model, the
monitors it armed, and the cards nobody has answered — computed by the daemon from its own state
and published only when it changes (`crates/sessionlog/src/activity.rs` is the argument). And
**`turn` is the PROMPT's span, not a round's**: `begin_turn_clock`/`end_turn_clock` are *"called
once per prompt, never per round"*, and the daemon publishes that span, so the window this row was
written about — a round's last call finishing before the next round's `TurnStarted` — is filled.
This head's row now asks the activity before the round-level state
(`App::turn_busy`, `crates/tui/src/app/turn.rs`) and therefore stays up across every round boundary
and stands down when the prompt's loop returns; asserted in
`a_prompt_in_flight_holds_the_row_up_between_its_rounds`. So the marker at the convergence point is
no longer needed *by this head*, and what is left of the row is the other half: leticl's row, and
whether an explicit `TurnEnded` marker is still wanted at all now that the prompt's span is on the
wire for any head to fold (the two routes above are unchanged if it is).

**LANDED as a third route: a field on the frame that already ends a round.** Neither a filtered
`TurnEnded` (a new variant is a frame an older head cannot parse — it dies at the first turn end, and
this daemon answers a skewed `Attach` by serving it rather than refusing it) nor a bump (the same
death). `SessionEvent::TurnFinished` gains `ends_turn`, `#[serde(default = "an_ending")]`, so **absent
means an ending**: an old daemon's frame reads as today, an old head ignores the field and keeps its
flicker, and no peer is handed a shape it has no arm for. The producer is the engine's own round fact —
`TurnEvent::TurnFinished.proposed_calls` is `!done.tool_calls.is_empty()`, and `lift` translates it to
`ends_turn: !proposed_calls` — so no second frame is published and no head is billed twice. Frames:
`crates/harnessd/tests/turn_end_marker.rs` (round one `false`, round two `true`, exactly two frames).
Heads: this one reads it in `turn_busy` and the bridge gates `Turn::Idle` on it. **Residual, and it is
the guess this row warned about**: a round that proposed NO calls but is followed by a nudge or a
steering round still says `true`, so the flicker survives there — only a marker at the convergence
point could remove it. `Hub::turn_in_flight` is unchanged and still reads the round, not the field.

---

## T15 - Replace §10's compaction design with structural eviction

**Status:** partly landed - structural eviction of units runs before every summary turn, the addressable map is in the new base, `recall` opens any id it names, and section 10 of the plan reads structural eviction. left: a standalone `prune`.

`docs/compaction.md`. Compaction today bundles *reducing what is resident* (necessary)
with *destroying what is recoverable* (an accident). Falsifier B removed the usual
justification — quality does not degrade with depth — so the trigger is the context
wall and memory pressure only.

The key constraint that rules out the easy fix: **plain eviction invalidates the
prefix just as summarisation does**, because the prefix *is* the old turns. Both cost
a re-prefill, so the question is what to get for it. Answer: replace an evicted span
with a **structured, addressable map** rather than prose, maintained incrementally so
there is no stop-the-world summarisation call, with zoom-in appended via `recall`
rather than rewritten in place.

Depends on nothing. Feeds `docs/memory.md`. Gives `SegmentMark` its missing producer.

— PARTLY LANDED: the record is sized by the room and gated on what it kept, and the tail keeps the
old turns most relevant to the plan (embedding relevance). Structural eviction of units runs before
every summary turn (`crates/turn/src/evict.rs`, test `crates/turn/tests/evict_summary.rs`):
superseded reads, settled subagents and finished calls go as stubs, and failures, the exchange in
hand and anything a plan row names stay. Each unit is kept before it goes, under the id the map
prints, in the session's spill store; the new base carries one line per unit under the record
(`evict::map_text`, written by `Harness::fork_onto`), and `recall id=<id>` returns that body
exactly, bounded to an eighth of the window (`crates/tools/src/recall.rs`, seats on every role that
can be compacted). An id the map never named is refused by name, listing the ids it does have.
Tested end to end in `crates/harnessd/src/harness/tests/recall.rs`. §10 of the plan
(`docs/implementation-plan.md:215-224`) now reads *structural eviction*. OPEN: a standalone `prune`
(`docs/compaction.md` §6, its own `/prune` verb, no model behind it).

---

## R105 - The work order after the 2026-11-01 review, folded with the open list

**Status:** steps 1, 2, 3, 5 and 7 are closed (evidence per step below), 4 is built and owes a live run, 6 is half done, 8 stops on two decisions, 9 is the backlog. left: step 4's four live facts on lab2x1 (R100), step 6's comment rule (486 blocks over three lines by `scripts/comment-blocks.py count`; the list below says 456, written earlier), step 8's two decisions (where a JavaScript engine comes from, and the JS-side contract), step 9 (the entries in this file).

Record: `docs/review/2026-11-01-ab5f7841-90dd9c84.md` (three reviewers, two rounds, synthesis). In order; each step relies on the one before it.

1. Guards. Remove the 15 `#![allow(clippy::all)]` crate by crate, each replaced by targeted allows with a reason. The gate's clippy step must not pass on a warm target. A check that every `#[test]` sits on its own function (or `--list` counts per binary). A test that cannot run on the gate host fails there instead of printing SKIPPED (`crates/bridge/tests/daemon_record.rs:84-89`).
2. Orchestration bugs that cost runs: R106, R107, R108; re-check R99's own Done-when now that R101 is closed.
3. Contracts from the review: version the sessions plugin answer so a layer written for groups is refused out loud; record reviewers-on-host in DECISIONS.md and restore a test of the VM arm (`mergequeue/review.rs:700`); the single-attempt retry test (`crates/http/src/retry.rs`); the room protocol bumps only at release points.
4. R100, firecode for remote sessions.
5. De-duplication: one queue renderer (`crates/tui/src/ui/panes/queue.rs` or `plugins/builtin/queue`), one `now_ms`, one truncation helper, `crates/harnessd/tests/common`.
6. Deslop per `hands-off/templates/deslop.md`: comments to the 2-3 line rule, `.letibot/notes` merged into rewritten topic docs.
7. Structure: split `open_with_registry`, `rounds`, `run_to_completion`; break `harness.rs` into modules.
8. Plugins: script layers (`docs/plugins-plan.md` step 11).
9. Backlog, unchanged: R95, R98 half 1, R104, R89-R93, R64-R66 and the older open entries below.

**Status 2026-11-03, each step re-checked against main.** 1, 2, 3, 5 and 7 are closed. 4 is built and owes a live run on lab2x1. 6 is half: the docs and the reviewed range's code are deslopped, the tree-wide comment rule is not. 8 is not built and stops on a decision. 9 is the backlog further down.

1. **Closed.** No crate root carries `#![allow(clippy::all)]` any more; they went crate by crate, with the items annotated that the allow had hidden (d1a22002 provider, 5f799705 turn, 76b1d3b9 sessionlog, 2c604e95 tools, a91283f7 tui, 77634a08 flowy, 90c249a9 opencode, e6f1239e dialect-glm). The gate's clippy step is `scripts/clippy-gate.sh`, which touches every member's root so a warm target cannot answer for the lints (3feb2606, and agents.md says why). `scripts/check-test-attrs.sh` is the gate's second step, and rejects a `#[test]` that is not on a function of its own (d0e344cb). The daemon-record test refuses on a box without python3 instead of printing SKIPPED (6daadf89).
2. **Closed, except R99's Done-when, which is a live run.** R106 landed as `ecae1af7` (`a_session_that_joins_a_daemon_runs_its_own_model`), R107 as `f1e7f9ec` (with `crates/bridge/tests/wait_activity.rs` holding a wait on a running job and answering through it when the daemon says quiet), and R108 is closed in TODO-settled.md (`8d1b0f2a`). R99's code is `ccf7b895`, `559048a7` and `36752792`; its Done-when, six packages placed on lab2x1 at once and a forced mid-write drop, needs a real host and a real sshd, so nothing in a sandbox can say it.
3. **Closed.** The sessions answer is versioned: `MOVED` (`crates/plugins/src/lib.rs:98`) and `[contracts] pane_sessions = 3` refuse a layer written for the groups at load, naming the layer and both numbers (`b5a2925a`, `a_layer_written_for_the_old_shape_is_refused_and_the_layer_below_answers`). D18 records reviewers-on-host, and the VM arm's test is back (`fcc559aa`, `a_project_that_asks_for_vms_gets_its_reviewer_placed_in_one`). The single-attempt retry test is `one_attempt_means_no_retry_and_the_default_still_retries` (`59c9c90d`). The room's number moves at release points, said above the constant (`609521f9`).
4. **Built; the shape has tests now, the live run still owes four facts.** `Placement::RemoteVm` is built, and the two arms the review found asserted nowhere have tests: the checkout a VM placement makes is a clone and a host placement's is a linked worktree (`7a964d46`), and the mode a placement puts a session in is `mode_in_vm`, with `RemoteVm` asserted at allow-all (`16e91852`). Still owed are the lab2x1 run's four facts: item 2 with a real `firecode` copy carrying a commit out, item 4 with a resumed session, the guest's `cargo test` with no card, and the copy-out's cost off the daemon's own line.
5. **Closed.** One queue renderer (`e147d979`: the pane is the `queue` layer's answer and the head hosts it), one clock (`682e5a69`, fifteen copies into `letibot-metrics`; the `i64` sites keep a named two-line adapter, and the copy that arrived after it went in `397b3ef2`), one truncation helper per meaning in `letibot_transcript::text` (`2b356d3a`; two aliases went in `397b3ef2`), and the tests' shared support is `crates/harnessd/tests/support` (`a43d2748`), not `tests/common`.
6. **Half.** The docs were deslopped one by one, and the notes were merged into them and cut: `.letibot/notes` holds 160 files, with the ones whose content moved deleted in the same commit. The comment rule is not tree-wide: `python3 scripts/comment-blocks.py count $(git ls-files 'crates/**/*.rs')` still finds 456 blocks over three lines.
7. **Closed.** `harness.rs` is 1,894 lines and its parts are `crates/harnessd/src/harness/` (`open.rs` e4e00ff0, `turns.rs` 967c5718, `children.rs` 0d2aca9c, `models.rs` 42931d7c, `wall.rs` c2a9c60d and the rest), and the three long functions are named steps (`rounds` 7aa68541, 60a3af8a, 8767a6f6, 8e3b3a9a; `run_to_completion` c86e6f44; `open_with_registry` 43f26515, 2b0701a2, 3ce469cf, 592241c4).
8. **Not built, and it stops on two decisions this entry cannot take** (`docs/plugins-plan.md` step 11): where a JavaScript engine comes from, with none in `Cargo.lock` and none in the offline registry cache, and the JS-side contract, including how the shadow gate's replay stays exact with a clock and `Math.random` inside it.
9. **Unchanged**: the backlog entries further down this file are their own work.

**Done when**: steps 1-8 are closed or moved to their own entries. **Still open?** step 4's live run, step 6's comment rule, step 8's two decisions, and step 9's backlog.

---

## R114 - Embeddings as the second similarity tier, in `letibot_tools::similarity` for every caller

**Status:** items 1 and 2 are built - `crates/embeddings` (modes off, internal, URL) blended in `similarity::blended` (`crates/tools/src/similarity.rs:297`) and reaching the notes and skills offers, the duplicate gate, the context claim, the waits offer and the already-done offer. left: item 3's measurement from the take-up logs (are the misses synonyms?), the paraphrase case scored against the worst measured negative, and the per-round cost on lab2x1.

`crates/tools/src/similarity.rs` is lexical, and its own measurement shows where that ends: a paraphrase of the review-queue note scored 0.126, under a docker prompt's 0.133 against an unrelated note, so no floor separates them. The skill offer (R113) will meet the same gap on synonyms (presentation and beamer).

1. Engine. Decided 2026-11-02 (operator): candle is the runtime, in pure Rust, with no llama. The built-in model is a fast small one (all-MiniLM-L6-v2, or multilingual-e5-small if English-only proves too narrow), downloaded once into the data dir and never compiled into the binary. A user who wants quality gives a model file: a local directory with `config.json`, `tokenizer.json` and `model.safetensors` of an architecture candle runs (BERT family, nomic, Qwen3-Embedding, Gemma3 to verify), e.g. Qwen3-Embedding-0.6B or nomic-embed-text v1.5. An architecture the build does not know is refused at open by name, not at the first round.
   Configuration, one key with three values: `[embeddings] mode = "off"` (lexical only, the default until point 3 says otherwise), `"internal"` (candle with the built-in model, or `model = "<dir>"` for the user's own), or a URL to an OpenAI-compatible `/v1/embeddings` endpoint with its model name. A URL sends note and skill text to that endpoint, so it is only ever the operator's explicit setting, and its failures fall back to lexical and are said once per session, not per round.
   Scope: the blended score lives in `letibot_tools::similarity`, so every caller gets it at once: the notes offer, the duplicate gate on a note write and on an edit made durable (a paraphrased duplicate is the case lexical misses), the context-claim check, and the skill offer (R113). `mode = "off"` turns it off for all of them. Each caller keeps its own floor, tuned from its own log: offering a note and refusing a duplicate want different thresholds.
2. Shape. Each note and skill is embedded once when it changes, cached on disk beside the corpus; per round only the round's text is embedded. Cosine over the whole corpus, no vector store, since the corpus is hundreds of documents. Storage is a cache, not a record: `<data dir>/embeddings.db` (SQLite, its own file so `sessions.db` gets no schema bump), one table keyed by `(model, sha256 of the embedded text)` holding the dimension, the vector as an f32 blob and a last-used time. A content key needs no invalidation: an edited note is a new key, a changed `mode` or model is a new key, and rows unused for 30 days are dropped at open. Deleting the file loses nothing but time. At open the corpus's vectors are loaded into memory; a few thousand 256-1024 dim vectors are a few MB. The score is combined with the lexical one, not a replacement for it, and one floor is tuned from the take-up log.
3. Data first. R113 logs every skill offer taken and missed, as the notes offer already does. The test set is the missed cases from both logs plus the paraphrase above. If the misses are not mostly synonyms, this entry closes as not needed.

**Done when**: the paraphrase case scores above the worst negative in the measured set, the offer's per-round cost on lab2x1 CPU is stated, and the harness builds with the embedder and without llama.
**Still open?** whether R113's log shows synonym misses.

---


More callers, from the operator on 2026-11-03:

- Waits. When a brief, task or subagent prompt is written, score it against the open todo rows of the session and its relatives. A close match is offered as "this looks like todo X, owned by Y - consider `needs`/wait_on it instead of starting it".
- Already done. Score a new brief against recent commit subjects and TODO-settled.md. A close match is offered as "looks like <sha> already did this - check before starting". On 2026-11-02 four of fifteen packages found their whole scope already fixed on main.
- Compaction relevance. Rank old turns by their score against the current todos and plan, keep the relevant ones in full and summarise the rest, instead of cutting by age. This feeds R22.

---

## R118 - A GitHub reviewer seat: `letibot attach github <org|repo>` reviews every PR and answers as @letibot

**Status:** open, filed 2026-11-03, the operator's ask. The worker is in and drafts are drawn (`docs/github-reviewer.md`). left: the seat in the daemon so the pane has a feed, a mirror per repository, the review panel, R116's verdict, and R114's similarity offers.

The operator wants letibot to work like CodeRabbit on their own repositories, starting with stroppy-io: "it just attaches to whatever pr is on the repo given to it and does reviews ... responding as @letibot".

**Done** (docs/github-reviewer.md): the poll, the row states, the config and its `post` gate, the 6-an-hour cap, and the worker - a review is a brief (title, description, diff, the comments so far) run in that PR's own letibot session, whose answer is the review kept as a draft on the row; a mention is answered in the same session; the pane draws each draft's words. The session stands in a detached worktree of the PR head, fetched as `refs/pull/<n>/head`.

**Left**: the seat lives in `letibot attach github`'s process, not the daemon's, so the daemon builds no `github` feed and `/pane github/github` still draws "no scope is attached"; a PR of another repository in an org scope is refused by name until a mirror per repository exists; the review is one session's answer and not yet R116's review panel; and R114's similarity offers are not wired.

- `letibot attach github <org or owner/repo>` starts a long-lived reviewer seat for that scope. It polls with `gh api` for new PRs and new pushes to open PRs. Polling is used rather than webhooks, so no endpoint is exposed.
- Each PR is a letibot session with its own worktree of the PR head. A new PR gets a review panel (reviewers plus the gatekeeper, the same machinery as the merge queue's review) and one GitHub review: inline comments on lines and a summary. A push gets a re-review of the new commits only.
- A comment that mentions `@letibot` in the PR's thread is answered by that PR's session. The operator can attach to the session like any other.
- Identity: the seat posts as its own GitHub account or App named `letibot`, with a token scoped to the configured repositories. It never posts as the operator. The operator creates the account or App; the token lives in letibot's config, mode 0600.
- Limits: one review per push, a per-repo rate cap, and no review of a PR from a fork without the operator's opt-in (an untrusted diff runs nothing; review is read-only).
- R114 callers: a new PR or issue similar to an open todo or a running session's brief is offered as "watch this"; a session whose brief matches an open PR attaches to its thread; similar past reviews are offered as context.

Depends on R116 (a review verdict schema shared by every reviewer) and R114 for the similarity offers.

A pane, `/pane github`, lists the attachers: one line per scope (org or repo) with its state (polling, rate-limited, token missing), and under it one row per open PR with its review state (queued, reviewing, posted, re-review pending, answering a mention), the last action and its age. Enter on a PR row attaches to that PR's session; a key opens the PR in the browser; another pauses or resumes a scope. It is a plugin pane like the queue's, so the seat's state lives in the daemon and the pane only draws it.

---

## R116 - Claude Code as a merge-queue reviewer, through headless `claude -p`

**Status:** not started. left: all of it - no `claude -p`, `--json-schema` or `structured_output` use exists under `crates/` (grep, 2026-11-03). The verdict schema is the queue's existing reviewer verdict.

The queue hands a branch to a reviewer and needs a verdict back. Headless Claude Code fits without waking any live session:
`claude -p --output-format json --json-schema <verdict schema> --permission-mode <mode> --allowedTools <read-only list>`, started with its cwd in a worktree of the branch.

- The verdict arrives in `structured_output`. A run without that field is a failed review, not a pass.
- Store the result's `session_id`, so a re-review after new commits uses `--resume <id>` and keeps the reviewer's context.
- Time limit enforced by the queue. End a run with SIGINT, which ends the turn cleanly; SIGTERM exits 143.
- The verdict schema is the queue's existing reviewer verdict, so a Claude reviewer and a letibot reviewer are interchangeable in a panel.

Docs: https://code.claude.com/docs/en/headless.md

---

## R117 - The leticode MCP bridge pushes package and queue events into the orchestrating Claude Code session as a channel

**Status:** not started. left: all of it - the bridge declares no `claude/channel` capability and sends no `notifications/claude` (grep under `crates/`, 2026-11-03). Claude Code's side is a research preview that needs `--dangerously-load-development-channels`.

Today an orchestrating Claude Code session learns that a package finished through a tracked background `leticode-wait` per session. A channel replaces that: the bridge is already an MCP server, so it can declare `capabilities.experimental["claude/channel"]` and send `notifications/claude/channel` with `{content, meta}` when a session ends its turn, asks, fails, or a queue row changes state. An idle Claude Code session starts a turn on it.

- Claude Code sends no acknowledgement. Expose a reply tool through which the orchestrator acknowledges each event, and re-send an unacknowledged event with backoff.
- Permission asks can be relayed through `experimental["claude/channel/permission"]`.
- Claude Code must be started with `--dangerously-load-development-channels server:leticode`. It works in interactive sessions only, and custom channels are not on the allowlist.
- Keep `letibot-mcp wait` as the fallback for sessions started without the flag.

Docs: https://code.claude.com/docs/en/channels-reference.md

---

## T16 - W9's open questions

**Status:** items 1 to 6 are fixed (66077505, d81863e8, 229555bf, c44d41e7; item 3 has no test). left: item 7 - call ids are positional per turn (`call_{n}` at `crates/turn/src/items.rs:175`, `call_{i}` at `crates/provider/src/openai.rs:681`), so an id repeats across turns; unclear: item 6 asked lubuntu3 whether RAGFlow signals "no coverage" in a field, no answer is recorded.

From the tool runtime, in descending order of consequence.

1. **§8.1 clause 3 gives the outcome vocabulary but never the mapping.** Which outcome
   is an empty `grep`? W9 decided: matches in scope or elsewhere ⇒ `Ok`; nothing
   anywhere under any relaxation ⇒ `Abstained`; a nonexistent path ⇒ `Failed`. That
   choice determines how often `propagate()` blocks a caller and should be recorded
   rather than inherited.
   — ✓ fixed in 66077505, test `a_term_that_is_nowhere_abstains_and_says_what_it_searched`;
   the nonexistent-path case is recorded as the listing rather than `Failed`
   (`a_scope_that_does_not_exist_returns_the_listing`, `crates/tools/src/builtins/grep.rs:915`).
2. **D6 answered means M1 ships with clause 5 switched off.** `NoBudget` is correct per
   D6 — unset is a genuine no-op — so **nothing spills until a budget is configured**.
   A session-config gap, not a code one, and invisible unless said.
   — ✓ fixed in d81863e8, test `every_thing_that_is_off_is_disclosed`: `Config::disclosures`
   says `UNSET` and that nothing will spill (`crates/harnessd/src/config.rs:2139`).
3. **`Gate` will collide with W11.** W9 defined the smallest adjudication seam the
   runtime needs (`admit(name, access, args)`). W11 should absorb it rather than build
   a parallel one.
   — fixed in 229555bf in code, untested: there is one `Gate` trait
   (`crates/tools/src/runtime.rs:621`) and its only implementation is `AdjudicatedGate`
   (`crates/tools/src/adjudicate.rs`), so no parallel seam exists to collide with.
4. **§8.1 clause 1 and §9.4 pull against each other.** Relaxing a pattern *is*
   rewriting the query, which §9.4 forbids. The reconciling word is **visible**: every
   relaxation appears in the result. Worth stating in §9.4 so nobody deletes one of
   the two.
   — ✓ fixed in 66077505, test `a_too_strict_anchor_is_relaxed_to_the_bare_identifier_and_says_so`;
   §9.4 now carries the rule ("and if it ever does, the rewrite must be visible",
   `docs/implementation-plan.md:213`, 7bdfd6c9).
5. **§8.4's `orchestrator` role has no `read_spill`** though its `read`/`grep` can
   spill — so for that role the omission notice is advisory, which §8.3 says it must
   not be. W9 shipped `read_spill` in its place.
   — ✓ fixed in 66077505, test `the_default_session_is_read_only_and_seats_exactly_what_it_did`
   (the orchestrator's seated set includes `read_spill`, `crates/harnessd/tests/wired.rs:112`).
6. **Retrieval is inert — no MCP server is running anywhere.** Confirmed by lubuntu3
   on 2026-09-09, checked rather than recalled.
   - `192.168.1.55:9755` is the **flowy node**, not oracle. Never was the address.
   - **RAGFlow and oracle are on lubuntu3 (192.168.1.82)** — but its MCP server **is
     not started by that deployment**. Docker publishes 9382-9384, so a **TCP connect
     succeeds while nothing is behind it** (`curl` → HTTP 000, zero mcp processes in
     the container). That is why a client reports "unable to connect to the url": the
     connection is fine, the service is absent. Same shape as the embed wedge — a
     check that succeeds without touching the thing it checks.
   - `:8100/sse` **looks** like an SSE endpoint and is not — it is arxiv-search, whose
     catch-all returns identical HTML for every path. Do not use it.
   - **Use the REST API instead: `192.168.1.82:9380`, base `/api/v1`, bearer auth.**
     Do not ask for an MCP listener to be started while ingestion is live.
   - `ask_code` is a **separate problem**: per-box, against the local codebase, and
     **nothing is running on this box** (checked — only qwen/bge/postgres listen).
   - **Open and load-bearing:** does RAGFlow signal "the corpus does not cover this"
     in a *field*, or only in prose? §8.1 clause 3 requires abstention to be
     structurally distinguishable. If it is a field, it maps to `Abstained` and the
     harness can never report it as grounded. **If it is only prose, we will not parse
     the prose and pretend** — the tool returns `Ok` with the text and the hole stays
     documented. Asked; awaiting an answer.
   - Note from lubuntu3: `.82:11434` enforces **one request per backend and will hold
     a call rather than refuse it**. Do not point volume at it unannounced.
   — ✓ fixed in c44d41e7, test `a_session_with_nothing_attached_discloses_only_retrieval`:
   retrieval is disclosed as `INERT` under its own name (T16.6) and an answer with no
   citations comes back `NO_RESULT` (`crates/tools/src/builtins/external/mod.rs:173-200`).
7. **Call ids are positional per turn** (`call_0`; GLM carries none), so an id-derived
   mark repeats across turns. Fine now, wrong once a head correlates across a session.
   — OPEN: still positional — `crates/turn/src/items.rs:175` mints `call_{n}` per round and
   `crates/provider/src/openai.rs:624` mints `call_{i}`, so an id still repeats across turns.

---

## T10 - Contract gaps found by W6

**Status:** items 1, 2, 3 and 5 are fixed (TODO-settled.md R1 to R4; 3 and 5 have no test naming them). left: item 4 - `render_incremental` still takes the whole history (`crates/turn/src/renderer.rs:24`) and there is no resumable state token, so one ledger row per item is O(n squared) replays. T1 may delete it.

The turn engine is the first real consumer of the crates below, and it found five
things. Listed in the order I would fix them.

1. **`TranscriptItem::Assistant` has no `truncated` field**, and both §5.7 and §5.8
   require one. Currently tracked on the turn record instead, which is why **one
   piece of steering is unbuilt**. Needs a `transcript` crate change — the only item
   here that blocks a feature rather than costing elegance.
   — ✓ fixed in e5a11374, test `truncated_survives_a_round_trip` (and
   `a_row_written_before_the_field_existed_reads_as_not_truncated`): `Assistant { truncated }`
   is on the row, `crates/transcript/src/lib.rs:102`.
2. **`ParsedSpan` carries no token offsets**, so a `Parser` cannot say which ids an
   item owns. Cost an entire module (`crates/turn/src/items.rs`) to work around
   without reimplementing the parser. Adding a span to `ParsedSpan` would delete it.
   — ✓ fixed in 5a01de3b, test `parse_round_trips_reasoning_and_two_tool_calls`: every variant
   carries its `Range<usize>` of ids (`crates/dialect/src/lib.rs:177`) and the test asserts the
   spans tile the stream from zero (`crates/dialect-glm/tests/invariants.rs:279-289`).
3. **`DialectSpec` has no `ReasoningField`** — a per-model fact of exactly the kind
   that crate exists to model as data. The engine takes it as config rather than
   guessing, which works but puts a model fact in the wrong place.
   — fixed in 5a01de3b in code, untested: `DialectSpec.reasoning_field` is data
   (`crates/dialect/src/lib.rs:266`) and the engine reads it there
   (`crates/turn/src/engine.rs:665`), but no test asserts the spec's field rather than the
   transcript's copy.
4. **`render_incremental(history, new)` replays the whole history** for boundary
   state, so building one ledger row per item is O(n²) replays. Microseconds today
   at our sizes; a resumable state token fixes it. Note T1 may delete this entirely
   when the template-driven renderer lands.
   — OPEN: still O(n²) by design — `render_incremental` takes the whole history
   (`crates/turn/src/renderer.rs:24-30`, whose comment says boundary state is not derivable
   from a `usize`) and there is no resumable state token; `append_items` renders one item at a
   time (`crates/turn/src/engine.rs:434`).
5. **`cargo:rustc-link-arg` does not propagate across crates**, so every crate that
   links `libllama` needs its own `build.rs` to bake the rpath. Without it, test
   binaries link fine and fail at exec looking like a missing library. Fixed in
   `crates/turn`; worth a note so the third crate does not rediscover it.
   — fixed in e51847ed in code, untested: that commit was the last crate to get its own
   `build.rs`, and every crate that links `libllama` now bakes the rpath through the shared
   `crates/llama/build-support.rs` (`crates/bridge/build.rs`, `crates/sessionlog/build.rs`,
   `crates/turn/build.rs`, `crates/harnessd/build.rs`, `crates/letibot/build.rs`), each header
   saying why. A build script is not testable.

---

# Needs the operator

A decision, an account, a live run on real hosts, or an install. Do not start these from an agent seat.

---

## R113 - Skills are offered like notes, the index shows only pins, and a live check proves the whole path

**Status:** items 1 to 3 landed (the offer in the request tail over the whole frontmatter, the index listing only pins, `[skills] pin` read from both levels; `SkillRegistry::ranked` at `crates/tools/src/builtins/skill.rs:572`). left: item 4's live check on lab2x1 - `/skills` hands a test skill over, `task {skills}` preloads it in a child, an unknown name is refused - each already pinned by an in-tree test.

The prompt's skill index (`crates/tools/src/builtins/skill.rs`, `SkillRegistry::index`) sorts by source priority then name and stops at 60 rows, 4000 bytes or 800 tokens. Nothing ranks by relevance, so a box with many installed skills fills the slots alphabetically, and nothing offers a skill the way `crates/harnessd/src/notes_offer.rs` offers a note. The operator (2026-11-02): the offer is wanted, listing skills in full is not, pins are wanted.

1. Offer. Per round, score the skill corpus (`SkillRegistry::ranked`) against the latest prompt, the plan, the todo board and the model's latest text and reasoning (the notes offer reads only the prompt and tool results), and name at most two above `QUERY_FLOOR` in the request tail (`TurnRequest::tail`), the same delivery as the notes offer: never a row, never a load, dropped when the model does not take it.
   The score is lexical (`letibot_tools::similarity`), so a skill's document is its whole frontmatter (`description`, `when_to_use`, keywords), not only the name and one line: "latex presentation" must reach a skill described as beamer slides. Embeddings are a later step, not this entry.
2. Index. The prompt lists only pinned skills plus a count of the rest and the `skill` verbs. No source gets listed in full.
3. Pins. `[skills] pin = ["name", ...]` in the project file and the operator's config. A pin that names no skill is said at open, by name.
4. Live check, before 1-3 land and again after: a session on lab2x1 with a test skill in the worktree's `.letibot/skills`, `/skills` lists and hands it over, `task {skills: [...]}` preloads it in a child, and an unknown name is refused. The R112 change moved the skill roots onto `Config`, so the check covers that too.

**Done when**: a session whose prompt is about a skill's subject gets that skill offered in the tail without the model searching, an unpinned skill is not in the index, and the live check passes on main.
**Still open?** this entry's list.

---

## R100 - A remote session cannot run in a firecode VM: `--host` and `--vm` are two placements, not one

**Status:** built and on main (`ad447d53`, `46949113`, `e4c69ac2`; `Placement::RemoteVm`). left: the live proof the 2026-11-02 lab2x1 run did not reach - a real `firecode` copy of a clone carrying a commit out, a resumed session keeping its clone, the guest's `cargo test` with no card, and the copy-out's cost off the daemon's own line.

The operator: *"how to run those remote letibots in firecode"*, and *"lab2x1 has firecode, it even
has source code for it"*. It does: `~/bin/firecode` (login `PATH`), the source in
`~/Projects/firecode`, and `/dev/kvm` with an ACL for the operator's user (checked 2026-10-28;
`firecode ls` answers). What is missing is on letibot's side: `Placement` is either `Firecode`
(local, this Mac) or a remote host, and `cli.rs` says a remote session is not in a VM.

**Build**: a combined placement, `leticode --host lab2x1 --vm` — `firecode up`/`in` driven over the
session's existing ssh link (login shell, so `~/bin` is on `PATH`), the session's tools inside that
guest, the VM measured as the session's boundary so D16 admits builds and tests in it, and the
commits coming back the way remote placements' do. Then `leticode_start {host, vm: true}` in the
bridge. This is also where Docker for sessions belongs (the operator, 2026-10-27: *"firecode suites
better here"*): lab2x1's `dockerd` is rootful, so the socket is root on the host, and a container
inside a guest stays inside a boundary.

**Done when**: a package started with `{host: "lab2x1", vm: true}` builds and tests letibot inside a
guest on lab2x1 at `writes-allowed` without a card, and its commits come back.

**Landed (2026-10-31): the SILENT DROP is fixed; the combination is refused by name.**
`--where` and `--host` wrote ONE field (`cfg.placement`), so the flag that came LAST won and the
other ask was dropped without a word: `--host lab2x1 --where firecode` booted a VM **here** on a
copy of this box's workspace, and `--where firecode --host lab2x1` ran on lab2x1 with no VM at all.
Neither refused anything, so both looked healthy. The two asks are now recorded separately and
resolved once after the whole line is read, by `cli::placement_from_flags`, which refuses the pair
with the reason and says what each flag alone does; `--help` says the same. **What the pair SHOULD
mean is a VM on `lab2x1`, whose copy source is the remote checkout** — and that is still not built,
because `FirecodeBackend` runs a `firecode` binary on THIS box and holds THIS box's paths
(`FirecodeSpec::bin`, `~/.cache/letibot/firecode`, a promoted job's scratch file, the `.git` it
reads to decide a worktree is mounted). It needs a remote runner for the firecode backend, not a
flag. `docs/subagents.md` §2 carries the decision. Tests:
`crates/harnessd/tests/cli_placement.rs` (both flag orders, through `cli::run`) and the pair test in
`cli::tests`, which now asserts the pair IS the placement (the build below). — ✓ fixed in ad447d53, test a_vm_and_a_host_are_one_placement_whichever_flag_came_first

**Built (2026-11-01, branch `firecode-remote`): the pair is the placement.** `leticode --host lab2x1
--vm`, `leticode_start { host: "lab2x1", vm: true }` and `where: "host:lab2x1:vm"` are one
`Placement::RemoteVm(name)`; either flag alone still means what it did. `SshSpec::vm` carries the
ask into `SshBackend::place`, and five things follow from it:
— ✓ fixed in 46949113, test a_vm_and_a_host_are_two_asks_and_the_pair_is_a_vm_there

1. **The host is asked once per host per process** (`Ssh::probe_firecode`): `command -v firecode` in
a login shell, and `$LETIBOT_KVM_DEVICE`/`/dev/kvm` readable and writable by this connection's user.
A host that cannot boot a guest is refused **by name before anything is arranged there** — no
bundle, no checkout — and a probe that did not answer is not cached. — ✓ fixed in 46949113, test a_host_without_firecode_is_refused_by_name
2. **The checkout is a CLONE when the ask is a VM**, not the linked worktree every other placement
gets. The guest holds a *copy* of the directory, and a worktree's `.git` is a file naming a
directory in the mirror that the copy does not carry — so no commit inside the guest would have
worked. `bring_back` fetches the branch from the checkout in that case, from the mirror otherwise. — fixed in 46949113 in code, untested: nothing asserts the VM checkout is a clone rather than a linked worktree, so a revert to `git worktree add` would leave a guest copy with no repository and only a comment to notice
3. `firecode up --project <checkout>`, and then **every call is `firecode in --project <checkout>
--cwd <checkout> '<script>'`** — `Ssh::remote_command` wraps every door at once, so a command, a
file read, a file write, a job's launcher, a poll and the group kill all run in the guest, and the
cancel path builds its own command for the same reason. — ✓ fixed in 46949113, test a_vm_placement_boots_the_guest_and_runs_every_call_in_it
4. **The guest is the boundary**, so `cfg.placement.is_vm()` puts the session at `allow-all`
(`Harness::open`'s `in_vm`) and no `bwrap` is probed for. — fixed in 46949113 in code, untested: no test opens a session at `Placement::RemoteVm`, so the `allow-all` arm (`crates/harnessd/src/harness/open.rs:594`) is asserted nowhere
5. **On close: `firecode down`, then the guest's work is taken onto the host checkout** — `down`
leaves a sibling `<checkout>-<stamp>` when something changed, that copy's branch is fetched into
the checkout and fast-forwarded, and the branch then comes home as `remote/<host>/<short id>` as
every host placement's does. — ✓ fixed in 46949113, test a_commit_made_in_the_guest_comes_back_as_the_branch

**Tests** (`crates/tools/tests/remote_vm.rs`, a fake `firecode` on the fake remote's `PATH` through
the same fake ssh `remote_hosts.rs` uses): `up`, then `in` for the session's calls, then `down`, in
that order; a commit made in the guest arrives as the branch that came back; a host with no
`firecode` is refused by name with nothing arranged there; and a host placement never asks for a
guest. Plus `cli::tests::a_vm_and_a_host_are_two_asks_and_the_pair_is_a_vm_there`,
`bridge::tools::tests::the_vm_flag_turns_a_host_into_a_vm_on_that_host` and
`bridge::daemon::tests::a_vm_on_a_host_is_both_flags`.

**The live run on `lab2x1` (2026-11-02), and the two defects it found.** `leticode --host lab2x1
--vm` **booted a guest** and a **foreground `bash` ran inside it**, so half of item 1 is answered for
a command that travels in the command text; a manual `firecode down` there took **15 s** and
delivered the copy to `<project>-<run id>`. It did not reach item 2, and item 1's `cargo test` (no
card) and item 4 were not tried, because:

1. **A background job never started.** Its launcher is staged by `Ssh::write_abs`, whose bytes travel
   on the ssh's **stdin** (`base64 -d > "$t"`). `firecode in` sends the command over the vsock and
   never the caller's stdin, so the guest waited for an end that cannot come: ten calls sat for 52
   minutes and the jobs directory was never made. Fixed: a step fed on stdin now takes the guest's
   own road, the payload in the command when it is small (`VM_INLINE_WRITE`) and staged on the host,
   where stdin works, then carried in by `firecode cp` above that. The audit of every stdin-fed call
   in `remote.rs` and `firecode.rs` found this one and no other: `Ssh::write_abs` is the only one,
   `firecode.rs` feeds no stdin at all (its writes are inline base64 or `firecode cp`), and the two
   other `run_bounded_fed` sites (`mergequeue/gate_host.rs`, `room/supervise.rs`) feed a declared
   host's ssh and never a guest. — ✓ fixed in e4c69ac2, test a_background_job_starts_in_a_guest_whose_client_forwards_no_stdin
2. **A one-shot stopped with SIGINT left the VM up**, with the calls still stuck in it. The forced
   exits (`stop_on_signal`'s second signal and its 5 s give-up) end the process without closing a
   backend, and the orderly stop closes no backend at all. Fixed: a guest this process booted is on
   one list (`remote::guests_down`), bounded at 30 s for `down` and 10 s for the copy-out, and every
   exit path runs it once and prints one line saying whether the VM came down and where the copy
   landed. A session's own close takes its guest off that list, so no guest is brought down twice. — ✓ fixed in e4c69ac2, test a_guest_nothing_closed_is_brought_down_once_by_the_last_act

**Still owed, for the next live run**: item 2 (whether a real `firecode` copy of a clone carries a
commit out), item 4 (a resumed session keeps its clone, and a file one command writes is there for
the next command), the `cargo test` inside the guest with no card, and the copy-out's real cost read
off the daemon's own line rather than a manual `down`. — OPEN: no live proof — the 2026-11-02 run saw a guest boot and one foreground `bash` inside it and nothing more, so the guest `cargo test` with no card, item 2, item 4 and the copy-out's cost are still untried on lab2x1.

**Done when**: a package started with `{host: "lab2x1", vm: true}` builds and tests letibot inside a
guest on lab2x1 at `writes-allowed` without a card, and its commits come back.

---

## R59 - The context wall's denominator moves a third between firings

**Status:** the wall line carries the session id (`bd012ed`) and prints the provider's count with its unit (`e8343165`). left: the operator's ruling on smoothing the ratio, not given. The tree keeps it per round, as recommended.

The operator, on letibot's compaction: *"something is off here, like too much too early too wtf."* Four
`context wall` lines on one daemon whose `context_window` is 999,999 — **and no way to say which session
each was about**, because the wall line carried no id, which is why that is the first thing fixed below.

    stopped after 13 round(s) at 1400086 of 1118167 tokens
    stopped after  7 round(s) at 1463534 of 1484966 tokens
    stopped after  2 round(s) at 1257759 of 1199811 tokens
    stopped after 16 round(s) at 1429974 of 1484829 tokens

**The denominator is `planning_window()`, and it is a measurement rather than a bug.** `Config::ledger_scale`
is `(ledger.len(), the last round's prompt_tokens)`, refreshed at the end of **every** round
(`harness.rs:5848`), and `planning_window` is `context_window * ledger / provider` clamped to
`[w/4, 4w]`. The four windows above are 1.12x–1.48x the configured 1M, i.e. the ledger ran 12% to 48%
wider than the provider's own count of the same prompt — **how much of that session was reasoning the
messages provider is never sent**, which the docstring already says varies per session and over the life
of one. So the ratio is doing its job; what moves is the printed pair.

**And the threshold the wall FIRES on does not move at all — the swing is in the display only.**
Verified from the code rather than argued: the check is `resident + headroom() >=
planning_window()` where `resident = session.ledger.len()` at the top of the round loop and `ledger_scale`
was taken at the bottom of the previous one, so `resident == ledger`; `headroom()` is `w/16` at these
sizes; substituting gives `ledger >= 15/16 * w * ledger/provider`, the `ledger` cancels, and the
condition is exactly **`provider_prompt_tokens >= 15/16 * context_window`** — one fixed fraction of the
configured window, independent of the ratio. The wall fires in the provider's units; only the *ledger*
pair printed beside it swings. That is the same unit confusion the operator already caught once
(*"1.35 is a lie - that top was shown as 900+"*, `Config::shown_tokens`' docstring), now in a log line
instead of a notice.

**Which makes the two asks different sizes.**

  · **The measurement is theirs to rule on: smooth or not.** A smoothed ratio (an EMA) would hold the
    printed denominator still and cost lag — the window would track a genuinely changing reasoning
    fraction a turn or two late — and it would not move the fail point, because the fail point is
    already ratio-independent. This tree's own argument for the ratio is *"it is not a constant: it is
    how much of the conversation is reasoning"*, and smoothing is the one change that makes it less of
    the measurement it was written to be. Recommendation: **leave it per-round**; if the printed number
    is the problem, fix the printing.
    — OPEN: the operator's ruling, not code. The tree took the recommendation — the ratio is refreshed per
    round (`crates/harnessd/src/harness/turns.rs:1729`) and no smoothing exists anywhere — so the printed
    denominator still moves between firings and the choice is unmade rather than made.
  · **The display is not: print the provider's figure.** `{resident} of {window}` is in ledger tokens
    and every notice a head draws is in the provider's (`shown_tokens`). Printing the provider-side
    number (or both, as the wall *notice* already does with its `(counted as the provider counts them;
    …the ledger says X of Y)` aside) makes the log line read in the same unit as the screen it
    describes.
    — ✓ fixed in `e8343165`, test `the_wall_failure_says_the_providers_count_and_names_its_unit`

**The id that makes it decidable is landed** — `bd012ed` gives the `context wall` line the session name
its `compacting:` neighbour has had since this same morning, so the next occurrence pairs a wall with the
compaction beneath it **by id rather than by adjacency**. That is what answers *"too much too early"*:
whether the 1.25M-to-633k folds and the 985k-to-8k compactions in the operator's log are one session
behaving differently or two behaving consistently — and note the two are different paths with different
contracts (a fold and a compaction), which the operator's own caution says not to conflate.
— fixed in `bd012ed` in code, untested: the id is an `eprintln!`
(`crates/harnessd/src/harness/turns.rs:1396`) and no test reads that line.

**still open?** `grep -n "context wall:" crates/harnessd/src/harness.rs` finds the id;
`grep -n "ledger_scale =" crates/harnessd/src/harness.rs` finds the one per-round refresh at 5849 and the
recovery at 2833; no smoothing exists anywhere, so both routes above are unstarted.

**done when** the wall's log line and the compaction's can be paired by id (done, `bd012ed`), and either
the ratio is left per-round with the printing moved to the provider's unit, or a smoothed ratio is added
with the argument for the lag written down — so that the denominator on the screen and in the log is the
number the operator can check, and the analysis *"too much too early"* rests on attributed lines rather
than adjacency.
— the id half is done (`bd012ed`, untested) and the printing half is done (`e8343165`, tested); the ratio
half is the operator's ruling and has not been given, so this entry is not settled — and the `still open?`
paragraph above is stale on its second route, which is now landed.

### The same mismatch from the DISPLAY side — **added 2026-10-04**

The operator, on a compaction that fired while the header read half a million: *"it showed me
compaction when screen showed only 500k tokens."*

**The number on the screen and the number that decides are different units.** `Config::shown_tokens`
is `provider_tokens(ledger_tokens)` — what a request would actually carry — while `should_compact`
and the `compacting:` line are in LEDGER tokens. MEASURED on that session:

    log:     compacting: 1390742 of 1000000 tokens resident   -> compacted: 834514   (the fold)
    log:     compacting:  947896 of 1000000 tokens resident   -> compacted:  12594
    header:  947896 of 1000000                                 (agreeing, once the ratio was near 1)

The two agree now and did not then: a ~2.8x gap, inside the clamp of four. **The gap IS the reasoning
the messages transport never sends** — the ledger holds it, the provider's count does not.

**And `config.rs` already names the direction of the error**, in `tokens_are_converted`'s neighbour:
*"a ledger figure standing in for a provider's — up to the clamp's factor of four out, **in the
direction that compacts early**."* So *"too much too early"* is not a misreading by the operator; it
is the documented failure mode seen from the only place they can see it.

**This is the same root as the entry above, from the other end.** There the denominator moved between
firings in the log; here the numerator is printed to the operator in one unit and decided in another,
with nothing on the glass saying which is which. A reader cannot predict a compaction from the
display, which is the whole of the complaint.

**done when** one unit reaches the operator — either the decision is stated in the unit the header
shows, or the header names both and says which one compaction uses. `tokens_are_converted()` exists
precisely so a message *"can name the other number once rather than per figure"*; nothing is calling
it on the compaction path.
— ✓ fixed in `e8343165`, test `the_wall_failure_says_the_providers_count_and_names_its_unit` — the wall's
sentence and the banner now print the provider's count and its unit, with the ledger pair in parentheses, and
`tokens_are_converted()` is what chooses that aside (`crates/harnessd/src/harness/turns.rs:1363`).

---

## Which side owns WEATHER - a classification both heads must answer the same way

**Status:** settled on this side on 2026-11-02 - the daemon classifies (`warning::ALARM_ONLY`, `crates/sessionlog/src/warning.rs:596`) and the head files the sentence under `/notes`. left: the operator's ruling on `model_endpoint_retry`, which is a row here and weather in leticl; unclear: `6b52aae6` now draws a retried model call as one row that moves and settles, check whether that closes the ruling.

The operator ruled: *"both heads should not emit it inside conversation"* for `model_slow_first_byte` —
the diagnostic is wanted, its PLACEMENT was wrong, so it leaves the transcript, lights the `⚠` and
`/notes` still lists the sentence. leticl landed its half in `973d58f`, and the shape it took is worth
copying or overruling deliberately rather than by accident:

  · the test is **not** *is this routine* but **is this an event in the record**. `+weather-warnings+` is
    `model_slow_first_byte` and `model_endpoint_retry`; `auto_compact` and `compacted` are routine too
    and STAY ROWS, because a compaction changes the conversation; `prefix_check_skipped` stays a row
    because a skipped check is *said, never counted as a pass*;
  · it is a **subset of the routine register** (`warning.rs`'s `Class::Routine`), asserted by a test
    (`821d218`) so a code cannot be silenced here alone.

**The question: where does WEATHER live?** leticl's list is head-local and yours is the daemon's
register, so the two heads now hold two answers to one question — and a code added to one and not the
other is two heads disagreeing about whether a sentence is an event, which is the two-lists defect the
archive's file set was made un-driftable for in `018fc45`.

Two shapes, and the second needs a reason written down: **the daemon classifies weather beside
`Class::Routine`** (one answer, both heads read it, neither can drift — leticl's preference, because it
is what the register already does), or the lists stay head-local with the reason they must exist
separately stated in both trees.

**done when** a code added on one side cannot silently change a head's placement without the other
side seeing it — one list, or two with a stated reason and a guard that reads the other.

**SETTLED, 2026-11-02: the daemon classifies, and the head holds no list of its own.** The first shape,
and leticl's preference, is what this tree already had and what it keeps. `warning::ALARM_ONLY` sits
beside `Class::Routine` in `crates/sessionlog/src/warning.rs`, the head asks it through
`warning::to_the_alarm`, and `crates/tui` has no placement list to drift — so a code added here cannot
be placed one way by the head that saw it live and another way by the head that attached later. The
guard is the register's own: `an_alarm_only_code_is_never_a_failure_or_a_refusal` fails if a code on
the edge is also a `Failure` or a `Refused`, and it names the routine codes that must stay rows
(`compacted`, `auto_compact`, `compact_half`, `reseated`, `cleared`) — the same subset argument
`821d218` makes on leticl's side, read off the table rather than restated.

**And the defect this entry was chasing was not in the list — it was in the FILING.** MEASURED on the
tree: `crates/tui/src/app/events/warnings.rs`'s alarm arm returned `Disposition::Filtered` *before*
filing anything, while the snapshot arm (`app/events/mod.rs`, the `Placed::Before` push) files every
warning it replays. So one `model_slow_first_byte` had two records, decided by WHEN THE HEAD ATTACHED:
a head that came later listed the sentence under `/notes`, and a live head kept only the `/status`
count — the diagnostic the ruling is about, gone on the head that watched it happen.

Landed: the alarm arm files the daemon's sentence through a new door, `App::note_beside`
(`crates/tui/src/app/notes.rs`), and then counts it. `note_beside` files `Placed::Before` — listed by
`/notes`, counted by `/status`, drawn nowhere — through `App::note`'s own identity test, and the note
cap is one function now (`cap_notes`, over a new `NOTES_KEPT`) because filing has two doors and a cap
written into one of them is a list that grows through the other. `/status`'s third number reads
`N not drawn` rather than `N from before this window`, because a live edge-bound note is not from
before the window. `ALARM_ONLY`'s docstring says where the sentence goes, so the next code added there
is told. Test: `an_edge_bound_warning_is_listed_in_notes_rather_than_deleted`, beside the older
`an_edge_bound_warning_raises_the_alarm_and_never_becomes_a_row`, which still pins the half that
matters most — it is not a row in the conversation.

**STILL OPEN, and it is the operator's ruling rather than a change to make: `model_endpoint_retry`.**
leticl's `+weather-warnings+` carries it and this side's `ALARM_ONLY` does not, so a retry ladder is a
row here and weather there — the one code where the two heads still answer *is this an event in the
record* differently. Nothing above changes it: it stays a routine row until somebody rules.

---

## R13 - Cloud GLM, DeepSeek and Grok as turn backends

**Status:** built (`crates/provider`, `MessagesBackend`, `--provider`) and tested against a fake server only (`crates/provider/tests/fake_provider.rs`). left: live tests behind a real provider key, which no box here has, and the providers' prompt caches are only relied on - `PrefixGuarantee::Asserted` (`crates/backend/src/lib.rs:230`) is the whole claim.

Done: `crates/provider`, `MessagesBackend` in `letibot-backend`,
`TurnEngine::run_turn_messages`, `--provider/--api-key/--thinking`, the `provider`
disclosure, cost in the footer; fake-provider tests and an end-to-end harness run
(`docs/providers.md`). — ✓ fixed in a592cc89, test a_turn_streams_reasoning_text_and_a_tool_call_and_the_request_carries_the_key_and_tools **Not verified live** — no key on lab2x1. — OPEN: nothing in the tree has run against a real provider — every provider test is a fake server (`crates/provider/tests/fake_provider.rs`) — and `Done when` asks for live tests behind the key. Open, per the
operator's note: an eager-compaction budget under a meter (compaction is manual
today), and the providers' prompt caches are relied on through the stable prefix. — ✓ the eager-compaction budget under a meter is fixed in e1419a07, test an_unmeasured_metered_session_is_not_folded_before_it_is_measured — OPEN: the prompt caches are still only relied on — `PrefixGuarantee::Asserted` (`crates/backend/src/lib.rs:224`) is the whole of the claim, and `cached_tokens` is disclosed rather than asserted (`crates/turn/src/compaction.rs:114-120`).

*"2. i want cloud glm and deepseek and grok to work in letibot/code."* D10 reserved
the seam (`crates/backend`: `TurnRequest` holds the transcript, `BackendCaps` states
facts, `Meter::Money`, `PrefixGuarantee::None`). All three speak an OpenAI-compatible
chat-completions API with tool calling; one `messages` backend with three endpoint
presets. Keys from the environment (`ZHIPU_API_KEY`, `DEEPSEEK_API_KEY`, `XAI_API_KEY`)
or `~/.config/letibot/providers.toml`. D10's rule holds: the invariant suites skip
loudly under a provider — `skip_reason()` says the prefix check did not run.

**Done when.** `harnessd --provider deepseek --model deepseek-chat` (and `glm`,
`grok`) runs a turn with tool calls through the same engine; a fake-server test
covers the message conversion, streaming and tool-call parsing; live tests behind the
key being set; cost reported as `TurnCost.micros_usd`.

---

## R98 - macOS has no boundary for a command, so tests there must ask (D16, amended)

**Status:** half 1 (the policy, `runs_project_code_unconfined`) and half 2 (`crates/tools/src/exec/seatbelt.rs`) are built, the seatbelt tests pass 8/8 on a Mac (2026-11-03), and the default local seat on a Mac now runs its commands inside the boundary. left: the live card check - a default local session at `writes-allowed` in a checkout runs `cargo test --offline` with no card and no `boundary_absent`.

D16 admits a command without a card only inside a measured boundary, and its amendment says
that on a raw host a command that executes code the agent wrote (a test runner) must keep
asking, and no standing grant may admit it, because the oracle does not read a runner's files.
The macOS host shell has no boundary at all (firecode VMs are the only macOS boundary letibot
has, `docs/firecode-on-macos.md`), so every `cargo test` on the Mac is either asked about or
theater.

Two halves, the first smaller and first:

1. **Make the rule true on a raw host now.** A `bash` call whose stages run a test runner or a
   build that executes project code (`cargo test|run|build` with build scripts, `pytest`,
   `go test`, `npm test`, `make`, a script the session wrote) is not settled by a session or
   durable grant when the backend is unconfined; it asks every time, and the card says why
   (*"runs code the agent wrote, on a host with no boundary"*). Measure what this costs on the
   corpus replay (how many past grants would no longer settle) before landing. — **BUILT.** — The
   rule is the policy now, and it has no setting: `runs_project_code_unconfined` reads one fact,
   `has_measured_boundary`, which `Harness::open` hands it off the backend — so a Linux test
   constructs the Mac's case by handing the gate `None`, and nothing in the gate probes a platform.
   `[permissions] ask_unconfined_code`, `with_unconfined_code_asks` and the startup note that
   disclosed it are gone with the switch — in `crates/leticode-config`, and in
   `crates/harnessd/src/harness/open.rs`. Tests:
   `a_host_with_no_measured_boundary_asks_with_nothing_switched_on`,
   `the_same_runner_is_admitted_inside_the_boundary` (unchanged) and
   `the_card_narrows_the_ladder_and_says_why`, whose note must say *no boundary*. The cost was
   measured on the corpus before it was turned on — `docs/boundary-and-adjudication.md` §R98 (1),
   **171** of the mac's 834 standing settles would ask, 137 of them the session's own programs and
   24 `cargo test`.
2. **Give the Mac a boundary** — **BUILT 2026-10-27**, `crates/tools/src/exec/seatbelt.rs`, and
   written up in `docs/boundary-and-adjudication.md` §4k. Seatbelt (`sandbox-exec -f <profile>`)
   renders the same `ViewSpec` the Linux side renders as a bubblewrap argv, and the probe measures
   it from inside: the deny list beating an allow (a canary inside a writable `$HOME`), a write
   outside the checkout refused, a write inside it succeeding, fork/exec, a nested `sh -c` denied
   the same path, `kill -0` on the launcher `EPERM`, and both egress subjects refused when egress
   is denied. `confine::probe` chooses the mechanism by platform, so `HostBackend::confined`, the
   session's `/tmp` and every grant re-probe through one dispatch, and a Mac session's `bash`,
   jobs and persistent shell all run inside it. A Mac that cannot measure one is **unconfined and
   says so** (`boundary_absent` on the session's log, with the refusal's own words).

   The measured cost: **+5.1 ms per exec** (100 `true` calls: 1.28 → 6.41 ms/call), +4.5 ms on a
   real `cargo --version`, and it does not grow with the command's work. What macOS cannot give —
   a PID namespace, so processes are unreachable but not absent, and no `PR_SET_NO_NEW_PRIVS` —
   is printed on the boundary's own description as `ABSENT` / `NOT APPLICABLE`, never omitted.
   Reaping stays the process-group tree (`exec::scope::ProcessGroups`), as it already was. — ✓ fixed in 5442b7c8, test the_probe_measures_a_boundary_and_says_what_macos_cannot_give

   **Two levels (2026-11-04).** The tight view alone made `git push` and `gh` fail in a top-level
   Mac session ("could not read Username", "not logged into any GitHub hosts"). A top-level session
   is now wide (real `$HOME` read-write, filesystem readable, `DENY_UNDER_HOME` less `.config/gh` as
   the only fence). A child keeps the tight view plus the operator's git identity and its worktree's
   git dir (hooks and config read-only), with no credentials, so it commits and cannot push. A
   top-level seat gets `GH_TOKEN` and a github.com-only git credential helper, from one `gh auth
   token` the daemon runs at open, masked in output; `Library/Keychains` stays denied. See §4k.

   **What half 2 still needs: a Mac.** `crates/tools/src/exec/seatbelt.rs` and
   `crates/tools/tests/seatbelt.rs` are in the tree now, and nothing on a Linux box can run
   `sandbox-exec` or a `#![cfg(target_os = "macos")]` test, so the profile has never been read by
   the kernel that enforces it and D16's admit *inside* a Mac boundary has never run.

**Done when**: (1) on an unconfined backend no grant settles a test run, with a test; (2) a Mac
session's commands run inside a measured boundary that `~/.ssh` and the profiles are absent
from, and D16 admits them there. — **(1) is done**: the rule is the policy and
`a_host_with_no_measured_boundary_asks_with_nothing_switched_on` proves it from the injected fact,
with the ladder narrowed and no grant recorded. **(2) is done on its Linux side**: macOS has no
mount namespace to make `.ssh` and the profiles *absent*, so they are **denied** rather than absent,
the probe measures the denial, and the description says which of the two it is holding. The
`bwrap` half is measured on this box; the seatbelt half is not.

**Measured on a Mac, 2026-11-03**: `cargo test --offline -p letibot-tools --test seatbelt --test
seatbelt_no_helper` passes 8/8. The default local session (`leticode` seat, `bash` on) was NOT inside
the boundary: a live `writes-allowed` session asked a card for `cargo test --offline` ("this host has
no boundary") and published no `boundary_absent`, because only `runner`/`coder`/`gatekeeper` took the
confined arm in `place_the_tools`. Fixed: on macOS that seat's commands run in the seatbelt boundary
over the checkout (`HostBackend::confined_over`, file tools still rooted at `/`), `backend.boundary()`
is set so `Prereq::Confinement` and the D16 gate see it, and a refused probe stays unconfined with
`boundary_absent`. Tests: `harness::open::mac_default_seat_tests` (boundary built, no warning, write
inside ok, write outside refused). Linux is untouched (the new arm is `cfg(target_os = "macos")`).

**Still open**: the live card. Build the daemon with this branch, start a default local session at
`writes-allowed` in a checkout, ask it to run `cargo test --offline`: it must run with no card and no
`boundary_absent`. The refused-probe arm is the old path and is exercised only on a Mac where the probe fails. Expect that a
build needing a new dependency fails inside the boundary (toolchain is read-only, offline only).

---

## R66 - A harnessd reached 113 GB

**Status:** instrumented (memwatch, 4eb1dbb) and measured on a scratch daemon (2026-11-02, extended 2026-11-03): 40 turns with a refused compaction each grow nothing, a child's retained state is bounded by a test, and the two-threads-per-subagent leak is closed on `r68-lifecycle` (R68.3, R67.13). left: the LOCAL-model route, which needs a local server and a GGUF on this box and so is the operator's, and the incident's own 113 GB.

MEASURED in the kernel's OOM report, 2026-10-11 00:47:00: `Killed process 3985588 (harnessd)
anon-rss:113190136kB` — the daemon's own heap, not a child's. It was the `/home/dead/Projects/letibot`
daemon serving `s-1789462738453908838` (qwen-3.8-flash-next on :8082), the session whose
compaction failed repeatedly the same week and accumulated six copies of the compaction
instruction in its transcript. Its window's scope peaked at 131.6 GB over 1 d 23 h. The
allocating thread named in the report is `harness::queue_`.

Nothing is known yet about the cause. **Start with:** what that session's transcript and
`harness::queue_` hold; whether a daemon's RSS grows per turn on a long session (sample
`/proc/PID/status` VmRSS across turns on a scratch daemon); whether the failed-compaction retries
grow anything.

**Done when:** the growth is reproduced on a scratch daemon and either bounded or explained.

**MEASURED 2026-10-11 — what it is NOT, and one smaller leak found on the way:**

- **Not job output.** `HostProcesses` keeps every job it ever ran (`jobs.push`, never removed),
  each with up to 8 MiB of capture — but across the session tree's 15,405 `bash` results between
  10-09 and the kill, the retained capture sums to ~0.1 GB.
- **Not the transcripts.** 76 transcripts, a few MB each in the store.
- **`harness::queue_…` is not a daemon thread.** It is the task that ALLOCATED when the box ran
  out — a test binary's thread (a `harness::queue_*` test); the kernel then chose the largest
  process, harnessd, at 108 GB RSS (131 GB in the task table's total_vm column).
- **On deepseek (metered) a scratch daemon does not grow like that:** 245 MB at start; +75 MB
  for six sequential subagents; +12 MB for three turns each printing ~15 MB of tool output.
- **A smaller leak, real:** each finished subagent leaves **two threads** behind (7 → 19 for six,
  never released) and ~12 MB. At the 519 subagents that tree started, ~6 GB — not 108.
  **CLOSED on branch `r68-lifecycle` (R68.2/R68.3):** R67 item 13 ended the `letibot-deadline`
  thread with its host, and the host now dies: a finished child idle for `child_park_after`
  (10 min) parks as store-only — its `subagent-*` thread leaves, its hub is removed from the
  registry, its harness drops. MEASURED (`tests/session_lifecycle.rs`, 50 children): threads
  (subagent, deadline) 0,1 before → 50,51 with all answered → 0,1 after parking, one registry
  entry left; with the policy off (the old behaviour) 50,51 and 51 entries stay.

**Instrumented 2026-10-11 (`harnessd::memwatch`), so the next growth is SEEN:** a counting global
allocator (heap live / peak, and a backtrace for any single allocation ≥ `LETIBOT_LARGE_ALLOC`,
64 MiB by default); a watchdog line every `LETIBOT_MEMWATCH_SECS` (60) with rss, heap, threads,
sessions, hub logs, view items, heads and job rows; a census at each rss step from 1 GiB
(doubling) — hubs holding most, threads grouped by name — with a `memory_high` warning on every
session; `BOUND BROKEN` when a hub's log or a head's queue is past its bound, or a view list
with no bound passes 10,000; and `memory.high` on the daemon's own leaf (`[memory] daemon_high`,
16 GiB default, `max` = off) so the kernel throttles the daemon instead of taking the box.
First reading on a scratch daemon: **rss 279 MiB with heap live 35 MiB** — most of a daemon's
resident memory is NOT on the Rust heap (SQLite's page cache and the vocabulary map use their own
allocations), so a census showing rss far above heap live points there and not at a Rust
collection. And the large-alloc log found its first: `Catalogue::load` re-reads the 5 MiB model
catalogue from disk on every `Config::settings`, i.e. on every session open.

**Not yet tried, and the likeliest place left:** the LOCAL-model path. The killed daemon served
qwen-3.8-flash-next on :8082, which streams token ids through `/completion` and keeps the token
ledger itself; deepseek does neither. Reproducing it needs the local server running, which is
the operator's to start. — OPEN: the cause is still unreproduced; the instrument is
`crates/harnessd/src/memwatch.rs`, and the local-model path that was killed has not been run
under it.

**MEASURED 2026-11-02 — a scratch daemon with a fake model does not grow per turn, and the four
lines that could have carried it are each bounded:**

- **The daemon itself.** `crates/harnessd/tests/r66_daemon_rss.rs` starts a real `harnessd` on its
own store, socket and runtime dirs under an address-space cap, with a fake model whose summary
requests it refuses, and drives one session: **40 turns of 8 KiB prompts, a refused compaction
after each — rss 230.5 → 223.5 MiB, and the last watch line reads `grew s-r66 events +7 · view +1 ·
warnings +1`.** The same file takes `R66_TURNS`/`R66_PROMPT_BYTES`/`R66_IDLE_SECS` for the probe at
its own scale. A session with a refused compaction refuses its next prompt for room (`0 refused`
above is what a session with room looks like), so the driver waits on the turn's own events rather
than on the model.
- **The probe at its own scale, and idling.** 120 turns of 16 KiB prompts with a refused compaction
after each: **rss 235.6 → 257.3 MiB over 119 turns — 0.18 MiB a turn against 1.9 MiB of prompts
appended**, most of it the token region (4 bytes a token on a byte vocabulary) and the hub's own two
copies, so ~1.4× what the rows cost and inside the 2× the CI test allows. Then **60 s idle: 257.3 MiB
at all six readings** — nothing grows with the clock.
- **The worker and the queue gain nothing per turn.** `crates/harnessd/tests/r66_queue.rs`: 40 turns
through the real `Sessions` and its worker leave `opened()==1`, one registry entry and a watcher set
holding nothing, at **0.01 MiB of heap a turn, the same in both halves**. Every map is keyed by session
and removed at close; `Outcome` is a return value; activity is rebuilt per publish; metrics are a
bounded channel. **Two maps were unbounded per JOB and are now bounded**: `JobWatchers.settled` (one
name per backgrounded job, never removed) and `ReadRecord.seen` (one entry per delivered completion),
which also made `want_now`'s guard read a record with no history.
- **What is per CHILD, and what still holds an answer after the child is gone:** `TaskSlots`
(`harness/children.rs`) is a `Vec` that only grows, one row a child of **at most 512 bytes** — a
handle is an address and not a payload — and a slot keeps an answer only while it is OWED. A reader
handed one drops it: both the answer the slot settled with and the last answer a later turn gave,
where the child's row kept that one (a slot is never the last copy, and the case it does keep is a
child whose row never landed — `TaskSlot::hand_over_answer`, `SubagentRecord::answered_again`). The
later answer was a real leak, found 2026-11-03: `spoke_again` wrote a whole answer into the slot and
no reader ever removed it, so a parent that steers its children held one answer each for the
daemon's life. MEASURED (`harness/tests/children.rs`,
`a_later_answer_the_record_keeps_is_not_kept_twice`, 20 children of a 17 600-byte later answer):
**355 200 B held before, 3 200 after, against a bound of 512 a child** — what is left is the
~160-byte sentence naming where the text is. What is left per child is the row; what a child costs
in its parent's transcript is the conversation and not the tree. `HostProcesses` also keeps a row per
command ever run (1.1 KB a row, per HARNESS and not per daemon, its capture already bounded at
64 MiB), which is what makes `job_output j12` answer at all.
- **One failed fold** leaves exactly one row — the 1 470-byte `SUMMARY_INSTRUCTION` — and 1 511
ledger tokens, and no failure arm takes it back (`crates/turn/src/compaction.rs:210`). Measured over
40 attempts on a 2 MiB conversation: +0.19 MiB of heap in total, flat. It compounds the *prompt*
(+1 494 bytes per attempt), not the heap, and 113 GB of it would be ~74 million attempts.
- **A round's working set** is two whole-prompt copies inside the stream (22.4 bytes per prompt
token at 244 139 tokens) and none of it survives the round; a fork frees both the rows and the token
region (40 rows / 1.06 MiB → 1 row / 0.27 MiB). The region only grows by doubling and never shrinks
between forks, which is the capacity the footprint already states.
- **Two blind spots were real and are fixed.** `restate_footprint` did not run for a compaction's own
rows, so a session that could not compact under-reported itself by exactly the instructions it kept
(58 800 bytes after 40 attempts) — `harnessd: walk the census after a compaction's own rows`. And
`TranscriptItem::bytes` counted a picture's base64 body as 32 and a tool call's arguments as zero,
so the ceiling and the hub's byte bound were blind to the largest rows a session can hold —
`transcript: count a picture's body and a write call's arguments in a row's bytes`.
- **The watch now says what GREW**, not only what is held: every line after the first prints
`memwatch: grew <term> +<bytes> · …` (biggest first, per session), and three growth readings in a
row with nothing in the census accounting for them raise `MEMORY CREEP` in the log and a
`memory_creep` warning on every session. That is the line the 47-hour daemon's log never had.

**What is still unexplained:** the incident's own 113 GB. Nothing measured here produces it, and the
one route that was never exercised is the LOCAL model (`/completion`, token ids, no GGUF on this
box). The watch now names the term that grows, so the next daemon to do it says which.

---

## R65 - The model chooses each tool call's memory cap, knowing what is free and what a cap costs

**Status:** landed on main 2026-10-11 (073c1ea, 51dd340) and its remaining items on 2026-11-03 (71ebd8e7, a1824340, 389dca2f, 1228e425, 31b496e1). left: the cross-session ledger only, which is a proposal the operator has not decided.

The operator: *"the question is how many for 2. I think model should try to decide, so it should
know the free, how much it can ask and what are the consequences"*.

Verified on lab2x1 that the mechanism exists: the user manager delegates `cpu memory pids`, and
`systemd-run --user --scope -p MemoryMax=64M` around a 200 MB allocation ends that process alone
with `Killed`, rc 137.

1. **The model asks.** `bash` (and a backgrounded job) gains a memory argument — `memory: "8G"`.
   — ✓ fixed in d755bf51, test uncapped_is_an_ask_and_a_blank_is_the_default
2. **The model knows what is free** — said where it decides, in the tool's description or the
   turn's disclosure: the box's total, `MemAvailable` now, swap, and what this daemon's live
   calls and jobs already hold under their caps. — ✓ fixed in 71ebd8e7 and a1824340, test
   every_round_says_what_is_free_and_no_schema_carries_it (`crates/harnessd/tests/memory_line.rs`).
   The description cannot carry it - a tool schema is the session's stable prefix, and this number
   moves every second - so it rides the ROUND'S TAIL instead: `harness::memory::memory_tail`
   composes one `[memory - the harness's own line]` item per round from `Meminfo::round_clause`
   (`crates/tools/src/exec/memory.rs`, which replaced the callerless `Meminfo::clause`) and
   `Holds::of` (the daemon's open scopes: their caps added up and their peaks). The test reads two
   real requests off a fake provider: the line is in both, once each - never in the transcript -
   and the two requests' `tools` are byte-identical.
3. **The model knows how much it can ask** — a ceiling per call, derived from what is free at
   call time. An ask over it is refused with the numbers, not silently clipped. — ✓ fixed in
   d755bf51, test
   the_default_is_granted_an_ask_up_to_the_ceiling_is_granted_and_anything_above_is_the_operators
4. **The model knows the consequences, in the description and in the result:** over the cap, the
   call's process tree is killed and only it (rc 137, the result names the cap and the peak).
   **With no cap, an exhausted box kills whatever the kernel picks, and systemd then stops the
   whole window** — R64's table. Omitting the argument gets a stated default, not no cap.
   — ✓ fixed in d755bf51, test
   a_kill_at_the_cap_says_what_to_ask_for_next_and_where_the_default_lives
5. **Every result reports its peak** (`memory.peak` of the call's scope), so the model learns
   what its commands cost instead of guessing again next time; a job's peak is in `job_list`.
   — ✓ fixed in 389dca2f, test a_finished_call_reports_its_peak: a call that ended well inside
   its cap now ends `memory: peak 12 MiB of a 64 MiB cap.`, and `MemoryUse::worth_saying` - the
   predicate that said the opposite - is gone with its test.
6. **A box with no user systemd says so** — the call runs uncapped, and the disclosure says it
   is uncapped. Never silently. — ✓ fixed in d755bf51, test 1228e425
   (an_uncapped_call_names_the_ask_and_the_reason): `MemoryCap::Uncapped` names the ask and the
   reason in the result (`crates/tools/src/builtins/bash.rs`)

**The default is a config option, 64 MB out of the box** — the operator: *"default memory either
as absolute or a fraction is a config option with 64mb being a good default"*. The setting takes
either an absolute size (`64M`, `2G`) or a fraction (`0.05` — of `MemAvailable` at call time),
and the description states the value in force, resolved to bytes. 64 MB is deliberately small:
most calls fit in it, and a call that does not is killed at once with a result that says what to
ask for, which is cheaper than a runaway found by the OOM killer.

**The ceiling is a config option too, and it is not a hard limit** — the operator: *"ceiling is a
config too, with both absolute and fraction and the max is uncapped but if model wants it - must
ask me. unaskable ask is 50%."* So there are two bands:

- **Up to the ceiling** (default `0.5`, of `MemAvailable` at call time; absolute sizes allowed),
  the model's ask is granted without asking anyone.
- **Above it, up to uncapped**, the ask is legal but goes to the operator as a card naming the
  amount, what is free, and the command. Refused → the call does not run, and the result says so.

**The mechanism, MEASURED on lab2x1 2026-10-11:** a process started with `systemd-run --user
--scope -p Delegate=yes` moves itself into a leaf (`<scope>/daemon`), writes `+memory` to the
scope's `cgroup.subtree_control`, makes the tree (`<scope>/tree`, `+memory` again) and a job under
it with `memory.max=64M`, `memory.swap.max=0`; a child that joins the job and allocates 200 MB dies
with SIGKILL, `memory.events` `oom_kill 1`, `memory.peak` 64 MB, and nothing else is touched. The
daemon must not be IN the cgroup whose children get controllers (cgroup v2's no-internal-processes
rule) — which is why the tree today, rooted in the daemon's own cgroup, cannot carry a cap.
**OPEN — the operator, same day:** *"then again - depends on who does what - if nothing else going
then even 80% becomes possible. and if something else going can be changed by other sessions - so
both sessions can ask 50% independently which means some central daemon that checks it and rejects
the second ask? no idea yet"*. The defect named: a ceiling read from `MemAvailable` at call time
does not see a GRANT that has not been USED yet, so two sessions (or two daemons — one per folder)
can each be granted 50% within a second of each other. A proposal, not a decision: **the kernel is
already the ledger** — every grant is a `memory.max` on a job cgroup under some daemon's scope, so
what is promised box-wide is the sum over `letibot*.scope/tree/**/memory.max` (minus what those
jobs already use, which `MemAvailable` has counted), read under one `flock` in
`$XDG_RUNTIME_DIR/letibot/` for the check-and-grant. No central daemon; the second ask is judged
against what is left. Nothing in the per-call cap, the default, the peak report or the operator
card depends on this, so those can land first.

(Trap met while measuring: `systemd-run` expands `$VAR` and `$$` in its arguments, so a probe
written inline as `sh -c 'echo $$ > …'` writes `$` and fails with EINVAL/EIO. Use a script.)

**Done when:** R64's done-when holds, the model's next call after a kill asks for a different
cap on its own, and `job_list` shows each job's cap and peak.

**Landed (branch `memory-caps`):** `exec::memory` (the policy: `Amount`, `MemoryPolicy`,
`Meminfo`, `Ask`, `decide`); `exec::scope::delegate_memory` (the daemon steps into
`<scope>/daemon` and enables `memory`, refusing by name when it shares its cgroup) called first
thing in `harnessd`'s `main`; `ScopeTree::{memory_caps, cap_memory, memory_use}`;
`SpawnRequest::memory` → `memory.max` + `memory.swap.max=0` before any process joins, carried
across a promotion; the job's `memory.peak`/`oom_kill` read at the settle; `bash`'s `memory`
argument, the description, the note on every model call's result (the cap and the peak, now
unconditionally), a dedicated *killed at its memory cap* outcome; `Gate::grant_memory`
with an `AlwaysAsk` card in `AdjudicatedGate`; `providers.toml` `[memory] default` / `ceiling`,
a malformed value refusing the start by name. The operator's own `!` run is uncapped.
MEASURED end to end through deepseek: no `memory` → `killed at its memory cap — peak 64 MiB of a
64 MiB cap`; `memory: "512M"` → ran, `peak 192 MiB of a 512 MiB cap`.
`tools/tests/memory_cap.rs` re-runs itself in a delegated scope and asserts both.

`job_list`, `job_wait` and a background job's completion notice carry a `memory:` line (cap,
peak, KILLED at the cap) — measured: `job_list` read `memory:  peak 97 MiB of a 128 MiB cap` for a
backgrounded 100 MB awk.

**Still open:** the cross-session ledger above, and nothing else. — ✓ the card above the ceiling is
answered by a person in a head: 31b496e1, test a_head_answers_the_memory_card_above_the_ceiling
(`crates/harnessd/src/answers/tests.rs`) puts the ask on a `HeadAdjudicator`, has a head answer
`grant_memory_session`, and shows the next, smaller ask is granted with no second card. — OPEN: no
ledger sums `memory.max` under `letibot*.scope/tree` — `flock` appears only in
`crates/room/src/lease.rs:206` — and the operator has not decided whether to build one (*"a
proposal, not a decision"*, above).

---

## N5 - the agent should know the view is hermetic, and propose the grant it needs

**Status:** built in part; the view is hermetic and the grant card exists. left: which asks must be refused rather than prompted is undecided - `grant_view` raises an ordinary card for `~/.ssh`, with no never-write precheck.

Operator, 2026-09-11: *"i guess the agent should know it is hermetic and prompt me with
his idea of shared data which i can allow."*

**Measured 2026-09-11, and it is why this matters now.** The confined backend builds on
this box — `bubblewrap 0.11.1`, cgroup entered, nested-namespaces held, no-new-privs
held — and it is `writable + EXEC`, so confinement costs no file access. But its view is:

    project /home/dead/Projects/letibot (rw); 19 read-only system paths;
    $HOME is a FRESH TMPFS at /run/letibot/home
      (so a build cache under $HOME is empty every run); no grants
    egress: DENIED — no interfaces, no routes, no DNS

So `cargo test` cannot run: `~/.cargo` is absent and there is no network to refetch it.
That is the boundary working — *"a secret outside it is ABSENT, not denied"* — pointed at
a toolchain instead of a secret.

### Most of this exists

`exec/confine.rs` already has the mechanism, and its discipline is the interesting part:

- `Grant::{ReadOnly, ReadWrite, AgentSocket}`, each carrying a **`why`**, because *"a
  grant nobody can explain is a grant nobody can revoke"*
- `ViewSpec::granting()`, and `Boundary::describe` prints **the consequence next to the
  grant** — `ReadOnly` means readable *into the transcript*, since §3's second half is not
  enforced by that module
- the `agent_from_env` precedent: when a key is needed there is no grant that binds it.
  The socket is bound, the key is not, and *"the error path is the important half"* — the
  refusal names the mechanism nobody built rather than quietly widening

### What is missing is the loop, and it is three things

1. **The model does not know it is hermetic.** The boundary description is a startup
   banner for the operator; nothing puts it in the model's context, so a failure inside
   the view looks like a broken toolchain rather than an absent one. — ✓ fixed in 91502115
   and 1ca335d6: `Boundary::model_brief` is the view in the model's own words, and it
   travels as `SeatFacts.view`, a `view` prompt section between `edit_files` and `scratch`.
   A session whose commands run on the host's disk has no view and composes the bytes it
   always did, which is why every unconfined prefix is unchanged. Tests
   `the_model_is_told_what_its_view_holds_before_anything_fails`,
   `a_confined_seat_is_told_what_its_view_holds_and_an_unconfined_one_is_not`,
   `the_prompts_view_comes_from_the_boundary_that_was_measured`. Left under this one: a
   firecode VM is a boundary by construction and has no `Boundary` to ask
   (`FirecodeBackend::boundary` is the trait's `None`), so a VM-placed session gets no
   `view` section at all — its facts are `Config::vm_copy_source` and the mount rules, and
   the sentence has not been written.
2. **An absence does not say what would fix it.** `cargo: command not found` or a missing
   registry should come back as *absent because outside the view*, with the grant that
   would change it — the same shape as `edit`'s read-before-write refusal and R8's
   near-miss, self-correcting in the same call. — ✓ fixed in 96569337 (the absence note,
   the missing-program path the card is raised for) and 91502115: the note names BOTH
   grants the card offers, since a write needs the read-write one and naming only the
   read-only sent it to ask for a grant that cannot let it through. Test
   `the_failure_note_names_both_grants_that_would_let_the_path_through`. The row is the
   model's to read; the grant itself is raised by the runtime (`needs_in_view`), so the
   model has no verb to ask with and needs none.
3. **Grants are construction-time.** `ViewSpec` is built before the spawn; nothing adds
   one for the rest of a session after an operator approves it. — ✓ fixed in 96569337:
   `ExecBackend::grant_into_view` re-probes the confinement, and `ToolRuntime::run` calls
   it on an approval and says the view as it now stands on the row.

### The questions, which is why this is a nod

**Does a grant go through the gate like any other decision?** It should — the prompt is
*"the model wants `~/.cargo` read-only, because: to run the workspace's tests"*, and
`grants session` then makes it stick for the session, which is the machinery that already
exists. The consequence line has to be in the prompt, not only in the banner: binding
`~/.cargo` read-only means its contents can reach the transcript.

**And which asks must be refused rather than prompted?** The ssh case is the precedent
and the flow rule (§3) is the test: a grant that would make secret bytes *readable* is
inexpressible, not adjudicable, however politely the model asks for it. A model that can
propose grants must not be able to propose that one and have it arrive as an ordinary
prompt. `NEVER_WRITE` is the first precheck and the tier mints no `Adjudicable` for an
inexpressible action — so the pieces are there; what is undecided is that a
model-proposed grant is routed through them rather than around them.

**Smallest version that closes the loop for tests:** the view learns one grant shape
(`ReadOnly` on a path), the model is told the view is hermetic and what it holds, an exec
failure names the absence, and the grant prompt carries its `why` and its consequence.
All four are in `main` now (2026-11-03): the loop is closed for a read-only grant that the
runtime raises, and what remains is the question above — the ssh path reaching `grant_view`
as an ordinary card.
That is N4's answer too — see `TODO-settled.md`'s N4 for why a shaped runner was wanted rather than
`bash`.

---

## T25 - Everything waiting on the operator

**Status:** a list of operator decisions D11 to D28. D11 and D12 are settled (their lines say so); D14, D15, D17, D20 to D24 and D26 carry an OPEN line; D13, D16, D18, D25, D27 and D28 carry none. left: the operator's answers to the open ones; unclear: the six without a status line, check each against the tree.

Filed on request, so a question does not live only in a scrollback. Continues the D
series. **Two arrived answered in the same message and are recorded as resolved** rather
than dropped, because the answer is the interesting part.

### Open

**D11 — Land the four branches?** session-resume (protocol 4, `--continue`), exec,
intent, outside-world. All green, all rebase cleanly. Landing costs the running daemon
(pid 384248) its live session, because protocol 3 → 4 makes a fresh head refuse it by
name. Every transcript is on disk and `--continue` now genuinely returns the newest.

> **Overtaken 2026-09-10: `PROTOCOL_VERSION` is 6.** The four branches landed and the
> assembly on top of them needed a wire change of its own — `SessionEvent::DenialRaised`,
> §4b's requirement that a refusal reach the operator when it happens. The cost D11 named
> is now paid once for both: a running daemon on 5 loses its live session to a fresh head
> either way. T24's `PromoteJob` was the *other* claimant on 6 (`ProcessHost::promote`'s
> doc specifies the frame verbatim) and it is **not** in this bump — it is a `ClientFrame`
> and needs a head that sends it, whereas a denial is a `SessionEvent` and needs only a
> head that renders it. Adding it later is another integer; the constant is the one line
> every wire-touching branch edits and taking 6 for the thing that was ready is the same
> "coordinate rather than race" D10 settled.

— settled: the four landed and the wire is at 55 (`crates/sessionlog/src/protocol.rs:19`), so
nothing waits on this one.

**D12 — A mounted board makes `todo` gated.** The flowy backing declares
`Access::Network`, so mounting a board without an adjudicator attached makes `todo` refuse
*entirely* — the unmounted local list works, the mounted one does not. Correct by the
rules and possibly intolerable in practice.

> **Sharper as of 2026-09-10, and generalised.** The same shape now decides whether a
> session *opens at all*: a seat whose tools declare `Write`, `Exec` or `Network` and has
> no adjudicator refuses to start, rather than starting and failing every call. That is
> the right end of the trade for `write` and `bash`. For **`say`** it is the D12 problem
> with a bigger blast radius — the planner seat needs an adjudicator because it can talk
> to a room, and a planner that cannot plan without one is exactly *"correct by the rules
> and intolerable in practice"*. The unasked question underneath both: is *reaching the
> fabric the operator already authorised this seat to be on* the same class of act as
> *reaching an arbitrary host*. `Surroundings::seen_hosts` says first contact and second
> contact differ; nothing yet says a **declared** attachment differs from a discovered one.

— settled by `Access::Session`: `todo_write` declares it (`crates/tools/src/builtins/todo.rs:2064`)
and `say` too (`crates/tools/src/builtins/room.rs:138`), and that class is unattended
(`crates/tools/src/schema.rs:129`) — so neither a mounted board nor a planner's `say` needs an
adjudicator to open.

**D13 — `Access::Session`, a new access class.** Declaring the intent tools `Read` would
have reproduced the gap the survey names in grok-build (§1.4). The wire carries access as
a String so nothing breaks, but no head has rendered one. — ✓ fixed in 449f7de0, test
the_description_is_clean_and_fits_the_prompt

**D14 — `loop_closes` needs an exclusive model server.** It drives live inference on
`:8080`; it passes alone in 11–32 s and times out under `cargo test --workspace` whenever
the box is busy. **Three separate agents have now reported it as a possible regression**,
and one of them burned a re-run attributing it. A test that reads as a failure whenever
the box is loaded is a broken instrument. Proposal: gate it behind an env var or `#[ignore]`
so `--workspace` means what it says.

> Fourth data point, 2026-09-10: green in 22.2 s standalone and green under
> `--workspace` on an unloaded box, alongside `turn/tests/live_qwen` (4 tests, 6.0 s).
> Two passes are not evidence against the entry — the failure mode is contention and
> the box was quiet — but they do say the instrument is not broken in some second way,
> which is what a fifth agent would otherwise spend a re-run finding out.

— OPEN: the test is still live whenever `127.0.0.1:8080` answers — `skip_live_test` gates
absence only (`crates/turn/src/serving.rs:70`), there is no `#[ignore]` and no opt-out, and
`agents.md:297` still tells a person which targets to avoid.

**D15 — 13 stale worktrees and one 30-hour orphan tmux** (`nano_test`). Safe to prune the
finished ones; two belong to live agents, so not a blanket sweep. T24 is the mechanism,
this is the backlog it would have prevented.

— OPEN, and not code: nothing in the tree prunes a stray worktree or a stray tmux session, and
the backlog itself is a fact about a box this audit cannot read.

**D16 — `~/bin/letibot` is not version-controlled**, and two real bugs were found in it
tonight (a `--continue` that showed an empty screen, and an `up()` that would have
orphaned a version-skewed daemon holding every session). It belongs in the repo.

— ✓ fixed in f0627c39, test tests/launcher.sh (a shell test: the launcher's own cases, run
against stub daemons).

**D17 — Group 2, subagents and admission.** Held all evening because it lands in
`crates/harnessd/src/sessions.rs`, which session-resume was rewriting. **Now unblocked.**
Its hard requirement is already written: `Fits{ceiling, cost_per_id, pool_free}`, because
436.7 MiB per sequence id charged at allocation makes N subagents an OOM path — and two of
the five surveyed harnesses ship subagents with no ceiling at all.

— OPEN: `Fits{ceiling, cost_per_id, pool_free}` appears nowhere in the tree but this line; what
exists is a depth cap on children (`crates/harnessd/src/harness/children.rs:1847`), and no
memory-cost admission.

**D18 — LSP.** The one gap in the survey's ten that no group covers. Four of five
harnesses have it, the most complete is 14 actions, and it needs a language server per
language. Its own decision, not a group.

— fixed in b5ee36d5 in code, untested beyond the config: `crates/tools/src/builtins/lsp.rs`
spawns a server and pulls its diagnostics, and the only test names the default language list
(`default_config_names_its_languages`).

**D20 — `never_hit` now sees network arguments.** A `web_search` or `github` string that
merely *mentions* `.config/gh` or `.password-store` is denied by the never-write list.
Fail-closed, and a real false positive.

— OPEN: `query` and the other content arguments are exempt now (`CONTENT_ARGUMENTS`,
`crates/tools/src/adjudicate.rs:3321`) with a test
(`the_never_write_list_is_about_paths_and_not_prose`, `crates/tools/src/adjudicate/tests/gate.rs:521`),
but `github`'s `title`/`body` (`crates/tools/src/builtins/external/github.rs:236-240`) are not,
so a body naming `.config/gh` is still refused.

**D21 — Enforcement binds the model, or the seat?** From `docs/closed-loop.md` §9. They
differ when a human is driving, and the operator should not be locked out of their own
restart because the model cannot be trusted with it. Tonight's blocked
`systemctl --user restart` is the worked example.

— OPEN: `docs/closed-loop.md:110` still lists it under §9 Open, and nothing in the tree binds
enforcement to the model or to the seat.

**D22 — The tolerance band: one, or per-operation?** Also §9. How large a deviation is
corrected silently versus faulted to a human is the whole design decision, and both ends
have measured costs. **One data point now exists**: T21.3's nudge is capped at one per
user turn, chosen at its narrowest for want of a measurement rather than because one is
right.

— OPEN: `docs/closed-loop.md:109` still lists it under §9 Open.

**D25 — The console adjudicator reads the daemon's own stdin, and a head cannot answer
it.** New, and the direct consequence of making roles reachable. `--role coder` gets
`ConsoleAdjudicator::stdio` by default, which blocks on stdin: correct and usable for
`harnessd --prompt …` and for a daemon in a foreground terminal, and **not** usable for
the shape people actually run — a daemon in the background with `letibot-tui` attached.
The head's answer affordance is D10, which is specified (`Answer { option, note, free }`,
a protocol bump, an affordance in the head) and not built.

So today a write or exec seat is a foreground thing. The startup disclosure says so; it
is not silent. What it means in practice is that the reachability this strand added is
reachable **from a terminal**, and a head-driven write session waits on D10. Two ways
out and they are not equivalent: wire the adjudicator to the head (D10, correct, costs a
protocol bump and head work), or attach the model adjudicator (D13, which needs an
oracle and a reviewed always-ask list, and is what the survey's 83% is about).

— ✓ fixed in a97d7da4, test a_subagent_in_a_tree_with_no_head_adjudicator_refuses_by_name:
`ClientFrame::Answer` (`crates/sessionlog/src/protocol.rs:629`) and `HeadAdjudicator`
(`crates/harnessd/src/answers.rs:908`) are the head answering, so a write or exec seat is no
longer a foreground-only thing.

**D26 — A wake spends a generation, and nothing bounds a chatty monitor.** T24's wake
turns a firing into a user item and runs a turn. A monitor watching a path that changes
every second would spend the box's slots on itself until its TTL expires. The TTL is the
only bound and it is up to an hour. A firing that should be *noted* rather than *acted
on* has no spelling — and inventing one is a decision about what a monitor is for, not a
fix.

— ✓ answered on branch `remote/07767000`: **a firing is noted, not acted on** — a `Notice` row with
its own kind rather than a `User` row, folded to one line a monitor with the count of the burst, and
a monitor rings at most once a `FIRING_RATE` (30 s) window besides its TTL, so a burst cannot start
more than one turn (`crates/tools/src/exec/monitor.rs`,
`crates/harnessd/src/turn_tail.rs::may_ring`). The spelling D26 asked for is the row's kind, and the
tests are named in T24's entry in `TODO-settled.md`.

**D27 — Six adjudication items from the assembly agent, none filed until now.**
Carried out of a subagent's report and into scrollback, which is exactly the failure
T25 exists to prevent. In descending order of how often they bite:
`allow_session` does not stick (likely the `host_other` misclassification breaking a
class-keyed grant); `host_other` misclassification itself; `/mode` and `--mode` are
unwired; mode does not persist per project; a resumed session does not disclose a
tool-list mismatch; and the write prompt has no one-keypress answer -- the operator
had to type the whole word, and mistype it. Operator, 2026-09-10: *"ok so got the
write prompt, but it wasnt as a choice but as sometihng i have to type (and
mistype) myself"*.

**2026-09-13 (D13):** two of the six are now wired — *mode does not persist per
project* and the mode side of *`/mode` and `--mode` are unwired*. The per-project
`ModeStore` is threaded into `Parts` and consulted in `open_with` after the
workspace is resolved, so a session's point is its project's row (longest ancestor
wins), not the daemon's `--mode` default. A new named point `allow-all` (opencode's
`bypassPermissions`) ships, and `Mode::parse` accepts opencode's four permission-mode
names. Still open: the `/mode` head command to *move* a project at runtime, the
`todo_write` rename completion, and the remaining four D27 items.

— item 1, *`allow_session` does not stick*: ✓ fixed in 3ddf5f4f, test
at_always_ask_an_allow_session_answer_stands_for_the_session.
— item 2, *`host_other` misclassification*: ✓ fixed in 545d9d79, test
a_path_decided_baseline_does_not_report_a_region_it_did_not_decide.
— item 3, *`/mode` and `--mode` are unwired*: ✓ fixed — `ClientFrame::Mode`
(`crates/sessionlog/src/protocol.rs:558`), `/mode` in the head
(`crates/tui/src/app/commands.rs:1250`) and `apply_queued_mode`, test
a_mode_set_mid_turn_survives_a_restart.
— item 4, *mode does not persist per project*: ✓ fixed as the paragraph above records —
`ModeStore` (`crates/harnessd/src/modes.rs`), test a_mode_set_mid_turn_survives_a_restart reads
the row back the way a restart does.
— item 5, *a resumed session does not disclose a tool-list mismatch*: OPEN — no warning code
carries it (`crates/sessionlog/src/warning.rs` has none), and `reseat_unchecked` is about the
carry against the window, not the tools the prompt was built with.
— item 6, *the write prompt has no one-keypress answer*: ✓ fixed — the prompt is a card whose
option ids answer by prefix (`crates/tui/src/app/tests/decisions.rs:183`) or by digit-and-Enter
(`crates/tui/src/app/keys/cards.rs:351`, and `a_digit_inside_a_half_typed_line_is_not_an_answer`).

**D28 — Text selection in the TUI, still undiagnosed and needing one answer from
the operator.** The repaint hypothesis is REFUTED by measurement: before/after
frames byte-identical over 1,256,038 bytes, and 30 s idle wrote **zero bytes**.
Two candidates remain and one question splits them: *can you select while it is
idle, between turns?* Yes -> the complaint is mid-turn and is content change under
the selection, not gratuitous repaint. No -> it is not this code at all, it is the
TUI holding SGR mouse mode so the terminal hands drags to the application instead
of doing native selection, and the fix is a mode toggle.

— ✓ fixed in 2b49304e, test a_drag_holds_the_view_and_esc_lets_it_follow: `ctrl-p` holds the
view so the head writes nothing while a selection stands (`crates/tui/src/app/keys/mod.rs:886`),
and an idle head already writes nothing.

### Not ours, tracked because we caused or found them

**D23 — The 702 long chunks.** 702 of 43,967,653 exceed ~2048 tokens and already hold
ollama vectors of *truncated* text. Routing them to TEI makes new ≠ old for exactly those
documents. Small enough to re-embed rather than manage; lubuntu3's corpus, our capacity.

— OPEN, not this tree: a corpus on lubuntu3, and nothing here can re-embed it.

**D24 — The chunker length assertion.** lubuntu1's close, unowned: while chunks stay under
~2048 every backend agrees and none of the truncation split matters. Nobody enforces it.

— OPEN, not this tree: lubuntu1's chunker, unowned, and nothing here enforces the length.

---

## T17 - harnessd

**Status:** built 2026-09-09. M1's exit criterion is not met as written - C4 `f_keep` p10 was 0.8771 against 0.99 on the only 30-turn run, taken before D11's metric landed; `letibot-m1` ends with one `M1 EXIT` line and exits 1 unless it says `MET` (`crates/harnessd/src/m1.rs`). left: the 30-turn re-measurement in D11's metric (a live server), M0's opencode baseline (the comparison has no left side), C7's server half and C8's `length`, and C2 run as part of the exit.

`crates/harnessd`. Scripted 30-turn session against `qwen-3.8-flash-next`: 53
submissions, 25 tool calls, 161 persisted rows, 22,201 final prompt tokens, 85 s.

| check | result |
|---|---|
| C1 exact prefix extension | **PASS**, off the ledger's token vectors |
| C3 generation-inclusive prefix | never violated; 3/52 server-side shortfalls |
| **C4 `f_keep` p10** | **0.8771 — FAILS ≥0.99** |
| C4b cache efficiency p10 | **1.0000** |
| C5 reasoning replay | PASS (worst 0.9859) |
| C9 mid-session system change | PASS — stable prefix byte-identical |
| C10 disjoint cache accounting | PASS on all 53 |
| C6 / C7 / C8 | not run — see below |

**See T22 first — C4 is mis-specified, and the run measured what the plan asked for
rather than what the plan meant.**

**On the failure, and why I do not think it is the harness.** `f_keep`'s denominator
is the *whole* prompt, so a submission appending a 1,093-token tool result to a
9,000-token prompt cannot exceed 0.88 however perfect the cache is. 19 of 52
submissions add more than 1% of their own prompt. The **ceiling the script allows is
p10 0.8813, and we got 0.8771 of it**. Session-wide, 14,901 of 640,076 prompt tokens
were prefilled — **97.7% avoided**.

So **§17's exit criterion measures the script's shape as much as the harness**, and
≥0.99 is unreachable for *any* tool-using session. The harness-only number is C4b, at
p10 1.0000. **The criterion needs rewriting; the plan is wrong here, not the code** —
but it is recorded as a failure because that is what it is against the stated bar.

C6/C7/C8 could not run: C6 needs a tool-call-only assistant turn, which did not occur;
C7/C8 need a `length` finish, which requires either the `n_predict` cap §5.7 removed or
a 262k context on a production box.

— C6 and C8 are measured on HEAD now (`crates/harnessd/src/m1.rs:34,36`); C7 stays NOT RUN by
design (`:40-43`). The run above is unchanged.

**C3's shortfalls have an exact signature** — every one equalled `previous generation +
3`: the server reused to the end of the last committed item and discarded the 4-token
generation prompt *and* the whole generation. It is intermittent (14/55 on an earlier
run of the same script), so it is a server-side cache event, not structural. Settling it
needs `-lv 4` and `glm-why-no-cache` — a server-log question the harness cannot answer
from its own metrics.

— OPEN: the criterion is unrewritten — `docs/implementation-plan.md:360` still reads `f_keep`
p10 >= 0.99 — and no re-measurement under D11's metric exists in the tree (the metric landed in
cca619f8; the 30-turn run was never repeated).

### Per-clause status against `main`, taken 2026-11-03

M1's exit is two claims: **C1-C10 pass where opencode failed**, and **`f_keep` p10 >= 0.99 over a
scripted session**. The second names `f_keep` bare in the milestone table and §18.2 defines it as
`lcp / cached_entry`, which D11 settled; the table now says so, so the criterion no longer reads as
the `cached / prompt` ratio T22 proved unreachable.

**Every check now reads its evidence through a function that a test holds. Three clauses are still
not answered: C4 has no number in the settled form, C7's server half cannot be produced here, and
the comparison against opencode has no left side at all.**

| clause | on `main` today | what is missing |
|---|---|---|
| C1 exact prefix extension | read off the token vectors (`m1::c1`, two tests): a divergence names the token, and a run whose every turn failed is `NOT RUN` instead of a pass over zero submissions. The 2026-09-09 run: PASS | a re-run, below |
| C2 renderer fidelity | not produced by the instrument, deliberately. Answered offline by `crates/dialect-glm/tests/invariants.rs` (12 tests) and `crates/dialect-qwen` (36 across render, parse, lib and json); `tests/fidelity/run_gate.py` is the full gate and needs an oracle process | one command that runs the gate as part of the exit |
| C3 generation-inclusive prefix | `m1::c3`: `FAIL` only on `Violated`; a shortfall is `PASS*` and the exact `previous generation + 3` signature is counted. Live: never violated, 3/52 shortfalls | a re-run |
| **C4 `f_keep` p10 >= 0.99** | `m1::c4` reads `C4_KEEP` and prints the bar it judges by, in D11's form, with `f_sim` printed apart under its own name | **the number: the 30-turn run was never repeated after the metric landed (cca619f8), so no p10 in the settled form exists in the tree. The 0.8771 in the table above is `f_sim`** |
| C5 reasoning replay | `m1::c5`. Fixed on the way: the flag meant "any reasoning ever in the session", so from the first thought on every submission counted; it is now the turn being continued | a re-run |
| C6 `content: ""` never null | `m1::c6`. The 2026-09-09 run produced no tool-call-only turn, so its live verdict is `NOT RUN` | a session whose model answers with a call and no text |
| C7 truncated tool args | the client half is proven offline: `letibot_turn::length`'s tests, `one_truncated_argument_fails_the_whole_batch_not_just_itself`, and `FinishReason::Length` parsed off the wire | **the server half: a real `length`, which needs the `n_predict` cap §5.7 removed or a 262,144-token context** |
| C8 `finish_reason` | `m1::c8`, `PARTIAL` by design because no `length` was ever seen, and `NOT RUN` on a run with no submission, where it used to say `PARTIAL` over zero | the same `length` C7 needs |
| C9 mid-session system change | `m1::c9`, on the same collapse bar as C5. Live: PASS, prefix byte-identical | a re-run |
| C10 disjoint cache accounting | `m1::c10`; a run that asked nothing is `NOT RUN`, not agreement. Live: PASS on all 53 | a re-run |
| **"where opencode failed"** | nothing in the tree records opencode's per-check pass or fail, which is M0's exit | **M0's baseline: the comparison has no left side** |

**The run now says whether the criterion is met.** `letibot-m1` ends with one `M1 EXIT` line and
exits 1 unless it says `MET`: a check that never ran has not passed, where the old exit status was
"nothing said FAIL", which a run that measured nothing passed. C2 and C7 are named on that line as
measured elsewhere rather than counted as failures.

**What is still missing, in the order it would close.**

1. **The re-measurement.** Every number in this entry is from 2026-09-09, before D11's metric
   landed, so the exit's own p10 has never been taken in the settled form. That is one 30-turn run
   against a local server, and nothing in the instrument blocks it.
2. **M0's opencode baseline.** "where opencode failed" needs a recorded per-check pass or fail for
   opencode; no run of C1-C10 against it is in the tree.
3. **C7's server half**, and with it C8's `length`. Both need a `length` finish the plan forbids
   producing, so they stay `NOT RUN` until a 262k context exists on a production box.
4. **C2 as part of the exit.** The gate is answered by other tests; the exit does not run them.

— OPEN: the four above. Nothing in `crates/harnessd/src/m1.rs` is unproven now: every check reads
the evidence through a function with a test that fails when the evidence violates it.

---

## T19 - Rewrite or not, on the KV representation

**Status:** rows 1 and 2 are settled negative, measured (`experiments/kv-stitching/RESULTS.md`). left: row 3, which should not be built as written yet - it is the one checkpoint at `TokenLedger::prefix_len()` and not section 3.4.1's placement. The section 6 measurement that says whether it pays needs the :8080 server restarted, which is the operator's.

The operator's framing, and it is the right one: llama.cpp's data structures are not
the question. The question is **what would require a rewrite, and whether that rewrite
pays.** Separating the two changes what T14's negative actually settles.

### llama.cpp's choices — changeable

- recurrent cells *are* sequence ids, so a save is one 111.4 MiB blob per sequence
- attention KV and recurrent state are saved as a single unit, all-or-nothing
- rewind has **three fixed re-entry points**: free to `d ≤ 3`, one checkpoint to
  `d ≤ 516`, then **the entire prefix from token 0**

### Mathematics — survives any rewrite

A recurrent state is a **fold**: the state at position *n* encodes all *n* tokens, so
there is no "block B's contribution" independent of what preceded it. It is not stored
badly; it does not exist as a separable quantity.

This is also why CacheBlend works on attention-only models and cannot here. Attention
has cross-block dependence too, but it is **diffuse** — a weighted average whose
distribution barely shifts under a different prefix, so recomputing the ~15% that
deviate most corrects the rest. A fold has no diffuseness to exploit: there is no
subset whose recomputation repairs the remainder.

### The split

| | verdict | why |
|---|---|---|
| composition across a divergence | **do not rewrite** | mathematically blocked, not an implementation limit |
| splitting attention from recurrent in the save format | **do not rewrite** | after a divergence every layer differs anyway, so there is nothing to reuse |
| **checkpoint density / rewind granularity** | **worth doing** | pure implementation; the `d > 516` cliff is checkpoint placement and nothing else |

**The third row is the actionable one and it was measured by accident.** Rewind is what
fork (§5.5), backtracking and eviction all actually need, and today it falls off a
cliff at 516 tokens. Storing recurrent state at more positions makes rewind cheap. That
is a parameter and a representation choice, not a redesign.

— rows 1 and 2 are measured now, not argued: `experiments/kv-stitching/RESULTS.md` answers
UNVERIFIED-16 negative (T14), so composition across a divergence and a split save format are
both settled against a rewrite.
— OPEN: row 3 is unbuilt — `docs/implementation-plan.md:71` still lists §3.4.1 checkpoint
placement among the six unlocks "none landed", and nothing in `crates/turn` places a checkpoint
or re-enters a rewind (`grep checkpoint` finds only `crates/turn/src/prefix.rs`'s shortfall
reason), so the `d > 516` cliff still stands.

**Status — read with `docs/kv-representation.md`.** Rows 1 and 2 stay settled and the rewrite
question is answered: the only rewrite left would re-buy, for a fork of
`llama_memory_recurrent` and three models to keep, what a checkpoint placement buys for one
flag. Row 3 should **not** be built as written yet, because the code says this harness never
asks for a long-suffix trim — there is no rewind, no backtrack and no truncate on the token
region, so the one request in this tree whose prompt shrinks is a **local compaction**
(`plan_compaction_tail` returns `TailPlan::None` when `!remote`,
`crates/harnessd/src/harness/wall.rs:1171`), whose new base is stable prefix + one summary
item and which therefore re-enters at the *end of the stable prefix* — a position no chat
checkpoint is due at, so the arithmetic says its first turn re-prefills from token 0. That is
a prediction and not a measurement, and it is invisible today: the witness lives on the engine
`Session` and a fork opens a new one (`crates/turn/src/engine.rs:390`), so the first turn of
any new base is `FirstTurn` and `cache_reuse_shortfall` cannot fire on it. What row 3 should
chase is §3.4.1 in placement form — **one checkpoint at `TokenLedger::prefix_len()`**, the
position the harness knows and the server cannot guess — and the one experiment that settles
whether even that pays is §6 of the doc: a post-compaction turn's `cached_tokens` at 40 k and
120 k, with `--checkpoint-min-step` at its default and lowered, `/slots` for the bytes. Arm B
needs the operator: the :8080 server is never restarted for a measurement.

---


---

## T18 - The prompt cache costs ~120 KiB/token on recurrent models

**Status:** operational; it caused a fifth OOM. left: nothing in this tree - every cost is the server's. Nothing sizes or caps the per-sequence accumulator, `--cache-ram` or `-c`, the eviction and warm/cold figures have no consumer, and `--cache-reuse`'s dense-model disable has no explanation.

Found while running UNVERIFIED-16, which **OOM-killed `qwen-flash-next` at 14:35 on
2026-09-09** (158.1 GiB peak; systemd restarted it, `qwen-slots-restore` put the slots
back, ~40 s down, in-flight requests from two other agents lost).

Cause, and it generalises: **every distinct prompt prefilled becomes a prompt-cache
entry at ~120 KiB/token, because 111 MiB of each entry is the recurrent accumulator** —
fixed per sequence, independent of length. A sweep over many short distinct prompts is
therefore far more expensive than its token count suggests. `server-context.cpp:4549`
records four previous occurrences; this was the fifth.

— OPEN: a fact about the server, not this tree — nothing here sizes or caps the per-sequence
accumulator; the only guard is the sweep's own floor (`experiments/kv-stitching/srv.py:29`).

**A second cost, measured on a DENSE model, 2026-09-14 by `lubuntu1-lab` on .76
(Qwen3.8-27B Q6, llama.cpp, stock).** Different mechanism from the recurrent
accumulator above and it compounds with it:

- A saved prompt-cache state costs roughly the **full `-c` allocation, not the
  tokens actually used** — ~1.5 GiB each at `-c 8192`. So capacity is
  `cache-ram / per-saved-state`, and the 8 GiB default holds about five
  conversations. At 40 GiB: 16 of 16. Two levers, and only two: raise
  `--cache-ram`, or lower `-c`.
  — OPEN: operational and the operator's — no code or config in this tree sets `--cache-ram` or
  `-c`; the server is `~/bin/glm-flash-server`, outside the tree.
- Eviction is **capacity, not conflict**: interleaving A,B,A,B,A,B hits 94–95 %
  once each has been seen. So the number to size against is *how many distinct
  briefs are live*, not how they are ordered.
  — OPEN: a measurement with no consumer — nothing in the tree sizes a pool by live briefs.
- Warm 644 ms median against cold 1986 ms; the first touch of a new conversation
  is always cold.
  — OPEN: a measurement with no consumer — nothing in the tree records or asserts it.
- **`--cache-reuse` is silently disabled on that model** — `cache_reuse is not
  supported by this context, it will be disabled`, logged with `--kv-unified`
  both true and false, so unified KV is not the gate. The 94 % hits are the
  slot's own longest-common-prefix match, not `cache_reuse`. Advice to rely on
  that flag is therefore **model-dependent**, and a measurement taken on one
  model does not port. Nobody has yet established what makes a context
  unsupported; the suspicion is an attention property rather than a flag.
  — OPEN: answered for a hybrid and not for the model it was measured on —
  `experiments/kv-stitching/RESULTS.md:99` gives the gate as `ctx_tgt_has_recurrent_state`
  (`server-context.cpp:5763`), which cannot be what disabled it on the DENSE 27B this bullet is
  about; no doc explains that case.

Mitigation used, and worth generalising: the sweep harness refuses to start a condition
below 40 GiB `MemAvailable`. Any future experiment that prefills many distinct prompts
needs the same guard.

— fixed in b82b906b in code, untested: `experiments/kv-stitching/srv.py:29` refuses to start a
condition below `guard(floor_gib=40.0)` of `MemAvailable`, with this OOM named in its docstring.

---

---

### 2026-09-11 — the sixth OOM, and why the disk tier did not save the turn

GLM was **killed by the kernel OOM killer** at 01:35:34 (`status=9/KILL`), taking a live
letibot turn with it: `malformed http response: connection closed mid-chunk: the server
went away before the terminating 0-length chunk`. Host RAM, not VRAM.

**The good news first, because it corrects a standing fear.** The spills survive a hard
kill and are re-used. 957 GB across 506 files, and the restarted server indexed them:

    L2: indexed 386 cache entries from disk (23,128,678 tokens, 931,079 MiB)

and then served **6,427,050 cached prompt tokens against 240,831 prefilled** — 26.7×
reuse, resuming a 171,901-token conversation rather than re-prefilling it. Critically,
`forcing full prompt re-processing` appears **zero** times, so the plan's "Root cause B"
— a disk restore that loaded the KV and then discarded it because `checkpoints` was
empty, which is exactly the hybrid/recurrent case — is **fixed** in this build. That is
the first evidence of it holding across a `SIGKILL`.

**What is wrong is that the tier is read-only in practice.** Every idle cycle logs

    flush: 0 sequence(s) with tokens, 0 saved, 0 already cached, 0 offloaded (0 saved),
           cache now 386 entries / 0.0 MiB

`0 sequence(s) with tokens` — by flush time no slot holds anything to save, which is
`spill-only-writes-cached-prompts` in the memory dir: *prompt_save is false for an
already-cached prompt, so live slots never spill*. So it serves brilliantly from what was
written before the kill and **adds nothing**. Any conversation that grows from here has
no disk entry, and that one really will prefill from zero after the next kill.

**And the shape of it is perverse.** The RAM tier is **write-back** — entries reach disk
on eviction or graceful shutdown — and write-back is precisely what a `SIGKILL` defeats.
`--cache-ram 81920` therefore both raises the chance of the kill and maximises what the
kill destroys. `/data` is a 7.3 GB/s NVMe with 5.3 T free; **write-through survives OOM
by construction** and the read side is already proven fast enough at 26.7× reuse.

**Also costing something:** 48 `index_disk` calls in one log, ~20 minutes apart, each
re-reading metadata for 931 GB across 386 entries — the sleep/wake cycle
(`--sleep-idle-seconds 600`) paying the index once per wake rather than once per start.

**The decision, and it is the operator's:** lower `--cache-ram` (less pressure, less
lost, more disk reads), or make the RAM tier write-through (survives the kill, costs
NVMe bandwidth that is measurably spare). Not a letibot change either way — it is
`~/bin/glm-flash-server` and llama.cpp's `server_prompt_cache`.

---

## T3 - Report the minja scoping bug upstream

**Status:** writeup ready (`docs/chat-templates.md` section 2), not filed - nothing in `docs/` records a link or an issue number. left: the operator files it, since `llama.cpp/AGENTS.md` makes reports and PR text the operator's to write.

`{% set %}` inside `{% for %}` is not scoped per iteration in llama.cpp's minja.
Repro needs no GPU: a template, four messages, one `/apply-template` call.
Full writeup and both engines' output in `docs/chat-templates.md` §2.

Affects any llama.cpp user serving GLM with `messages` — the model is conditioned on
reasoning it never produced. We are insulated because we submit token ids.

— OPEN: ready and unfiled — nothing in `docs/` records a link or an issue number, and the
writeup at `docs/chat-templates.md:55-65` cites no upstream report.

---

## T6 - Workstreams not started, from `docs/workstreams.md`

**Status:** the list below is stale - `docs/workstreams.md:20-27` maps W6 to `crates/turn`, W9 to `crates/tools`, W11 to `tools/src/adjudicate` and W13 to `turn/src/compaction`. left: W1 and W2 wait on `pytest`, which is not installed (`python3 -m pytest` fails), and this list needs rewriting from the workstreams doc.

W1 measurement rig · W2 prefix-invariant suite (grok port, Apache-2.0) · W6 turn
engine (**critical path**) · W7 session log and head protocol · W8 heads · W9 tool
runtime · W10 firecode substrate · W11 adjudication · W12 flowy connector · W13
segments and compaction · W14 EXPLAIN · W15 server track · W16 experiments.

`pytest` is still not installed; `docs/workstreams.md` calls that blocking for W1/W2.

— OPEN: the list is stale — `docs/workstreams.md:20-27` now maps W6 to `crates/turn`, W9 to
`crates/tools`, W11 to `tools/src/adjudicate`, W13 to `turn/src/compaction`, so only W1/W2 and
the docs' own status still wait; `pytest` is genuinely absent (`python3 -m pytest` → `No module
named pytest`).

---

# Parked

Stopped or deferred on purpose, with the reason in the entry's status line.

---

## R120 - One seat and a working automode, instead of roles

**Status:** parked by the operator on 2026-11-03. No open, locally hosted model passes the etalon oracle corpus (`crates/harnessd/src/etalon_oracle.rs`), and automode without a passing oracle is a model approving its own actions. left: everything, until a model passes the corpus.

The operator, deciding N4: "the more i use things the more i understand the roles were overcomplication - we need working automode". N4 (a coder can write a test and cannot run it) is superseded: the answer is not a shaped verb for one role, it is fewer roles.

- One working seat that can read, write and run commands. What it may do without asking is the MODE's business, not the role's. The coder/runner/m2 split and role switching go, except where a confined seat is the point (a VM or a placement).
- `automode` works out of the box: today it requires an adjudicator and a reachable oracle and refuses without them. Its default decider is the session's own provider or a cheap fast model, with no setup. It admits in-checkout reads, writes, builds and tests; asks the operator for the network, paths outside the project, the always-ask list and destructive or irreversible actions; tells the model why when it denies; and records each decision with its reason, so the decisions can be reviewed and measured (the etalon oracle corpus exists for that).
- It must be predictable: the same call in the same context gets the same answer, and the operator can read why.

N4 moves to TODO-settled.md as superseded by this entry.

Parked by the operator the same day. There is no default oracle because every open, locally hosted model tried so far FAILED the oracle tests (the etalon corpus, `crates/harnessd/src/etalon_oracle.rs`), and automode without a passing oracle is a model approving its own actions. The work resumes when a model passes the corpus; until then the seats keep today's modes.

---

## R84a - Worktrees and landing trees beside the checkout (`wt/`, `landing/`)

**Status:** parked by the operator on 2026-10-12. It was built on branch `parked/layout-wt-landing` (`e7f4e55`), which is not an object in this checkout; main has no `crates/harnessd/src/layout.rs` and the landing tree is still `<data dir>/landing` (`crates/harnessd/src/mergequeue/git.rs:435`). left: the three behaviour changes below, which the operator has not ruled on, then rebasing the branch onto main.

The operator proposed the placement: *"so id suggest to correct the placement - ~/Projects/rano/rano
<- the main checkout and then ~/Projects/rano/wt/* ~/Projects/rano/landing/*"*, and for a repository
not inside its own project directory chose a per-project option with a fallback. It was built, then
parked the same night: *"on placement - lets park it in todo. keep worktree, fix that .git thing with
something small. i dont have time to reason about proposed behaviour changes now"*. Main keeps R84
(landing in the data dir, out of `.git`) and subagent worktrees stay in `<workspace>/.claude/worktrees`.

What `e7f4e55` does, for when it is picked up (rebase it onto main; it was cut on top of `d27ed56`):

- **The rule** (`crates/harnessd/src/layout.rs`), first match wins: `$LETIBOT_LAYOUT_ROOT`;
  `[layout] root = "<dir>"` in the project's `leticode.toml` (absolute or `~/`; relative, `..` and
  unknown keys refused by name); the X/X layout (`~/Projects/rano/rano` → root `~/Projects/rano`,
  so `wt/` and `landing/` in it); otherwise siblings named after the checkout
  (`~/Projects/firecode-wt`, `~/Projects/firecode-landing`).
- **Landing** at `<landing>/<target>` (branch name sanitised to one folder), replacing R84's data-dir
  path; migration of `.git/letibot-landing` as in R84.
- **Subagent worktrees** at `<wt>/agent-<slug>` instead of `<workspace>/.claude/worktrees/`; existing
  ones not moved. Because `wt/` can be outside the workspace, a session's write access to its
  children's trees is kept three ways: the sandbox binds `wt/` read-write, the file tools reach it
  through `HostBackend::with_extra_root`, and the permission gate treats it as the workspace
  (`Surroundings::worktrees`).

**The behaviour changes the operator has not ruled on** — the reason it is parked:

1. With the X/X layout and a session rooted at the project directory (`~/Projects/rano`), `landing/`
   is inside the workspace: the sandbox sees it read-only but `write`/`edit` can still write there
   (their check knows only the workspace root). Closing it needs a per-session read-only path in the
   backend.
   — OPEN: still parked — `e7f4e55` is not an object in this checkout, there is no `crates/harnessd/src/layout.rs` and no `[layout]` key, and the landing tree is still `<data dir>/landing` (`crates/harnessd/src/mergequeue/git.rs:435`).
2. Child daemons, whose workspace becomes `wt/agent-*` outside the checkout, no longer take the
   repository's merge-queue lease; only the project's own daemon serves its queue.
3. Every place that assumed `.claude/worktrees` moves with it (grants, cleanup, texts).

---

# Review records

Kept verbatim and not repeated here: the five read-only reviews of 2026-10-12 (R89 to R93) are in `docs/reviews/2026-10-12-edge-cases.md`. The four stabilization reviews of 2026-10-11 (R61, R67 to R70) are in `docs/reviews/2026-10-11-stabilization.md`.
