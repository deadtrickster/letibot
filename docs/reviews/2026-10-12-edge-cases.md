## The review of 2026-10-12 — five read-only reviews of main at `50b3e55`, filed as R89–R93

The operator's ask: *"start review subagents - we need to find edge cases, ux inconsistencies,
notifications that posted in full instead of line + popup pattern etc etc"*. Five subagents read
the code (and `sessions.db` / `metrics.db` read-only); nothing was built or changed. Every finding
is below as its own entry; each report is kept verbatim in section 4 under *"The review of
2026-10-12, in full"*, and the numbers in brackets point into it. Line numbers are on `50b3e55`.
CONFIRMED means the reviewer read the whole path or saw it on disk; SUSPECTED means it inferred
the last step. **Still open?** for each: re-read the cited lines on the current main.
**R89 and R90** are settled 2026-11-03 and their settled lines are in the closing commit messages; the other three are below.


## The review of 2026-10-12, in full

Five read-only reviews of main at `50b3e55`, run as subagents on the operator's ask (*"start review
subagents - we need to find edge cases, ux inconsistencies, notifications that posted in full instead
of line + popup pattern etc etc"*). Kept verbatim so nothing a summary dropped is lost; R89–R93 are
filed from them, and their `[name N]` pointers refer to the numbering here.

### Notices drawn in full instead of one line + popup (R89)

#### Notice walls in the transcript (letibot main 50b3e55, read-only review)

Nothing was edited or built. I read `sessions.db` and `metrics.db` read-only, with Python `-I` scripts in my scratchpad.

**How I measured.** Hub warning details are not stored anywhere: `sessions.db` has no warning table, and `metrics.db` `event` rows of kind `warning` have empty `attrs`. So real text below comes only from rows the session writes in the person's place (`transcript_item` kind `user` with speaker agent) and from System rows. For warnings I give the frequency from `metrics.db` (923 warnings all-time) and the producer's own wording.

To find which session rows fall through to a raw wall, I ported the tui's `compact_cards` / `compact_notice` / `folded_notice` / `ended_jobs` gates to Python. I ran it over the last 4 days, with duplicate texts removed. Row counts assume about 100 text columns.

**How the transcript draws things now:**
- **Session rows** go through `user.rs:61` (speaker Agent): first `compact_cards`, then `compact_notice`, then `folded_notice`. Anything else falls through to `session_block` (`blocks.rs:7`, rano `SessionBlock`). That draws `  session · ` plus the WHOLE text wrapped, in faint grey. These are the gray walls.
- **System rows** are drawn whole by `item.rs:~263` through rano `screens::system_row`. There is no fold. They are shown at every rung except Conversation.
- **Hub warnings** go through `panes/notes.rs:120 note_lines`:
  - a code with a card reader becomes a one-line card you can click;
  - `compacted`, `auto_compact`, `compact_half` and `reseated` become one line ending `· /notes`, which cannot be clicked;
  - everything else gets rano's `Note::lines` (rano 9c53f50 `agent/notes.rs:187`): 3 wrapped lines plus `  … +N lines · /notes`. It cannot be clicked, and the only way to see the rest is the `/notes` listing.

#### Ranked findings

**1. `[todo check]` that does not fold: RAW WALL.** 264 distinct rows in 4 days, about 4,490 screen rows in total, median 8 rows, max 66 (6,233 chars).
- Renderer: `user.rs:96` → `session_block`. The fold is `blocks.rs:177 folded_notice`, which folds a group only if every line after the bullets matches `PROMISES` or is a `Waiting on…` line.
- Producer: `tools/src/builtins/todo.rs:1728-1820` and `:2412`.
- Why it fails, by count:
  - 168: the old Model closing `do this one, or mark it done, or drop it — a plan left open…`. The fold dropped this phrase on purpose, but the store still has them (5 written today, 1,081 on 10-10).
  - 13: the Queue advice `this row is the merge queue's, not your plan's: …` has no promise phrase.
  - 10: the Parent advice `your parent asked for this one, so do it and say in your reply…` has no promise phrase.
  - 13: the `[todo check] this turn is finished and NOTHING can start — every item left waits on another:` variant, whose closing `unblock one of these rather than starting something new: …` is not recognised.
  - Some of the 61 rows that do carry the current promise: the `blocked_said` line (`3 rows waiting on the operator: …`) comes after the promise and is not a `Waiting on…` line.
  - 18: the `session X was seated … with a prompt this daemon would not compose now: …` paragraph is added as a second `\n\n` group, which drops the whole row to raw.
- Real example (1,545 chars, 5 lines): `[todo check] this turn is finished and one item is not done (1 more open):\n  - review-agent/a-pass-reads-one-row (entry s-…): Your branch is cut from …— the merge queue's, and you were asked about this one already…\nthis row is the merge queue's, not your plan's: …\n\nsession s-gatekeeper was seated 10-10 22:42 with a prompt this daemon would not compose now: …`
- Should be: `todo · <item head> · yours|operator's|queue's|parent's · N more open · click for details`, or `todo · nothing can start · N wait on <child/operator> · click for details`. The stale paragraph should split off as its own stale card.

**2. `Tool call X was not executed: …`: RAW WALL, worst single row.** One row of 1,083,613 chars and 6,902 lines, about 13,800 screen rows.
- Session `s-1789462738453908838-sub-1791503723529407810`, 10-09 02:03. It is the same sentence once per truncated call (`read` ×1, `bash` ×6,901).
- Producer: `turn/src/length.rs:139`. Renderer: the `session_block` fallback.
- Should be: `length cap · 6902 calls not executed (bash ×6901, read ×1) · click for details`.

**3. `message from your parent session `s-…`: …`: RAW WALL.** 130 rows that are not review-shaped; median 20 rows, max 46 (3,302 chars, 25 lines).
- Producer: `harness.rs:14865`. These are messages a parent model wrote to its child (briefs, extra requirements).
- Only the review-shaped ones are unwrapped (`review_card.rs:222 unwrap_message`, 21 rows, which become cards).
- Real example (1,392 chars, 7 lines): `message from your parent session `s-…`: Two new constraints from the operator, fold them into the plan before you finish:\n\n1. REPLICATION IS IN SCOPE, …`
- Should be: `parent s-gatekeeper · <first clause> · click for details`.

**4. `[intent check]`: RAW WALL.** 52 rows in 4 days, median 12 rows, max 20 (1,518 chars, 10 lines).
- Producer: `tools/src/builtins/intent/ledger.rs:304`. No reader.
- Real example: `[intent check] this turn declared 6 item(s); 0 tool call(s) succeeded and 0 ran without producing a result.\n  - #1 "Write src/datum.rs: …" was declared this turn and no tool ran in this turn — start it, or say why not\n … board: 0 of 6 complete — 0 in progress, 6 pending, 0 blocked`
- Should be: `intent check · 6 declared, 0 ran · board 0/6 · click for details`.

**5. `[context check]`: RAW WALL, short but frequent.** 99 rows, 4–5 rows each (up to 491 chars).
- Producer: `harnessd/src/context_claim.rs:462` and `:530`.
- Real example: `[context check] you said "Each reports on its own, and my context is nearly out, …". Your context is 54% full — 547,198 of 999,999 tokens. That is not the wall, … Continue with what you stopped.`
- Should be: `context check · 54% full (547k/1M) · not the wall · click for details`.

**6. Merge-queue session notices with no card reader: RAW WALL.** 8 rows, median 28 rows, max 52 (4,177 chars, 16 lines).
- Shapes:
  - `The merge queue cannot get a verdict on `X` (entry …), so it will not land … placing this session in a firecode VM: io…`
  - `The merge queue is holding `X` … has no merge gate on main`
  - `The merge queue cannot finish with `X` …: steering_urgent: the daemon is stopping…`
  - `The merge queue cannot land anything in `X`: its target … has no … section in AGENTS.md`
- Only the last is still produced (`harness.rs:8202`). The others come from older builds but stay in carried or reseated history.
- `red_card` only opens `EMERGENCY — `, `The merge queue's gate of ` and `The merge queue CANNOT JUDGE `.
- Should be: red-card or review-card lines, e.g. `‼ queue · X · cannot judge (VM: io error) · click for details`.

**7. `[reseat]` session row: RAW WALL.** 8 rows, up to 22 rows (1,684 chars, 8 lines). Producer: `harness.rs:2263`.
- Real example: `[reseat] The person re-seated this conversation with `…`: the conversation was carried across as it was, nothing summarised and nothing dropped. Mes…`
- The same event also draws the `reseated` warning one-liner and a System(update) row, so one act becomes three rows.
- Should be: `reseated · verbatim · N items carried · click for details`.

**8. `[monitor]`: RAW WALL.** 7 rows, up to 15 rows (1,272 chars, 3 lines). Producer: `harness.rs:990`.
- `job_card.rs:249 notice_groups` lists `[monitor] ` as a tag, but `folded_notice` has no `[monitor]` case. So any row that mixes `[job]` with `[monitor]` fails `compact_notice` and the WHOLE row falls back to raw.
- Real example: `[monitor] 1 watch(es) fired:\n  - `child-nag-suite` (pid 1299446 (/bin/sh -c cd … cargo test …) — process(es) … leaving …), declared by turn …: FIRED after 129.8s — pid 1299446 (/bin/sh -c …the whole command again…)…\nOnly a FIRED watch is an answer about the world; …`
- Should be: `monitor · child-nag-suite FIRED after 129.8s (pid gone) · click for details`.

**9. `The harness is watching what your tool calls return…`: RAW WALL.** 6 rows, 8–9 rows each (725 chars). Producer: `harnessd/src/progress.rs:246`.
- Should be: `progress · last N rounds: N calls, N returned nothing new · click for details`.

**10. `[the deliverable]`: RAW WALL.** 6 rows, 5 rows each (497 chars). Producer: `harness/deliverable.rs:98` and `tools/src/builtins/task.rs:366`.
- Real example: `[the deliverable] Your worktree `…` has uncommitted changes, so the branch `…` does not hold them: ?? .letibot/notes-offers.jsonl. The branch is the deliverable — commit this…`
- Should be: `deliverable · worktree has uncommitted changes (1 file) · click for details`.

**11. Stale-prompt sentence as a session row: RAW WALL.** 3 rows on their own, 8–9 rows each (875 chars), plus 18 rows where it is attached to a todo check.
- `stale_card.rs:131` registers only a `WARNING` kind (`prefix_stale`), with no `SESSION` kind.
- Real example: `session s-gatekeeper was seated 10-10 22:42 with a prompt this daemon would not compose now: the system instructions and the tool schemas have changed since. … Say this to the person in your reply, …`
- Should be: the same `prompt stale · seated … · …` card, registered in `SESSION_KINDS` with an end marker, so it also splits off a todo check.

**12. `[answer] on "…"`: RAW WALL.** 1 row, 968 chars on one line, about 10 rows. Producer: `operator_ask.rs:75`.
- Should be: `answer · <row head> · operator chose "<x>" · click for details`.

**13. Orphan `[task]` completion: RAW WALL.** 1 row, 467 chars, 6 rows. Producer: `harness.rs:1114`, `:1146`.
- Text: `[task] a subagent a session below this one started has finished:\n  - `…` done: … (started by `…`)\nThis session is told because the one that started it cannot be: …`
- `blocks.rs PROMISES` lacks the closing `This session is told because the one that started it cannot be:`. `job_card::CLOSINGS` has it for `[job]`, so the job and task folds disagree.
- Should be: the same folded `Agent …` line, clickable.

**14. Short raw session nudges: RAW, 2–3 rows each.**
- `Your previous turn was stopped at the context wall…` (`harness.rs:9037`).
- `You keep overthinking. Cut it short and do things…` (`harness.rs:11368`, `turn/src/compaction.rs:243/253`).
- Should be: `context wall · compacted, continuing · click for details` and `nudge · reasoning-only turn · click for details`.

