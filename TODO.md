# TODO

Work queue, open work only. The INDEX below has one row per open entry. A settled entry is deleted
from this file: its commit message carries the settled line (what closed it, the commits and tests),
and the operator's private archive (`hands-off/letibot/TODO-settled.md`) holds the history. Review
records are in `docs/reviews/`. Nothing closed belongs in this file.

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
4. **If you find it already done**, delete the entry and say in the commit message what closed it
   (commits and tests), then stop. That is a complete, valuable outcome.
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

# INDEX

One row per open entry. `left` is what is really left, checked against the tree. Entries marked "in flight on a branch" are being changed right now and their text is not rewritten here.

| id | title | group | left |
|---|---|---|---|
| R74 | What the 2026-10-11 landings left open | Code work | item 39 (a live gate on lubuntu1/2: the seam and the rows' naming are held by tests, and the fleet credential is not in this placement) |
| R55 | A round's end and a turn's end are the same event on the wire | Code work | leticl's `:turn-finished` arm (`src/session/seq-gap.lisp:254`) does not read `ends_turn`, so every round's end is drawn as the turn's end |
| R121 | What a command boundary holds is hardcoded and invisible | Code work | items 5-7 (perimeters as config, children narrow and widening is an ask) — items 1-4 and 8, including the file tools' fence and the probe line, are built |
| R126 | Reviewers fan out to the declared hosts, opt-in | Needs the operator | setting `[queue] reviewer_hosts` in the operator's `leticode.toml` and one live review on lab2x1 (the fan-out is landed and tested against fakes, never against a real host) |
| R125 | Script plugin layers: where a JavaScript engine comes from, and the JS-side contract | Needs the operator | the two decisions, then `docs/plugins-plan.md` step 11: no JavaScript engine is available offline here, and a vendored one compiled in would run freely as a built-in |
| R114 | Embeddings as the second similarity tier, for every similarity caller | Code work | item 3's measurement from the take-up logs (are the misses synonyms?), the paraphrase case scored against the worst measured negative, the per-round cost on lab2x1 |
| R118 | A GitHub reviewer seat: `letibot attach github` reviews every PR and answers as @letibot | Code work | the seat in the daemon so the pane has a feed, a mirror per repository, the review panel, R116's verdict, R114's similarity offers |
| R116 | Claude Code as a merge-queue reviewer through headless `claude -p` | Code work | all of it - no `claude -p`, `--json-schema` or `structured_output` use exists under `crates/` |
| R117 | The leticode MCP bridge pushes package and queue events into Claude Code as a channel | Code work | the plugin; a self-published channel plugin still needs `--dangerously-load-development-channels` during the preview |
| R127 | The idle nag names every free head of the plan, so independent rows run in parallel | Code work | all of it - `the_row_to_name` (`crates/tools/src/builtins/todo.rs:1333`) picks one ready row; postponed by the operator on 2026-11-07 |
| R129 | One store file per project and a shared file, so one daemon's write cannot stall another's | Code work | the build, in five packages at the end of `docs/store-split.md` (P0 first: the instrument cannot yet tell a lock wait from I/O); the design, the measurement and the build plan are in the doc on branch `r129-store-split`, not merged |
| R130 | The seat's `GH_TOKEN` is readable in `ps` from bwrap's command line | Code work | the operator rotates the token; a command this box spawns and a remote placement both keep every secret off every argv now |
| R132 | `skills_path.rs` refuses its own session as written by another process, once under load | Code work | the root cause; seen once in a full gate, 6 reruns and the next gate passed |
| R134 | Scrolling stops at a line above the tail, only Esc frees it | Needs the operator | the next stop's line from `~/.local/state/letibot/scroll-stalls.log`, which the head now writes at each stop |
| R136 | Answer waits: a row waits on another top-level session's answer | Code work | plan steps 1, 4, 5, 7, 8 (the room's asks, the `answer`/`decline` verbs, the passes, end to end, docs) on room protocol 10; steps 2, 3, 6 (the todo side and the head) are landed |
| R137 | Todo graphs that span projects | Code work | the design; an answer-wait ask is the first cross-project edge |
| R138 | Stable session identities across hosts, through migrations | Code work | the design; until then asks are same-host only |
| R144 | The MCP bridge gives Claude Code seats a todo board with `wait_on` and the idle nag | Code work | the head's pane for a seat's board; the board, its waits and its nag are landed |
| R152 | `tests/launcher.sh` stubs outlived their run, and four of its cases had drifted | Code work | putting `tests/launcher.sh` in the merge gate |
| R149 | A placement caps a package's build width from the host's free cores | Code work | all of it; postponed by the operator on 2026-11-07 |
| R153 | Placing a session on a busy host times out at 20 s | Code work | all of it |
| T16 | W9's open questions | Needs the operator | an answer to item 6: does RAGFlow signal "no coverage" in a field |
| R150 | Plugin extension points: the harness and head open to plugins | Needs the operator | answers to the 13 questions in `docs/plugin-extension-points-wide.md` §8 (each has a default), then packages K1-K11, P5 and P6; H0 (the pane contract types), K7 (the `adjudicate_` floor, wired into the daemon's gate) and the room UI P0-P4 are landed |
| R113 | Skills are offered like notes; a live check proves the whole path | Needs the operator | item 4's live check on lab2x1 (`/skills` hands a test skill over, `task {skills}` preloads it in a child, an unknown name is refused) |
| R100 | A remote session in a firecode VM: `--host` and `--vm` as one placement | Needs the operator | the live proof on lab2x1: a real `firecode` copy carrying a commit out, a resumed session keeping its clone, the guest's `cargo test` with no card, the copy-out's cost |
| R59 | The context wall's denominator moves a third between firings | Needs the operator | the operator's ruling on smoothing the ratio (the tree keeps it per round, as recommended) |
| WEATHER | Which side owns WEATHER: one classification both heads answer | Needs the operator | the operator's ruling on `model_endpoint_retry`; unclear: `6b52aae6` now draws a retried model call as one row that moves and settles, check whether that closes the ruling |
| R13 | Cloud GLM, DeepSeek and Grok as turn backends | Needs the operator | a live run behind a real provider key (none on lab2x1); the providers' prompt caches are only asserted (`PrefixGuarantee::Asserted`, `crates/backend/src/lib.rs:230`) |
| R66 | A harnessd reached 113 GB | Needs the operator | the LOCAL-model route (needs a local server and a GGUF on this box), and the incident's own 113 GB |
| R65 | The model chooses each tool call's memory cap | Needs the operator | the cross-session ledger, a proposal the operator has not decided |
| N5 | The agent should know the view is hermetic, and propose the grant it needs | Needs the operator | which asks must be refused rather than prompted: `grant_view` raises an ordinary card for `~/.ssh`, with no never-write precheck |
| T25 | Everything waiting on the operator | Needs the operator | the operator's answers to D14, D15, D17, D20 to D24 and D26; unclear: D13, D16, D18, D25, D27, D28 carry no status line, check each against the tree |
| T17 | harnessd: M1 exit criterion not met as written | Needs the operator | the 30-turn re-measurement in D11's metric (one run against a local :8080 server), M0's opencode baseline, C7's server half and C8's `length`, C2 run as part of the exit |
| T19 | Rewrite or not, on the KV representation | Needs the operator | row 3 (one checkpoint at `TokenLedger::prefix_len()`); the measurement that says whether it pays needs the :8080 server restarted |
| T18 | The prompt cache costs ~120 KiB/token on recurrent models | Needs the operator | nothing in this tree: the cost is the server's (`--cache-ram`, `-c`, per-sequence accumulator cap, eviction and warm/cold figures, `--cache-reuse` on dense models) |
| T3 | Report the minja scoping bug upstream | Needs the operator | the operator files it; the writeup is `docs/chat-templates.md` section 2 and no upstream link or issue number is recorded |
| T6 | Workstreams not started, from `docs/workstreams.md` | Needs the operator | install `pytest` (W1/W2 are blocked on it); this entry's list is stale and needs rewriting from `docs/workstreams.md` |
| R120 | One seat and a working automode, instead of roles | Parked | everything, until a model passes the etalon oracle corpus |
| R84a | Worktrees and landing trees beside the checkout (`wt/`, `landing/`) | Parked | three behaviour changes the operator has not ruled on, then rebasing the parked branch |

---

# Code work

Things an agent can do in this repo, ordered by value: stability and UX first.

---

## R93 - What the model is told: tics, leaked numbers, contradictions, schema mismatches

**Status:** SETTLED 2026-11-05. Filed 2026-10-12; items 1, 4, 5 and 6 were done in R93b (2026-11-04), A1 to A7 are in, and the rest was worked on 2026-11-05: item 3's last clause (B1), item 7 (every F finding, re-read against the tree - each one either was already fixed or is fixed here) and item 8 are done. left: nothing a session can do. The one residue is a platform note - `restart_restore`'s integration tests cannot open a harness on a box without `/sys/fs/cgroup`, so the restart wording has no end-to-end read there.

From real data (`sessions.db`, fork copies removed: 4,347 injected items) and the code. The full
report has the exact text and a replacement for every item — work from it. **Every lettered item
in that report (A1–A7, B1–B13, C1–C14, D1–D4, E1–E16, F1–F58, G1–G10) is an entry of R93 in its
own right**, addressed as R93.<id>; the list below groups them by what to fix first:

1. **The `[context check]` quotes the model's own sentence back, ✓ marks and all** (`context_claim.rs:460-466, :517-523`). The "my context N%" tic began two minutes after the first numbered check in s-1789919514688401228 (108 of 113 live checks are there); the latest one still quotes "Hmm — **my *context* ✓** ✓ ✓" (14×). Stop quoting. Also A2 (`repeated` reaches the model via `harness` warnings), A3 stale doc comment, A4 (`context_fill` `harness.rs:12341` ignores `--no-auto-compact`), A5/A6 (reply/reasoning variants), A7 (`digest.rs:192-197` invites context sizing). [model A1–A7]
   - **A1–A7 are all in** — the item is done. A1/A5: `notice` and `reasoning_notice` quote nothing and say *if context was the only reason you stopped*; A2: the `harness` tool's `warnings` view withholds the repeated-claim detail by code, so the quote and the numbers stay the operator's (`builtins/harness_view.rs`'s `WITHHELD`, test `the_repeated_context_claim_is_withheld_from_the_model`); A3/A4/A7 in `9031b890` and `7d876be2`; A6: a claim found in the reasoning is said only where a nudge continues the turn, so it is not the last row of a finished turn (`harness/turns.rs::run_rounds`, test `a_reasoning_claim_on_a_turn_that_ends_is_not_said`).
2. **The memory/box note is on 93% of bash results** (3,979/4,256 today; subagents ~95%) and peak/cap on every job — resource sizing the model should not size by. Say it only on a kill or near the cap. `bash.rs:140-168, :1077-1081`; `exec/memory.rs:236-243`; `exec/host.rs:221-235`; `jobs.rs:378, :713-717`; `jobwatch.rs:1269`; `mergequeue/gate.rs:252-257`. [model F1–F8, C4]
3. **The todo nag**: 45% name a row already in progress (793/1,778); 45% got no tool call back, only restated status (113× "Not done: three boxes open…"); "(N more open)" in the compared text resets the R77 backoff; criterion rows nagged every window (124× one row); `nothing_can_start` fires on job/young-child roots `todo_write` promised were silent (`todo.rs:2381-2416`, `:1813`); `attach_now` appended with no plan or no `todo_write` seated (`todo.rs:1355-1360`); 60–70-word advice on every nag. 393 identical nags at 66 s in one session were the pre-R77 metronome, fixed by ebc2af7. [model B1–B13]
   - **B4, B7, B8 and B13 are in**; B2/B3/B5/B6/B9–B12 landed earlier (`todo_write`, the idle clock). B4: a row whose own words are a running job's description is work in flight, so the check is silent instead of offering the wait it is already doing (`todo.rs::unfinished_plan_for`; tests `a_row_a_running_job_reads_like_is_not_nagged`, `the_offer_still_names_a_running_job_the_row_does_not_read_like`). B7: a need on a row the operator struck off is repaired by re-sending the row in `todos`, not by re-wording it, which adds a row and leaves the old one blocked (`blocked_line`). B8: the unknown-argument refusal names every door `wait_on` has — job, child, the operator's answer, `detach` (`tests/r93.rs::the_argument_list_does_not_call_wait_on_job_or_subagent_only`). B13: the tool's description is four paragraphs and one line per field, about 1,100 words before, guarded by `the_description_is_one_short_paragraph_per_field`. **B1's first clause** — a row already InProgress was still asked about at the next window; the report's fix is to leave it until it has been idle for the cap interval. **It is in (`ac7993e6`)**: `TodoNagClock::rearm` arms at `NAG_REPEAT_CAP` when the row the check would name is one the model has marked in progress (`sessions::the_named_row_is_in_progress`, read through the same two doors the arming takes), and the repeat keeps that floor; a row nobody has started keeps the base window (`a_row_the_model_marked_in_progress_is_left_until_the_cap`). The which-row rule is unchanged on purpose: `an_open_plan_names_one_item_and_what_to_do_about_it` still has the in-progress row named, only later.
4. **Completion texts contradict themselves and arrive after the model already read the result** (21% of `[task]`, ≤6% of `[job]`): "don't call `task_result` … read it with `task_result`", placeholder "…" for a known id, the command echoed in full, `[meanwhile]` heads explaining the holding policy (and leaking the 10-min cap), `[monitor]` announcing the model's own retire as a firing, plural bugs. `harness.rs:1064-1148, :1260, :990-1004, :8657-8660, :13552-13554, :14865`; `delivery.rs:356-368`; `deliverable.rs:97-113`. [model C1–C14]
5. **"You keep overthinking"** leaks `reasoning_only`, says "Reason after" to models that cannot, and D2 is factually wrong (`harness.rs:11367-11387`; `compaction.rs:243/253`); the length-cap and progress-watcher texts are long (`length.rs:139-141`, `progress.rs:246-268`). [model D1–D4]
6. **Compaction/reseat/restart/stale notes**: `continue_after_wall` names the "context wall" and can fire with nothing compacted (`harness.rs:9037-9040`, `turn_tail.rs:177`); **literal 14-space gaps** in the cut note (`harness.rs:10520`); transcript ids, R27 and KV-cache internals (`event.rs:1061-1080`); reseat notes stacked 2–3× and `[reseat]` asks for a one-line restatement and re-plants `/reseat` (`harness.rs:2252-2283, :10581-10584`); the stale notice is 120 words of operator text (`:2205-2209, :2483-2514`); `[restart]` contradictions, nested tags, `pkill` hint missing the required `pattern`, `wait_on` named as a tool (`restart.rs:266-303, :353-354, :439-440, :480-482`; `subagent_record.rs:393-411`); `[cleared]` leaks ids (`harness/clear.rs:171-178`). [model E1–E16]
7. **Tool descriptions and results contradict the code**: `wake_sentence` (F12) vs R79 and six other texts; `bash` says a foreground command is moved to the background at `timeout_ms` but it is killed (F14, subreviewer's claim, not re-read); `job_wait` says it blocks but returns at once (F16–F18); `task`'s schema lacks `model` (F21) and `task_start` lacks `wake` (F22); `monitor`'s description contradicts its schema (F28); `retire` renders "it had retired by the model" (F29); two list tools (`todo` vs `todo_write`) and the intent check names the unseated one (F30); merge-gate text tells the model to commit AGENTS.md to main (F31); **literal `{raw}`** in `notes.rs:374` (F42); **~18-space gap** in `gate.rs:218` (F10); duplicated clause in the predicate note, 436× (F27); the snapshot-budget note 5,611× (F38); `[repaired]` notes every call (F56); the intent-check threat on `todo`, 163×, for a check off by default (F58); and the rest F2–F59 in the report. [model F1–F58]
   - **Item 7 is done**, worked 2026-11-05: every finding was re-read against the tree, and each one either was already fixed or is fixed here. Fixed in `c1a7a569`: F23 (the repo-specific format-check brief is named as this tree's, not as a rule of the tool), F25 (the `pkill` and `ps` tools named where the text told a model not to use them), F26 (a process the harness did not start is the `pkill` tool's), F30's residue (`intent/ledger` no longer names a tool a seat may not have), F32 (no internal ticket number, and the door `grep`/`read` is named), F33 (the boundary note is two sentences, and the grant paragraph is unchanged), F34 (one spill notice: the `[note]` copy in `result.rs` went, and `SpillRef.notice` with it), F36 (a redirected job's answer is an `Ok`, so `NO_RESULT` no longer wraps *read the file*), F37 (`[the operator moved … to the background]`), F41 (no score internals in the near-copy refusal; `a_paraphrase_with_no_shared_word_is_refused_by_the_second_tier` now asserts what it teaches instead), F43 (`WHEN_READ` says session open and base rebuild, which is when the reader runs), F45 (`P(allow)` without the oracle's latency). Fixed in `68b2a448`: F35 (the provider sends the payload `render` built, which already names its outcome: `items_become_the_apis_messages_without_tools`), F40 (`[note offer]` names the path and the door and no similarity score, keeping whose line it is), F47 (`TOOL_WAYS` is one clause) - and F47 is the one that bit: the head splits a session notice after `NOTICE_END`'s words, so re-blessing the fixtures was not enough and `red_card::NOTICE_END` now takes the new closing and the old one (`a_notice_splits_after_the_closing_it_was_written_with`). Fixed in `719b4d51` and `4bc2295a` (the item-2 tail and the queue's own texts): F1 (`memory_note` says nothing about a run that ended inside its cap; `a_finished_call_says_nothing_about_its_peak`), F5 (`ask_after_kill` is one ask sized to start, and the kill sentence states the cap once; `a_kill_at_the_cap_says_one_ask_sized_to_start`), F6 (the operator's refusal says the ask in the model's own words, with no free-of-total), F9/F11 (`killed_at_cap_words` and the killed-gate notice say the cap is the operator's to raise and name no providers.toml key), F44 (`harness` what=warnings withholds `auto_compact`, `memory_high` and `memory_creep` as well as `context_claim_repeated`, and the line says the numbers are the harness's), F46 (`stopped before line {n}; call read again with offset={n}` - the KiB cap is the reader's number), F48 (the red-target notice says a reviewer and a fix branch are automatic), F51 (`verdict`'s schema says what `accept` means in a review of a red tip), F52 (`desk` names the tool's `repository` first), F53 (no "the verdict is bound to the head it judged"), F54 ("say in your reply", not on a log), F55 (a reviewer's cards are the operator's, not "YOUR head"), F57 (`write`/`edit` say the backend is read-only, not what gate it is below). F4 is half: the duplicate sentence went, and the sentence naming `[memory] default` stays because it names a real knob in the tree. Already fixed in the tree, verified here: F2, F3, F7, F8, F10, F12–F22, F24, F27–F29, F31, F38, F39, F42, F49, F50, F56, F58. F13 stays closed on the report's own terms: the 250-word result is one start sentence, one *do not start it again*, and the `0ms` line is gone.
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
   - **G1–G10 are in main** (`Config::compose_for_seat`, `config.rs:2389`, called from `Harness::open_with_registry` when `Config::seat_prompt` is set; `SECTION_NAMES`, `config.rs:1126`; `default_section`, `:1192`) — the branch they were built on is gone. The base prompt is composed at open from `SeatFacts` — the seated tool names after role and downgrade, the backend's real scratch path, subagent, linked worktree. Sections (`config::SECTION_NAMES`: identity, language, tree, find_and_read, edit_files, scratch, notes, working, tone) say only what is true for the seat; a section with nothing true is left out, overridden or not. New `working` section: the R79 delivery sentence, `ask_user_question` (blocks) vs a `todo_write` `wait_on` operator question (the plan goes on), the harness manages context (no numbers), stop/continue (root) or "your last reply is your parent's answer" (child), the queue rule. Children compose for their own seat (no own-session paragraph, no fabric block). Fabric block renders `skill` / `flowy get` (CLI through `bash`) only when the seat has them; "Reasoning goes in a row" dropped; gatekeeper and notes-keeper "reason as long as you like" → "explain what you found; keep to the evidence"; standing-notes index line has no token number; dead `researcher()`/`reviewer()` roles removed. `prompts.toml` keys renamed: `read_only_tools` → `tree`, `scratch_path` folded into `scratch`, `working` added. The scratch pid is masked in `prefix_fingerprint` (`harness::instructions_of`) so a restart does not call every session stale. Left: no tool reads a fabric memory body (only the CLI); the fabric block's "Read from the node at <time>" still changes per open, so seated roots read as stale after a restart (pre-existing). **Both are closed (`bf173960`)**: the `flowy` tool has an `artifact` action that reads one row's body in full - `id`, plus `offset` - and cuts a long one at `MAX_BODY_BYTES` naming the size, the byte it stopped at and the offset that continues it (`tool::artifact_body`; `an_artifact_body_is_read_whole_and_cut_loudly`, and end to end against a fake node in `tests/seat.rs::the_artifact_action_reads_a_body_and_cuts_a_long_one_with_the_numbers`). The block names that door when the seat has the tool and the CLI when it only has `bash` (`Reach::flowy`, `SeatFacts::fabric_reach`, `the_block_names_its_source_and_the_doors_to_the_rest`), and its live line names no clock, so two opens compose one prompt (`an_open_time_does_not_change_the_live_block`).

---

## R74 - What the 2026-10-11 landings left open

**Status:** audited 2026-11 (nine fixed, seven already done, six not a session's to settle, 37 since settled), then worked (4, 11, 12, 16, 20, 23, 25, 33 and 40 closed). left: item 39 (a live gate on lubuntu1/2 - the seam and the rows' naming are held by tests, and the operator's fleet key is not in this placement).

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
- 20 **FIXED** (`52ce9b63`) — one construction of the variant and one idiom at every site.
  `SessionEvent::warning`/`warning_with` (`crates/sessionlog/src/event.rs`) build it, `Hub::warn`,
  `Harness::warn` and `Sessions::warn` publish it, and the doors that take an event rather than a
  hub (a queue's `announce`/`announce_to`, memwatch's `Registry::broadcast`, a head's refused job
  read) are handed the constructor. Every site in `crates/harnessd/src` and in sessionlog itself is
  routed, `#[cfg(test)]` fixtures included.
  `crates/harnessd/tests/one_warning_door.rs` holds it: `no_site_builds_a_warning_by_hand` reads
  the crate's sources and names any site that spells the variant's third field (`compaction:`, which
  a pattern reaches only through `..`), `the_scan_tells_a_construction_from_a_pattern` proves the
  scan has teeth, and `the_constructor_and_the_sink_agree` proves the two build one event. The
  routing also put every code where the tree's census reads it: `warning_codes.rs` learned the
  doors' argument shape, which found two codes with no severity row — `embeddings_unavailable`
  (`docs/embeddings.md`'s spelling, beside `embedder_unavailable`) and `memory_bound_broken`, both
  now rows in `TABLE`'s failure register (`TABLE` is 185, 75/15/95). What is NOT routed is the
  head's own fixtures: `crates/tui/src/app/tests/` builds the variant by hand in about seventy
  places, to hand a head the event it is being tested on, and `crates/tui/src/head.rs`'s stub hub
  with them. That is a fixture's business rather than a report site, and the item was scoped to
  `harnessd/src`.
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
  `mergequeue/gate_host.rs`), and the readable half of its done-when is now held by a test: a
  landing's merge row, the transition a pane reads while the gate runs, and the green tip's health
  row all carry `(<host> gate)` from `GateRunner::name` (`2aac5991`). Before it, a gate on lubuntu1
  and a gate on this box read alike on the row a person looks at last.
  **Left, and not runnable from this placement:** the live half — a branch landing with its gate on
  lubuntu1/2. It needs `~/.config/letibot/hosts.toml` and the per-host fleet key
  `~/.config/letibot/keys/<host>` that `Hosts::load` and `scripts/rrun` read, and a session's own
  `$HOME` has neither: they are the operator's, on the Mac, and a plain `ssh dead@lubuntu1` from
  this box answers `Permission denied (publickey)` while `~/.ssh` is refused to a command here by
  the boundary. To run it on a box that holds those two files: `[queue] gate_host = "lubuntu1"` in
  the project's `leticode.toml`, a daemon over that project, and a branch through `/queue` — the
  merge row must say `lubuntu1 gate`, as must the target's `gating` and green evidence. The fleet's
  `~/.actrc` is item 27's, and `act` is in no gate here.
- 40 **DONE** — the largest inline block moved: `harnessd/src/plugins.rs`'s 3 404-line `mod tests`
  is `crates/harnessd/src/plugins/tests.rs`, declared `#[cfg(test)] mod tests;` as harnessd's other
  moved blocks are, and `--lib -- --list` names the same 1 201 tests before and after. Nothing was
  edited, renamed or dropped; the move cost the block its four columns of indent, which let rustfmt
  rejoin nine wrapped calls. What R70 still has: fifteen inline `mod tests` blocks over 800 lines,
  largest `tools/src/exec/host.rs` (1 466) then `harnessd/src/mergequeue/gate.rs` (1 458), counted
  by walking `mod tests {` to the closing brace at the same indent.
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
    — ✓ fixed in 5d3b2f05, test the_tree_and_the_repository_arrive_and_the_steps_run_there; the
    live landing itself is still open — see the audit's item 39 above.
40. R70 — move the largest inline test blocks into sibling files (unblocked now the big edits have
    landed).
    — OPEN: only part done — `crates/harnessd/src/plugins.rs:2226` still carries a 2,469-line inline test block (the settled archive, `hands-off/letibot/TODO-settled.md`).
41. R60 — the three things `!term` did not do: **built on `r60-term`**, see R60.
    — ✓ fixed in 26913ddd, test a_second_pane_runs_beside_the_first_and_each_is_named_by_its_number

---

## R55 - A round's end and a turn's end are the same event on the wire

**Status:** closed in this repo. Left in leticl, checked on 2026-11-07 at `12a4c72`: the `:turn-finished` arm in `src/session/seq-gap.lisp:254` sets the turn `finished` and calls `note-turn-finished "finished"` for every `turn_finished`, and nothing in `src/` reads `ends_turn`. A round that ends with `ends_turn: false` must leave the turn running and the row's clock untouched; an absent field (an older daemon) keeps today's reading. `Hub::turn_in_flight` (`crates/sessionlog/src/hub.rs`) reads `TurnState::Finished.ends_turn` (9bb46f2c7, test `a_turn_stays_in_flight_across_rounds_until_the_ending_frame`). The round loop already decides the ending (`CapturingSink::finish`, `crates/harnessd/src/harness/turns.rs`), so a no-call round that a nudge continues says `false`; asserted without a model file in `crates/harnessd/tests/turn_end_nudge.rs`. A loop that returns after a round that said `false` (a backgrounded job with a queued prompt) now gets its ending from `Hub::end_the_prompt`, called in `one_turn` (`crates/harnessd/src/turn_tail.rs`) before the clock stops (tests `a_prompt_left_between_rounds_is_ended_once`, `an_interrupt_between_rounds_stands_the_turn_down`). Failures publish `TurnFailed` in `settle`, interrupts `TurnInterrupted`, and a restarted daemon's hub starts with no turn. left: leticl's `:turn-finished` arm is not under ~/Projects and is unchecked.

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
Heads: this one reads it in `turn_busy` and the bridge gates `Turn::Idle` on it. (The residual named here, a no-call round followed by a nudge saying `true`, is closed: see Status.)

---

## R121 - What a command boundary holds is hardcoded and invisible

Status: OPEN, filed 2026-11-04 by the operator. Items 1-4 and 8 are built: 1-4 (except 4's tool half) on branch z-r121, their remainders and 8 on the branch this row was rewritten on. left: items 5-7, the two seat levels as configurable perimeters. The macOS seatbelt suite passed on the Mac (22 tests, including the new hide test).

The paths a confined command sees are two constant lists: `GRANT_CANDIDATES` in
`crates/tools/src/exec/confine.rs` (`~/.cargo`, `~/.rustup`, `~/.local/bin`, mise's data and
state, `~/bin`, `~/.gitconfig`, `~/Projects/llama.cpp`, `~/models`) and the toolchain list in
`crates/tools/src/exec/seatbelt.rs`. What exists is granted read-only, with `CARGO_HOME`,
`RUSTUP_HOME`, `MISE_*` and `GIT_CONFIG_GLOBAL` pointed at it. The operator cannot configure
it and cannot see it: the only account of the view is the manifest the model is told.

1. `[boundary] grants_ro`, `grants_rw`, `env`, `hide` in `leticode.toml` at user and project
   level; today's lists become the documented defaults; a project adds to the user's; `hide`
   always wins.
2. One candidate list for Linux and macOS, through `exec/seat.rs`'s shared builder.
3. `/boundary` in the TUI and `letibot boundary`: every path in the view (rw/ro, and why), the
   egress rule, the env pairs handed in (secrets redacted), the hidden paths, and what the
   probe measured.
4. A refusal for a path or tool outside the view names the key that would grant it.
5. The two seat levels are configurable perimeters, not code: a top-level session's perimeter
   (today: real `$HOME` and the filesystem, network per mode, `DENY_UNDER_HOME` minus
   `.config/gh` as the only fence) and a "default subagent perimeter" (today: throwaway home,
   project and toolchain, the operator's git identity read-only so it can commit, no
   credentials, no gh, no push), each overridable per project and per spawned child.
6. Perimeters compose: a child starts from its parent's perimeter and may only narrow it, so
   the workflow holds (a child cannot push, so its code goes through review). A parent that
   needs a WIDER child (the operator's example: a disk-cleanup subagent that removes placements
   on the hosts) names the extra paths or grants at spawn, with a reason; that raises a card
   for the operator in the head and as an MCP ask (`leticode_asks` / `leticode_answer`), even at
   `allow-all`, because widening a perimeter is not a call the mode decides. The answer is
   recorded on that child only; its own children inherit at most the widened set.
7. The coding flow this gives: the main session (allow-all, wide perimeter) spawns children in
   the narrow one; children commit on their branches and cannot push, so the only way their
   code reaches main is the review queue and its gate. The main session keeps its credentials
   and can push directly: the operator's "manual" mode, kept on purpose as the fallback for when
   the queue itself is the thing that is broken.
   Today the split is hardcoded: a top-level Mac seat gets `GH_TOKEN` injected at seat start
   (the daemon runs `gh auth token` outside the sandbox) plus a github.com credential helper
   through `GIT_CONFIG_COUNT/KEY/VALUE`, and a child gets `credential.helper` reset and no
   token. Which secrets a perimeter may carry, how each env var is computed (a command run
   outside the box, a file, a constant) and where it goes (which seats, which calls) belongs in
   the same configurable perimeter as the paths, so roles can be expressed rather than coded:
   e.g. a reviewer child that may comment on a PR but not push, a cleanup child with host
   access, a coder child that commits only.
   Known gap of today's injection: the masking of `GH_TOKEN` happens when a call's output is
   read, so a background job's output file on disk keeps the token unmasked.
8. The file tools obey the same perimeter as the shell. Today the default seat's shell cannot
   reach `~/.local/share/letibot` (DENY_UNDER_HOME), but its file tools are rooted at `/` and
   reach the daemon's store, other sessions' folders and their plugins; a session was seen
   writing its own plugin under `sessions/<id>/plugins/` that way. Session plugins only narrow
   (they sit before the gate and can pass, rewrite, refuse or answer, and a rewrite is gated),
   so that one was harmless, but a session's file tools should reach its own `sessions/<id>/`
   and nothing else of letibot's store.

Closed on z-r121 (docs/boundary-and-adjudication.md section 4m):

- Item 1: `[boundary] grants_ro, grants_rw, hide, env` at both levels, a project adds to the user's,
  `hide` is denied after every grant. Parser `boundary_keys` in `crates/leticode-config/src/lib.rs`
  (test `the_boundary_table_keeps_its_lines_and_a_project_adds_to_the_users`, bad shapes
  `a_bad_boundary_table_is_refused_with_its_line`), rules in `crates/tools/src/exec/boundary.rs`.
  Never-grant list: `~/.ssh` and every fenced store, the home, `/` (`ssh_is_never_granted_by_any_spelling`).
  Env deny-list: `boundary::env_refusal`, the one place (`env_refuses_secrets_and_loader_variables`).
  Fail closed: `a_boundary_config_that_is_refused_refuses_the_seat` in `crates/harnessd/src/harness/open.rs`.
  `gh` in the wide seat: `config_grants_and_hides_reach_the_seat_and_gh_stays`. Hide wins over a
  grant in bwrap: `a_hidden_path_inside_a_grant_is_masked_after_the_bind`.
- Item 2: `GRANT_CANDIDATES` is the one home-relative list; `TOOLCHAIN_UNDER_HOME` is gone. The Linux
  view and a remote probe gain `.config/git`, which macOS already had.
- Item 3: `/boundary` (`Slash::Boundary`) and `letibot boundary` (`crates/letibot/src/boundary.rs`) over
  `crates/harnessd/src/perimeter.rs` (`the_view_shows_paths_hidden_and_env_without_the_token`).
- Item 4: the absence note names `[boundary] grants_ro` / `grants_rw`, or says a never-granted path
  has no key (`an_absence_note_names_the_boundary_key_or_says_none_grants_it`).

Closed after z-r121, on R121's own branch (item 3's probe line, item 4's tool half, item 8):

- Item 8: the file tools obey the shell's perimeter. `crates/tools/src/exec/fence.rs` (`FileFence`)
  is built from the seat and the view that were MEASURED at open - one list, so it cannot drift
  from the shell's - and `HostBackend` applies it in its own operations (`read`, `write`, `list`,
  `stat`, plus `ExecBackend::file_refusal`, which `grep` and `glob` ask before they walk, because a
  walk whose every `list` refused reports "nothing matched" about a denied path). Three rules: a
  masked path is denied (the credential stores, the profiles, every `[boundary] hide`); a tight
  view holds nothing else, so a path outside it is denied and a write under a read-only grant is
  denied while its read is not; and the letibot store is denied except this session's own
  `sessions/<id>/`, where its plugins live. Wired in
  `crates/harnessd/src/harness/open.rs::place_the_tools`, from the backend's measured boundary and
  `cfg.session_id`. Tests: `crates/tools/tests/file_fence.rs`,
  `read_write_edit_grep_and_glob_refuse_the_store_outside_the_sessions_own_folder`,
  `a_tight_seat_refuses_outside_its_view_naming_the_key`.
- Item 4's tool half: a file tool's denial is the sentence a command's absence note gives, and it
  names the key. `a_tool_refusal_names_its_boundary_key`: a `[boundary] hide` path names the key
  with the file and line that wrote it, a path outside a tight view names `grants_ro` /
  `grants_rw`, and a credential store or the store itself says no key reaches it - the answer a
  model can act on, which is the difference between a refusal it retries and one it does not.
- Item 3's probe line: `boundary::probe_lines` renders what a MEASURED boundary says - the
  mechanism and the age of the measurement, the namespace table (or a policy's properties), and
  the seals - in `Boundary::describe`'s own words. `perimeter::lines_with_probe` puts it last in the
  report, and `Harness::boundary_lines` hands it `ExecBackend::boundary()`, so `/boundary` shows
  the boundary this session actually got. `letibot boundary` has no session, so it prints "probe:
  not measured here" and names the command that does measure one. Tests:
  `the_probe_lines_say_what_was_measured` and `the_report_shows_what_the_probe_measured`.

Open:

- `grants_rw` is the user file's alone: a project file is editable by a session, and a writable
  grant is a widening. Operator ruling 2026-11-05: user file only, as built.
- Item 3: the view prints what the probe measured, from the live session's boundary (closed
  above).
- Item 4: a path is refused with its key, and so is a file tool's path (closed above).
- Items 5-7 are untouched. A child reads the same `[boundary]` as its parent and nothing is passed
  at spawn, so no child can be wider yet; the narrow-only rule and the widening ask have nothing to
  attach to until perimeters are config. Secrets per perimeter (`GH_TOKEN` and the credential
  helper) stay coded. Operator ruling 2026-11-05: keep them as coded, top-level only.
- Item 8's fence is this process's, not a sandbox: it refuses a path a file tool is asked for, and
  a session that runs its own code goes round it by construction - that is the shell's view's job,
  which is why the fence is built from the same view. A `leticode` seat that seats no shell has no
  boundary at all, so its file tools keep the whole host (item 5's territory). `read_many` over a
  remote or VM backend is not fenced: those substrates have their own view.
- `load_user` in `leticode-config` reads the user file as `Level::Project`; `perimeter::load` uses
  `load_as(.., Level::User)` instead. Left alone.
- The R95 seam: `boundary::env_refusal` is the env deny-list. R95's carry-over should call it.

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
- Already done. Score a new brief against recent commit subjects. A close match is offered as "looks like <sha> already did this - check before starting". On 2026-11-02 four of fifteen packages found their whole scope already fixed on main.
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

**Status:** mostly built. The bridge declares `claude/channel` (`crates/bridge/src/mcp.rs`), pushes room messages (`crates/bridge/src/rooms.rs`), pushes every held session's turn end, ask, failure and provider trouble (`crates/bridge/src/push.rs`, one watcher per head, said once per change), and pushes the queues' moves for the bridge's own folder (`crates/bridge/src/queue.rs`). left: the plugin.

**The queue events** are one channel notification per move that changes what an orchestrator must do: an entry enqueued, a verdict (`approved` / `refused` / `cannot_decide`, with the reason line), a merge `landed` or `landed_local` with its `landed_sha`, a merge `failed` with the gate's failing test, and the target going `red`. A row picked up, a rebase, a withdrawal, a target that went green and the merge row an approval births are not news and push nothing - a verdict is one event, not two. The follower reads `queue_transition` through `Store::transitions_after` every 2 s, at most `PER_PASS` (8) moves a pass, and remembers its place in `$XDG_RUNTIME_DIR/letibot/<sha12(repository)>.queue`, so a bridge restarted on the same folder says the moves it missed and not the ones it already said; a folder followed for the first time says nothing about the history before it. Rows of another repository pass the cursor without being pushed, and a folder's daemon being down pauses the follower rather than losing a move.

Today an orchestrating Claude Code session learns that a package finished through a tracked background `leticode-wait` per session. A channel replaces that: the bridge is already an MCP server, so it can declare `capabilities.experimental["claude/channel"]` and send `notifications/claude/channel` with `{content, meta}` when a session ends its turn, asks, fails, or a queue row changes state. An idle Claude Code session starts a turn on it.

- Claude Code sends no acknowledgement. Expose a reply tool through which the orchestrator acknowledges each event, and re-send an unacknowledged event with backoff.
- Permission asks can be relayed through `experimental["claude/channel/permission"]`.
- Claude Code must be started with `--dangerously-load-development-channels server:leticode`. It works in interactive sessions only, and custom channels are not on the allowlist.
- Keep `letibot-mcp wait` as the fallback for sessions started without the flag.
- **The allowlist, as documented on 2026-11-07:** "A channel published to your own marketplace still needs `--dangerously-load-development-channels` to run, since it isn't on the approved allowlist." Only Anthropic's allowlist (`claude-plugins-official`) or an organisation's managed `allowedChannelPlugins` with `channelsEnabled: true` lifts it. A plugin still removes the per-session `claude mcp add` and carries the skills.
- **Open choice:** whether Claude Code wakes on every room message, as letibot sessions now do, or only on one addressed to it, as the bridge does today (`Attention::ToMe`). Every message is a Claude turn each.
- **The plugin** is what replaces the alias and the per-seat Monitors; the flag stays until the plugin is allowlisted. One installable plugin carries the leticode MCP server, its channel, the room waiter and the skills, so a session gets push without being started specially. Custom channels are not on Claude Code's allowlist today, so check what an approved plugin needs (marketplace entry, review) before building.

Docs: https://code.claude.com/docs/en/channels-reference.md

---

## R127 - The idle nag names every free head of the plan

**Status:** not started, postponed by the operator on 2026-11-07. left: all of it.

The plan is a DAG and the check exists to keep it moving. Today `unfinished_plan_for` names one ready row (`the_row_to_name`, `crates/tools/src/builtins/todo.rs:1333`), so a plan with five independent heads is worked one at a time. The check should list every ready row, with its author's verb, and say that independent rows go to subagents or jobs in parallel.

- The idle clock compares the check's text, so a list that changes when a row closes is a new check at the base window. That is wanted: closing a row is progress.
- The head folds the check on its sentences (`crates/tui/src/ui/transcript/notice_card.rs:212`), and the fold changes with the text.
- A row matching a running job's description is still left out, row by row rather than silencing the whole check.

## R129 - One store file per project and a shared file

**Status:** proposal, design settled, nothing built. left: the build, P0-P4 at the end of the doc. `docs/store-split.md` on branch `r129-store-split`.

Every daemon writes one `sessions.db` (9.4 GB on 2026-11-06, a WAL that reached 346 MB while long readers held snapshots). Writes retry on busy for up to 60 s, so one slow writer stalls every daemon. The proposal puts each project's sessions, transcripts, jobs and decisions in `projects/<key>.db`, written by that folder's daemon only, and the index, writer leases, queues and host leases in `shared.db`.

- Settled: decisions live only in project files, with no slim copy in `shared.db`. The readers are the `decisions` tool, the start-up counts, and `/gate` (which calls `Store::corpus()` beside `tokencore`'s own tests, so the first draft of this item was one call site short); a corpus-wide read sweeps every project file read-only and a scoped read opens one.
- Measured 2026-11-07 (the step is built, `busy::SLOW_CALL`): 7 lines in ~65 minutes of one 9.4 GB file shared by 16 daemons, every one a write with 0 busy retries, and 328 `database is locked` plus 154 `database or disk is full` failures in the per-folder logs. The split separates the locked writes by construction and the readers' WAL effect by construction; it cannot separate the disk. Left: time the wait itself (P0), because a 30 s busy timeout absorbs a lock wait inside one attempt and the retry counter sees nothing.
- A saturated disk stalls both files alike: on 2026-11-07 a test gate linking on the root disk blocked every daemon's reads. Builds on lab2x1 now go to `/build`.

## R130 - The seat's `GH_TOKEN` is readable in `ps`

**Status:** fixed, for a command this box spawns and for a remote placement. left: rotating the token.

`confine.rs` passed the seat's secret environment to bwrap as `--setenv NAME VALUE`, so every user on the box read `GH_TOKEN` from `ps`. A secret's pair now travels on bwrap's `--args` from a pipe the child holds at `SECRET_FD` (`confine::hand_secrets`, used by `HostProcesses::spawn`), and the argv carries only `--args 200`. `boundary::is_secret_name` is the one list both the argv and the pipe read.

- A remote placement renders no secret either: `pieces_of` leaves the pair out for `EnvSource::Remote` too, and the far shell puts it on that same descriptor out of the ssh session's stdin — `exec 200< <(dd bs=1 count=<n> 2>/dev/null)` before the helper runs. `dd` moves exactly its own bytes, so what is left of stdin is the command's own.
- A job's launcher cannot read stdin (it is `/dev/null`, or the job dies with the connection), so its payload sits in that file as base64 and reaches the helper the same way: still on no argv.
- A host with no `bwrap` exports the pairs itself out of the same descriptor, so an unconfined remote command is not the door that keeps the old spelling.
- A `firecode in` guest has no stdin at all, so a command that needs a secret there is refused by name rather than run without it.
- Tests: `remote_confine.rs` (a confined command and a confined job) and `remote_hosts.rs` (a host with no boundary) assert the token is on neither the ssh argv nor the helper's, that the far command still receives it in its environment, and that a command reading stdin reads its own.
- Left: the token was readable while the old binary ran, so the operator rotates it.

## R132 - `skills_path.rs` refuses its own session once under load

**Status:** open. left: the root cause.

`an_unknown_skill_name_is_refused_and_runs_nothing` failed once in a full gate with `session s-skills-path is being written by another process` naming the test's own pid, 0 s ago. Six reruns and the next full gate passed.

- A second claim from one process is refused by design (`writer::me` suffixes the owner with a nanosecond, so two claims in one process are two claims), so the refusal means a claim on that store was still held.
- Two candidates, neither confirmed: an earlier `Claim` in the same test not yet dropped when the dispatch reopened the session, or two fixtures in parallel getting the same `TempDir` name (`harnessd-skills-path-store-<pid>-<now_ns>`), the second wiping and reusing the first's store. An atomic counter in that name rules out the second for nothing.

## R135 - The context-claim detector's `carry` anchor is not calibrated

**Status:** SETTLED 2026-11-07. Measured on this box's store, read once and read-only (4,316,024 sentences of assistant and reasoning prose, 1,421 of them scorable), every scorable sentence labelled by hand - 141 claims, 1,280 not - into `~/.local/share/letibot/calibration/context_claim_sentences.tsv` on lab2x1 (session text, kept out of the public repository; the test skips where it is absent). left: nothing.

A session stopped at 842k of 1M with five rows open, saying *"I'm at the end of what I can carry in this session"*. `context_claim` anchors only on `context` and `room`, so the sentence was never scored. `carry` is now an anchor when `session` is in its window, with two exemplars, and the module's tests pass. The floor (0.25) was measured on 747 labelled sentences that did not include this anchor, so the store's sentences with `carry` near `session` should be scored against it the same way.

**The `carry` walk (R135's own):** 122 sentences have `carry` inside the window around `session`. None is a claim, and every one scores 0.000 - no window carries a capacity word as well, so the anchor adds no false positive to the floor. The sentence that filed the row is NOT in the store any more (compacted), so the anchor's only positive evidence is its own exemplar, asserted by `the_family_is_caught_in_other_words` and again by the calibration test.

**The 747-set walk, re-derived the same way, moved the floor.** The code as filed caught 27 of the 141 claims and called 74 of the 1,280 negatives a claim: precision 0.267, recall 0.191 at 0.25. Today's store speaks mostly in *holding* claims ("I have enough context", "both are in my context") and in this repository's own noun, `room`, so `HOLDING` grew the holding family (6 to 16), `EXEMPLARS` the budget family (`given my context budget`, `I've used a lot of context`, 14 more), a percentage is a token (`49%`), and three gates now decide whether a window is a claim at all: an owner, a capacity word (`CAPACITY`; `ROOM_CAPACITY` and no article for the uncountable `room`), and `session` for `carry`. `FLOOR` moved 0.25 to 0.28.

| floor | claims over | negatives over | precision | recall |
|---|---|---|---|---|
| 0.25 (before this change) | 27/141 | 74/1280 | 0.267 | 0.191 |
| 0.25 (after) | 90/141 | 5/1280 | 0.947 | 0.638 |
| **0.28 (after, FLOOR)** | **87/141** | **2/1280** | **0.978** | **0.617** |
| 0.40 (above every negative) | 59/141 | 0/1280 | 1.000 | 0.418 |

The two negatives over the floor are `"the enormous amount of context-only messages"` (0.391) and `"the spike used a fresh context per configuration"` (0.367) - code-sense collisions no word gate separates; 0.40, above every negative, costs a third of the recall. Test: `context_claim::tests::the_floor_is_measured_on_the_labelled_sentences`, which asserts these counts; the module doc carries the same measurement.

## R136 - Answer waits: a row waits on another top-level session's answer

**Status:** open. left: the build, steps 1, 4, 5, 7 and 8 of `docs/answer-waits.md`; steps 2, 3 and 6 are built on branch `remote/83414738`.

The design is `docs/answer-waits.md`. A's row carries an `Ask` edge, the room holds the ask, B gets a row on its own board saying A waits, and only B's explicit `answer` or `decline` settles it. The contract under it: top-level sessions are services. A session gone for 5 minutes raises an operator ask to restart it and settles nothing. An ask ends only by answer, decline, withdrawal by A or the operator, or B's session being deleted (`Retire`, the one `DOWN`). Claude Code seats are asked through bridge tools `room_answer`, `room_decline`, `room_asks`.

Built here: `TodoNeed::Ask { ask, of, room, question }`, `TodoNeed::Owed { ask }` and `Answered.ask` in the store, the wire and both conversions; `wait_on`'s `ask` through a new `crates/tools` seat, `AskLink`/`AskMade`, that `harnessd`'s room link is to implement (`fn ask(of, room, question) -> Result<AskMade, String>)`, with `needs`' refusal of `ask`, `keep_answers` keeping an ask through a restatement, `detach`, `NeedFate::PeerAsked` as a root of the check, `TodoBoard::settle_ask` and the four settle sentences in `AskSettle::reply`; and the head drawing of both shapes. Still open: the room's own state and frames (step 1), the link and verbs (step 4), the passes that write `Owed` rows and apply settles (step 5), the two-daemon end-to-end test (step 7), and `docs/room.md` (step 8).

## R137 - Todo graphs that span projects

**Status:** open. left: the design.

Today a board's edges stay inside one session tree. An answer-wait ask (R136) is the first edge from one project's board to another's. Consider whether rows in one project may need rows in another directly, how the head draws such a graph, and how the idle check treats a cross-project edge.

## R138 - Stable session identities across hosts, through migrations

**Status:** open. left: the design.

A session's identity is stable on its host only. Answer waits (R136) assume a stable graph of services, so they refuse targets on other hosts. A session that migrates between hosts must keep its name and its open asks, both as asker and target, for asks to cross hosts.

## R144 - The MCP bridge gives Claude Code seats a todo board with `wait_on` and the idle nag

**Status:** built. left: nothing but the head's todo pane, which lists sessions and not seats (a `crates/tui` change, outside this row's brief).

The design and the build are `docs/seat-todo.md`: `seat_todo` in the shared store under the handle `cc@<project>` (schema 41), three MCP tools - `todo_write` with the session's own schema, `todos` to read the board back, `todo_answer` for the operator's reply - and a watcher that runs the session's `TodoNagClock` over the seat's packages and pushes `unfinished_plan`'s own sentence as a channel notification. The schedule moved out of `crates/harnessd/src/sessions.rs` into `letibot_tools::builtins::todo::nag`, so the daemon and the seat run one clock and not two, and harnessd re-exports it under its old names. What a `wait_on` a seat may write: a package's turn end (a package is a child while its turn is in flight), a package's open job (through the new `JobTable` seam), and a question for the operator, which has no card and is answered in the conversation and recorded with `todo_answer`.

A Claude Code seat orchestrating letibot sessions has no board: its waits live in Monitors and its own memory. The bridge should serve the same `todo_write` / `todos` / `update` tools a session has, kept in the store under the seat's handle, with `wait_on` edges to letibot sessions, their jobs, room asks (R136) and the operator. The same idle check nags the seat through the channel push when a row is ready or in progress with no wait. Then Monitors are not needed for waiting on packages, and a seat's plan is visible on the head's todo pane like any session's.

## R149 - A placement caps a package's build width from the host's free cores

**Status:** postponed by the operator on 2026-11-07. left: all of it.

On 2026-11-07 four packages on lubuntu1, each with subagents running cargo at full width, drove its load to 254 on 32 cores. Remote writes then timed out (`remote_call_slow`, then a failed `edit`), and R139's turn was stopped by the progress check. The fix was a message asking each package for `CARGO_BUILD_JOBS=6`. A placement should set it instead: the daemon placing a session on a host knows the host's cores (`hosts.toml` `reserve` holds some back for the owner) and the packages already placed there, so it hands each session `CARGO_BUILD_JOBS` and a test-thread count from the free share, and a session's subagents divide their parent's share rather than each taking the whole box. The placement's status should say the share.

## R150 - Plugin extension points: the head's UI moves out to plugins

**Status:** H0 landed; K7 (the `adjudicate_` floor) built; P1-P6 not started. left: the operator's answers (`docs/plugin-extension-points-wide.md` §8, 13 questions with defaults), then the rest of the build per its §6.

K7 is the gate's tighten-only floor, and it is wired: `plugin-sdk/src/adjudicate.rs` is the contract (`adjudicate_floor`, merged through `Tier::strictest`), `plugins` holds the hook in `FAIL_CLOSED` and refuses a session layer's manifest at admit and skips one already on the stack, `tools/src/adjudicate.rs` asks it once inside `reading_for` - the reading every card and every decision is built from - and the daemon installs `plugins::FloorHooks` on the gate `harness/open.rs` builds, reading the host on the `plugin_slot` the session's layers fill. It is a served kind: `plugin-hooks` teaches it, `harnessd/tests/data/floor-layer` is the committed project layer the daemon test loads, and that test (in `open.rs`) shows a project's `ask` turning a `writes allowed` write into a head's card while a session with no layer admits it unasked. What it cannot reach: a read-access tool never calls the gate at all (`runtime.rs`'s `gated && !effective.is_unattended()`), so no floor can turn a read into a question - that needs a call site in the runtime and is not K7's.

H0 (the contract types only, in `crates/plugin-sdk`, `crates/sessionlog/src/pane.rs`, `crates/plugins`): the `stream` block (`Stream`, `StreamEntry`, `Divider`, `Mention`), a role's `hue:0`-`hue:7`, the `chat` feed and its window (`Feed::Chat`, `Chat`), `View.input`/`keys`/`dock`/`width` with their refusals, acts in an answer (`Answer`, `Acted`), the clock in the input (`now_ms`, `utc_offset_s`), the `complete_` hook's types and prefix, and `blobs:room`. All additive with serde defaults: an old pane document round-trips byte-identically, and no existing layer changes. Two things are TYPES and not yet behaviour, and P5 must wire them: no host fills `now_ms`/`utc_offset_s` (the head's two pane inputs say so), and `Manifest::admit_for` - the rule that the `chat` feed is a built-in's or a pinned layer's - is not yet called by `Stack`'s admission, which needs the scope and the trust standing in one place.

`docs/plugin-extension-points.md` is the first review, scoped to the room chat: its decisions can be a plugin, its mechanics stay host. Ten host additions (X1-X10: a `chat` feed, a `stream` block, pictures by blob id, theme hues, an input surface, a `complete_` hook, acts in answers, cursor-mode keys, a clock in the input, and dock/built-in/exec/read fixes for the other panes) and packages H0, P5, P6 beside room-tui's P2-P4. The operator's ruling: the native renderer stays only as a safety net until the built-in chat plugin is proven, then goes. The review was too room-focused, so a second one covers the whole harness and head before questions 2-6 are answered.

## R152 - `tests/launcher.sh` stubs outlived their run, and four of its cases had drifted

**Status:** fixed. left: putting `tests/launcher.sh` in the merge gate.

Twenty `python3 /tmp/tmp.*/bin/harnessd` stubs were still listening on lab2x1 on 2026-11-07, 20 to 27 days old - all from before `0f9ddad8` gave the file its EXIT-trap cleanup. The trap does not run on SIGKILL (a runner's timeout), so the python stubs now poll `LBT_OWNER`, the test script's pid, and leave once it is gone. The same run showed four cases failing on `main` before this change: the python start stub crashed on the launcher's `--resolve-session` store query (no `--socket`), `daemon_argv` counted that query as a daemon start, and `scripts/migrate-notes.sh` was missing from the bash-3.2 guard. All cases pass now. Nothing runs this file in the gate, so the drift went unseen.

## R153 - Placing a session on a busy host times out at 20 s

**Status:** open. left: all of it.

On 2026-11-07 four placements failed at the same step while their host was building: `making the mirror` and `handing the bundle to the remote repository` got no answer within 20 s, the ssh was killed and the daemon shut down before the brief was sent. A retry minutes later always worked. Also seen: an ssh `Connection closed` on lubuntu3 while three packages opened connections at once (sshd `MaxStartups`), and `remote_call_slow` warnings with budgets under a tenth of a second. The placement's one-off steps should take a budget sized for a loaded host, and the link should reuse one ssh connection per host (`ControlMaster`) instead of opening one per call. Beside R149, which sizes the build width.

## R134 - Scrolling stops at a line above the tail

**Status:** not reproduced. left: a reproduction from the operator.

Seen twice in the arango window: the view scrolls up and down but not below one line, and only Esc frees it. The head showed `holding your place · 119 line(s) below arrive underneath`. `walking_down_from_far_up_a_long_session_reaches_the_tail_while_rows_arrive` passes on main. Seen again on 2026-11-07 in arango after a restart. A probe test does not reproduce it: 1,800 recorded rows attached as a snapshot, paged to the top and walked down with rows arriving, reach the tail. The suspect is the drawn window's ceiling: `App::hold` resumes following only when `hist_ceiling` is unset, and the window has rows above the snapshot that arango pages in from the daemon, which the probe has not. The head now appends one line per stop to `~/.local/state/letibot/scroll-stalls.log` (`App::note_scroll_stall`) with the window's numbers; the next stop's line says which boundary held.

## T16 - W9's open questions

**Status:** item 7 is fixed (minted call ids); items 1 to 6 are fixed (66077505, d81863e8, 229555bf, c44d41e7; item 3 has no test). left: unclear - item 6 asked lubuntu3 whether RAGFlow signals "no coverage" in a field, no answer is recorded.

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
7. **Call ids were positional per turn** (`call_0`; GLM carries none), so an id repeated across
   turns. A head or a store correlating rows by id saw one call where the session made two.
   — ✓ fixed: `letibot_transcript::mint_call_id` is one per-process nonce and counter, used where
   `crates/turn/src/items.rs` and `crates/provider/src/openai.rs` used to mint positionally, so an
   id is unique across a session's turns and across a restart (a nonce minted once per process is
   never re-minted); a server's own id is kept exactly as given, and a stored transcript's
   positional ids still replay, because the pairing window is the assistant message and not a
   session-wide id index. Tests `two_minted_call_ids_differ_and_are_not_positional`,
   `two_turns_with_tool_calls_mint_different_ids`, `two_replies_without_call_ids_mint_different_ones`,
   `a_stored_transcript_with_positional_ids_still_replays`.

---

# Needs the operator

A decision, an account, a live run on real hosts, or an install. Do not start these from an agent seat.

---

## R126 - Reviewers fan out to the declared hosts, opt-in

**Status:** landed opt-in. D18 stands: reviewers run on this machine unless `[queue] reviewer_hosts` (or `--reviewer-hosts`) names hosts. `["*"]` is every host in `hosts.toml`, `["local"]` or no key is this machine, and `lubuntu3` is refused by name. `reviewers_at_once` is per project and the pool is per daemon. left: the operator sets the key in `leticode.toml` (its comment still says "not landed yet") and runs one live review on lab2x1.

A remote reviewer is a `task` child with `Placement::Remote(host)` and the queue's clone as `vm_source`. Its verdict comes home through `review_verdict` and frees its place (`ReviewHosts::keep`), a placement failure falls back to this machine without spending an attempt, and the fallback's desk entry survives the pass that opened it. Tests are under `mergequeue/review/placement_tests.rs`, `review/hosts/tests.rs` and `serve/tests.rs`, all against fakes.

**Done when**: a review with `reviewer_hosts = ["lab2x1"]` runs on lab2x1 and its verdict lands on the row.
**Still open?** the live review.

---

## R125 - Script plugin layers: where a JavaScript engine comes from, and the JS-side contract

**Status:** not built, and deferred by the operator on 2026-11-05, because it stops on two decisions an agent seat cannot take. left: both decisions, then `docs/plugins-plan.md` step 11 itself.

A script layer is a layer that is source text, with no toolchain: the model writes plain JavaScript, the host runs it, and nothing is compiled. Step 11 of `docs/plugins-plan.md` is the only step there not built.

1. Where the engine artifact comes from. No JavaScript engine is available offline on this box: `Cargo.lock` holds none and the offline registry cache has no `quickjs`/`rquickjs`/`boa`/`javy`, and `crates/builtins/build.rs` compiles Rust guests with cargo, not C. A vendored blob compiled into the binary would be a BUILT-IN, so `docs/plugins-plan.md` §2.5 runs it freely, `exec` included, and the model's `.js` text would be the only untrusted thing in the path. That is a trust decision, not a build detail.
2. The JS-side contract, including how the shadow gate's replay stays exact with a clock and `Math.random` inside it. What a script layer's module IS is already decided and built: `plugin.toml` carries `module = "<file>"` (default `plugin.wasm`), one relative path inside the layer's own directory, refused at admit if it leaves it, so a `.js` layer pins on the script text and an edit to it un-pins the layer like any rebuild.

**Done when**: both decisions are made and recorded in `docs/plugins-plan.md`, and a `.js` layer loads, pins on its text, and answers a hook.
**Still open?** the two decisions.

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
That is N4's answer too - a shaped runner was wanted rather than `bash` (N4 is in the settled archive,
`hands-off/letibot/TODO-settled.md`).

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
tests are named in T24's entry in the settled archive (`hands-off/letibot/TODO-settled.md`).

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

**Status:** rows 1 and 2 are settled negative, measured (`hands-off/letibot/experiments/kv-stitching/RESULTS.md`, the operator's private archive). left: row 3, which should not be built as written yet - it is the one checkpoint at `TokenLedger::prefix_len()` and not section 3.4.1's placement. The section 6 measurement that says whether it pays needs the :8080 server restarted, which is the operator's.

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

— rows 1 and 2 are measured now, not argued: `hands-off/letibot/experiments/kv-stitching/RESULTS.md` answers
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
accumulator; the only guard is the sweep's own floor (`hands-off/letibot/experiments/kv-stitching/srv.py:29`).

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
  `hands-off/letibot/experiments/kv-stitching/RESULTS.md:99` gives the gate as `ctx_tgt_has_recurrent_state`
  (`server-context.cpp:5763`), which cannot be what disabled it on the DENSE 27B this bullet is
  about; no doc explains that case.

Mitigation used, and worth generalising: the sweep harness refuses to start a condition
below 40 GiB `MemAvailable`. Any future experiment that prefills many distinct prompts
needs the same guard.

— fixed in b82b906b in code, untested: `hands-off/letibot/experiments/kv-stitching/srv.py:29` refuses to start a
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

N4 is superseded by this entry and is in the settled archive.

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