**15. System(update) rows: RAW WALL whenever tools, thinking or system rows are shown.**
- Renderer: `item.rs:~263` → rano `screens::system_row`, which wraps the whole text with no fold. It is hidden only at the Conversation rung (the operator's read-edits).
- Store, last 6 days:
  - 39 compaction-summary rows (`This conversation was compacted: what was said before this point is replaced by …`), up to 18,684 chars and 116 lines, about 190 screen rows each;
  - 25 reseat rows (`This conversation was re-seated onto a new prompt…`, 253 chars);
  - 1 compaction instruction (`The conversation above is what this record stands in for…`, 1,466 chars, 18 lines).
- Producers: `turn/src/compaction.rs:448`, `:1756`; `harness.rs:10594`; `dialect.rs:156`; `httphead.rs:230`.
- Should be: `system · compacted summary (18.7k chars) · click for details`.

**16. Hub warnings with no card: 3 wrapped lines plus `… +N lines · /notes`, no click, no popup.**
- Renderer: `panes/notes.rs:120` → rano `notes.rs:187`. A short detail is 1–3 rows; a long one is 4 rows with the rest only in `/notes`.
- Frequency (`metrics.db`, all-time):

| Code | Count | Notes |
|---|---|---|
| `context_claim_repeated` | 63 | |
| `resume_note` | 55 | |
| `daemon_stopping` | 24 | about 330-char detail, so 4 rows: `` `{who}` asked this daemon to stop. Every head detaches, the socket goes, … nothing new is started.`` (`sessionlog/src/server.rs:1589`) |
| `merge_queue_store_error` | 23 | `the queues could not read or write their store while {doing}: {why}. Nothing moved; …` (`mergequeue.rs:131`) |
| `merge_queue_recovered` | 23 | |
| `queue_lease_lost` | 17 | `serve.rs:1553` |
| `queue_wrong_build` | 13 | `this daemon serves NO queue: the store's schema is … Restart it on the current build.` (`serve.rs:1455`) |
| `queue_target_proposed` | 11 | |
| `memory_high` | 11 | `memwatch.rs:236`, about 230 chars. **Not in the 127-code census**, so it defaults to Failure and is drawn red. |
| `waiting_on_operator` | 10 | `WAITING ON YOU: the model asked "…" on the row `…` — open the question card…` (`operator_ask.rs:139`) |
| `daemon_stopping_runs` | 10 | |
| `operator_question_settled` | 7 | |
| `mode_set` | 7 | |
| `subagents_stopped` | 4 | |
| `model_endpoint_retry` | 4 | |
| `todo_answer_refused` | 2 | |
| `context_wall` | 2 | |
| `child_door_refused` | 2 | |
| `slash_refused` | 1 | |
| `reseat_refused` | 1 | |
| `length_empty_turn` | 1 | |
| `calls_abandoned` | 1 | |

- `auto_compact_no_progress` and `auto_compact_failed` are deliberately drawn whole (3 lines plus the seam). The comment says their detail IS the finding. That still works as a one-liner with the reason's first clause and a popup.
- Should be: `<mark> <code> · <first clause> · click for details`, a card through `WARNING_KINDS` or a generic fallback card for every code.

**17. Other notes this head makes itself, same rano fold, no click:**
- `denied:<req>` refusals (`app/events/asks.rs:344`): `REFUSED {tool} — {summary}. {outcome} by {by}: {basis}{repeat}. {grant}`. Red, 3 lines plus the seam; this is the old "red wall", only shortened.
- `Note::NotRun` (`asks.rs:331`), `Note::Answered`, `Note::Decided`: folded to 3 lines.
- `Note::Pane` (`app/events/term.rs:153`): never folded, a head line plus up to `PANE_LAST_LINES` = 4 rows.
- `sudo`, `prompt`, `provider_key_refused`, `protocol_skew` and `daemon_replaced` (`hello.rs:194`), `unreadable_frame` (`session.rs:500`), `anchor_lost` (`scroll.rs:1115`).

**18. One line, but no way to see the details (compaction one-liners).**
- `compacted`, `auto_compact`, `compact_half`, `reseated` (`panes/notes.rs:222 compaction_row`).
- Drawn as `· compacted — compacted: 958397 → 100005 tokens, on transcript s#t25. … · /notes`. The code is said twice.
- They are not in `note_cards`, so a click does nothing; the only way in is the `/notes` listing.
- Should be: `compacted · 958k → 100k tokens · click for details`.

**19. Folded notices: one line per item, but no popup.**
- `[task]` (177 rows, up to 10 lines each, one per subagent), `[todo check]` (103), `[meanwhile]` (2), folded by `blocks.rs:177`.
- `diff_popup.rs:228 notice_rows_in` marks a row clickable only if it has `[job]` groups or a card, so a pure `[task]` or `[todo]` row cannot be clicked. An 8-subagent `[task]` row is 8 lines with no popup.
- Should be: one line (`agents · 8 finished · click for details`), or at least clickable.

**20. (Not the transcript) The flash notice above the composer** (`ui/screen.rs:296`, rano `screens::notice`) is cut to one line with no way to read the rest.

#### Inconsistencies among the existing one-liners

- **Hint wording.**
  - `click for details` (`line_cards::CLICK`): red, review, asked and restart cards.
  - `alt-p for the card` (`stale_card.rs:28 POINTER`): the stale card. alt-p opens the reseat card (`app/reseat.rs`), but a click on the line opens the NoteCard popup, a different view.
  - `… · /notes`: the compaction one-liners, which cannot be clicked.
  - No hint at all: the job card's two lines and the folded `Job/Agent/Todo/Meanwhile` lines.
  - The hint sits at the end of the line, so `trim_to` cuts it first at narrow widths.
- **Lead and glyph.**
  - Only the red card has a glyph (`‼ main RED at 60bf819c`).
  - The other cards lead with a word: `review ·`, `asked ·`, `restarted ·`, `prompt stale ·`.
  - Compaction uses rano's `· code — ` with an em dash and repeats the code.
  - Jobs lead with `j182 · slug · exited 0 after…`.
  - Folded lines use `Job `/`Agent `/`Todo `/`Meanwhile: `.
- **Colour and frame depend on where the card came from.**
  - A session-row card (`blocks.rs session_card_block`) sits inside `  session · ` with an `HH:MM:SS` stamp; the parts outside the accent are Faint.
  - A warning card (`panes/notes.rs:213 card_row`) is a bare line: no label, no indent, no stamp, and the parts outside the accent are `Span::raw`, the default bright colour.
  - So the same `‼ main RED` card looks different as `target_red` than as an `EMERGENCY —` row.
  - Accent roles: red is Failure, review uses a per-verdict role, asked, restart and stale use Attention, compaction is Faint (Routine).
- **Click paths.**
  - Session cards and job notices use `Target::Notice` → `open_notice_popup(item row)`.
  - Warning cards use `Target::NoteCard` → `open_card_popup(index)` (`keys/mouse.rs:97-105`).
  - Compaction one-liners, rano notes and folded rows cannot be clicked.
  - No key opens any notice popup from the transcript. It is mouse-only, plus the todos pane (`todos.rs:831`).
- **Popup.**
  - Every popup is the same `PopupBody::Notice`: full text wrapped, Esc or ctrl-c closes it, footer `esc closes · ↑↓ PgUp PgDn Home End`. These agree.
  - Titles differ:
    - a card's title is drawn in its role;
    - a job's is `j1 · a job ended` in Strong;
    - the fallback is `notice` in Role::Failure, red (`ui/diff_popup.rs:71`).
  - A NoteCard loses its code, so `popup_title` tries every warning reader in turn.
  - A parent-wrapped review card's title does not say it came through the parent.
- **Height.** The job card is 2 lines per job (the operator's spec); every other card is 1.
- **One event, several rows.**
  - A red target: a `target_red` warning card plus an `EMERGENCY` session card.
  - A restart: a `restart_restored` / `restart_reminded` warning card plus a `[restart]` session card.
  - A reseat: the `reseated` one-liner, a System(update) row, and a raw `[reseat]` wall.
  - A stale prompt: the `prefix_stale` card plus the raw session sentence (finding 11).
- **Census gaps.** `memory_high` and the `denied:<id>` codes are not in `warning::TABLE`, so they default to Failure and are drawn red.

My scratch scripts (the Python port of the fold gates and the store queries) are in `/tmp/claude-1000/-home-dead-Projects-letibot/7a4b7ff7-173b-4f07-a444-4fda796b95b7/scratchpad/` (`classify.py`, `why.py`, `rows.py`, `sys.py`).

### Keys, cards and popups (R90)

UX consistency review of crates/tui at 50b3e55. I only read code: nothing was built or run, no files were edited, and no process was touched. Paths below are relative to `crates/tui/src/`. Findings are ranked by how likely the operator is to hit them.

#### A. Likely to be hit

**1. With the reseat card open, a pane's keys go to the card, so Enter or `1` reseats.**
- **Where:** `app/reseat.rs:183-230` (`key_reseat_card`) runs before `key_surface` (`app/keys/mod.rs:235` vs `:239`). `reseat_card_up()` → `slot_taken()` (`reseat.rs:110`) does not count an open surface. `ui/hint_bar.rs` has no branch for the reseat card.
- **How:** attach to a stale session with an empty composer (the card opens on every Hello). Then:
  - ctrl-t, ↓ to a row, Enter: you expect "reads it whole", but `/reseat` is sent (sel 0 is "Reseat now").
  - ctrl-s, type `1` to switch session: `digit_row(k,2)` reseats now. Typing `2` pins the card and swallows the digit.
  - In the todos pane, Tab toggles the card's "whole" view instead of marking a row done; Esc pins the card instead of closing the pane.
- **What happens now:** the bar shows the pane's hints ("↑↓ moves · enter reads it whole · esc closes") while the keys act on the reseat card.
- **Should:** an open surface should count as taking the slot (draw the strip instead), or `key_reseat_card` should only run when `focused_surface()` is None.

**2. Digits answer question cards whose rows carry no numbers, and the first digit of a typed answer is taken as a choice.**
- **Where:** `app/keys/cards.rs:415` (`_ if !typing && digit_row(..)`). rano draws question and permission rows as `▸ body` with no number. The question bar (`hint_bar.rs:124-130`) does not mention digits.
- **How:** a question "how many workers?" with choices `["2","4","8"]`. Type `16` on the empty composer: the `1` answers choice 1 ("2") at once.
- **Should:** on question cards, either drop digit-answer or draw numbers and require Enter. The permission bar (rano `Deciding`: "a row number answers") also points at numbers that are not drawn. The reseat and quit cards do number their rows (`▸  1  Reseat now`), so the cards disagree.

**3. A question that arrives while the operator is typing turns the half-typed prompt into its answer.**
- **Where:** `app/composer.rs:193-213`. A question's Enter with text calls `question_answer` → `QuestionAnswer::free(line)`. The permission path holds the words ("answered the ask — your line is held"); the question path sends them as the answer.
- **The same happens with alt-a:**
  - `bring_back_question` (`app/todos.rs:316-352`) refuses only when the away card has its own draft *and* the composer is non-empty. A card with an empty draft comes back over an unrelated prompt.
  - `open_operator_ask` (`todos.rs:136-165`) has the same rule.
- **Sequence:** Esc the question away, start typing "check the logs first", press alt-a to reread it, press Enter. The question is answered with "check the logs first".
- **Should:** when a question arrives or comes back over composer text, stash that text and give the card an empty field, or hold the words as the permission path does.

**4. A new decision arriving resets the highlight on the card already on screen; answering one card leaves the highlight where it was for the next.**
- **Where:** `app/events/asks.rs:59` sets `self.sel = 0`, then `open.push` puts the new card at the back. `open[0]`, the visible card, is unchanged. Nothing resets `sel` when `DecisionAnswered` removes `open[0]`.
- **How (first case):** on a permission card, ↓↓ to "deny". A second permission arrives (parallel calls, a subagent). The highlight jumps to row 0 (allow_once), and Enter allows.
- **How (second case):** answer card A at row 3. Card B appears with row 3 (clamped) already highlighted.
- **Should:** reset `sel` only when the front card changes, and always reset it when it does.

**5. Esc means something different on every card; on a permission card it silently arms the interrupt.**
- **What Esc does on each card:**
  - Question: put away (alt-a brings it back).
  - Reseat: not now (alt-p brings it back).
  - Prompt card: put away. No key brings it back, only `!send`, and there is no resident line (`cards.rs` `key_prompt`; `prompt_away` is not drawn anywhere).
  - Secret: refuse, which sends `None`.
  - Key ask, todo draft, setting picker: cancel.
  - Quit card: stay.
  - mode_confirm and term_ask: any key cancels.
  - Permission: falls through `key_decision` (`cards.rs:397` only matches questions).
- **How (permission card):**
  - With a pane open, Esc closes the pane behind the card.
  - Otherwise Esc reaches the editor and arms the Esc-Esc interrupt; a second Esc interrupts the turn that is waiting on the card.
  - Inside a subagent, Esc says "a decision is waiting".
  - Neither the card nor rano's `Deciding` tail says anything about Esc.
- **Should:** give the permission card an explicit Esc (put away, as questions do, or swallow it with a notice). Also give the prompt card alt-a and a resident line like the question card's.

**6. Typing a quit chord while a decision is up shows the quit hints, but Enter and the digits answer the decision.**
- **Where:**
  - Key order: `key_decision` (`keys/mod.rs:224`) runs before `key_quit_card` (`:227`).
  - Draw order: the decision arm comes before the quit arm (`ui/screen.rs:173` vs `:192`).
  - Bar: `hint_bar.rs:83` checks `quit_card` first, giving "1/2 or ↑↓ then enter · esc stays".
- **How:** a permission card is up; ctrl-c ctrl-c on an empty composer. The bar shows the quit keys while the permission card stays on screen. Pressing `1` (meaning "detach") answers the permission's first option.
- **Should:** don't open the quit card while a decision is up, or give it priority in both drawing and keys.

**7. With the editor pane focused, a permission or question card is drawn, but every key goes into the file.**
- **Where:** `app/editor.rs:411-440` (`route`) sends all keys to rano while `p.focused`. Nothing unfocuses it when a decision arrives (no `focused = false` on any ask event). The hint bar checks `editor_focused()` / `editor_drawn()` (`hint_bar.rs:116-119`) before `open`, so it shows the Editor/EditorBehind hints.
- **How:** ctrl-] into rano during a turn. A permission card appears with "↑↓ to choose · Enter to answer". Pressing `1` or Enter edits the file.
- **Also:** with the editor drawn but not focused, the bar says "enter sends" while Enter answers the card.
- **Should:** a new ask returns focus to the composer (or at least the bar names ctrl-]), and the bar checks `open` before the editor modes.

**8. In the todos pane, Enter on a row waiting on you opens a question card, but the bar says "enter reads it whole".**
- **Where:** `app/keys/bindings.rs:412` (`row_says` → "reads it whole") vs `app/keys/panes.rs` `todos_read`, which opens the card for a waiting row.
- **Should:** `row_says` should say "answers its question" on waiting rows.

**9. Nothing shows how many decisions are queued behind the one on screen.**
- **Where:** only `open[0]` is drawn (`screen.rs:173`). Neither the rano card nor the head adds a count.
- **Worst case:** a head-local todo-ask card at `open[0]` hides a model question with a deadline at `open[1]`. That question can time out unseen.
- **Should:** add "+N more waiting" to the card. A daemon decision should not queue behind a head-local todo card.

#### B. Moderately likely

**10. ↑↓ do not scroll /help or /status, and they pull history into the composer.**
- **Where:** `bindings.rs:157`: `READING = [PAGE, WHEEL]`, while Listing, TodoRow, QueueEntry, SubOut and JobOut all bind ↑↓ to scroll.
- **What happens:** ↑ in help falls through to `editor.vertical` and recalls an old prompt. Help and status own Enter (`surface.rs:90`), so a following Enter is silently swallowed.
- **Should:** add the `UPDOWN` scroll binding to READING.

**11. Enter is silently dropped on reading surfaces when a line is typed.**
- **Where:** `owns_enter` covers Help, Stats, Queue, QueueEntry, Standing, Config, Jobs, SubOut, Listing. It does not cover the ctrl-v window, where Enter still sends.
- **How:** with /help open, type a prompt and press Enter. Nothing happens and nothing is said.
- **Also:** TodoRow swallows every key (`On::Any`), while Help lets letters through to the composer. So "can I type here" differs between overlays.
- **Should:** say something on a swallowed Enter, or make the reading overlays consistent.

**12. A head-local todo-ask card is never closed when its row stops waiting.**
- **Where:** `app/todos.rs`. Nothing removes `todo-ask:` cards from `open` on `TodosUpdated` / `ServerFrame::Todos` (`events/todos.rs:16-30`, `events/frames.rs:115-127`).
- **How:** another head answers the row, or the model completes it or changes the question. This head keeps the card up, and answering it sends `AnswerTodo` for a stale question.
- **Should:** on every board update, drop or refresh `todo-ask` cards whose row and question are no longer in `operator_asks()`, and prune `away` the same way.

**13. The tab progress bar can stay on "Waiting" with nothing on screen waiting.**
- **Where:** `app/attention.rs:25` and `:48` count all of `away`, across every session, including stale todo-ask cards (see 12).
- **How:**
  - Put a question away in session A and switch to B. B's tab shows Waiting, while B's waiting line (filtered by session, `ui/status.rs:143`) shows nothing.
  - A put-away todo card whose row was answered elsewhere keeps the tab on Waiting indefinitely.
- **Should:** use the same rule as the waiting line, or name the other session.

**14. A resync or re-attach throws away an open todo-ask card, and its draft becomes a prompt.**
- **Where:** `app/events/mod.rs:343`: `self.open = open` replaces the list with the daemon's. The head-local card is dropped, not stashed in `away`, and the composer keeps the words typed for it.
- **How:** a resync arrives while answering a todo row. The card vanishes, and Enter sends the half-typed answer to the model.
- **Should:** stash the front todo-ask card (with its draft) into `away` before replacing `open`.

**15. In a subagent, Esc on a question puts it away, and the next Esc leaves the session with it unanswered.**
- **Where:** `cards.rs:397` takes Esc before the subagent arm (`keys/mod.rs`, which guards permissions with "a decision is waiting in this subagent").
- **What happens:** in the parent, the waiting line is filtered to the current session, so the child's question with its deadline is invisible.
- **Should:** keep the operator in the child, or carry the question onto the parent's waiting line.

**16. A new todo card and an arriving decision split the screen from the keys.**
- **Where:** `key_todo_draft` (`keys/mod.rs:189`) runs before `key_decision`, but `screen.rs:173` draws the decision ahead of the todo card. The bar shows Deciding.
- **How:** the todo card is open and a permission arrives. The permission card is shown, but Enter files the todo ("a todo item needs a title…") and Esc cancels the draft. ↑↓ and digits (on an empty title) act on the permission.
- **Same mismatch, rarer:** `key_ask` takes every key while the decision is drawn over it.
- **Should:** make the draw order match the key order.

**17. `/peek ID` opens a subagent's output, but the bar says "esc back to the subagents".**
- **Where:** `bindings.rs:298` and the special case at `:478`, which only handles Todos underneath. `events/frames.rs:304` pushes SubOut onto whatever is on top.
- **How:** `/peek ID` with no pane open (Esc goes to the conversation) or over the jobs pane (Esc goes to jobs).
- **Also:** a late `Peeked` reply opens SubOut after the operator has already closed todos.
- **Should:** say the actual surface below.

**18. Mode and model pickers show the wrong bar while a decision is open.**
- **Where:** `hint_bar.rs:120` checks `pick` before `open`. `/mode` can open the picker while a decision is up, because `/` lines run first. The decision is drawn and takes the keys, but the bar shows the Pick hints.

**19. Cards that own the keyboard show conversation chords in the bar.**
- **Where:** `hint_bar.rs` has no mode for `secret`, `prompt`, `key_ask`, `todo_draft`, `term_ask` or `mode_confirm`.
- **What happens:** while the sudo password card is up, the bar still reads "ctrl-s sessions · ctrl-n clears notes · ctrl-t todos…". Those chords are swallowed (secret/prompt/key_ask) or cancel the card (`mode_confirm`: pressing the advertised ctrl-t cancels allow-all).
- **Should:** give these cards their own bar.

#### C. Less likely: edge cases and wording

20. **/help omits alt-a, alt-r, alt-p, Esc-puts-away, digit answers and /reseat.** `ui/panes/help.rs` mentions alt-p only as the editor's "shows the change again", so one chord has two meanings and only one is documented.

21. **alt-p does nothing visible while another card holds the slot.** `reseat_expand` sets `open=true`, but `reseat_card_up()` is false, and the strip keeps saying "alt-p shows the card". The card pops up later, possibly in front of the next Enter.

22. **The reseat card's content can be windowed, but PgUp/PgDn cannot scroll it.**
    - **Where:** `card_window` (`ui/cards/decision.rs`) draws "… N line(s) out of view · pgdn scrolls" for every slot card. `scroll_card` (`keys/chords.rs:263`) returns false when `open` is empty, so PgDn scrolls the transcript instead.
    - **How:** reseat card with "tab shows all" on a short terminal.
    - **Same on:** the quit and todo cards.

23. **A question with no choices shows "↑↓ to choose · Enter to answer".** With `n == 0`, ↑ recalls history into the answer field and an empty Enter does nothing silently (`cards.rs:402-411`, `answer_marked` returns None). An empty-string choice renders as a blank highlighted row that Enter answers.

24. **Options containing " | " are re-split in the transcript.** `ui/transcript/asked_card.rs:43` splits on " | ". An unchosen option that contains it is drawn as two options (the code acknowledges this). The todos pane joins the same options with " / " (`ui/panes/todos.rs:396`): two separators for one list.

25. **The todos pane says "enter on the row, or alt-a, answers it".** alt-a opens the first waiting row (or a put-away model question), not this row (`todos.rs:316-365`).

26. **The waiting line and alt-a messages are confusing in some states.**
    - "WAITING ON YOU … alt-a to answer" stays up while that row's card is already open; alt-a then does nothing.
    - With a model question open, alt-a for a todo row answers "this question is the open card…", which is about the other card.
    - With two model questions put away, the line names one and counts only todo questions (`ui/status.rs:159-163`).

27. **Notifications call every open ask "permission needed".** This includes model questions and todo cards (`app/attention.rs:80`). Reseat and todo rows waiting on you never notify.

28. **The popup's bottom edge stays the same when a decision is open.** It still says "esc closes · ↑↓ PgUp PgDn Home End" (`ui/diff_popup.rs`), but `key_diff_popup` passes ↑↓/Home/End to the card and composer. With a decision open the bar says "the diff: pgup/pgdn scroll…" even for a notice popup. Casing differs between "PgUp PgDn" and the bar's "pgup/pgdn".

29. **Question card answers stay up until the daemon confirms.** Model question and permission answers stay on screen until `DecisionAnswered` arrives, and a second Enter resends for the same req_id. Todo-ask cards vanish at once. The feedback differs.

30. **The quit card's Enter has no empty-composer guard; its digits do** (`cards.rs` `key_quit_card`). Words typed after it opens are lost on Enter, which quits.

31. **Narrow widths:** reseat rows use `trim_to`, so `RESEAT_SUMMARISE` (79 chars) is cut, while question rows wrap. The popup body uses `inner = (w-4).max(20)` plus 2 columns of indent, which overflows below about 22 columns.

32. **Wording:**
    - The question hint appears twice, on the card and in the bar, with different casing ("Enter to answer" vs "enter answers").
    - Permission rows show internal ids (`allow_session`); this is intentional for typing, but it is internal naming.
    - The held-prompt notice says "this message goes to the model when the run ends" (`composer.rs:330`), but it is only held: the operator must press Enter again.
    - Help says ctrl-c twice "quits", but it opens the quit card.
    - A one-key `alt-a` is used for both "brings it back" and "to answer".

33. **Hint buttons are inconsistent:** the reseat strip's alt-r and alt-p are clickable, but the waiting line's "alt-a to answer" is not, because its segment starts with "WAITING" (`app/keyhints.rs` `scan_row`).

The highest-risk items are 1, 2, 3, 4 and 6: each can send an answer or `/reseat` the operator did not choose. 1, 2 and 6 are the easiest to pin with a key-sequence test.

### Delivery, waits, restore, reseat and the store (R91)

I found 18 problems. Three of them lose data or put a session in the wrong state, and they are likely enough to fix first. Everything comes from reading code at 50b3e55: I ran no tests, changed nothing, and did not touch any daemon or sessions.db. Each finding is marked **[confirmed]** (I read the whole path) or **[suspect]** (I read part of it and inferred the rest). The order is severity × likelihood.

---

##### 1. Messages handed back by a failed or interrupted round are dropped (HIGH × likely) [confirmed]
- **Where:**
  - `crates/harnessd/src/harness.rs:11188` builds a new `HubSteering` (`let mut steering = self.steering();`) on every pass of the HTTP retry `loop`.
  - `self.steering()` at harness.rs:12214 starts each one with an empty `held: VecDeque::new()`.
  - On failure the engine calls `pending.give_back(steering)` (turn/src/engine.rs:1418/1438/1464/1474 cloud, 760–987 local), which only pushes into `HubSteering::held` (harness.rs:1686).
  - The next pass, or `break attempted`, drops that `HubSteering`. It has no `Drop` impl, so the messages go with it.
- **Sequence:**
  1. The model is streaming.
  2. The operator types a follow-up. `on_delta` absorbs it through `try_next` into `pending.queued`.
  3. The operator presses esc-esc (`BackendError::Aborted`), or the provider drops mid-stream (`Unreachable` / `Malformed` / `Refused`).
  4. `give_back` moves the follow-up into a `HubSteering` that is thrown away. The prompt is gone. On a retry that succeeds, nobody is ever told.
- **Same loss for every source `try_next` drains mid-stream:**
  - R79 immediates and the starvation batch from `at_round_boundary` (harness.rs:1628). `read_now` has already marked them delivered and set `deliver_subagent_run` in the store, so a restart never brings them back.
  - Monitor firings: `monitor_cursor` is already advanced (1646).
  - Parent messages.
  - AnswerTodo lines: the board already holds the answer, but the model is never told.
- **Authorisation side effect:** the operator's words are already in the trail (`t.say(Speaker::Operator, …)`, 1522). The authorisation trail can then cite words the model never read.
- **Fix:** build `steering` once per round, outside the retry `loop`, so handed-back messages survive into the next attempt. On a final failure, push what is left back onto the hub or into the next turn.

##### 2. A fork registers the new transcript before its rows are written; a crash or error mid-carry resumes onto a partial conversation (HIGH × medium) [confirmed for the write order; resume reading the newest transcript is from the store's own doc at store.rs:2862–2878]
- **Where:** `harness.rs:10464` (`put_fork`), `10483` (`set_prefix_fingerprint`), `10498` (`set_stale_prefix(None)`), all before `engine.open` (10502), `next.append_items` (10631) and the chunked `persist_reporting` (10685–10699).
- **Sequence:**
  - A `/reseat` carries 971 rows in chunks for minutes, or waits up to 30 minutes on a busy store.
  - If the daemon is killed or crashes during that, `current_transcript_id` (newest, `created_at DESC, rowid DESC`) is the partial `#tN`. The next open resumes onto a truncated or empty conversation.
- **Error after `put_fork` but before the swap (10505, 10631):**
  - The harness carries on in memory on the old transcript.
  - The store's newest transcript is the dead fork, so the next restart loses everything written since.
  - The fingerprint and stale flag were already cleared, so the head's reseat offer disappears although the session is still on the stale prompt.
- **Error in the persist after the swap:** `reingest` returns at `let mut out = out?;` (9880) before `self.prefix = next` (9892).
  - Metered turns then send the OLD schemas against the new transcript, the exact mismatch the comment at 9884 warns about.
  - The operator is told `reseat_refused … Nothing was changed`, which is false.
- **Fix:**
  - Write the transcript row (or a "complete" mark) in the same transaction as the last chunk, or have resume ignore a fork whose `item_count` is short of what it should hold.
  - Move the fingerprint and stale-flag writes after the body is stored.
  - Set `self.prefix` together with the swap.

##### 3. Job handles are reused after a daemon restart and match old waits (MED-HIGH × likely) [confirmed]
- **Where:**
  - `crates/tools/src/exec/jobs.rs:47`: `JobId::next()` is a process-wide counter that starts at `j1` in every daemon.
  - Store `job` and `job_context` are keyed `(session_id, handle)` with UPSERT. The `job` upsert keeps the old `command` when the new one is empty (store.rs:3357–3361).
- **Sequence:**
  1. A row waits on `j7`. The daemon restarts.
  2. The restart notice says the wait is RELEASED (restart.rs:478).
  3. The session's next job mints `j7` again.
  4. All of these now see an unrelated job as the waited one:
     - `RunningJobs::is_running("j7")`
     - `job_fate` (live table first, harness.rs:13723)
     - `delivery::names` (delivery.rs:247)
     - the `when: job` condition
  5. The row stays quiet until that job ends, then fires as `immediate:needed` on the wrong result.
- **Store side:** the new `j7` row starts with the previous life's command, and the old row's history is overwritten.
- **Fix:** make the handle unique across restarts (instance prefix, or seed the counter from the store's maximum), or keep the instance on `TodoNeed::Job` and compare it.

##### 4. An interrupt is followed at once by a turn the operator did not ask for (MED × high) [confirmed path; I did not check which state a stopped child reports]
- **Where:**
  - `note_turn_ended` (harness.rs:10779, 5481) runs however the turn ended.
  - `JobWatchers::turn_ended` (jobwatch.rs:660–671) rings the session's wake whenever anything is held.
  - The interrupt arm runs `stop_children_first` (harness.rs:1535). The stopped children then settle (`watch_task` → `Failed` or `Done`, jobwatch.rs:1354–1360).
- **Sequence:** the operator presses stop → the turn ends → the held batch, or the children's settlements arriving after `ended_at` (rule `idle`), ring the wake → `Harness::wake` starts a fresh `[meanwhile]` turn. The model may pick the work back up seconds after it was stopped.
- **Failed turns too:** a provider-failed turn gets the same follow-up turn, which fails again.
- **Fix:** have the harness tell `turn_ended` whether the turn was interrupted. If it was, ring nothing and mark held and new settlements to ride the next operator-started turn (`Rule::Batch`).

##### 5. A job result not yet delivered when the daemon stops is never delivered (MED × medium) [confirmed]
- **Where:** `restore_after_restart` only collects jobs with `state == "running"` (harness.rs:8784–8787). Jobs have no `delivered` column; subagents do (`subagent_run.delivered`).
- **Sequence:** a job settles during a long turn and is held under R79 (up to the 10-minute cap, or until the turn ends) → the settle is recorded (`exited 0`) → the daemon stops → after restart the job is neither running nor delivered, so the model never hears its result. R79 makes this window much wider than it used to be.
- **Fix:** add a `delivered` flag to `job` (set in `read_now`, as `subagent_delivered` is), and include "settled, not delivered" jobs in the restart notice.

##### 6. Store writes that skip the busy retry, or still kill the turn (MED × medium) [confirmed]
- **No retry at all:** every write in `crates/tokencore/src/store/subagents.rs` uses `self.conn.execute` directly, not `retry_sql` or `write_tx`:
  - `put_subagent_run` (167), `settle_subagent_run` (201), `interrupt_subagent_run` (212), `deliver_subagent_run` (222)
  - `put_monitor` (267), `end_monitor` (281), `put_job_context` (316)
  - `life_*` (355/366/375)

  Each gets one 30 s busy-timeout attempt, which busy.rs calls "a single attempt of a lottery". What each failure costs:
  - `deliver_subagent_run` fails → the result is delivered a second time after restart (`Decision::Deliver`).
  - `interrupt_subagent_run` fails at an orderly stop → `resumes` is not reset, so after three of those the child is abandoned (`RESUME_LIMIT`).
  - `put_subagent_run` fails at spawn → the child can never be restored.
  - `life_stopped` fails → the stop time is estimated from the last heartbeat.
- **Wrapped, but a busy store still ends the turn after 60 s instead of waiting the 30 minutes:**
  - `self.persist_context(..)?` (harness.rs:11561): the context-size row, a measurement, fails the whole turn.
  - `self.flush_todos()?` (11791).
  - `put_fork` / `set_prefix_fingerprint` / `put_stable_prefix` in forks.
- **Fix:** route the subagents.rs writes through `self.exec` / `write_tx`. Make the context write non-fatal (warn). Give the todo flush the same patience loop `persist_reporting` has.

##### 7. A todo write that fails is never retried (MED × medium) [confirmed]
- **Where:** `flush_todos` (harness.rs:12478–12487) sets `self.todos_version = v` before `put_todos`. If the write errors (busy past 60 s), the next flush sees the same version and returns `Ok` without writing.
- **Consequence:** the store's board stays stale until the board changes again. A restart then reverts it: the operator's AnswerTodo, `wait_on` edges and consumed conditions are lost, and conditions fire again.
- **Fix:** set `todos_version` only after the write succeeds.

##### 8. The restore threshold measures the daemon's downtime, not how long the work has been stopped (MED × medium) [confirmed]
- **Where:** `back_ms = self.session_registry.started_ms()` (harness.rs:8762) is fed to `restores()` (8831) and to the monitor TTL check (8885).
- **Sequence:** the daemon restarts 5 minutes after the stop → the operator `/resume`s the session 4 hours later (lazy open) → it reads as "down 5m" → subagents are resumed on a tree that has moved, and monitors are re-armed with a fresh full TTL ("TTL starts again from now"). The operator's rule was "more than 1 hour after the stop".
- **Fix:** measure from the stop to now (the open time), and close a monitor whose TTL ran out before the open, not just before the stop.

##### 9. Restore and `task_resume` treat any other daemon instance as gone (MED × low–medium) [suspect: I did not confirm that a daemon for another folder can open the same session]
- **Where:**
  - harness.rs:8771–8781 filters on `r.instance != me` only.
  - `subagent_record::decide` (263) and `resumable` (355) refuse only for `instance == me`.
  - The single-instance `flock` is per folder (sessionlog/server.rs:98–130), but `sessions.db` is shared by every daemon on the box.
- **Sequence:** daemon B opens a session whose children are running under live daemon A → A's last heartbeat is about 30 s old, so the stop reads as recent → children are resumed twice in the same worktree, monitors run twice, and A's live jobs are marked `lost at restart`. `task_resume` has the same hole.
- **Fix:** treat another instance as gone only when its `daemon_life` row has `stopped_ms`, or its beat is older than a few `BEAT`s.

##### 10. During the 30-minute busy wait the whole worker is stalled and an interrupt does nothing (MED × low) [confirmed]
- **Where:** `persist_reporting` (harness.rs:12711–12745) loops `sleep_unless_closed(5s)`, which checks only `hub.is_closed()` (12631–12641).
- **What the operator sees:** a `store_busy_wait` warning every 120 s on this one session.
  - An esc-esc on it is not acted on for up to 30 minutes.
  - Every other session on the single worker waits too, and their heads show only "queued" with no reason.
  - Patience is counted from the start of `persist_reporting`, not the start of the busy wait, so a long carry has less of it.
- **Fix:** let an interrupt end the wait (rows stay in memory and are written later), tell every other affected session's head why it is waiting, and count patience from the first busy failure.

##### 11. A wake marks results delivered before the turn that delivers them can fail (LOW-MED × low) [confirmed]
- **Where:** `Harness::wake` calls `between_turns` (which does `read_now` → store `delivered`, and empties held) and drains `restart_notices`, at 8631–8641/8569. Only after that does it call `submit_item` (8691).
- **Failure point:** `submit_items` can fail in `append_items` or `persist` (10774–10776) before `run_rounds`. The text is then lost, and `note_turn_ended` is never reached.
- **Fix:** do what the function already does for fired conditions — mark delivered only after `submit_item` succeeds, and put results back into held on failure.

##### 12. The held-results counter is wrong after an attach or reconnect (LOW-MED × high, display only) [confirmed]
- **Where:**
  - `HeldResults` is ephemeral (scrub.rs:102) and in no snapshot.
  - The daemon publishes only when the count changes (`held_said`, jobwatch.rs:768–784).
  - The head keeps its last value (tui/src/app/events/turn.rs:44).
- **Effect:**
  - A head that attaches mid-turn shows nothing held while results are held.
  - A head that reconnects after missing the zero keeps showing a stale "held: 2 jobs" on every later turn's status row.
- **Fix:** republish the current count on `HeadAttached`, or put it in the snapshot or settings row, and clear `held` on TurnFinished / TurnFailed / reconnect.

##### 13. Reseat side effects happen before the reseat is known to succeed (LOW × medium) [confirmed]
- **Where:**
  - `reseat_tight` "re-seated verbatim at …" is published (harness.rs:9805–9819) before the fork, which can still fail.
  - `reseat_target` changes `self.cfg.system` (9655) and writes a `stable_prefix` row (9683) even when the reseat is then refused as Over (9780).
- **Effect:**
  - A message saying the reseat happened, for a reseat that failed.
  - An orphan prefix row for every refused attempt.
- **Fix:** publish the warning after the fork succeeds, and write the prefix row only once the fit check has passed.

##### 14. An interrupt arriving at the end of a successful round is dropped (LOW × low; in `crates/turn`, next to this scope) [confirmed]
- **Where:** turn/src/engine.rs:1229–1250 (local) and 1614–1631 (cloud). `pending.absorb` → `hold_urgent(u)` → `return Ok(...)`. `Pending` is local and does not survive the return, and nothing hands the urgent back.
- **Effect:** an esc-esc between the last streamed piece and the end-of-round check stops the children (`stop_children_first` already ran when it was taken) while the parent carries on.
- **Fix:** give the held urgent back to the source on the Ok path, or return it in `TurnOk`.

##### 15. A child can answer its parent and then run another turn (LOW × medium) [suspect]
- **Where:** a child's turn that ends with held results rings its own wake (jobwatch.rs:669 → `serve_child` `OwnWork::Wake`, harness.rs:15316).
- **Sequence:**
  - The child writes its final answer without results that were held mid-turn.
  - The parent receives it.
  - The child then runs a `[meanwhile]` turn whose work goes nowhere the parent hears.
- **Fix:** for a child, deliver held results at its turn end before the answer is final (keep checking once more), or do not hold results for children.

##### 16. The R79 sort runs on every streamed piece of output (LOW × certain, cost only) [confirmed]
- **Where:** `Pending::drain` calls `try_next` on each delta. `try_next` → `at_round_boundary` → `delivery::sort` copies the whole todo board (todo.rs:746), takes and puts back the held set, and checks starvation — per piece of streamed output, not per round.
- **Fix:** run the R79 sort only at real step boundaries (an argument, or a flag the engine sets).

##### 17. The `wanted` map leaks entries (LOW × medium) [suspect]
- **Where:** `JobWatchers::delivered` removes an entry only on immediate delivery (delivery.rs:286).
- **Sequence:** a `job_wait` / `want_now` for a job already delivered, or a want recorded after its result was batched, stays in the map for the session's life.
- **Fix:** also call `delivered()` in `batch` / `between_turns`, and skip `want_now` for handles already in the `settled` set.

##### 18. Closing a root session throws away held and queued results (LOW × low; may be intended) [confirmed]
- **Where:** `JobWatchers::stop` (jobwatch.rs:868–886) takes held and the queue, and returns when there is no parent. `Sessions::close` calls it (sessions.rs:2595–2598).
- **Effect:** the subagent rows say this daemon's instance and not delivered, so nothing redelivers them until a different daemon opens the session.
- **Fix:** leave `delivered = 0` but stamp the run so the next open in the same daemon redelivers, or say on the hub what was dropped.

---

**Checked and found sound:**
- **R79 ordering:**
  - A result arriving at turn end: the queue rings when the result is pushed, and the bell keeps one pending ring per session.
  - An operator prompt racing a wake: commands are served before wakes, and the batch goes in ahead of the prompt.
  - Compaction does not count as a turn ending (it does not run `run_rounds`).
  - A wake for a child is not lost while the child is mid-turn: the `own_wake` flag is sticky.
- **`wait_on`:**
  - It refuses a job that has already ended.
  - A job that ends between the check and the board write is picked up as `needed` at the next sort.
- **Restore:**
  - A subagent restored after restart keeps its id.
  - A subagent whose worktree was removed is abandoned with the reason.
  - Reopening the same session in the same daemon does not restore twice (rows are re-stamped, and the in-memory slot check covers a failed re-stamp).
  - Over the threshold, undelivered settlements are still delivered.
- **Reseat:**
  - Reseat runs only between turns, behind any running turn, and the turn after it picks up held results.
  - Two heads pressing reseat: the second is refused with "nothing to re-seat".

### Lifecycle, jobs, the queue and worktrees (R92)

#### Review of session lifecycle, jobs, queue and worktrees — letibot @ 50b3e55 (read-only)

I read the code only. Nothing was edited, built or signalled. Git checks were read-only (`worktree list`, `status`, `merge-base`, `prune --dry-run`). I covered the lifecycle and the git state myself and gave subagents/jobs and queue/gate to two read-only sub-reviewers. Their findings are merged in below, with their file:line references.

CONFIRMED means the whole path was read, or the fact was seen on disk. SUSPECTED means the code points that way but the path was not fully traced. The list is ranked by how often the operator will hit each one.

---

##### A. Most frequent

**1. Jobs in a firecode session are never watched, so a finished job is announced only on the next turn. CONFIRMED.** This is the "firecode job sat finished until I typed" report.
- **Path:**
  - `harness.rs:3939-3942` builds the job watcher from `backend.processes_arc()`.
  - `FirecodeBackend` (`tools/src/firecode.rs:737-860`) implements `processes()` (856) but not `processes_arc()`, so it gets the default `None` (`tools/src/backend.rs:152`).
  - `JobWatchers::watch` (`jobwatch.rs:917-962`) then drops the handle ("nothing here can ever settle this handle").
- **Effect:**
  - No `JobSettled`, no `JobCompletion`, no wake.
  - The store row from `record_start` stays `running` for ever, and the jobs pane never settles.
  - `wait_on` / `needs: job` rows are never delivered.
  - The completion surfaces only when the model calls `job_wait`/`job_list`, i.e. after the operator types.
- **Should:** implement `processes_arc()` on `FirecodeBackend`, and make the trait method non-defaulted so the next backend cannot forget it.
- **Related, CONFIRMED (`harness.rs:~18186-18203`):** a VM child's backend is torn down right after its first turn. Any background job it left running dies with the VM, and nobody is told.

**2. Slow start: the daemon binds the socket before it is ready to work, and the head shows no sign that it is still booting. CONFIRMED.** This is the rano "took ages and showed nothing but the queued prompt" report.
- **Ordering in `harnessd/src/cli.rs`:**
  - `create_under` (1066) and `Daemon::serve` (1077) come first ("Bind before opening the session").
  - Then come: `open_seat` (flowy, network, 1104), and `served_model` / `served_media_marker` / `served_ctx` (1370, ~1458-1466). These are HTTP calls with a 10 s connect and a 3 min read timeout (`turn/src/serving.rs`).
  - Then `Parts::load` (vocabulary), then `Sessions::open_first_with_seat` (resume of the whole transcript, 1476).
  - Only after all of that does `daemon.run` start the worker (1766).
- **What the launcher does:** `scripts/letibot:1932` polls only "is the socket listening". That is true almost at once, so it `exec`s the head.
- **What the operator sees:**
  - The head attaches (the hub already exists) and shows the transcript.
  - A prompt typed now sits in the hub queue, shown as queued, until boot finishes.
  - Nothing says "daemon is starting: loading vocabulary / resuming N rows / probing model server". A grep for any boot or progress state in harnessd, sessionlog and tui found none.
- **Should:** publish a "starting: <step>" status on the hub during boot, and a line on the queued prompt saying it will run when the worker starts.

**3. If the daemon fails during boot, the head never sees why. CONFIRMED.**
- `open_first_with_seat` failing (cli.rs ~1478) or `--flowy` failing (1104-1108) calls `daemon.shutdown()` and returns `Err`. The reason goes only to the daemon's stderr.
- That stderr is `~/logs/harnessd.log`, **one file shared by every folder's daemon** (`scripts/letibot:268`, `detach … >>"$LOG"`).
- The launcher has already exec'd the head, so the operator sees the connection close without the reason.
- The launcher's own failure path (`scripts/letibot:1933`, `tail -15 "$LOG"`) can show another folder's daemon's lines.
- **Should:** publish the error on the hub before `shutdown`, and give each folder its own log, or prefix lines with the workspace.

**4. Every finished subagent worktree looks dirty, so nothing ever cleans it up. CONFIRMED on disk.**
- **Cause:** the daemon writes its own logs into the session workspace, which for a child is the worktree:
  - `.letibot/delivery-log.jsonl` (`delivery.rs:91,464`),
  - `.letibot/context-claims.jsonl` (`context_claim.rs:568`),
  - `.letibot/notes-offers.jsonl` (`notes_offer.rs:331`).
  - The child's own notes land in `.letibot/notes/`.
- `.letibot/` is neither gitignored nor in `info/exclude` (the main repo tracks `.letibot/notes/*`). So `git status --porcelain` shows `?? .letibot/`.
- `retire_branch` (`mergequeue/git.rs:470-488`), the only cleanup (called from `mergequeue/serve.rs:1296` after a landing), refuses any dirty tree. It logs only an `eprintln` to the shared daemon log.
- **Measured:**
  - 27 of 31 checked worktrees have `.letibot/` (mostly `notes/`, some `notes-offers.jsonl`).
  - 20 of the 37 `agent/*` worktree branches are already merged into main and still have worktrees. The sub-reviewer counted 46 of 64 `agent/*` branches merged.
  - `.claude/worktrees` is **240 GB**: 34 per-tree `target/` dirs of 3.7–7.9 GB each.
- **Knock-on effects:**
  - `EndOfWork` nudges the child about files the daemon wrote (`harness/deliverable.rs:166`, `harness.rs:11655`).
  - The child may `git add -A` the logs onto its branch.
  - The child's notes are stranded in an untracked directory.
- **Should:**
  - Write the daemon's logs to the main tree or the data directory, not the workspace, or add `.letibot/*.jsonl` to the common `info/exclude`.
  - Decide where a child's notes go.
  - Have `retire_branch` ignore known daemon files.
  - Raise the "kept" reason on a hub.

**5. The queue standings on the bottom edge are stale, count every repository, and count only red. CONFIRMED.**
- `tui/src/ui/panes/queue.rs:117-139` draws from `self.queues`. That is refreshed only when the pane opens or is drawn, or on `QueueMoved` while the pane is open (`surface.rs:294`, `events/todos.rs:41`).
- **Operator sees:**
  - `‼ 1 red target(s)` stays on every session's edge after main is green.
  - It counts other repositories' targets.
  - An unjudged or timed-out main raises no edge alarm.
- **Should:** refresh on `QueueMoved` even with the pane closed, filter by the session's repository, and count `blocked`/`timed_out` as alarms.

**6. Nothing ever says "main is green again". CONFIRMED.**
- `red_card.rs:32` states it. `target_is_green` (`serve.rs:883`) only withdraws reviews, and `queue_emergency_notices` (`harness.rs:8232`) only reports trouble.
- **Operator sees:** the transcript and the project model's last word stay "EMERGENCY… nothing lands" after a fix lands.
- **Should:** one recovery warning and session notice per red→green pair.

**7. A daemon restart repeats every alert and wakes dormant sessions. CONFIRMED.**
- The dedupe sets are in memory only: `RepoQueue.rung` (`serve.rs:145`) and `Harness.gate_told` (`harness.rs:1976`).
- `regate_at_start` (`verbs.rs:540`) re-gates the red tip, so "EMERGENCY — main is RED" is told again.
- `ring_project` (`mergequeue.rs:366-391`) calls `registry.create` to resume and wake old root sessions for every blocked or cannot-decide review.
- `gate_killed` and `gate_setup_failed` re-raise on every re-gate.
- **Should:** persist what was told or rung, per sha/state or row/state.

**8. Reviews freeze while any gate runs in that repository. CONFIRMED (structural).**
- `RepoQueue::pass` (`serve.rs:224-279`) runs upkeep, then health, then land on one thread. The target gate (`serve.rs:724`) and the landing gate (`serve.rs:1122`) are synchronous and can take up to 30 minutes.
- During that time there are no claims, reviewer deadlines are not enforced (`serve.rs:399-445`), and capacity holds are not rechecked.
- **Operator sees:** reviews sit `waiting` with no reason given.

**9. The single daemon worker thread runs a root session's turn inline, so other work starves. CONFIRMED structure; impact SUSPECTED.**
- `daemon.rs` `run`: `Work::Command` goes to `sessions.dispatch`, then `run_prompt`, then `Sessions::turn`, all on the worker thread (`sessions.rs:2036-2119`).
- Nags, abandoned-call sweeps, child parking and `serve_reviews` / `ensure_gatekeeper` run only in the `WorkOrIdle::Idle` arm. That arm fires only when no work arrives before the idle deadline.
- **Effect:**
  - A long turn in one root session delays prompts to other root sessions in the same daemon, and delays all of the Idle tasks.
  - A slow `Work::Open` (resuming a big session) blocks the same way.
- This may also contribute to "why a queued message waits".

---

##### B. Frequent

**10. A child's later turns never reach the parent. CONFIRMED.**
- The slot settles once, at the end of the child's first turn (`harness.rs:~18210`; `task.rs:370-380`). Later turns run silently (`serve_child_with`, `harness.rs:15260-15470`).
- **Sequence:** the child ends with "waiting on j97". The parent takes that as the answer. The child later finishes and commits, and the parent never hears.

**11. The branch is enqueued right after the child's first turn, whatever state it is in. CONFIRMED.**
- `enqueue_finished` (`harness.rs:18203` for VM children, `18224` for host children) runs even with jobs still running, a dirty tree, or head == base.
- `mergequeue/review.rs:631-685` does `branch_head(...).unwrap_or_default()`, so a deleted branch is enqueued with an empty `head_sha`.
- The enqueue is idempotent by id, so commits from later turns are never re-enqueued. SUSPECTED: whether the gate re-reads the head.

**12. The main checkout carries the `.cargo/config.toml` trap that `.gitignore` warns about, and every nested worktree inherits it. CONFIRMED on disk; the cargo behaviour is standard config discovery.**
- `~/Projects/letibot/letibot/.cargo/config.toml` contains `paths = ["/home/dead/Projects/rano/rano"]`.
- `.claude/worktrees/*` is nested under that checkout, and cargo merges config from every parent directory. So every subagent worktree build silently uses the local rano checkout instead of the locked `v0.8.3` tag. The local checkout is currently `v0.8.3-1-gde2ae1f`, one unreleased commit ahead.
- The landing worktree (`~/.local/share/letibot/landing/…`) and VM clones are **not** nested, so they build the real tag. That gives "builds in the subagent, fails or differs in the gate".
- It goes silently wrong the moment the rano checkout moves to a different version (the 2026-10-08 incident in `.gitignore`).
- **Legacy files:** 31 worktrees also hold a per-tree `.cargo/config.toml` with `target-dir = ".../letibot/letibot/target"` ("Written by `task_start`"). The sub-reviewer reports the runner no longer writes these; the build env now comes from `worktree_build_env` (`harness.rs:19012`). They are gitignored, so they do not count as dirty.
  - **Inferred, not measured:** these files point at the main target. An explicit `CARGO_TARGET_DIR` overrides them, but a build without that env (a hand-run cargo in the tree) shares and locks the main target.
- **Should:** delete the main checkout's `.cargo/config.toml`, and have cleanup remove the legacy per-tree files.

**13. Job ids restart at j1 after every daemon restart. CONFIRMED.**
- `JobId::next()` is a process-global counter (`tools/src/exec/jobs.rs:44-48`), never seeded from the store.
- The store `job` table is upserted by `(session_id, handle)` (`tokencore/src/store.rs:490-501`, 3352), so the new j97 overwrites the old one's history.
- `job_fate` (`harness.rs:13719-13730`) and `delivery::names` (`delivery.rs:249`) match `when/needs: job j97` written before the restart to the new j97.
- In the TUI, "waits on j97" links (`tui/src/app/todos.rs:763-860`) can open the wrong job's output or slug.

**14. A job slug used in `needs` reads as "already ended". CONFIRMED.**
- `todo_write` stores the raw handle (`tools/src/builtins/todo.rs:3437-3449`); only `wait_on` resolves slugs (`todo.rs:3114`).
- `job_fate` looks up `JobId(slug)`, gets `Unknown`, and treats it as ended (`harness.rs:13568`). The row unblocks while the job runs.
- Slugs are unique only per daemon life (`exec/host.rs:1139-1160`).

**15. Worktree debris: nothing cleans up beyond a clean landing. CONFIRMED on disk.**
- **Registered vs on disk:**
  - `git worktree list`: 48 entries. `.claude/worktrees`: 62 dirs.
  - 24 dirs (`agent-a<hex>`) have a `.git` file pointing at `/home/dead/Projects/letibot/.git/worktrees/…`, which does not exist. These are orphans from before the repo moved down a level, and git cannot see them.
  - 3 registered worktrees (`agent-a8e2db02…`, `agent-ac359f48…`, `agent-adff1e33…`) are **locked** at paths under `/home/dead/Projects/letibot/.claude/worktrees/` that no longer exist. `prune` will not touch locked entries.
  - Also `wt-drill`, `wt-jobs`, `wt-visibility`, `wt-yellow` sit outside `.claude`.
- **Dirty worktrees, 5 of them:**
  - `agent-agents-md` is mid-merge with a `UU` conflict in `standing_notes.rs` (2 days old).
  - `agent-operator-input` has 5 modified, uncommitted TUI files (4 days old). This is a subagent that finished without committing.
  - `agent-store-busy` has a modified `.commit-msg`.
  - `agent-why-a-queued-message-waits` has an untracked test file.
  - `agent-quiet-shell` has `.scratch/`.
- **Cleanup coverage:** nothing removes worktrees of children that failed, were refused, were killed or are dirty. `retire_branch` is the only path.

**16. A firecode `task_start` child is given a deliverable it cannot meet. SUSPECTED.**
- The host worktree is created and the brief says to commit to `agent/<slug>` (`harness.rs:16725-16780`, `18120-18135`). But the VM works on a copy (`task.rs:52-65`).
- So the host tree always reads clean, and an empty branch is enqueued.

**17. Slug collisions and unvalidated slugs. CONFIRMED.**
- **Derived slugs collide:** `slug_from_prompt` (`task.rs:299-328`) takes the first 4 ASCII words. Briefs that begin alike collide, and non-ASCII prompts all become `task`. The second spawn is refused "already exists" (`task.rs:1131-1153`, `harness.rs:16731-16737`). The `-2` suffixes on disk show this has been worked around by hand.
- **A stale branch or registration blocks the slug:** with the directory gone but the branch or registration left, `worktree add -b` fails with no hint to prune (`harness.rs:16744-16752`).
- **User slugs are only trimmed** (`task.rs:1098-1102`):
  - `/` nests directories and can clash with a ref.
  - `..` escapes `.claude/worktrees`.
  - Spaces give an invalid branch name.
  - On disk, `agent-agent-refresh` shows the prefix doubled.

**18. The design rule "red main halts both queues and returns items to review" is not what the code does. CONFIRMED.**
- `serve.rs:465` and `803` keep reviews and enqueueing going while red (the "relaxed rule").
- `land_next` (`serve.rs:918-923`) just leaves rows `queued`, and their evidence stays the approval text.
- On green, they land without re-review.

**19. `queue_settings_unreadable` floods every hub about every 2 s. CONFIRMED.**
- `serve.rs:231-243` has no dedupe, at Failure class, for a malformed `[queue]` in leticode.toml.

---

##### C. Occasional

**20. Exit-and-stop against an OLDER daemon goes straight to SIGKILL and can lose the in-flight turn. CONFIRMED path.**
- The head takes the force path at once (`tui/src/driver.rs:746-764`, `ForceWhy::OlderDaemon`). The force path is `head.rs:675` `force_stop`: interrupt (3 s budget), then SIGTERM, then SIGKILL 5 s later.
- `server.rs:670` notes that daemons built before that change still refuse a mismatched `Attach`. Against those, `interrupt_turns` → `discover` is refused, the interrupt never lands, the old daemon's SIGTERM finishes its turn, and SIGKILL ends it after 5 s. The comment's promise that "no turn's rows are lost" does not hold for those daemons.
- **The operator does see why:** "the interrupt did not go through (…)".
- **Should:** when the interrupt cannot be delivered, say before signalling that the running turn's last rows may be lost, and offer to wait instead.

**21. In `interrupt_turns`, the "N turn(s) still running" message can never be produced. CONFIRMED.**
- `head.rs` `interrupt_turns`: once the deadline passes it calls `ask_sessions(&mut client, &rx, deadline)` with a deadline already past. `ask_sessions` returns `Err("timed out waiting for the session list")` immediately.
- The `?` inside the polling loop does the same near the deadline.
- **Operator sees:** `letibot --stop` and the head's force path report "timed out waiting for the session list" instead of "2 turn(s) still running after 30s".
- **Also:** plain `--stop` sends the interrupt reason `"letibot --stop --force"` (hard-coded), so the abort row names the wrong command.

**22. The quit card under-counts what a stop kills. SUSPECTED.**
- `tui/src/ui/cards/quit.rs` `quit_view` counts `self.heads - 1`, this session's jobs and this session's subagents.
- Stopping the daemon also kills the other sessions' turns and heads, the gatekeeper's reviews and any merge-queue gate (`merge_stop`). None of these are counted.
- **Should:** ask the daemon for its totals across sessions before offering "stop daemon".

**23. Launcher stop edge cases. CONFIRMED.**
- `letibot --stop --force 3` ignores the `3` and stops **this** folder's daemon. `scripts/letibot:1514` skips `retarget` when `$2` is `--force`. `--stop 3 --force` works.
- `--stop --all`:
  - It never sends `--interrupt-all` and never escalates with `--force`.
  - A busy daemon finishing a turn is reported as "ignoring SIGTERM after 5s", which is the wrong reason.
  - It advises `letibot --stop --force`, which acts only on this folder.

**24. Nothing checks that the workspace still exists. CONFIRMED (no check); SUSPECTED effect.**
- `retire_branch` force-removes a worktree with no check that its child is alive. A host child stays reachable for 10 minutes and a parked child reopens on the next prompt (`config.rs:1739`).
- A grep for any workspace-existence check in harnessd and tools found none. Tool spawns `current_dir` into the missing directory (`exec/host.rs:1353`), so every call fails with a bare ENOENT.
- `Deliverable::uncommitted()` returns `None` when git fails (`deliverable.rs:59-75`), so the completion treats the gone tree as clean.
- The daemon's own log writers `create_dir_all` the `.letibot/` dir back into the removed path.
- **Should:** refuse to retire a live child's tree, and say "workspace gone" on open or wake.

**25. Gate killed at its memory cap: it is unjudged and named correctly, but retries are "never, or every restart". CONFIRMED.**
- **Target side:**
  - `gate.rs:357-399` classifies it as `Blocked` with `GATE_KILLED`, and the TUI shows "UNJUDGED · gate killed at its memory cap". That part is good.
  - It is not retried in the daemon's life (`serve.rs:676-681`), but it is retried on every lease take (`verbs.rs:540`) with no backoff. With the same cap it dies the same way, and the notices repeat.
- **Merge side:**
  - A killed landing gate is `Blocked` until `/queue restart` (`serve.rs:1175-1186`).
  - Its text says "could not run", but it ran and was killed.
  - The advice names `regate`, but the verb for a branch row is `restart` (`gate.rs:77`).

**26. A memory kill not at the cap, or with no cgroup, is judged RED. CONFIRMED path.**
- `red_cause` (`gate.rs:207-228`) only counts a signal on the step leader.
- A box-wide OOM or oomd kills a rustc, and cargo exits 101 with "(signal: 9, SIGKILL". That becomes `RedCause::Code`: RED, an emergency, a red-tip review and possibly a fix branch.

**27. A timed-out target, and several blocked causes, raise nothing on any hub. CONFIRMED.**
- `serve.rs:764-781` handles `TimedOut` in the `_ => {}` arm.
- "the gate could not be read", "landing worktree could not be made ready" and non-`GATE_KILLED` `Infra` are not in `emergency_health` (`serve.rs:1308`).
- They freeze landings until a restart, regate or new commit, and show only as a ⚠ line in the pane.

**28. Merge-row failures tell nobody, and parked merge rows never settle. CONFIRMED.**
- A rebase conflict (`serve.rs:1050-1063`) and a gate failure (`serve.rs:1145-1153`) raise nothing. `ring_the_projects` (`serve.rs:288`) looks at reviews only.
- `settle_if_needless` covers review rows only, so a merge row whose branch was landed by hand stays parked for ever.

**29. Red-tip reviews survive green. CONFIRMED.**
- (a) `target_is_green` withdraws only `is_open()` rows (`queues.rs:248`). `CannotDecide` and `Rejected` rows remain, and `CannotDecide` is re-rung after each restart.
- (b) A landing sets Green directly (`serve.rs:1281-1296`) without calling `target_is_green`. A waiting red-main review for a later sha can still reject, which opens a fix branch and raises "EMERGENCY" while main is green.

**30. Alerts reach only the lease-holding daemon's sessions; some reach every repository. CONFIRMED.**
- `announce_to` / `publish_for_repository` / `ring_repository` (`mergequeue.rs:355-430, 513`) use this daemon's registry only. A second daemon in the same repository (for example one seated in a worktree) gets nothing.
- `queue_lease_lost`, `queue_wrong_build` and the store-error warnings use the global `announce`, so they reach every repository's hubs.
- `publish_for_repository` falls back to the gatekeeper hub alone when `Store::open` fails.
- **Repository identity:** it is the canonical path of the parent of `git-common-dir` (`git.rs:116-174`). Worktrees map to the main checkout. Remote URLs are not used.
- SUSPECTED: a non-repo directory with exactly one repository child counts as that repository (`git.rs:155-166`).

**31. The same sha going red twice is silent. CONFIRMED.**
- The review id is `red-<target>-<sha12>` with INSERT OR IGNORE (`queues.rs:807`). The withdrawn row stays withdrawn, `fresh=false` (`serve.rs:862`), and the in-memory `rung` / `gate_told` keys are already set.

**32. Reviewer start failures are handled inconsistently. CONFIRMED.**
- A synchronous non-capacity error (for example a `clone_branch` failure) blocks on the first strike (`review.rs:261-280`), while an asynchronous one gets 3 attempts.
- "No reviewer started within 5 min" blocks on the first strike (`serve.rs:402-411`), and blames the review session even when `ring_reviewers` silently no-op'd because `s-gatekeeper` was not in the registry (`mergequeue.rs:359-364`).

**33. Push failure and install failure are mislabelled. CONFIRMED.**
- A push failure with no install step raises nothing; the raise is inside `if !install.is_empty()` (`serve.rs:1235-1265`).
- An install failure after a good landing is raised as `queue_blocked`, though nothing is blocked (`serve.rs:1251`).

**34. VM-capacity waits mostly name the real reason. CONFIRMED, minor gaps.**
- The evidence says "4 of 4 VM slots are busy — no attempt is counted…" (`notices.rs:360`), and it is deduped durably.
- **Gaps:**
  - The overlay shows `failure` only when attempts > 0 (`queue.rs:378-384`).
  - The evidence is overwritten on re-claim.
  - Only firecode's tap-slot wording is recognised (`capacity.rs:67-71`); any other capacity refusal is counted as an attempt and blocks after 3.

**35. The `wake: later` sentence tells the model the wrong thing. CONFIRMED.**
- `tools/src/builtins/bash.rs:~308-318` says a held result "never starts a turn of its own".
- The delivery rules contradict it (`delivery.rs:27-48`, `jobwatch.rs:660-671`, `harness.rs:1448-1491`). A model that believes it may poll, or wait to be prompted.

---

##### Checked and fine

- **Protocol skew:** the daemon accepts a mismatched protocol and says so (`server.rs:670`). The head locks sends against an older daemon, with a `ctrl-^` override (`app/session.rs:695-760`). The launcher probes before attaching and refuses with an explanation (`scripts/letibot:1142, 1961`). The messages name the direction.
- **Two daemons for one folder:** the folder lock, `live_detached` / `refuse_detached` and `wait_gone` cover the stop/start race (`scripts/letibot:1086-1122, 1870-1910`).
- **Restart notes** (`restart.rs`): a crash estimates the down time from the last heartbeat, which can be up to 30 s early. An unknown stop is treated as over the threshold and said so.
- **Root-session job completion** is pushed, not left to the next input: `SettlementQueue` → `ring_wake` → `Work::Woken` → `between_turns`. Late announcements come from findings 1 and 10.
- **Not reviewed in depth:** the stale-prompt / reseat card path and two heads on one session. Grep shows explicit multi-head handling; I found no defect there.

### Lifecycle, second hand-back — the same review re-ordered, with a few details the first lacks (R92)

#### letibot @ 50b3e55: session lifecycle, subagents/jobs and queue edge cases (read-only review)

I edited nothing, built nothing, and did not touch any process or sessions.db. Part 1 and the git state are my own reading of the code and the disk. Parts 2 and 3 were reviewed by two read-only subagents. I re-checked their two headline claims myself (finding A1 and the job-id counter in A5); everything else is their work, labelled as they marked it.

**Confirmed** means the whole code path was read, or the state was seen on disk. **Suspected** means the code points that way but not every step was traced. Findings are ranked by how often the operator hits them, across all three areas.

---

##### A. Hit daily

**A1. A job in a firecode session is never watched, so its finish is only seen when someone asks. Confirmed (this is the firecode report).**
- **Where:**
  - `harness.rs:3939-3942` builds the job watcher only when `backend.processes_arc()` returns a host. Otherwise it builds `JobWatchers::watching_tasks`, which has no host.
  - `FirecodeBackend` (`tools/src/firecode.rs:737-860`) implements `processes()` but not `processes_arc()`. It falls back to the trait default, `backend.rs:152`, which returns `None`.
  - `JobWatchers::watch` (`jobwatch.rs:917-962`) then drops the handle ("nothing here can ever settle this handle").
- **Sequence:** the model runs `bash --background` in a firecode session and the job finishes. Nothing follows: no `JobSettled`, no `JobCompletion`, no wake.
- **Operator sees:**
  - The finish only shows up on the next turn, when the operator types and the model calls `job_*`.
  - The job's row in the store stays `running` for ever, and the jobs pane never settles it.
  - `wait_on` / `needs: job` rows are never told the job is done.
- **Related, confirmed (`harness.rs:~18186-18203`):** a VM child's backend is shut down right after its first turn. Any background job it left running dies with the VM, and nobody is told.
- **Should:** `FirecodeBackend::processes_arc` returns its `Arc<HostProcesses>`. Better, remove the trait default (or derive it from `processes()`) so the next backend cannot forget it.

**A2. While the daemon boots, the head is attached to a live-looking session that cannot run anything: the "rano took ages, showed only the queued prompt" report. Confirmed.**
- **Where:** `harnessd/src/cli.rs:1065-1077` creates the first session's hub and binds the socket ("Bind before opening the session"). The worker loop `daemon.run` only starts at `cli.rs:1766`.
- **What runs in between, on this thread:**
  - `open_seat` (flowy, over the network)
  - `served_model`, `served_media_marker` and `served_ctx`: HTTP calls to the model server with a 10 s connect timeout and a 3 min read timeout each (`turn/src/serving.rs`)
  - `Parts::load` (the vocabulary)
  - `Sessions::open_first_with_seat` (a resume, which re-tokenises the transcript)
- **Sequence:** the launcher's `live` check passes as soon as the socket listens (`scripts/letibot:1932`), and it execs the head. The head gets its `Hello` and shows the transcript. The operator types, and the prompt is queued in the hub, but no worker takes it until `daemon.run`.
- **Operator sees:** the queued prompt and nothing else, for as long as boot takes. Nothing on the hub says "daemon still starting". A grep for any boot or progress status in harnessd, sessionlog and tui finds none.
- **Should:** publish a "starting: loading vocabulary / resuming N rows / asking the model server" status on the hub from bind until `daemon.run`. The head should label queued prompts as "queued until the daemon finishes starting".

**A3. Merged subagent worktrees are never cleaned up, because the daemon's own files make every worktree look dirty. Confirmed (on disk and in code).**
- **Where:**
  - `retire_branch` (`mergequeue/git.rs:470-488`) refuses when `git status --porcelain` shows anything at all.
  - The daemon writes into `<workspace>/.letibot/`: `delivery.rs:91,464` (`delivery-log.jsonl`), `context_claim.rs:568`, `notes_offer.rs:331`. Children also write notes to `.letibot/notes/`.
  - For a `task_start` child, the workspace is the worktree. `.letibot/` is not in `.gitignore` and not in `info/exclude`, which holds only `.claude/`.
- **On disk:**
  - 23 worktrees hold `.letibot/notes`, and 3 more also hold `notes-offers.jsonl`.
  - 20 of the 37 `agent/*` branches checked out in worktrees are already merged into main, and their worktrees are still there. The other reviewer counted 46 of 64 `agent/*` branches merged.
  - The refusal is only an `eprintln` to the daemon log (`serve.rs:1296-1299`), so the operator never sees it.
- **Knock-on effects:**
  - The end-of-work nudge (`harness/deliverable.rs:166`) tells the child about files the daemon itself wrote.
  - A child may `git add -A` the daemon's logs onto its branch.
  - Notes a child wrote are stranded in an untracked directory.
- **Should:** keep the daemon's jsonl logs out of the workspace (in the data dir), or exclude `.letibot/*.jsonl` in the common `info/exclude`. Decide where a child's notes belong. Say the retire refusal on a hub.

**A4. The bottom-edge queue standings are stale, count every repository, and only count `red`. Confirmed.**
- **Where:**
  - `tui/src/ui/panes/queue.rs:117-139` builds the edge from `self.queues`.
  - That is set only by `ServerFrame::Queues`, which the daemon sends only when asked.
  - The head asks only when the queue pane is open (`surface.rs:294`, `queue.rs:147`, `events/todos.rs:41`).
- **Operator sees:**
  - `‼ 1 red target(s)` stays on every session's edge after main goes green, until the pane is reopened.
  - Repo A's red main shows in repo B's sessions.
  - An unjudged (gate killed, setup failed) or timed-out main raises no edge alarm at all.
- **Should:** push or refresh the snapshot on `QueueMoved` even while the pane is closed, filter by the session's repository, and count `blocked` / `timed_out` as alarms.

**A5. Job ids restart at j1 on every daemon restart, so "waits on j97" can point at the wrong job. Confirmed (I verified the counter).**
- **Where:**
  - `tools/src/exec/jobs.rs:44-48`: `static N: AtomicU64 = 1`, never seeded from the store.
  - The store's `job` table is keyed `(session_id, handle)` and upserted (`tokencore/src/store.rs:490-501`, `put_job` at 3352), so a new j97 overwrites the old j97's history.
  - `job_fate` (`harness.rs:13719-13730`) and `delivery::names` (`delivery.rs:249`) match a `when` / `needs: job j97` row written before the restart to the new j97.
  - The TUI resolves the "waits on j97" link (`tui/src/app/todos.rs:763-860`) against the current list or the newest notice, so it can open the wrong job's output.
- **Should:** namespace handles per daemon instance, or seed the counter from the store's `max(handle)`.

**A6. A job slug written in `needs` reads as "already ended". Confirmed.**
- **Where:**
  - `todo.rs:3437-3449` stores the raw string. Only the `wait_on` door resolves a slug (`todo.rs:3114`).
  - `job_fate` looks up `JobId(handle)` only. A slug gives `Unknown`, which is treated as ended (`harness.rs:13568`).
- **Operator sees:** the row unblocks, and the nag talks about the job, while it is still running. Meanwhile `delivery::names` does match the slug, so the two readers disagree.
- **Also:** slugs are unique only within one daemon's life (`exec/host.rs:1139-1160`).

**A7. A child's later turns never reach the parent. Confirmed.**
- **Where:**
  - The task slot settles once, at the end of the child's first turn (`harness.rs:~18210`; `task.rs:370-380`).
  - `serve_child_with` (`harness.rs:15260-15470`) runs every later turn, and none of them notifies the parent.
- **Sequence:** the child ends turn 1 with "waiting on j97", and the parent takes that as the answer. When j97 settles, the child wakes, finishes the work and commits. The parent never hears.
- **Operator sees:** "child done" on stale text, then silence. The only way to learn more is `task_result`, which returns a live reading rather than the new answer.
- **Should:** a child turn that ends after the slot has settled pushes a second completion to the parent.

**A8. The branch is enqueued after the child's first turn, whatever state it is in. Confirmed path.**
- **Where:** `enqueue_finished` runs at `harness.rs:18203` (VM children) and `18224` (host children). It runs even when:
  - jobs are still running, or the child will commit in a later turn (see A7);
  - the tree is still dirty after the one nudge;
  - the head equals the base.
- **Also:** `mergequeue/review.rs:631-685` uses `branch_head(...).unwrap_or_default()`, so a deleted branch is enqueued with `head_sha = ""`.
- **Also:** the enqueue is idempotent by id, so later commits are never re-enqueued with the new head. Whether the gate re-reads the head is suspected, not traced.

##### B. Hit weekly

**B1. Every daemon restart re-rings the same alerts and wakes dormant sessions. Confirmed.**
- **Where:**
  - The dedupe state is in memory only: `RepoQueue.rung` (`serve.rs:145`) and `Harness.gate_told` (`harness.rs:1976`).
  - `regate_at_start` (`verbs.rs:540`) re-gates the red tip, which raises again.
  - For every blocked or cannot-decide review, `ring_project` (`mergequeue.rs:366-391`) calls `registry.create`, which resumes a dormant root session to re-tell the same notice.
  - `gate_killed` and `gate_setup_failed` re-raise on every re-gate. Only `target_red` is durably deduped.
- **Should:** persist what has been rung and told, keyed by row and state or by sha and state.

**B2. Nothing ever says "green again". Confirmed.**
- `red_card.rs:32` says so outright. `target_is_green` (`serve.rs:883`) only withdraws reviews.
- The model's last word stays "EMERGENCY… nothing lands", and it keeps acting on a red main until it calls `queue status`.

**B3. The review queue freezes while any gate runs in that repository. Confirmed (structural).**
- **Where:** `RepoQueue::pass` (`serve.rs:224-279`) runs review upkeep, the target gate (`:724`) and the landing gate (`:1122`) on one thread. A gate can take up to 30 minutes.
- **Effect:** for the whole gate, no review is claimed, reviewer deadlines and the never-started check (`serve.rs:399-445`) are not enforced, and capacity holds are not rechecked.
- **Operator sees:** reviews sit `waiting`, and nothing says why.

**B4. Separately, the daemon's worker loop is single-threaded for root sessions. Confirmed structure; suspected effect.**
- **Where:**
  - `daemon.rs:150-300` runs `Work::Command` (`sessions.dispatch` → `run_prompt` → `turn`, inline, `sessions.rs:2036-2119`) and `Work::Open` on one thread.
  - The upkeep in the `Idle` arm runs only when no work rings before the deadline: `deliver_due_nags`, `serve_reviews`, `park_idle_children`, `sweep_abandoned_calls`.
- **Effect:** a long turn in one root session delays prompts to other root sessions in the same folder's daemon, and starves nags and review service. Children run on their own threads.

**B5. The code does not follow the operator's design ("a red main halts both queues and returns items to review"). Confirmed.**
- **Where:** `serve.rs:465` and `target_went_red` (`:803`) keep reviews going on red. `land_next` (`:918-923`) just leaves merge rows `queued`.
- **Effect:**
  - No row is moved back to review, and each row's evidence stays its approval text, so nothing says why it is not moving.
  - When main is green again, those rows land without being reviewed again.

**B6. Stopping a daemon while a turn is running prints the wrong reason. Confirmed.**
- **Where:** in `tui/src/head.rs:579-605` (`interrupt_turns`), once the deadline passes, `ask_sessions(…, deadline)` is called with a deadline already in the past. It returns "timed out waiting for the session list" at once, so the intended "N turn(s) still running after Xs" can never be printed.
- **Also:** the interrupt's reason is hard-coded as "letibot --stop --force" (`head.rs:597`), so a plain `--stop` is recorded in the transcript as a forced stop.
- **Also:** `scripts/letibot:1554` runs `--interrupt-all || true`, which swallows the failure. The script then reports "NOT stopped: … is ignoring SIGTERM after 5s" (`:1572`) when the daemon is really finishing a turn the interrupt never reached.

**B7. Stopping an older (protocol-skewed) daemon. Confirmed path; whether rows are lost is suspected.**
- **Where:** the head goes straight to the force path (`driver.rs:746-764`, `head.rs:675-760`): interrupt (3 s), then SIGTERM, then SIGKILL 5 s later.
- **The problem:**
  - A daemon built before the "accept any protocol version" change (`sessionlog/src/server.rs:670`; its own comment says older daemons "go on refusing") refuses the interrupt's attach.
  - So the turn is never interrupted. An older daemon finishes its turn before honouring SIGTERM (`scripts/letibot:1546-1552`).
  - SIGKILL then lands mid-turn, which is the row loss the protocol half of the force path exists to prevent.
- **What the operator sees is honest:** "the interrupt did not go through (…)".
- **Gap:** neither the force path nor the `--stop` path in `scripts/letibot` waits for the turn to end before escalating.

**B8. A gate killed at its memory cap is correctly counted as unjudged, but the retry policy is "never, or on every restart". Confirmed.**
- **Target side:** classified `Infra`, state `Blocked`, prefix `GATE_KILLED`, shown as "UNJUDGED · gate killed at its memory cap" (`gate.rs:357-399`, `serve.rs:774`). That part is right.
  - It is never retried within a daemon's life (`serve.rs:676-681`).
  - It is retried on every lease take with the same cap and no backoff, so it likely dies again, and the alert repeats (B1).
- **Merge side:** a killed landing gate goes to `Blocked` until someone runs `/queue restart`.
  - The message says "could not run", but it ran and was killed.
  - The advice names `/queue regate` / "gate it again" (`gate.rs:77`). The verb for a branch row is `restart`.

**B9. A memory kill far from the cap, or with no cgroup, is reported RED. Confirmed path.**
- **Where:** `red_cause` (`gate.rs:207-228`) only counts a signal when the step leader itself died by one.
- **Sequence:** oomd or a box-wide OOM kills a rustc. Cargo prints `(signal: 9, SIGKILL` and exits 101.
- **Effect:** RED, an emergency, a red-tip review, and possibly a fix branch. The same happens on every `Unscoped` placement.

**B10. Removing a subagent's worktree under a live child goes unsaid. Confirmed that there is no check; frequency suspected.**
- **Where:**
  - `retire_branch` removes the tree with `--force` after a landing and does not check whether the child is alive. Host children stay live 10 minutes (`config.rs:1739`) and reopen when prompted.
  - `Deliverable::uncommitted()` returns `None` when git fails (`deliverable.rs:59-75`).
- **Effect:** a later `task_message` runs the child in a directory that no longer exists, and nothing says the worktree is gone.
- **The same applies to a removed workspace in general:** a grep finds no "workspace exists" check anywhere in harnessd, tools or sessionlog. Tool spawns use `current_dir(cwd)` (`exec/host.rs:1353`), so I suspect every tool call fails with a bare ENOENT.
- **And:** the daemon's log writers call `create_dir_all`, so they would recreate `<gone>/.letibot/`.

##### C. Hit occasionally

**C1. A startup failure after the socket is bound loses its reason. Confirmed.**
- **Where:**
  - `cli.rs:1104-1108` (flowy seat) and `1478-1481` (`open_first`) call `daemon.shutdown()` and return `Err`.
  - The error goes only to the daemon's stderr, which is `~/logs/harnessd.log` (`scripts/letibot:268`), and that file is shared by every folder's daemon.
  - The launcher has already exec'd the head, so the head just sees the daemon close.
- **Also:** the launcher's "did not come up; last lines of $LOG" (`:1933`) tails that shared log, which can show another folder's daemon's lines.

**C2. Exit-and-stop reports only this session's heads, jobs and subagents. Suspected.**
- **Where:** `tui/src/ui/cards/quit.rs` builds `other_heads` from `self.heads`, which is session-scoped, and `running_jobs` and `running_subagents` from this head's own lists.
- **Effect:** stopping the daemon also kills other sessions' heads and turns, the gatekeeper's reviews, and a running merge gate, without any of that being named on the card.

**C3. `letibot --stop --force N` ignores `N`. Confirmed.**
- **Where:** `scripts/letibot:1514` retargets only when `$2` is not `--force`, so `--stop --force 3` stops this folder's daemon.
- **Also:** `--stop --all` never uses `FORCE` (no SIGKILL), yet tells the operator to run `--stop --force`, which acts only on this folder.

**C4. Smaller queue findings, all from the queue reviewer and marked confirmed:**
- **Timed-out target:** gives no hub warning (`serve.rs:764-781`).
- **Silent blocked causes:** "the gate could not be read", "the landing worktree could not be made ready", and an `Infra` step that could not spawn raise nothing anywhere. They also never retry until a restart, regate or new commit.
- **Settings flood:** `queue_settings_unreadable` is raised on every pass, about every 2 s (`serve.rs:231-243`).
- **Silent merge failures:** a rebase conflict (`serve.rs:1050-1063`) or a failed gate (`:1145-1153`) on a merge row tells nobody.
- **Merge rows never settle:** `settle_if_needless` covers reviews only, so a parked merge row whose branch was landed by hand stays parked for ever.
- **Red-tip reviews survive green:**
  - `target_is_green` withdraws only open rows, so `CannotDecide` and `Rejected` red-tip reviews survive, and the `CannotDecide` one is re-told after each restart.
  - A landing sets health Green directly (`serve.rs:1281-1296`) without withdrawing a pending red-tip review. That review can still reject and raise "EMERGENCY" on a green main.
- **The same sha going red twice is silent:** `red-<target>-<sha12>` plus INSERT OR IGNORE (`queues.rs:807`) means no review and no raise; only the pane shows it.
- **Alerts reach only the lease-holding daemon's sessions:** `mergequeue.rs:355,393-430,513`.
  - A second daemon in the same repository hears nothing.
  - `queue_lease_lost` and `queue_wrong_build` use the global `announce`, so they reach every hub of every repository.
- **Reviewer start failures are inconsistent:** a synchronous start error (for example a failed clone) blocks on the first failure, while an asynchronous one gets 3 tries (`review.rs:261-280`).
  - "No reviewer started within 5 min" blocks on the first strike (`serve.rs:402-411`).
  - When s-gatekeeper is not in the registry, the message blames the wrong thing: `ring_reviewers` silently does nothing.
- **Push failures:**
  - A failed push with no install step raises nothing (`serve.rs:1235-1265`).
  - An install failure after a successful landing is raised as `queue_blocked`, though nothing is blocked.
- **VM capacity wait:** mostly shows the real reason ("4 of 4 VM slots are busy…", `notices.rs:360`, deduped durably).
  - Only firecode's slot wording is recognised (`capacity.rs:67-71`). Any other capacity refusal counts as an attempt and blocks after 3.
  - The detail overlay hides the failure while attempts are 0.
- **Repository scoping:** the key is the canonical parent of `--git-common-dir`, so worktrees map correctly to their repository.
  - Suspected: a non-repo directory with exactly one repository child is treated as that repository (`git.rs:155-166`).

**C5. Slug collisions and unvalidated slugs. Confirmed.**
- **Derived slugs collide:** `slug_from_prompt` (`task.rs:299-328`) takes the first 4 ASCII words. Same-start briefs share one slug, and non-ASCII prompts all get `task`. The second spawn is refused. The `-2` worktrees on disk show this being worked around by hand.
- **Leftovers block a slug:** a leftover `agent/<slug>` branch or a stale `.git/worktrees/<name>` entry makes `git worktree add -b` fail, and the error suggests no prune (`harness.rs:16744-16752`).
- **A supplied slug is only trimmed** (`task.rs:1098-1102`):
  - `/` nests directories and can clash with a ref.
  - `..` escapes `.claude/worktrees`.
  - Spaces make an invalid branch name.
  - `agent-agent-refresh` on disk shows the prefix being doubled.

**C6. A firecode `task_start` child is told to commit to a host branch it cannot reach. Suspected.**
- **Where:** `harness.rs:18120-18135` gives the deliverable for the host path, but the VM works on a copy (`task.rs:52-65`).
- **Effect:** the host tree always reads clean, the nudge never fires, and an empty branch is enqueued.

**C7. The `wake: later` sentence contradicts the delivery rules. Confirmed.**
- **Where:** `wake_sentence(None)` (`bash.rs:~308-318`) tells the model a held result "never starts a turn of its own".
- **But:** the R79 rules (`delivery.rs:27-48`, `jobwatch.rs:660-671`) do deliver it as a turn of its own. A model that believes the sentence may poll with `job_wait` instead.

**For reference:** a normal host job in a root session does push its completion, without waiting for the operator: watcher → `SettlementQueue::push` → `ring_wake` → `Work::Woken` → `Harness::wake`. So the late announcement comes from A1 or A7, not from the root delivery rules.

---

##### Git state: the stale worktrees

All of this is confirmed on disk:

- **Counts:** `git worktree list` shows 48 entries. `.claude/worktrees` holds 62 directories and takes **240 GB**; 34 of them carry their own `target/`, the largest 7.9 GB (`agent-job-slugs`).
- **24 orphaned checkouts:** the `agent-a<hex>` directories have `.git` files pointing at `/home/dead/Projects/letibot/.git/worktrees/...`, which no longer exists (it looks like an old repository location). Git does not know about them, and nothing will ever clean them up.
- **3 locked registrations with no directory:** `agent-a8e2db02c7b03ba25`, `agent-ac359f48783c335c5` and `agent-adff1e3334619ee0f` are registered at `/home/dead/Projects/letibot/.claude/worktrees/…`, which does not exist. Because they are locked, `git worktree prune` will not remove them.
- **Merged but still kept:** 20 of the 37 `agent/*` branches checked out in worktrees are merged into main, kept because of A3.
- **Dirty worktrees, i.e. uncommitted subagent work left behind:**
  - `agent-agents-md`: a staged change with an unresolved merge conflict (`UU standing_notes.rs`)
  - `agent-operator-input`: 5 modified TUI files
  - `agent-store-busy`: `.commit-msg` modified
  - `agent-why-a-queued-message-waits`: an untracked `tests/queued_latency.rs`
  - `agent-quiet-shell`: an untracked `.scratch/`
- **What cleans up:** only `retire_branch`, after a landing, and only for a clean tree. Nothing removes worktrees for children that failed, were refused or were never enqueued, and nothing prunes the orphans.

**The `.cargo/config.toml` question: yes, it still breaks things, but not the way expected.**
- **The worktree files are harmless leftovers.** 31 worktrees carry the old `task_start` file (`[build] target-dir = "/home/dead/Projects/letibot/letibot/target"`). The runner no longer writes it (`harness.rs:16752-16758`), and the session's `CARGO_TARGET_DIR` overrides it. It is gitignored, so it does not block a retire. Nothing removes it.
- **The real trap is in the main checkout.** `~/Projects/letibot/letibot/.cargo/config.toml` exists and contains `paths = ["/home/dead/Projects/rano/rano"]`. That is exactly the override `.gitignore` warns against.
- **It reaches every worktree.** Cargo searches parent directories for config, and every worktree under `.claude/worktrees` sits inside the main checkout. So every subagent build, and the operator's own, silently uses the local rano checkout, currently `v0.8.3-1-gde2ae1f`, one unreleased commit past the locked tag.
- **The gate does not see it.** The gate's landing worktree is at `~/.local/share/letibot/landing/`, outside the checkout, so the gate builds the real tag. "Builds in the worktree, fails in the gate" (or the reverse) is possible.
- **It will break the build** when the rano checkout moves to a version the lock does not want, which is the 2026-10-08 incident recorded in `.gitignore`.
- **Fix:** delete that file.

##### Highest-value fixes

1. `FirecodeBackend::processes_arc` (A1).
2. A boot status on the hub from bind until `daemon.run` (A2).
3. Keep `.letibot/*.jsonl` out of worktrees, plus a worktree reaper (A3).
4. Delete the main checkout's `.cargo/config.toml`.
5. Persist the alert dedupe (B1) and say "green again" (B2).
6. Make job ids unique across restarts (A5).

### What the model is told (R93)

I reviewed the text the daemon puts into the model's context at letibot main 50b3e55, read-only. Nothing was edited, built or committed, and no running process was touched. sessions.db was opened read-only only (`?mode=ro`).

The worst problems:
- The `[context check]` taught the "my context N%" tic, and still quotes the model's own sentence (✓ marks included) back to it.
- A memory/box note goes on 93% of bash results.
- 45% of todo nags name a row the model already marked in progress, and 45% get no tool call in reply, only a restated status.
- Several messages contradict each other about whether to wait, whether to call `task_result`, and what the system prompt says the model can do.

Three sub-reviewers covered harness notices, tool results and the system prompt. I re-checked eight of their claims in the source and all eight held: the 14-space gaps, `pkill` needing `pattern`, "You have read-only tools", `task` missing `model`, the literal `{raw}`, the gate.rs gaps, the stale-prompt wording and the reseat wording. The rest of their file:line claims are as they reported, not re-read by me. That includes B14 (they say a foreground command is killed at timeout_ms, not moved to the background).

Paths are under /home/dead/Projects/letibot/letibot.

### Part 1 — Real session data (sessions.db, read-only)

**Method.**
- Injected items are `transcript_item` rows of kind `system`, plus kind `user` where `speaker='agent'` (or null with a daemon prefix). Notes inside tool results come from `tool_result.payload`.
- When a transcript forks (compaction or reseat), its items are copied into the child with new item_ids. I removed copies by matching content in order against the parent transcript: 57,662 of 347,039 rows were copies. Without this, counts double; for example, the 945× below is really 393×.
- Totals after removing copies: 5,321 non-operator user/system items, of which 4,347 are injected.

**Most frequent injected texts (template counts):**

| Count | Text |
|---|---|
| 1,649 | `[todo check] this turn is finished and one item is not done (N more open):` |
| 1,346 | `[job] a job you backgrounded has ended:` |
| 260 | `[task] a subagent you started has finished:` |
| 100 | `[todo check] … one item is not done:` |
| 96 | `[job] N jobs … have ended` |
| 94 | `[intent check] this turn declared N item(s); 0 tool call(s) succeeded…` |
| 88 | `[context check] your reasoning sized the work by your context: "X". Your context is N% full — N of N tokens —` (old wording, with numbers) |
| 76 + 49 | compaction system notes |
| 54 | `Your previous turn was stopped at the context wall…` |
| 27 | re-seat system notes |
| 26 | `You keep overthinking…` (two variants) |
| 19 | `[context check]`, new wording without numbers |
| 11 | stale-prompt `session s-gatekeeper was seated…`, old wording ending "keep saying it until they act" |

**Worst exact repeats in one session:**
- **393×** in s-1791151788256943467: the same text, from 10-11 00:15 to 09:30 UTC, about every 66–68 s (median gap 66.3 s):
  ```
  [todo check] this turn is finished and one item is not done (1 more open):
    - do everything from todo.md — the operator's, and it is marked in progress and still open
  the operator asked for this one, so do it — or mark it done with `todo_write`'s `operator` field, quoting the text above exactly. You cannot remove their row: if you think it should not be done, say why in your reply.
  ```
  - The model's reply was the same each time. Reasoning: "My context is at its absolute end. The loop continues…". Answer: "Not done: three boxes open. 25 closed; suite 7,809/7,809; tree clean; pushed; frozen." That pair appears ≥86 times.
  - This was the pre-R77 metronome. The clock recorded the delivered wording but compared the plan wording, so the backoff never engaged. ebc2af7 (`Harness::nag_said_as`, harness.rs:7904) fixed it on 10-11 at 20:45 +0200.
  - Since R77 the ladder works on unchanged text, with gaps of 261, 490, 974 then about 1,809 s.
- **124×, 87×, 43×, 41×, 40×, 35×, 23×** in s-1789919514688401228: a row the model itself titled "CRITERION, NOT A TASK (stays open until the gate is green…)". Asking about it every window can only produce "still waiting".
- **29×** in s-1789462738453908838: "Your previous turn was stopped at the context wall and the conversation was then compacted…".

**Todo-check states (1,778 live checks):**
- 793 (45%): "it is marked in progress and still open". The model had already acted on the row.
- 518: "you were asked about this one already".
- 227: "you had it in progress".
- 487: the row is a gate criterion, not an action.
- Only 120 offered `wait_on`.
- After a todo-check, 777 of 1,737 (45%) got no tool call at all, only a status restatement. Top replies:
  - 113× "Not done: three boxes open…"
  - 70× "Row N stays open — the criterion, unchanged. Waiting on a head or the grants."
  - 31× empty
  - 19× "Stopping here deliberately…"
  - many "Running — `jNN` …, completion arrives on its own."
- That last group is a nag firing while a job runs, because the row was not attached with `wait_on`.
- Length: median 672 chars, p90 1,292, max 6,631.
- Why consecutive post-R77 nags differ: 74 named a different row, 23 had the same row but other lines differed (job lines, memory lines), and 5 changed only the head count, e.g. "(17 more open)" → "(18 more open)". Each such change resets the backoff ladder to the 60 s base window, because the "(N more open)" count is part of the compared text.

**The context-check tic:**
- In s-1789919514688401228 the first `[context check]` came at 10-11 18:23:56 and quoted numbers: "Your context is 21% full — 214,088 of 999,999 tokens. That is not the wall…".
- The first reasoning saying "my context is N%" came at 18:25, two minutes later. "my context" mentions then ran 38, 42, 160, 85, 76 per hour through 22:00, and 101/hour at 10-12 03:00.
- 108 of the 113 live context checks are in this one session.
- 98 of 113 quote ✓-laden sentences back to the model; 94 of 113 carried % numbers (pre-890d0eb).
- The latest one (10-12 12:28, after numbers were removed) still echoes the tic: `[context check] your reasoning sized the work by your context: "Hmm — **my *context* ✓** ✓ ✓ — **WRITE** ✓ ✓.". The context window is not yours to budget…` That exact quote was sent 14×.
- The ✓ habit itself predates the check (first seen 09-20). But the check feeds it back as a quoted, highlighted sentence.

**Notes inside tool results (live):**
- `[note] memory: peak X of a Y cap · box: Z GiB free of 185.8 GiB; you may ask up to W GiB without asking the operator`: 6,700 total, 3,979 of 4,256 bash results today (93%). Subagents are at about 95% (336/356, 269/286, 265/276, 227/240 of their bash calls).
- `[note] 1 file(s) were already modified and beyond the snapshot budget, so a change to one of them is not reported`: 5,611 total, 840 today. Internal and unactionable.
- `[note] this predicate also matches the process asking … the evaluating shell (the shell that will evaluate this command), the evaluating shell (the shell that will evaluate this command)`: 436. The clause is duplicated in the text.
- `[note] started. If this turn ends with no successful tool call, the intent check will say so…`: 163.
- `[note] this command ran over a minute in the foreground…`: 157.
- `[job]` completion bullets also carry `memory: peak 15.8 GiB of a 48 GiB cap`.

**Completion notices after the model already read the result:**
- 57 of 269 `[task]` notices (21%) arrived after the model had already called `task_result` on that child.
- 54 of 891 `[job]` notices (6%) arrived after `job_output`, `job_wait` or `job_list` had already shown that job. This is an upper bound, since a `job_list` mention may not be a read.

### Part 2 — Findings in the code

Format: file:line | exact text | problem | replacement.

#### A. Context check (crates/harnessd/src/context_claim.rs)

**A1.** :460-466 `notice` and :517-523 `reasoning_notice`
- Text: `[context check] you said "{claim.sentence}". The context window is not yours to manage: the harness compacts when it is reached and the work carries on. Stop sizing the work by it and continue with what you stopped.` (the reasoning variant is similar)
- Problem: it quotes the model's own context sentence back verbatim, tic marks and percentages included. That re-plants the phrase every time, and the data shows the tic followed the check.
- Replace: do not quote. `[context check] Don't size work by context; the harness handles it. Continue the task.`

**A2.** :467-480 `repeated`
- Text includes `(context {}% full — {} of {} tokens)`.
- This is operator-facing (a hub warning). It reaches the model only through the `harness` tool's `warnings` view (see F47). Make sure that view redacts it.

**A3.** :446-454
- The doc comment of `notice` still says "with the numbers — because a number is what the model did not have", right above "**No numbers.**". Stale and self-contradicting (not model-facing).

**A4.** `context_fill` harness.rs:12341
- Problem: it never checks `cfg.auto_compact`. With `--no-auto-compact` or a stuck wall controller, "the harness compacts when it is reached" is false (turn_tail.rs:226-243 says nothing was compacted).
- Fix: return None when auto-compaction is off.

**A5.** Reply variant
- Problem: "continue with what you stopped" fires even on a legitimate stop that merely mentioned context. The `[todo check]` explicitly allows "If you are stopping here deliberately, say why", and the loop continues (harness.rs:11619-11622).
- Replace: `…if context was the only reason you stopped, continue.`

**A6.** Reasoning variant on a clean turn
- Problem: it is appended without continuing (harness.rs:11620), so it is the last item the model sees at the start of an unrelated later turn.
- Fix: drop it when the turn ended cleanly.

**A7.** digest.rs:192-197
- Text: `ASK A QUESTION ABOUT MORE TRANSCRIPT THAN FITS IN YOUR CONTEXT … The rows never enter your context: you pay for the answer, not the conversation.`
- Problem: it invites exactly the context-sizing reasoning the check punishes.
- Replace: `Ask a question over many transcript rows; a subagent reads them and returns the answer.`

#### B. Todo check (crates/tools/src/builtins/todo.rs, crates/harnessd/src/sessions.rs)

**B1.** todo.rs:1816/1820 head, :1824-1827
- Text: `[todo check] this turn is finished and one item is not done (N more open):\n  - {row}{state}\n{advice}{offer}{waiting}`
- Problems:
  - It fires on rows the model already marked InProgress (793/1,778).
  - "this turn is finished" is said inside a fresh idle turn.
  - "(N more open)" changes reset the backoff.
- Fix:
  - Do not nag an InProgress row until it has been idle for the cap interval.
  - Drop "(N more open)" from the compared text (nag_said_as) or from the head.
  - Head: `[todo check] Open: {row}`.

**B2.** :1699-1709 state suffixes
- Text: `— {who}, and it is marked in progress and still open` / `, and you were asked about this one already and it has not been started`
- Problem: this asks for a restatement. The model answers "still in progress" (113× / 70× identical replies).
- Replace: drop the suffixes. Say `(in progress)` only.

**B3.** :1716-1763 advice
- Model row text: `do this one, or mark it done with \`todo_write\`'s \`update\` field, quoting the text above exactly — a row left out of \`todo_write\` STAYS on the board, so a line of work you are dropping is something you say in your reply, not something a call does. If you are stopping here deliberately, say why in your reply.` (about 60 words, on every nag)
- The Operator advice is similar. Queue advice (:1745-1752) is about 70 words that say no action moves the row; it is a nag about something the model cannot act on.
- Replace (Model): `Do it, or mark it done with \`todo_write\` \`update\` (quote it exactly).` This keeps the substring the fold matches.
- Queue rows: do not nag at all until a reviewer child has finished or gone quiet. The same pressure pushes the gatekeeper toward starting reviewers, which gatekeeper.rs:403-410 forbids.

**B4.** :1768-1810 wait_on offer
- Text: `If this row is only waiting on a running job or subagent, attach the wait instead of answering — \`todo_write\` {"wait_on": [{"content": "<the text above>", "kind": "handle"}]} — and this check stays quiet until it ends.`
- Correct field names, but it was offered only 120 times while "Running — jNN… completion arrives on its own" replies were common.
- Better: when a running job's description matches the row (LIKE_FLOOR), suppress the nag rather than offer.

**B5.** :2381-2416 `nothing_can_start`
- Text: `[todo check] this turn is finished and NOTHING can start — every item left waits on another: … unblock one of these rather than starting something new: finish a row they wait on, or correct a need … A plan where nothing can start is a plan to fix, not a plan to work.`
- Problems:
  - It fires when the root wait is a running job or a young child, which todo_write (:2555) promised would be SILENT.
  - Within one notice it says both "unblock one" and "there is nothing to unblock" (:2394-2410).
- Fix: extend `all_on_the_operator` (:1813) to job and young-child roots. Then say nothing, or `[todo check] Rows wait on running work; nothing to do until it ends.`

**B6.** :1355-1360 `attach_now`, appended to every bash background start (bash.rs:792, :901) and task start (task.rs:922, :1209)
- Text: ` If a row on your plan is waiting on this, attach it now — \`todo_write\` {"wait_on": …} — and the end-of-turn check stays quiet about that row until this ends.`
- Problem: it is sent even with an empty plan, and to seats without `todo_write` (m2_coder has `todo`; gatekeeper and runner have neither).
- Fix: emit only when `todo_write` is seated and an open row exists. Shorter: `If a todo row waits on this: todo_write {"wait_on":[{"content":"<row>","job":"{id}"}]}.`

**B7.** :2108 StruckOff
- Text: "re-word it with `todo_write`".
- Problem: re-wording creates a new row; the old one stays.
- Replace: `re-send this row in \`todos\` with that need removed.`

**B8.** :2754-2758 refusal
- Problem: it describes `wait_on` as job/subagent only and omits `operator` and `detach`.

**B9.** :2522 vs :2607
- Problem: "`update` moves ONE" vs "ONE OR MORE". Pick one.

**B10.** :2656
- Problem: the `needs` schema is `items: string`, but the parser and description accept tagged objects, so a schema-constrained model cannot emit them.
- Fix: `anyOf [string, object]`.

**B11.** Abstentions contradict.
- todo.rs:2582-2583: `An abstention unblocks it too … state the assumption you are making`.
- intent/ask.rs:452-453: `an abstention is an ANSWER and not permission to proceed on an assumption`.
- Pick one rule.

**B12.** Blocking contradicts.
- `ask_user_question` blocks ("however long", ask.rs:468), while `todo_write`'s operator wait says "THE REST OF THE PLAN KEEPS GOING" (:2577).
- Nothing says which to use. Add one line to the base prompt.

**B13.** `todo_write` description, todo.rs:2496-2584
- About 1,100 words. Compress to the core (one line per field).

#### C. Completion, delivery and wake notices (harnessd)

**C1.** harness.rs:1078-1080 `[job]` tail
- Text: `This is the completion arriving on its own — you do not need to wait for it, and \`job_wait\` would only block you for a result you already have. Read what it wrote with \`job_output\` (job="…"), then carry on with what you were doing.`
- Problems:
  - It also fires right after the model called `job_wait` (delivery.rs:219 Awaited), and after it already read the output (6%).
  - It plants `job_wait` on every completion.
  - It uses the placeholder "…" where the id is known.
  - "carry on" is said in a turn-end batch.
- Replace: `Output: job_output job="{id}".`

**C2.** harness.rs:1139-1142 `[task]` tail
- Text: `…calling \`task_result\` to block would only hold you for a result you already have. Read what it said with \`task_result\` (task="…")…`
- Problem: it says don't call `task_result`, then call it. 21% arrive after the model already called `task_result`.
- Replace: `Its full answer: task_result task="{handle}".`

**C3.** harness.rs:1064
- Text: `  - \`{label}\` {state} after {elapsed}, wrote {produced} bytes: {command}{whose}`
- Problem: the byte count, plus the model's own command echoed back in full (the monitor sample in the data shows the same command three times in one notice).
- Replace: `  - \`{label}\` {state} after {elapsed}`. Keep the command only for jobs another session started.

**C4.** harness.rs:1073 (from tools/src/exec/host.rs:221-235)
- Text: `    memory: peak X of a Y cap`, on every job.
- Problem: it leaks resource-sizing numbers.
- Replace: print only when killed: `    killed: hit its memory cap`.

**C5.** harness.rs:1084-1087 and 1146-1148 (job started by a session that has since stopped)
- Text: `This session is told because the one that started it cannot be: it has stopped and will not run again… this is the fact of the ending and no more.`
- Replace: `(Started by {owner}, which has stopped; its output is gone.)`

**C6.** harness.rs:1260
- Text: `Waiting on this: "{text}" ({whose}) — update {them} with \`todo_write\` now that {verb}.`
- Problem: it can appear under a `[meanwhile]` head that says "nothing in your plan was waiting on it". Contradictory.
- Fix: recheck the rows before using that head clause.

**C7.** delivery.rs:356-358, :360-364, :366-368 `[meanwhile]` heads
- Texts:
  - `[meanwhile] {n} result arrived while you were working — nothing in your plan was waiting on it, so it was held until your turn ended instead of interrupting you.`
  - `[meanwhile] {n} results held for over {cap} while this turn ran — … so they do not wait for ever. Carry on…` (leaks the 10-min cap)
  - The "busy" variant.
- Problems: they explain the daemon's holding policy, and the Starvation and Batch arms say "them/they" for n=1.
- Replace: `[meanwhile] Finished while you were working:`

**C8.** harness.rs:990-1004 `[monitor]`
- Text: `[monitor] {n} watch(es) fired:\n  - \`{name}\` ({watch}), declared by {turn id}: {word}\n…Only a FIRED watch is an answer about the world; an expiry, a cancellation and an owner ending are three ways a watch stopped existing.`
- Problems:
  - The model's own `retire` (tools/src/exec/monitor.rs:834-845) and turn-scope owner endings are pushed as firings. `wake()` (harness.rs:8574) then spends a turn telling the model about its own retire.
  - The head says "fired" for expiries.
  - It leaks turn ids, and the closing aphorism repeats on every notice.
- Fix: filter self-retire and OwnerEnded. Use `[monitor] \`{name}\` fired: {why}.` / `[monitor] \`{name}\` expired without firing.`

**C9.** harness.rs:8657-8660
- Text: `[todo] {n} row you attached a condition to is due:` (plural bug).
- Replace: `[todo] Due (its condition is met):`

**C10.** harness.rs:13552-13554
- Text: `…The result is not here: read it up yourself if the next step depends on it.`
- Problem: it names no place to read it.
- Replace: `\`{handle}\`: unknown job (lost in a restart, or a wrong handle); its result is unavailable.`

**C11.** harness.rs:14865
- Text: `message from your parent session \`{from}\`: {text}`
- Replace: `[parent] {text}`

**C12.** jobwatch.rs:1358 + harness.rs:366 `ContextWall` Display (the subreviewer did not confirm the exact path)
- A failed child's `why`, e.g. `stopped after 41 round(s) at the context wall: 980,112 of 1,000,000 tokens…`, is likely to reach the parent's `[task]` bullet as token numbers.
- Map to: `failed: ran out of room; resume it with task_message.`

**C13.** deliverable.rs:105-113 `completion_line`
- Problem: it is prepended to a dirty child's answer, so the parent's first-line bullet (jobwatch.rs:1423) shows "has uncommitted changes" instead of the answer.
- Fix: put the salvage line after the answer.

**C14.** deliverable.rs:97-106
- Text: `[the deliverable] Your worktree \`{path}\` has uncommitted changes, so the branch … does not hold them: ?? .letibot/delivery-log.jsonl. The branch is the deliverable — commit this work … check that \`git status --porcelain\` prints nothing, and then finish…`
- In the data it fired on the daemon's own `.letibot/delivery-log.jsonl`, a file the model did not create.
- Fix: ignore `.letibot/` runtime files. Shorter: `[the deliverable] \`{branch}\` is missing uncommitted changes ({list}). Commit them (or say why one stays out), then finish naming the commit.`

#### D. Step and salvage nudges

**D1.** harness.rs:11367-11372 (text also in turn/src/compaction.rs:253)
- Text: `You keep overthinking. Cut it short and do things. Your last turn spent its whole output on reasoning ({reason}) and nothing was recorded. Answer first. Reason after, or not at all.`
- Problems:
  - It leaks the enum `reasoning_only` / `no_visible_content`.
  - "Reason after" is impossible for GLM, DeepSeek and Qwen, whose thinking always comes first.
  - It is identical on every salvage, and "You keep" accuses on the first occurrence.
  - It conflicts with gatekeeper.rs:325-327 "You may reason in prose for as long as you like".
- Replace: `Your reply was all reasoning and nothing was kept. Make the tool call or give the answer now.`

**D2.** harness.rs:11383-11387 (turn/src/compaction.rs:243)
- Text: `You keep overthinking. Cut it short and do things. Your last turn thought until it ran out and said nothing. Stop reasoning and act now.`
- Problem: factually wrong. This arm is end-of-sequence inside reasoning, not running out.
- Replace: `Your reply ended inside reasoning with no output. Make the tool call or give the answer now.`

**D3.** turn/src/length.rs:139-141
- Text: `Tool call {name} was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.`
- Replace: `Tool call {name} was cut off and not run. Send it again complete (split a large write into several edits).`

**D4.** progress.rs:246-268 (injected at harness.rs:11866)
- Text: `The harness is watching what your tool calls return, not how many you make, and the last {n} rounds ran {calls} calls and 0 of {calls} returned a result this turn had not already seen ({tally})… If the next round also produces nothing new the turn stops and the operator is shown this, so say in that round what you are still looking for: an operator who can see what you were after can widen the search, and one who cannot see it can only see that you stopped.` (about 90 words, 9 in the data)
- Replace: `Your last {n} rounds returned nothing new. Answer if you can; otherwise try a different query and say what you're looking for. One more empty round ends the turn.`

#### E. Compaction, reseat, restart, stale prompt

**E1.** harness.rs:9037-9040 `continue_after_wall`
- Text: `Your previous turn was stopped at the context wall and the conversation was then compacted: the summary above stands in for everything said before it. Continue the work you were doing, picking up from the summary rather than repeating what it already records.` (54 live, 29 in one session)
- Problems:
  - It names the "context wall" the context check says to ignore.
  - It duplicates the system compaction note right above it.
  - turn_tail.rs:177 can send it when nothing was compacted.
- Replace: `Continue from the summary above; don't redo what it records as done.` Send only after a real compaction.

**E2.** harness.rs:10520 `cut`
- Text: `" It was CUT OFF at the model's length limit before it finished: what              follows is incomplete, it stops mid-sentence, …              re-read the earlier              transcript if something is missing."`
- Bug: runs of 14 literal spaces, because the line breaks have no `\` continuation. Confirmed in source.
- Replace: ` The summary is incomplete (cut off); treat anything missing as unknown and check with \`transcript\`.`

**E3.** harness.rs:10564-10565
- Text: ` The last {n} item(s) of it follow this note VERBATIM — as they were written, not as a description of them.`
- Replace: ` The most recent messages follow verbatim.`

**E4.** harness.rs:10571-10573
- Text: ` The verbatim part begins in the MIDDLE of an exchange: {dropped} item(s) of it were dropped from the front … Stated rather than left to be discovered.`
- Replace: ` The verbatim part starts mid-exchange; its first message answers something not shown.`

**E5.** harness.rs:10588-10590
- Text: `This conversation was compacted: what was said before this point is replaced by the summary below, which was written over the full history of transcript {old_id} and proposed no tool calls.`
- Problem: it leaks the transcript id and an acceptance-check detail.
- Replace: `Earlier conversation is replaced by this summary (full history: \`transcript\` tool).`

**E6.** sessionlog/src/event.rs:1061-1080 `why_line` (used at harness.rs:10561)
- Text: `No verbatim tail: this conversation's turns go to the LOCAL model, and R27's split gives the tail to a remote one only — a local model is bounded by the KV cache…`
- Problem: rule ids, KV cache, budgets.
- Fix: omit for the model.

**E7.** harness.rs:10581-10584
- Text: `This conversation was re-seated onto a new prompt: message zero now announces the tools this daemon seats, and everything said before this point follows unchanged from transcript {old_id}. Nothing was summarised and nothing was dropped.`
- Problem: it duplicates `[reseat]`. In the data it appears 2–3× per reseat (t10 and t11 notes stacked).
- Fix: drop it, or `System prompt and tools updated; conversation continues unchanged.`

**E8.** harness.rs:2252-2283 `reseated_for_the_model`
- Text: `[reseat] The person re-seated this conversation with \`/reseat summarise\`: the {was_tokens} tokens of history before this point are replaced… Message zero is now the prompt this daemon seats… stop asking for \`/reseat\`. Say in one line that the re-seat landed and what it changes for you, then carry on with what you were doing.`
- Problems:
  - A token count.
  - It asks the model to restate state ("Say in one line…").
  - "stop asking for /reseat" re-plants the request that stale_notice already forbade.
- Replace: `[reseat] Your system prompt and tools were updated{; earlier history summarised}. New: {gained}. Removed: {lost}. Changed: {grew}. Carry on.`

**E9.** harness.rs:2205-2209 `stale_notice_for_the_model` wrapping `stale_prefix_sentence` :2483-2514
- Text: `session {id} was seated {when} with a prompt this daemon would not compose now … message 0 is frozen, and rewriting it is a full cold re-prefill — so the model cannot call a tool seated since… For your information, said once: … Do not ask them to re-seat and do not repeat this…` (over 120 words)
- Problems: operator text addressed to the model in the third person, and it explains `/reseat` and then forbids it.
- The old wording "Say this to the person in your reply, and keep saying it until they act" is in the data 11× (s-gatekeeper), but is fixed in code.
- Replace: `[stale prompt] Your tool list is out of date: {added} exist but you cannot call them; {gone} will refuse; {grew} changed. Updating it is the person's call: don't ask; mention a missing tool only when you need it.`

**E10.** harness/clear.rs:171-178 `[cleared]`
- Leaks the session id, transcript id and "Jobs ({n})" counts; long.
- Replace: `[cleared] The operator cleared the conversation; assume nothing from before. Your board, jobs and subagents are listed below; read the old conversation with \`transcript\` if they refer to it.`

**E11.** subagent_record.rs:393-398
- Text: `[restart] the daemon restarted while you were working (at {utc}, build {sha}); your conversation, worktree and plan are as you left them — carry on from where you were.`
- Problems: it leaks the build and time, and conflicts with the "Nothing above is running now" restore notice sent in the same turn. Data shows it 4× identical in a row.
- Replace: `[restart] You were interrupted and resumed; background jobs you started are gone (see below). Carry on.`

**E12.** restart.rs:266-303
- Text: `[restart] the daemon was down for {down} (stopped at {utc}, estimated from its last heartbeat; back at {utc}) — under the {threshold} restore threshold…`
- Data: "down for 0s … estimated from nothing (no record said when)".
- Replace: `[restart] The daemon restarted. Restored:` / `…nothing was restored. You were running:`

**E13.** subagent_record.rs:403, 409-411
- Problem: nested `[restart]` tags inside a `[restart]` notice.
- Replace: `- subagent {name}: resumed.` / `- subagent {name}: not resumed ({why}); restart with task_start if still needed.`
- Also :316-319 "is in a directory beside the copy" gives no path.

**E14.** restart.rs:439-440
- Text: `…(\`pkill\` with \`action: "list"\` checks, killing nothing).`
- Problem: `pkill` requires `pattern` (pkill.rs:65), so the suggested call is refused.
- Replace: `…(check: pkill pattern="<cmd fragment>" action="list").`

**E15.** restart.rs:480-482
- Text: `…if they still need its result, run it again and \`wait_on\` the new handle.`
- Problem: `wait_on` is a `todo_write` field, not a tool.
- Replace: `…re-run it and set wait_on on that row with todo_write.`

**E16.** restart.rs:353-354
- Text: `Nothing above is running now…`
- Problem: it is skipped for the Restored outcome even when job lines say "NOT run again".
- Fix: append it whenever a job line is present.

#### F. Tool results (crates/tools)

**F1.** bash.rs:140-168, :1077-1081, exec/memory.rs:236-243
- Text: `[note] memory: peak {p} of a {cap} cap · box: {free} free of {total}; you may ask up to {ceil} without asking the operator`
- Problems: it is on 93% of bash results (3,979/4,256 today), and the numbers are resource sizing that changes every call.
- Fix: emit nothing on success. Only when killed or within about 10% of the cap: `[note] this command used nearly all of its memory cap; pass memory: "2G" if it needs more.` Drop `box:` entirely.

**F2.** bash.rs:409-415 description
- Text: `…Each result also says how much the box has free and how much you may ask for. Without a cap, a command that ate the box would be killed … the session, its daemon and the operator's terminal window have all gone that way…`
- Replace: `Every command runs under a memory cap and is killed alone if it reaches it. Set memory for builds/tests (e.g. "4G").`

**F3.** bash.rs:110-121 `memory_arg`
- Text: `…Every result reports the command's peak, so you can size the next ask from the last one.`
- Problem: explicit sizing guidance.
- Replace: `Memory cap: a size ("512M","4G") or "uncapped". Default {d}. Large asks go to the operator.`

**F4.** bash.rs:126-136 `default_sentence`
- Problem: it duplicates F3 and invites editing `leticode.toml`. Merge, and drop the config clause.

**F5.** bash.rs:1040-1056 + exec/memory.rs:186-209 (killed at cap)
- Problems: peak and cap are said twice, the ceiling is disclosed, the model is told to edit `leticode.toml`, and the F1 note is appended too.
- Replace: `[killed: out of memory at its {cap} cap] Run it again with memory: "{step}".`

**F6.** bash.rs:205-209, :214-218, exec/memory.rs:314-326
- Text: `nothing was run. You asked: the model asks for {X} … {free} of {total} is free now. The operator said no…`
- Problems: free/total exposed, and the model is addressed in the third person.
- Replace: `Not run: the operator refused {X} ({why}). Ask for {ceiling} or less, or make the work smaller.`

**F7.** exec/host.rs:221-235 → jobs.rs:378 (`job_list`), :713-717 (`job_wait`), jobwatch.rs:1269
- Peak/cap on every job row. Print only when the job was killed at its cap.

**F8.** mergequeue/gate.rs:252-257 (into notices::health and the red-tip reviewer brief, serve.rs:827)
- Text: `[gate memory: peak {} of a {} cap]`
- Fix: strip it from model-facing evidence, unless killed: `[gate step killed: out of memory — not a code failure]`.

**F9.** gate.rs:77-82
- Text: `…The cap is [memory] gate in providers.toml (unset, it is half of what is free…); raise it…`
- Problem: the model may not edit the operator's providers.toml.
- Replace: `{step} was killed at the gate's memory cap; the tip is UNJUDGED, not red. Raising it is the operator's call — tell them.`

**F10.** gate.rs:218
- Text: `"it died with its peak within {}% of the cap ({}) and no failing code in its                  output — …"`
- Bug: about 18 literal spaces (no `\` continuation). Confirmed in source; percent leak too.
- Replace: `it died near its memory cap with no failing code in its output — out of memory, not a verdict`.

**F11.** mergequeue/notices.rs:114-121
- Text: `…once there is room, gate the tip again.`
- Problem: it asks the model to watch box memory.
- Replace: `…tell the operator; regate once they raise it.`

**F12.** bash.rs:315-330 `wake_sentence`
- Text: `…its result is HELD and arrives … at your next break … It never starts a turn of its own.`
- Problem: false under `Rule::TurnEnd`/`Idle` (delivery.rs:304-309), and contradicted by bash.rs:765, :791-799 ("You are woken when it finishes"), task.rs:1216, :1253, todo.rs:2129, :2406, :2556 ("as a turn of its own"), and jobs.rs:583. Sometimes the contradiction is inside one result.
- Use one sentence everywhere: `Its completion reaches you automatically — when your current turn ends, or at once if a todo row needs it or wake is "now".`

**F13.** bash.rs:777-811 background start result + result.rs:138-148 STILL_RUNNING
- About 250 words: "reaches you on its own" four times, "THIS COMMAND IS STILL RUNNING… Do NOT start it again", "ran in the foreground for: 0ms", then the HELD sentence.
- Replace: `started {label} in the background (pid N). Its completion reaches you automatically; don't poll. Output: job_output job="{id}".`

**F14.** bash.rs:381-385 description
- Text: `A command you did NOT mark background is moved to the background by the runtime once it has run longer than timeout_ms…`
- Problem (subreviewer, not re-read by me): false. At the deadline it is killed (bash.rs:966 "was killed after {}s — it outlived its deadline"). Promotion happens only on the operator's request.
- Replace: `A foreground command is killed at timeout_ms (default 120 s); use background: true for anything longer.`

**F15.** bash.rs:366-371 vs :966-969
- The slow note says "Raising timeout_ms only makes the wait longer…". The kill says "call bash again with a larger timeout_ms, or background: true".
- Kill text: `Killed at its {N}s deadline. Re-run it with background: true.`

**F16.** jobs.rs:650-656 `job_wait` description + predicate.rs:484-490
- The description says it blocks until done. The code (jobs.rs:775-781) returns at once for every watched job ("Waiting here would hold the floor").
- Description: `For your own bash jobs this returns at once; their completion is delivered to you.`

**F17.** jobs.rs:844-848
- Text: `…Wait again with a longer timeout_ms…`
- Contradicts F16. Replace: `{label} still running after {t}; not killed. Output so far: job_output job="{id}".`

**F18.** task.rs:1420-1426 vs :1342
- Text: `call task_result again with a longer timeout_ms…` vs "Waiting here would hold the floor".
- Replace: `{handle} has not answered yet; its answer is delivered when it finishes.`

**F19.** task.rs:1252-1254 vs :820/:1034
- "You do not need this to get its answer" vs "Collect its answer with `task_result`".
- Replace both: `Its answer is delivered to you; task_result shows its state or re-reads the answer.`

**F20.** result.rs:433-436
- Text: `…STILL RUNNING … Wait on them, or read what they have written, before reporting anything as an answer.`
- Problem: "Wait on them" contradicts the don't-wait texts.
- Replace: `{n} call(s) still running ({handles}); results will be delivered. Don't report an answer yet.`

**F21.** task.rs:808-826
- The description lists `model`, and invoke reads it, but `task`'s input_schema has no `model` property. Confirmed: the properties are prompt, role, access, where, verify, wake.
- `task_start` says "exactly as task takes them". Add `model` to the schema.

**F22.** task_start (task.rs:1021-1047)
- Problem: no `wake` property, yet its result promises "comes back as a turn of its own". Add `wake`.

**F23.** task.rs:822-826
- Repo-specific `sh scripts/check-fmt.sh … cargo test --workspace --no-run` sits in a generic tool schema. Move it to AGENTS.md.

**F24.** task.rs:1408-1411, :458
- Text: `…its transcript is on the board under {handle}.`
- Problem: no "board" tool.
- Replace: `Its transcript: transcript session="{handle}".`

**F25.** bash.rs:401-404 and jobs.rs:300, :1026-1027
- Texts: "Do not write pkill, pgrep…", "Use this instead of ps or pgrep", "instead of pkill or kill".
- Problem: `pkill` and `ps` are real seated tools, and ps.rs:37 recommends `pkill`. There is also a typo, "an scope".
- Replace: `Don't use shell pkill/pgrep/sleep loops: job_kill/job_wait for your jobs, the pkill tool for other processes.`

**F26.** exec/predicate.rs:487-492
- Text: `A process this harness did not start is not yours to kill from here`
- Contradicts the `pkill` tool. Replace: `…use the pkill tool (list, then kill by pid).`

**F27.** predicate.rs:433-437
- Text: `…which is the failure TODO.md T21 is about, and it was measured on this box five times…`
- Replace: `Not run: its pattern matches the process running it (or a protected one).`
- Also fix the duplicated clause "the evaluating shell (…), the evaluating shell (…)" seen 436× in data.

**F28.** monitor.rs:61-81
- Text: `exactly one of job … process` / `There is deliberately no way to watch a shell command or a process name`
- Problem: the schema has `command`, `timer_ms` and `process`.
- Fix: list all eight conditions and delete the sentence.

**F29.** monitor.rs:143-150 retire
- Bug: it renders "it had retired by the model after 30s left", and "It never fired" is false for a repeat monitor.
- Replace: `retired {name} (watching {watch}) after {age}.`

**F30.** intent/board.rs:181-187 (`todo`) vs todo.rs:2496 (`todo_write`)
- Two list tools with different models, both registered. intent/ledger.rs:315/317 tells leticode sessions to "call todo with op: \"list\"", and `todo` is not seated there.
- ledger.rs:191 prints "board: 0 items — nothing has been declared" while `todo_write` has rows.
- Fix: seat one, or read the `todo_write` board.

**F31.** merge_gate.rs:166-171, :245-248, harness.rs:8202-8208
- Text: `put the choices to the operator…; then write the section into AGENTS.md and commit it to main`
- Contradicts config.rs:1020-1021 and notes.rs:652 ("AGENTS.md … you read them rather than edit them") and task_start ("NEVER pushed — landed by a merge queue"). It also does not name `ask_user_question`.
- Replace: `Ask the operator with ask_user_question (options = these choices); after they pick, add the section to AGENTS.md (state this is the bootstrap exception to the queue rule).`

**F32.** retrieval.rs:128-133
- Text: `attach a retrieval backend… (T16.6)`
- Problem: no tool for it, and an internal ticket number.
- Fix: don't seat ask_code/ask_corpus, or `No retrieval backend; use grep and read.`

**F33.** exec/confine.rs:836-844
- Text: `…That ENOENT is the boundary, NOT an answer … the mount namespace does not contain it…` (long)
- Replace: `{paths} are outside this session's view (absent, not missing). The operator has been asked to grant access; don't retry another spelling.`

**F34.** result.rs:108-112 + spill.rs:386-391, :410-415
- Problem: the spill "Omitted N bytes… Call read_spill" notice is emitted twice per result. Drop one.

**F35.** provider/src/messages.rs:311
- Text: `[{name}: {reason}]\n{payload}`
- Problem: it re-adds the reason already in the envelope (result.rs:150-163 "reason once").
- Fix: pass the payload through unchanged.

**F36.** result.rs:121-126 NO_RESULT
- Text: `This call produced no result. Nothing above is an answer…`
- Problem: it wraps jobs.rs:578 (redirected output), which tells the model to read a file.
- Fix: return that as ok.

**F37.** bash.rs:908-912
- Text: `[{label} was moved to the background — you did not ask for it] … do not wait on it.`
- Replace: `[the operator moved {label} to the background] It's delivered when it ends.`

**F38.** `[note] 1 file(s) were already modified and beyond the snapshot budget, so a change to one of them is not reported`
- 5,611× in data (the subreviewer did not cite a line). Internal and unactionable: emit once per session, or never.

**F39.** standing_notes.rs:420-421 (system prompt)
- Text: `### {} — {} line(s), indexed: over what is left of the {2000}-token notes budget…`
- Problem: a token number in the prompt.
- Replace: `### {path} — indexed (too long to include); read it with a range.`

**F40.** notes_offer.rs:149-154
- Text: `[standing-notes offer — the harness's own line, not the operator's] {path} looks relevant … (similarity {:.2} of 1.00, lexical over its abstract, title and headings)… If it is worth the context, open it…`
- Problems: score internals and "worth the context" sizing.
- Replace: `[note offer] {path} may be relevant; read it if useful.`

**F41.** notes.rs:1159-1170
- Text: duplicate refusal with "(score {:.2} of 1.00)… IDF-weighted cosine… {DUPLICATE_FLOOR} floor" (about 110 words).
- Replace: `Not written: too similar to {other}. Append to it, or say in the abstract what differs.`

**F42.** notes.rs:374
- Text: `Err("\`{raw}\` is too long for a filename (over 100 characters).".into())`
- Bug: not `format!`, so a literal `{raw}` reaches the model. Confirmed in source.

**F43.** notes.rs:424-426 vs config.rs:1016-1017
- Notes re-read "every turn on a provider session" vs "after every compaction". Unify the wording.

**F44.** harness_view.rs:150-160 (`harness` what=warnings)
- Returns warning details verbatim, including harness.rs:9269-9274 `{r} of {w} {unit} resident… compacting now`, memwatch.rs:238 `the daemon now holds {} resident (heap {})…`, and context_claim::repeated with percent and tokens.
- Fix: redact auto_compact, memory_* and context-claim details for the model.

**F45.** decisions.rs:399-403
- Text: `P(allow) {p:.3}, in {ms} ms`, `oracle answered in {ms} ms, no logprobs…`
- Drop the latency.

**F46.** read.rs:268-273
- Text: `stopped before line {}: one read is capped at {} KiB of text…`
- Replace: `stopped before line {n}; continue with offset={n}.`

**F47.** mergequeue/notices.rs:49-55 `TOOL_WAYS`
- About 90 words restating the `queue` schema, appended to every health notice (:121, :134, :142, :155).
- Replace: `See queue action:"status"; regate once the cause is gone.`

**F48.** notices.rs:146-155
- Text: `EMERGENCY — {at} is RED … a reviewer looks for what … broke it. {TOOL_WAYS}`
- 26 in data, 3 near-identical in a row for leticl. It never says whether the model should act.
- Add: `A reviewer and fix branch are automatic; you need do nothing unless you know the cause.`

**F49.** notices.rs:166-201 `parked`
- Text: `/queue approve {id}`, `/queue veto {id} WHY`
- Problem: operator slash verbs the model cannot run. Name model actions only (`queue action:"restart"`), or mark them "(operator only)".

**F50.** notices.rs:409-423 `handed_over`
- Text: `…land it on {target} yourself if it is green…`
- Contradicts the queue rule. Prefix it `Exception to the queue rule:`.

**F51.** mergequeue/review.rs:387-398 vs verdict.rs:316
- In a red-tip review `accept` means "the red is not in the code"; the schema says it means "the change does what the brief asked". Note this in the schema.

**F52.** mergequeue/desk.rs:37-40
- Leads with `/queue regate /path…`. Replace: `name the repository with repository:"<path>".`

**F53.** notices.rs:229-236
- Text: `(the verdict is bound to the head it judged)`
- Internal. Replace: `…a new head is reviewed again automatically.`

**F54.** sessions/gatekeeper.rs:409
- Text: `Say on your own log what you found and leave the row where it is.`
- Problem: there is no log tool. Replace: `Say what you found in your reply.`

**F55.** gatekeeper.rs:247-251, :255-261
- Text: `…its calls are permission cards, and they come to YOUR head… The operator's remedy is a firecode on the daemon's PATH…`
- Problem: the model cannot answer cards or fix PATH.
- Replace: `Reviewers run on this machine; their bash calls need the operator's approval, so a stalled review may be waiting on that.`

**F56.** args.rs:154-402 `[repaired]` envelope notes
- Examples: "arguments arrived inside a markdown code fence"; in data, 132 `[repaired] \`id\` was given as a number`.
- Problem: they recur every call for a model that always fences its arguments.
- Fix: once per session for `envelope.*`.

**F57.** write.rs:270-273, edit.rs:695-697
- Text: `…a second, independent gate below §11's adjudication…`
- Replace: `The session's backend is read-only; nothing was written.`

**F58.** `[note] started. If this turn ends with no successful tool call, the intent check will say so before the next one begins.`
- 163× on `todo`. A threat about a check that is off by default (`--intent-prose`; cli.rs:757). Drop it.

#### G. Base system prompt vs everything else

**G1.** The real prompt is config.rs:987-1024 `DEFAULT_SYSTEM`, plus standing_notes::section (standing_notes.rs:285), plus `OwnSession::prompt` (sessions.rs:1878; gatekeeper::PROMPT gatekeeper.rs:377-415), plus the fabric block (sessions.rs:1520, flowy/src/context.rs:112-145). prompt.rs is the operator `!send`/sudo cards, not the system prompt.
- It has no rule on waiting, jobs, subagents, todos, context, stopping, asking the operator, the queue or reasoning length.
- So every such rule lives only in tool descriptions and notices, with nothing above them to settle a contradiction.

**G2.** config.rs:990
- Text: `You have read-only tools.`
- Problem: false for leticode, coder, planner and runner. It contradicts config.rs:998-1007 "Change files with `edit` and `write`". Confirmed in source.
- Replace: `Use your tools for questions about **this tree**…`

**G3.** config.rs:995
- Text: `Do not call a tool for a question about the world …; answer those directly.`
- Problem: it forbids `web_search` and `web_fetch` when they are seated.
- Add: `…unless a web tool is seated and the answer depends on current facts.`

**G4.** One prompt for every seat, but it names tools many seats lack:
- `transcript` (:993) is missing in m2_coder, gatekeeper and notes_keeper.
- `harness status` scratch path (:1008) is leticode-only. A coder child is told to use only that path and nothing under /tmp, with no tool to read it.
- `edit`/`write` are absent in gatekeeper, whose own prompt says "You have no write tool" (tools/src/gatekeeper.rs:316).
- `notes` is absent in gatekeeper.
- Fix: compose these sentences from the seated tools, or inject the scratch path.

**G5.** Children inherit the parent's finished prompt (harness.rs:17823-17836 `..self.base.clone()`), including the own-session paragraph and the fabric block.
- That block says `skill load …` and `flowy get …`. The `flowy` tool is withheld from children, and m2_coder has no `skill`.
- A reviewer under the queue session would carry "You are the review queue…" plus "You are the gatekeeper." Inferred from code, not confirmed on a live transcript.
- Fix: compose per seat.

**G6.** flowy/src/context.rs:138, sessions.rs:1534
- Text: `flowy get /api/artifact/<id> --jq .body`
- Problem: the flowy tool has no `get` action (tool.rs:384: status, attention, subscribe, unsubscribe, say, dm, read, replay).
- Replace: say it is the CLI through `bash`, or add the action.

**G7.** context.rs:127
- Text: `skill load <id or title>`
- The real call is `skill {action:"load", name:"…"}` (skill.rs:280-292).

**G8.** flowy tool.rs:380
- Text: `Reasoning goes in a row.`
- Problem: no flowy action writes a row. Drop it, or name the door.

**G9.** gatekeeper.rs:325-327
- Text: `You may reason in prose for as long as you like before and after the call`
- Problem: it conflicts with the D1/D2 overthinking nags.
- Replace: `Explain what you found in your reply; keep it to the evidence.`

**G10.** Dead role tool names: `researcher()` names `search_corpus` and `reviewer()` names `git` (runtime.rs), and neither exists. They seem unused.

### Highest-value changes, in order

1. **Context check (A1):** stop quoting the model's sentence back.
2. **Memory/box note (F1–F7):** drop it from every bash/job result; say it only on a kill.
3. **Todo nag (B1–B5):**
   - don't nag InProgress rows or criterion rows each window
   - keep "(N more open)" out of the compared text
   - suppress when a running job or child is the root
   - shorten the advice to one line
4. **Wake/delivery contradictions (C1, C2, F12–F20):**
   - one sentence about how completions arrive
   - no "don't call X… call X"
   - fill in real handles instead of "…"
5. **G2:** "You have read-only tools" is false and contradicts the next paragraph.
6. **Formatting/schema bugs:**
   - E2: 14-space gaps in the compaction note
   - F10: about 18-space gaps in gate.rs
   - F42: literal `{raw}` in notes.rs
   - F21: `task` schema missing `model`
   - F22: `task_start` missing `wake`
   - F28: monitor description contradicts its schema
   - E14: `pkill` hint missing the required `pattern`
   - E15: `wait_on` named as a tool
7. **Echo requests:** D1/D2 "You keep overthinking", E8 "Say in one line that the re-seat landed", E1 "context wall".

Scratch analysis scripts (read-only, `mode=ro`) are in /tmp/claude-1000/-home-dead-Projects-letibot/7a4b7ff7-173b-4f07-a444-4fda796b95b7/scratchpad/s/. live2.py removes the fork copies; tr2.py, reply.py and acted.py produce the counts above.
