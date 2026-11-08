# Plugin extension points, wide: the harness, the tools, the head, the bridge

The first review (`docs/plugin-extension-points.md`) asked what the room chat needs from the pane
language and answered with X1-X10 and packages H0, P1-P6. This one asks the same question of the
whole system: where is behaviour decided, can a layer change it, what should it be able to change,
and what moves out of the core.

The answer, short:

- Plugins reach the tool boundary (`on_tool_call`, `on_tool_result`, `tool_`), monitor conditions,
  some notice wording, panes, and `/todo`. Almost every DECISION the harness makes between tool
  calls is closed: the nag, turn-end continuation, compaction, the prompt, offers, reviews, briefs,
  spawns, model fallback, the queue's clock. The places operators corrected agents most are exactly
  those.
- Subagents have no plugin host at all. Every hook is a no-op in a child (§1.1). That is the first
  thing to fix.
- Half of what the harness says to a model never passed the `notice_` hook: the idle nag, the queue
  census, every mid-turn injection (§1.3). **Closed by K2**: one door, twenty kinds, and even the
  sentences a running turn reads are offered to it.
- A fixed number belongs in a config key. A decision over facts is a hook. I/O, identity, the
  perimeter and the store stay in the core. Several operator asks are config, not plugins (§4.3).

Sixteen new extension points (X11-X26, §3), a ranked move-out list (§4), what stays (§5), and one
plan merged with the first review's (§6).

Paths are this repository at `9c1fa2d5`, `crates/` implied where a path starts with a crate name.
`rano/` is `~/Projects/rano/rano`. Session ids are from `~/.local/share/letibot/sessions.db`, read
read-only. `seq` is the transcript item.

## 1. Where behaviour is decided, and whether a layer can change it

`hook` = a hook reaches the decision. `partial` = it reaches the wording or one input, not the
decision. `none` = nothing does. Hook call sites: `on_tool_call` `tools/src/runtime.rs:1815`
(`plugin_patch` :1609, wired `harnessd/src/harness/open.rs:2697`), `on_tool_result`
`harnessd/src/harness/turns.rs:2166` (`Harness::plugin_result`, `harnessd/src/harness.rs:1395`),
`notice_<kind>` `harnessd/src/plugins.rs:380` via `turns.rs:3036-3058`, `condition_<name>`
`tools/src/exec/plugin_condition.rs:131`, `pane_<name>` `plugins.rs:792`, `tool_<name>`
`harnessd/src/plugins/tool.rs:269`, `command_todo` `tui/src/app/commands.rs:796`.

### 1.1 Plugin host coverage

| fact | where | effect |
|---|---|---|
| a plugin host is installed per session by `Sessions::open_plugins` | `harnessd/src/sessions.rs:2324`, called at :1165, :2310 | top-level sessions only |
| `harness/children.rs` never mentions plugins | `harnessd/src/harness/children.rs` | **a child has no stack**: no `on_tool_call` guard, no notice rewording, no `tool_` layer, no condition. A reviewer child's `review_verdict` cannot be guarded either |
| `on_tool_call` skips `invoke_operator` | `runtime.rs:1751` | operator calls, `pr_watch` arming and `plugin-arm` are never seen by a layer. Correct for the operator, a gap for `pr_watch` |
| `provider_`, `digest_`, `link_` admitted, never called | `plugins/src/lib.rs:111-121` | reserved names, no caller |
| `command_` is head-only | `tui/src/app/sessions_pane.rs:130` | daemon slash verbs (`harnessd/src/slash.rs:27-43`) cannot be extended |
| `tool_` looks are `read`/`list`/`grep` only | `plugins/tool.rs:446-469` | `exec:`/`http:` only raise the access class (`tool.rs:105-113`). The exec look exists for conditions only (`plugins/conditions.rs:143-165`) |
| `tool_` paths resolve on the host filesystem | `plugins/tool.rs:478-500` | a plugin tool in a confined or remote session reads the daemon's disk, not the session's view. A bug in R121 item 8's family |

### 1.2 The turn loop (`harnessd/src/harness/turns.rs` unless named)

| decision | where | reach | hard-coded policy |
|---|---|---|---|
| round loop and backstop | `rounds` :1688, :1739 | none | `max_tool_rounds` 0 (`config.rs:1615`) |
| one intent nudge per user turn | `rounds` :1699 | none | `nudges_left = 1` |
| stop at the context wall | `stop_the_turn_at_the_wall` :1551, `harness/wall.rs:1991` | none | warning text :1617 |
| salvage, empty or reasoning-only reply, retry | `judge_the_round` :2207 | partial: `salvage` :2219, `nudge` :2226, :2238 | *"Your reply was all reasoning…"* :2228 |
| turn end: continue or stop (context claim, intent diff, deliverable, queued steering) | `rounds` :1932-1983, `close_the_turn` :2913, `children.rs:3931`, `harness/deliverable.rs:122` | partial: the sentence as `nudge` :1982, never the decision | |
| mid-turn steer, context-claim steer | `after_the_round` :2098-2113 | **none**, pushed straight to `injected` | |
| stall nudge, then stop | `rounds` :2031-2043, `progress.rs:210` | **none** | `stall_rounds` 5 (`config.rs:1617`), *"One more empty round ends the turn"* `progress.rs:218` |
| retry ladder and model fallback | `take_the_round` :2325, `worth_moving_for` :2790, `fall_back_from` :2810 | none | `fallback_after_timeouts` 3 (`config.rs:1512`) |
| mid-turn `/mode`, `/models` | `apply_queued_mode` :2565, `apply_queued_model` :2622 | none | |
| which between-turn source becomes a turn | `wake` :308 | partial, per-source notices | |
| mid-turn injections: operator prompt, parent message, answers, room, job and child results, monitor firings | `HubSteering::try_next`, `harness/hub_io.rs` | hook since K2, per kind: the same facts pass `notice_` on both doors | |

### 1.3 What the model is told

Since K2 the answer is *one door*: every sentence below is offered to `notice_<kind>` from
`turns::say` (`harnessd/src/harness/turns.rs`), and this section is now the inventory of the kinds
rather than the list of bypasses. The kinds are `plugin-sdk/src/notice.rs`'s `KINDS` (twenty); the
six droppable ones are `monitor`, `nudge`, `nag`, `steer`, `stall`, `context_claim`, and everything
else may be reworded only. `plugin` has no site (failures go through `note_to_model`, on purpose: a
layer's own failure is not handed to the layer that failed).

| text | site | kind |
|---|---|---|
| the queue's verdicts, handovers, emergencies | `wake` `turns.rs` | `merge_queue` |
| the room's bells | `wake`, and `HubSteering::try_next` mid-turn | `room` |
| what a restart brought back | `wake`, `continue_after_restart` | `restart` |
| a monitor firing | `wake`, and mid-turn | `monitor` |
| a job or subagent that settled | `wake`, and mid-turn | `job` |
| a todo row's condition met | `wake` | `todo` |
| the workspace gone | `wake` | `workspace` |
| a call the gate admitted unasked | `flush_gate_admissions` | `gate` |
| a salvaged round | `judge_the_round` | `salvage` |
| the harness's own "all reasoning" lines | `judge_the_round` | `nudge` |
| the turn's own correction | the turn-end block, and mid-turn | `steer` |
| the stall steer | `rounds`, one round before the bound | `stall` |
| a claim the measurement contradicts | `judge_context_claim`, both doors | `context_claim` |
| a reseat, or a restart row | `say_without_a_turn` | `reseat` |
| the idle plan-check | `nag_turn` `harness/nags.rs` | `nag` |
| the queue census turn | `census_turn` `harness/queue.rs` | `census` |
| the fallback continuation | `fall_back_from` (offered where it is armed) | `fallback` |
| a parent's or a peer's message | `submit_a_parents_message`, `parents_message_turn`, and mid-turn | `parent` |
| the operator's answer to a row | `HubSteering::try_next` | `answer` |

A sentence for a RUNNING turn goes through the door at its PUSH (`queue_mid_turn`), so a dropped
`steer`, `stall` or `context_claim` never reaches the turn at all.

### 1.4 The prompt and the per-round tails

| decision | where | reach | policy |
|---|---|---|---|
| system prompt sections | `Prompts::compose` `harnessd/src/config.rs:1157`, `SECTION_NAMES` :711 | none; `prompts.toml` per model is config (:1019) | merge-queue rules :960 |
| seat composition | `Config::compose_for_seat` :1833 | none | |
| the review queue's own prompt | `sessions/gatekeeper.rs:126` | none | 40 lines of policy |
| standing notes, AGENTS.md | `standing_notes.rs:123`, :133, :189, :442; re-run `harness/wall.rs:724-732` | none | `NOTES_BUDGET_TOKENS` 2000 :18 |
| offers: notes, skills, waits, done | `turns.rs:1748-1797`, :1899-1906 | none | floors 0.18, 0.18, 0.35, 0.35 (`notes_offer.rs:17`, `skills_offer.rs:16`, `waits_offer.rs:15`, `done_offer.rs:14`) |
| mid-session system update | `system_update` :1287 | none | |

### 1.5 Board, compaction, models, queue, monitors, children, room, lifecycle

| area | decision | where | reach | policy |
|---|---|---|---|---|
| nag | when | `TodoNagClock` `sessions.rs:252`, `nag_should_arm` :214, `deliver_due_nags` :3337 | none | `TODO_NAG_AFTER` 60 s :55, `OPERATOR_ADD_NAG_AFTER` 10 s :60, `NAG_REPEAT_CAP` 30 min :93, doubling |
| nag | which rows | `the_check_may_ask_about` :102, `the_plan_as_checked` :200, `the_row_to_name` `todo.rs:1333` | none | one ready row (R127) |
| nag | text | `nag_notice_naming` `nags.rs:137`, :174 | none | |
| board | writes | `todo_write` | partial: `on_tool_call` can refuse or shrink | |
| compaction | when | `headroom_of` `config.rs:1210`, `should_compact` :1393, `pre_emptive_compact` :1400 | none | w/16, min 2048, max w/4 |
| compaction | which fold | `Fold::next` `harnessd/src/wall.rs:75`, `fold_for` :180 | none | `RELEASE_HEADROOMS` 2 |
| compaction | what is kept | `compact_inner` `harness/wall.rs:1088`, `compaction_relevance.rs` :14 | none | `FLOOR` 0.35 |
| compaction | the summary | `turn/src/compaction.rs:46` `SUMMARY_INSTRUCTION`, :78 `SUMMARY_SECTIONS` | none | budgets :101-923 |
| compaction | continue after | `WALL_CONTINUES` `sessions.rs:50` | none | 3 |
| models | switch | `harness/models.rs:641`, :1072, :946, :995 | none | `KEY_PATIENCE` 600 s :1390 |
| models | tie-breaker | `room/coordinator.rs:106` | none | `deepseek/deepseek-flash` literal |
| models | per role | `leticode.toml` `[roles]`, `children.rs:1497` | config | |
| queue | reviewer brief | `review_prompt` `tools/src/gatekeeper.rs:163` | none | |
| queue | verdict | `mergequeue/review.rs:817`, by `review_verdict` | none in practice (children have no stack) | |
| queue | gate steps | AGENTS.md `Merge gate`, `builtins/merge_gate.rs:414`; `mergequeue/gate.rs:866`, :914 | config (AGENTS.md) | `GATE_DEADLINE` 30 min |
| queue | serve clock | `mergequeue/serve.rs:26-47`, `capacity.rs:11-52`, `stuck.rs:12` | none | 1 h review, 3 attempts, 2 reviewing, 4 cores per reviewer |
| queue | wording | `mergequeue/notices.rs:53-627` | partial: `merge_queue` | |
| jobs | held or now | `delivery::decide` `delivery.rs:127` | none | `HELD_RESULTS_CAP` 10 min |
| jobs | wording | `harness/deliveries.rs:290`, :372, :594 | partial, between turns | |
| monitors | what is watched | `condition_<name>` | hook | |
| monitors | PR auto-watch | `pr_watch::opened` `pr_watch.rs:11`, `WATCHED` :7 | none (via `invoke_operator`) | five watches |
| children | spawn, depth, role | `children.rs:2218`, :1896, :3261 | partial: `task` through the parent's `on_tool_call` | depth 3 |
| children | brief | `worktree_brief` `builtins/task.rs:573`, submitted `children.rs:3758-3784` | none | |
| children | resume | `subagent_record::decide` :377 | none | `RESUME_LIMIT` 3 |
| room | wake text | `room/wake.rs:153`, :189 | partial between turns, none mid-turn | |
| context claim | detect | `context_claim.rs:385`, `FLOOR` 0.25 :66 | none | |
| lifecycle | open, stale prompt, restore, idle exit | `sessions.rs:2087`, `open.rs:277`, `turns.rs:658`, `sessions.rs:3293` | none | `RESTORE_WITHIN` 1 h, `idle_exit_after` 1 h |
| slash | daemon verbs | `Slash::parse` `slash.rs:195` | none | closed set |

### 1.6 The tools and the gate

Runtime order (`tools/src/runtime.rs:1761`): unknown name :1768, argument repair :1797,
**`on_tool_call`** :1814, heredoc rewrite :1834, **gate** :1890 (skipped when unattended :1854),
script re-check :1904, invoke :1948, view grant :1975, spill :2029. `on_tool_result` runs after the
spill (`turns.rs:2148-2167`).

The gate is `AdjudicatedGate::admit` (`tools/src/adjudicate.rs:2558`): breaker :2578, unreadable
:2611, `Tier::Blocked` :2633, never-write :2660, `permission` :2702, measured boundary :2765, mode
:2802, approved shape :2845, session grant :2860, unasked stop :2891, model decider :2910,
adjudicator :2933. No hook. Tighten-only seam: every table merges through `Tier::strictest` (:87):
`intent::ALWAYS_ASK` (`tools/src/intent.rs:2332`), `FLAG_RULES` :863 plus `documented_flags.rs:5`,
`EXECUTION_VEHICLES` :358, `SECRET_FILE_NAMES` :1494, `NETWORK_IMPORTS` :3862, `secrets::STORES`
(`secrets.rs:37`), `never_write()` (`adjudicate.rs:1257`), `permission.json` (`permission.rs:184`).

Two facts about `on_tool_call` a reviewer should see:

- `answer` returns before the gate (`runtime.rs:1639-1650`). It runs nothing, so it is not an
  escalation, but it is the model reading words that are not a tool's. The `[note]` line
  (`plugins.rs:2193-2205`) is the only defence.
- `only_smaller` (`plugins.rs:2146`) refuses new argument KEYS, not changed values. A rewritten
  `command` is a different command. The gate judges the rewritten call, so this is safe for
  gated tools and is not safe on the unattended path (:1854), where no gate runs.

Bash: heredoc to `write` (`heredoc.rs:59`, :110), refusals in `builtins/bash.rs:330-494`, spill
`spill.rs:288`. `digest_` has no caller; its seat is beside `Spiller::apply` (`runtime.rs:2029`).

Built-in tools a `tool_` layer could carry: `glob`, `grep` (lossy at 40 matches, 4 rounds),
`outline`, `merge_gate`, the read halves of `skill`, `hosts`, `notes`; with an exec or http look:
`external/github.rs`, the search half of `external/web.rs`, `ps`, `host_config` reads, `retrieval`.
Everything that executes, writes, holds harness state or asks a person stays native (`bash`,
`fanout`, `scenario`, `jobs`, `pkill`, `monitor`, `edit`, `write`, `task`, `todo`, `ask_user_question`,
`queue`, `review_verdict`, `room`, `read`, `plugin`).

### 1.7 The head (`tui/src`)

| part | where | reach |
|---|---|---|
| panes: todos, jobs, monitors, subagents, notes, standing, skills, config, help, term, editor | `ui/panes/*`, `app/*`; `Surface` fixed 19 (`app/surface.rs:11-80`) | none |
| sessions picker, queue | `app/sessions_pane.rs:476`, `app/queue_pane.rs:387` | hook, head-side, on draw |
| daemon panes | `/pane NAME` `app/plugin.rs:127` | hook |
| status rows, turn status, progress, header, hint bar | `ui/status.rs`, `ui/turn_status.rs`, `ui/progress_row.rs`, `ui/header.rs`, `ui/hint_bar.rs` | none; git field template is the `git_format` pref |
| transcript items | `ui/transcript/item.rs:151` | none |
| tool cards | `ui/transcript/call.rs:223`; verb `rano/src/agent/card.rs:66` (read, edit, write, search, list, run, fetch, Other); one override `drawn_as` `call.rs:128` | none |
| notice cards | `session_notice` `ui/transcript/notice_card.rs:78`: 15 readers that PARSE TEXT PREFIXES, then generic | none; a reworded notice can fall out of its card |
| other cards | `job_card`, `red_card`, `review_card`, `restart_card`, `stale_card`, `asked_card` | none |
| permission and ask cards | `ui/cards/decision.rs:91`, `ui/cards/asks.rs` | none, by design |
| diff popup | `app/diff_popup.rs`, `ui/diff_popup.rs` | none |
| slash verbs | `SLASH_COMMANDS` `app/commands.rs:1201-1350`, head match :492-744, rest to the daemon :745 | `/todo` only, `add_todo` only |
| keys | `app/keys/bindings.rs`, `keys/chords.rs`, all `&'static` | none, not configurable |
| themes | `head.toml` `theme`, `color.ROLE` (`tui/src/prefs.rs:49-55`) | config |
| links | `app/ghlinks.rs:97`, `ui/screen.rs:72-91` (`url_of` :86) | none; `link_` has its seat at :86 |

### 1.8 The bridge (`bridge/src`)

23 MCP tools, inline `json!` schemas and a literal match (`tools.rs:756-1165`, `rooms.rs:845-1113`,
count asserted `mcp.rs:269`). Channel pushes: rooms (`rooms.rs:366`) and the closed `Attention`
enum per held head (`head.rs:1330`, `push.rs:19-94`). No `letibot-plugins` dependency. A plugin
cannot add a tool or a push.

## 2. Evidence: where operators and agents hit a closed decision

Operator messages (`speaker = operator`, 3564 distinct texts), assistant workarounds, the monitor
and job tables, `head.toml`, TODO.md. The first review's W1-W10 are not repeated.

G1. The nag is policy and the operator kept re-tuning it by hand.
*"looks like our todo nag is overkill"* (`s-1789639478142928813` seq 2846); *"the nagger should
mention only one todo at a time, so a model will not be defocused"* (seq 3829); *"it should be
nagging on idle timer too"* (seq 2086); *"can we handle postponed todo item properly? i.e. they
persist but without nag"* (`s-1789462738453908838` seq 1402); *"todo list chose an item for a nag
and keeps nagging till it is in progress"* (seq 835); *"all todo items complete, yet it gets todo
nags for my item"* (`s-gatekeeper` seq 393). The same correction to the model, five times in one
session: *"dont be upset about repeated todo nags ... use wait_on"* (`s-1789919514688401228` seq
311, 759, 801, 840, 1079). R127 asks for the opposite of seq 3829: every free head. Two operators'
wishes, one constant. And the fleet runs its own nag outside letibot (`board-nag.sh` as a Monitor,
the operator's global instructions) because a Claude Code seat has none (R144).

G2. Compaction loses what the operator wants carried.
*"when conversation compacts new turn doesnt start automatically, i have to send continue"*
(`s-1789462738453908838` seq 161); *"this last confusing compaction summary didnt include notes at
all it also didnt include agents.md"* (seq 303); *"you keep forgetting about tmux after compaction"*
(seq 863); *"what i'd want is that we just keep compacting the first half"* (seq 150);
*"compaction leads to too much noise in the conversation for me ... i want a one line"* (seq 857).
The workaround: *"add a todo item that it must pass ... this will keep you reminded across
compactions"* (`s-1789919514688401228` seq 446). A todo row used as a compaction pin.

G3. Agents drive operator verbs by typing into the head through tmux.
66 assistant items in 3 sessions run `tmux send-keys` with `/queue reset`, `/reseat`, `/compact`
(`s-1789462738453908838` seq 868, 871, 976). It is lossy: *"`/reseat` sent as `send-keys -l`
arrived as `/eset`"* (seq 982). The agent's own reading: *"whenever the answer needs a person's
verb ... I can type it myself"* (seq 874). The operator asked for it: *"there was a 'prompt was
stale' reseat offer, you should catch that and reseat yourself"* (seq 891).

G4. The prompt per model, and its staleness.
*"we need a way to compund/override base prompts for models - for small utility models like this
qwen using web_search should be encouraged"* (`s-1789462738453908838` seq 280); *"i wonder if we
can detect new letibot comes with new base prompt"* (seq 633); *"model should be aware what exactly
changed, so it can beg me for a reseat"* (seq 1160). `prompts.toml` answers the first as config.
The second and third are lifecycle facts.

G5. Warnings the operator dismisses one at a time.
`head.toml` `retired` holds 127 `model_slow_first_byte`, 64 `merge_queue_recovered`, 63
`merge_queue_store_error`, 33 `queue_branch_kept`, 22 `provider_stall`, 22 `auto_compact`
dismissals. No rule says *never show me this kind*. Config, not a plugin (§4.3).

G6. Rendering is policy the operator wants to compose.
*"the verbositiy and visibilty toggles need a rewrite ... a list of things that can be shown and
then verbosity profiles composed"* (`s-1789462738453908838` seq 1486); *"how i can have verbosity
'conversation' + edits?"* (seq 784); *"do new read verbosity mode where edit cards are shown - call
it read-edits"* (`s-1791017230755743833` seq 837); *"how about we do the colours too and the
configurable format too"* (seq 617); the subagents pane's prompt and response lines (seq 1034, 1042,
1085).

G7. Review policy lives in prose.
*"gatekeeper reviews and drives subagents to completion by nagging them via messages"*
(`s-1789462738453908838` seq 535); *"actually refuse ... the ball is back to the original subagent
with corrective instruction"* (seq 520). Both landed as text in `sessions/gatekeeper.rs:126` and
`mergequeue/notices.rs`, where only a release changes them.

G8. A child is a session, the operator says, and the harness disagrees.
*"subagent is a normal session. it must be able to compact on tool call overrun"* (seq 1541);
*"well childs being just a session should get nags"* (seq 1815). A child gets nags today and no
plugin host (§1.1), so a project's guard layer does not guard the work its children do.

G9. Monitors are shell scripts.
Every `session_monitor` row is a built-in kind: `command` (`etcd/fuzzing/wait-criteria.sh`, four of
them), `path`, `timer`, `pid`. No session wrote a `condition_` layer. The plan's conditions are
built; nobody reaches them (W5 of the first review: agents do not know).

G10. A second dashboard language exists.
leticl reads `~/.config/letibot/dashboards/*.json` and `watchers/*.json`
(`~/Projects/leticl/src/dashwatch/watchers.lisp`), data files with a shadowing rule by name. The
pane language is wasm. Two contracts for one idea.

G11. Per-project model choice was asked and landed as config.
*"a config file where i set up things like main model, subagent models, gatekeeper model"* (seq
233) is `leticode.toml` (`leticode-config/src/lib.rs:445`). The right answer: a list of names is not
a plugin.

G12. The boundary is not a plugin problem.
*"plain `git push` fails ... because the gate scrubs the environment"* (seq 406, push over HTTPS
with gh's token). That is R121's perimeter, config and core.

## 3. Proposed extension points, X11-X26

Each: contract (in, out), capability, never, safety, what it replaces. Common rules for all, unless
said: pure function, `v` in every input, `now_ms` and `utc_offset_s` (X9) in every input, the
layer's `state` kept by the host, per call `HOOK_FUEL` 50M and 64 MiB, a trap or bad answer falls to
the layer below and then to the native code, said once as a `plugin` notice. A decision hook that
writes into the cached prefix takes effect at reseat only, like `tool_`.

### X11. Children get a stack

Not a hook: a fix. `spawn_child` (`children.rs:1896`) installs a plugin host for the child: built-in,
user, project, and the parent's pinned session layers. Unpinned parent layers are not inherited,
because a child's work goes to review and an unpinned layer is the parent's experiment.
Safety: unchanged ladder. Replaces nothing; closes G8 and makes X18's reviewer guard possible.

### X12. One notice door

Every model-facing sentence the harness composes goes through `Harness::say(kind, text, facts)`,
mid-turn included (`hub_io.rs:162-302`). New kinds: `nag`, `census`, `restart`, `fallback`,
`parent`, `steer`, `stall`, `context_claim`, `answer`. Must-see adds `census`, `restart`,
`fallback`, `parent`, `answer`. Droppable: `stall`, `steer`, `context_claim` beside `monitor`,
`nudge`. Facts are typed per kind in `plugin-sdk/src/notice.rs`, not `Value`, so a head card (X22)
can render from them instead of parsing text. Replaces the nine bypass sites of §1.3.

### X13. `nag_` - which rows, what words, when next

```jsonc
// in
{"v":1, "board":[{"id","content","status","by","waits","under_review"}…],
 "eligible":["r3","r7"],                 // the_check_may_ask_about's answer, the host's
 "clock":{"since_last_ms":…, "repeats":2, "idle_ms":…, "operator_added_ms":…},
 "jobs":[…], "children":[…], "now_ms":…}
// out
{"name":["r3"], "text":"…", "next_ms":240000}   // or {"quiet":true, "next_ms":…}
```

Never: name a row outside `eligible`; stay quiet past `NAG_REPEAT_CAP` while an eligible row is
open; touch a queue row's nag (`s-gatekeeper`'s board is the queue's). `next_ms` is clamped to
[`TODO_NAG_AFTER`, `NAG_REPEAT_CAP`]. No capability: the board is the input. Built-in answerer:
`plugins/builtin/todo` gains `nag_todo` with today's text, so one row (G1, seq 3829) and every head
(R127) are two layers, not two releases. Replaces `the_row_to_name` (`todo.rs:1333`),
`unfinished_plan_for`'s wording (`todo.rs:1171-1272`), `nag_notice_naming` (`nags.rs:137`). The
clock and the eligibility filter stay native.

### X14. `steer_` - continue or stop at the turn's end

```jsonc
// in
{"v":1, "reply_tail":"…", "open_rows":[…], "deliverable":{…}, "intent_diff":[…],
 "claim":{"score":0.41,"anchor":"context"}, "stall_rounds":3, "budget":{"continues_left":1}}
// out
{"continue":"…sentence…"} | {"stop":true}
```

Never: continue past `budget.continues_left` (the host's `nudges_left`, `WALL_CONTINUES`); start a
turn when the operator interrupted. The detectors (context claim score, progress, deliverable) stay
host and are inputs. Fail: the native decision. Replaces the decision half of `rounds`
:1932-1983 and the wording of `progress.rs:218`, `context_claim.rs:423`. Also answers G2's first
item: *continue after compaction* is a `steer_` rule a project can turn on.

### X15. `compact_` - what a compaction carries

```jsonc
// in
{"v":1, "sections":["goal","state","decisions",…],    // SUMMARY_SECTIONS
 "candidates":[{"id","kind":"agents_md"|"note"|"row"|"item","tokens","score"}…],
 "budget":{"summary_tokens":…, "pinned_tokens":…}}
// out
{"sections":[…], "extra_instruction":"…", "pin":["agents_md","row:r4"], "after":"continue"|"wait"}
```

Never: exceed the host's budgets (pins past `pinned_tokens` are cut in order and the cut is said);
choose when to compact or which fold (`Fold::next` stays). Replaces nothing native outright;
`SUMMARY_INSTRUCTION` (`turn/src/compaction.rs:46`) becomes the built-in layer's answer. G2's
AGENTS.md, notes, tmux reminder, *one line* in the head: all pins or the head card.

### X16. `prompt_` - sections of the system prompt

In: seat, model, dialect, `SECTION_NAMES` with their built-in text, the project's facts. Out:
`{"append":[{"after":"working","text":"…"}], "replace":{"tone":"…"}}`. Never: replace a section the
host marks fixed (`view`, `scratch`, the boundary and gate disclosures, the queue rules :960).
Applied at open and reseat only; the prefix fingerprint changes and `prefix_stale` says why (G4).
Scope: built-in, user, project. Not session: a session layer editing its own prefix is a reseat per
rebuild. Replaces `prompts.toml` for computed policy only; `prompts.toml` stays for text.

### X17. `offer_` - the per-round tails

In: the round's candidates per offer kind with the host's scores (notes, skills, waits, done).
Out: chosen ids and wording. Never: read anything; raise a candidate the host did not score.
Tails are not stored, so a bad layer costs one round. Replaces the four `OFFER_FLOOR`s and `tail`
wording (`notes_offer.rs:44`, :182).

### X18. `review_` - the reviewer's brief and the verdict floor

```jsonc
// in (gatekeeper session, per entry)
{"v":1, "entry":{"id","branch","ask","diffstat","gate":{"result","steps"}}, "brief":"…native…"}
// out
{"brief_add":"…", "require":["tests touched for crates/tools", …]}
// review_verdict_<name> - on the reviewer child's verdict
{"verdict":"accept", "reasons":[…]}  →  {"floor":"needs_human", "why":"…"}   // or pass
```

Never: raise a verdict (`reject` to `accept`); land; skip the gate. The verdict floor is
tighten-only, merged like `Tier::strictest`. Answered from the gatekeeper's stack, which needs X11.
Scope: built-in and project only, because the reviewed session's own layers must not reach its
reviewer. Replaces the composition in `review_prompt` (`tools/src/gatekeeper.rs:163`), the policy
paragraphs of `sessions/gatekeeper.rs:126` (G7). The landing, leases, serve clock stay native.

### X19. `brief_` - what a child is told and seated with

In: the `task` call's args, the parent's facts, the default seat and model. Out:
`{"brief_add":"…", "seat_drop":["bash"], "model":"…"}`. Never: widen a seat, pick a model above the
parent's (`docs/subagents.md` §1), change placement. Replaces `worktree_brief`
(`builtins/task.rs:573`).

### X20. `adjudicate_` - a tighten-only floor at the gate

In: the normalised action (Layer A's intents, tier, program, paths, hosts), the mode, the project.
Out: `{"intents":["prod-host"], "floor":"ask"}`, merged through `Tier::strictest`
(`adjudicate.rs:87`) after `baseline_for` (:1830). Never: admit, lower a tier, remove an
`ALWAYS_ASK` or never-write entry, answer the call. In `FAIL_CLOSED` (`plugins/src/stack.rs:120`)
like `on_tool_call`. Pinned, project or built-in only. A project's production hostnames and its
own secret stores become a reviewed file, not a release. Replaces nothing; the tables stay.

### X21. `lifecycle_` - open, reseat, restart, close

In: `{"event":"open"|"reseat"|"restart"|"close", "stale":{"prompt":…, "tools":[…]}, "rows":[…]}`.
Out: acts only, from the manifest's `actions`: `add_todo`, `arm` (a monitor, through the operator
door as `on_tool_result` does), `say` (a must-see sentence), `room_say` as the session. Never: block
the event. Replaces `WAKE_TAIL` and `RESTART_PROMISES` (`restart.rs:548`, :555) wording; answers
G4's *what exactly changed* as a layer over the `stale` facts.

### X22. Head cards from facts: `card_`

In: a transcript item's kind and typed facts (X12), or a tool call's name, args and result summary.
Out: a pane-language block set plus `{"verb":"read"|"edit"|…}` for rano's `Verb::of`. Never: paint
cells (the host draws, as in the first review §3). Replaces the 15 text-prefix readers of
`notice_card.rs:78` with one built-in `cards` layer reading facts, so a reworded notice keeps its
card. Head-side, asked once per item and cached by item id.

### X23. `command_` for every unknown verb, head and daemon

The head's `other =>` arm (`commands.rs:745`) offers the line to `command_<verb>` before sending it
to the daemon; the daemon's `Slash::parse` (`slash.rs:195`) does the same with its own stack. Acts
grow by the first review's X7 list plus `monitor_arm`, `job_kill`, `queue_op{verb, id}` (operator
identity, the head only). Never: shadow a host verb (`HOST_VERBS` refused at admit, as `HOST_KEYS`).

### X24. Self-scoped verbs for the model (native, listed so a plugin does not take it)

G3 is not a plugin gap. The model needs `reseat_self`, `compact_self`, `nag_pause_self` as tool
verbs on its own session, natively, through the gate. Operator verbs (`/queue approve|veto|reset`)
stay the operator's. A plugin cannot grant a model an operator verb; a manifest asking for one is
refused at admit.

### X25. Bridge: plugin tools and push policy

- The bridge serves `tool_` layers of the user and project stacks as MCP tools, under the same
  looks and capabilities (`tools.rs:756`). A Claude Code seat gets `claude_transcript`-shaped tools
  without a bridge release.
- `push_` hook over `Attention` and room messages: in `{event, session, room, to, text}`, out
  `{"push":true|false, "text":"…"}`. R117's open choice (*every room message or only addressed*) is
  a layer. Never: suppress an `Ask` or a `Failed`.
- R144's board for Claude Code seats reuses the `todo` built-in and X13, so the fleet's
  `board-nag.sh` has a successor in-tree.

### X26. Callers for the reserved prefixes

- `digest_` beside `Spiller::apply` (`runtime.rs:2029`), as `docs/plugins-plan.md` §4.5 says.
- `link_` in `url_of` (`ui/screen.rs:86`), batched per frame, as §4.6 says.
- `tool_` gets the `exec` look conditions have (`plugins/conditions.rs:143-165`), pinned layers
  only, and its `read` looks go through the session's backend view (`tool.rs:478`).
- `provider_` stays reserved. Model routing is the core's (§5).

## 4. What moves out of the core

### 4.1 Move to a built-in plugin

Ranked by value over cost and risk. The native code stays as the fallback a broken layer falls to
until the layer has run without traps, then the duplicate goes, as ruled for the chat (first review
§7).

| rank | what | to | needs | value | cost | risk |
|---|---|---|---|---|---|---|
| 1 | nag selection and wording | `plugins/builtin/todo` `nag_todo` | X12, X13 | high: G1, R127 | M | M: a silent nag stalls a plan; the cap holds |
| 2 | notice cards in the head | `plugins/builtin/cards` | X12 facts, X22 | high: fixes cards lost on reword | M | L |
| 3 | GitHub family: `pr_watch`, `GithubCondition`'s decision half | `plugins/builtin/github` | `condition_` exec look exists | M: plan step 3 | S-M | L |
| 4 | turn-end steering wording and rules | `plugins/builtin/steer` | X14 | M-high: G2, stalls | M | M: continuation loops; budget holds |
| 5 | compaction template and pins | `plugins/builtin/compact` | X15 | high: G2 | M | H: a bad summary costs a session; replay through the shadow gate first |
| 6 | reviewer brief and queue prose | `plugins/builtin/review` | X11, X18 | M: G7 | M | M |
| 7 | per-round offers | `plugins/builtin/offers` | X17 | M | S-M | L |
| 8 | jobs, todos, monitors, notes, subagents panes | first review §4 | X1, X2, feeds | per first review | | |
| 9 | room push policy in the bridge | `plugins/builtin/push` | X25 | M: R117 | S | L |
| 10 | read-only tools: `glob`, `outline`, `merge_gate`, `hosts` listing | `tool_` layers | X26 | L: no ask for it | M | M: loses the backend view until X26 |

Rank 10 is listed to say no: those tools work, nobody asked, and a `tool_` layer is lossier.

### 4.2 Keep native, expose a hook

| native | hook | why native |
|---|---|---|
| the round loop, retries, fallback | X14 at turn end only | it owns the model call and the wire |
| the nag clock and eligibility | X13 | timers and the queue's rows are correctness |
| compaction trigger, fold, budgets | X15 | the wall is arithmetic over the ledger |
| the gate's ladder and tables | X20, tighten-only | the security boundary |
| spawn, depth, placement | X19 | perimeter and VMs |
| the queue's serve clock, gate, landing, leases | X18 | landing is the plan's *never wrong* list |
| detectors: context claim, progress, deliverable | X14 inputs | embeddings and calibration are host work |
| delivery held-or-now | X12 wording | timing is correctness |
| the bridge's 23 tools | X25 beside them | they are the wire to Claude Code |

### 4.3 Config, not a plugin

A fixed number or a list of names is a key in `leticode.toml` or `head.toml`. A plugin for them
costs a build and a pin to change a number.

- `[warnings] mute = ["model_slow_first_byte"]` and `coalesce` in `head.toml` (G5).
- `[keys]` in `head.toml`: rebind within a surface, never the global chords' escape (W8 of the
  first review).
- `[queue]` keys for `serve.rs:26-47`, `capacity.rs:11-52`, `stuck.rs:12`.
- `[nag]` `after`, `repeat_cap` (X13 clamps to them).
- `[verbosity.<profile>]` as a list of shown things (G6), the profiles the operator composes.
- `prompts.toml` per model (G4, exists). `leticode.toml` `[roles]` (G11, exists). `[boundary]` (G12,
  R121).
- The tie-breaker model literal (`room/coordinator.rs:106`) as `[room] tie_breaker`.

## 5. What stays in the core

| thing | why |
|---|---|
| the perimeter: bwrap, seatbelt, cgroups, the flow rule, `NEVER_WRITE`, `ALWAYS_ASK` | `docs/boundary-and-adjudication.md` §3, §4l: nothing overrides them, not even `allow-all` |
| the allow side of the gate, grants, the breaker, cards | a layer cannot put a question to a person and cannot widen |
| the trust book, the plugin host, the `plugin` tool, the shadow gate | the thing that decides what runs cannot be what runs |
| the store, the append-only transcript, the writer claim | one writer, triggers, the ledger's hashes |
| the wire: sessionlog protocol, room protocol, MCP framing | two processes and other heads read it |
| dialects, tokencore, the prefix and its fingerprint | per-token hot path, cache correctness |
| the queue's landing, leases, the gate run | the plan's *never wrong* list |
| sudo, VMs, `vm_reap`, `memwatch`, supervise | privilege and process control |
| operator identity: the room's operator seat, `bangrun`, operator verbs | a layer speaks as nobody but its session |
| the way out: the native session list, permission cards, help | must work the day a layer breaks |

## 6. One plan

The first review's order stands: P0, then H0, then P1-P4 in parallel, then P5, then P6. R151 (the
four walls) runs now, independent. The packages below slot around them. Every package is gated by
`cargo test -p letibot-room -p letibot-harnessd -p letibot-sessionlog -p letibot-tui -p letibot-tools
-p letibot-bridge -p letibot-plugin -p letibot-plugins` and clippy, `CARGO_TARGET_DIR` under
`/build`.

```
R151 ─┐
K1 ───┼─ K2 ─ K3 ─ K4
      │     └─ K6 (cards, needs K2's facts) ─ after P2
      └─ K5 (review/brief, needs K1)
K7, K8, K9 independent
P0 ─ H0 ─ P1..P4 ─ P5 ─ P6        (first review, unchanged)
K10 after H0 (uses its acts table)
```

### K1 - children get a stack (S, now)

Owns `harnessd/src/harness/children.rs` (spawn installs the host), `harnessd/src/sessions.rs`
(`open_plugins` takes an inherited set), `harnessd/src/plugins.rs` (`PluginHost::for_child`).
Done when (`harnessd/src/plugins/tests.rs`):
`a_child_answers_the_parents_project_on_tool_call_layer`,
`a_child_inherits_a_pinned_session_layer_and_not_an_unpinned_one`,
`a_reviewer_child_runs_the_gatekeepers_layers`.

### K2 - one notice door (M, after K1) - **landed 2026-11**

Owns `harnessd/src/harness/turns.rs` (`say`), `harnessd/src/harness/hub_io.rs`,
`harnessd/src/harness/nags.rs` (call `say` only), `harnessd/src/harness/queue.rs`,
`harnessd/src/restart.rs` (call sites only), `plugin-sdk/src/notice.rs` (kinds, must-see, typed
facts). Done when:
`every_model_facing_sentence_passes_the_notice_hook` (a recording layer, one fixture per §1.3 row),
`a_mid_turn_room_wake_is_reworded`, `a_census_cannot_be_dropped`,
`a_stall_steer_can_be_dropped`, `facts_round_trip_per_kind`.

What landed:

- **One door.** `turns::say(host, hub, facts, text)` is the only place a model-facing sentence is
  offered to `notice_<kind>`; `Harness::say`, `say_kept` (a kind the model is owed), `say_all` (a
  batch from one source) and `say_mid_turn` are its callers inside the harness, and
  `HubSteering::said` is the mid-turn one - a `HubSteering` carries the session's host, so a bell, a
  parent's message, the operator's answer, a settlement and a firing are offered to the stack the
  between-turns door asks. A session with no layer gets its sentence back untouched, byte for byte.
- **Nine kinds** added (`nag`, `census`, `restart`, `fallback`, `parent`, `steer`, `stall`,
  `context_claim`, `answer`): `KINDS` is twenty, `MUST_SEE` is the fourteen a session is owed, and
  the six droppable ones are `monitor`, `nudge`, `nag`, `steer`, `stall`, `context_claim`. A `nag`
  is droppable because it is the harness's own reminder and K3 hands the decision to a layer.
- **Typed facts.** `notice::Facts` has one variant per kind, with `kind()`, `to_value()`,
  `from_value()` and `one_of_each()`; the wire is `{"kind": …, …}` and `Notice::of(text, &facts)`
  takes the kind off the facts, so a site cannot say one kind and hand over another's. A site that
  has numbers to hand fills them (the census's counts, the claim's fill and window, the two models
  a fallback names, the row and question an answer settles); one that has none says so.
- **Where the door sits.** `wake`'s five sources, the firing row, the due-row line, the held batch
  and the workspace line; the gate admissions; the salvage and both nudges; `nag_turn`,
  `census_turn`, `continue_after_restart`, `say_without_a_turn`, both parent doors; and the fallback
  continuation, offered where it is ARMED (`fall_back_from`) because that is where the two models it
  names are in hand. A sentence for a running turn goes through the door at its **push**
  (`queue_mid_turn`), so a dropped `steer`, `stall` or `context_claim` never reaches the turn.
- **`note_to_model` stays outside**, on purpose: a layer's own failure must not be handed to the
  layer that failed (§4.3).
- **The page follows the code**: `plugin_hooks`' notice page now names the droppable kinds from
  `notice::droppable` instead of a hand-written list that went stale the moment a kind was added.
- Not in this package: K3's `nag_` decision, K4's `steer_`/`compact_` (the turn-end block still
  decides continue-or-stop itself), and no head card yet reads the facts (K6). One model-facing
  sentence still bypasses the door on purpose-of-scope: the turn a limit park resumes
  (`resume_after_limit`, `harness/limit.rs`) opens with `[limit] the limit on … lifted`, and X12's
  kind list has no kind for it - it is §1.5's park, and giving it a kind is a decision for the
  package that owns the park's clock rather than one to invent here.

### K3 - `nag_` and the todo layer (M, after K2)

Owns `harnessd/src/sessions.rs` (clamp `next_ms`, nag clock only), `tools/src/builtins/todo.rs`
(`the_row_to_name` and wording become the fallback), `plugin-sdk/src/nag.rs` (new),
`plugins/src/lib.rs` (`nag_` prefix), `plugins/builtin/todo/**`. Done when:
`a_nag_layer_cannot_name_a_row_outside_eligible`, `quiet_is_overridden_at_the_repeat_cap`,
`next_ms_is_clamped_to_the_config_bounds`, `the_builtin_nag_says_what_the_native_nag_said` (on the
store's recorded nags), `a_layer_naming_every_free_head_is_r127` (R127 closes as a layer),
`a_trapping_nag_layer_falls_to_the_native_nag`.

### K4 - `steer_` and `compact_` (M, after K2)

Owns `harnessd/src/harness/turns.rs` turn-end block only (:1932-1983, after K2 lands),
`harnessd/src/harness/wall.rs` (`compact_inner` asks the hook), `turn/src/compaction.rs`
(template as data), `plugin-sdk/src/steer.rs`, `plugin-sdk/src/compact.rs`,
`plugins/builtin/steer/**`, `plugins/builtin/compact/**`. Done when:
`a_continue_past_the_budget_is_refused`, `an_interrupted_turn_is_never_continued`,
`continue_after_compaction_is_a_steer_rule`, `a_pin_past_the_budget_is_cut_and_said`,
`agents_md_is_pinned_by_the_builtin_compact_layer`,
`the_builtin_compact_layer_asks_what_summary_instruction_asked`.

### K5 - `review_` and `brief_` (M, after K1)

Owns `tools/src/gatekeeper.rs` (`review_prompt` composes from the answer),
`harnessd/src/sessions/gatekeeper.rs` (prose moves to the layer), `harnessd/src/mergequeue/review.rs`
(verdict floor), `tools/src/builtins/task.rs` (`worktree_brief`), `plugin-sdk/src/review.rs`,
`plugins/builtin/review/**`. Done when:
`a_verdict_floor_cannot_raise_reject_to_accept`, `a_session_layer_of_the_reviewed_branch_does_not_reach_its_reviewer`,
`a_brief_layer_cannot_add_a_tool_to_the_seat`, `the_builtin_review_layer_briefs_as_the_native_prompt_did`.

### K6 - cards from facts (M, after K2 and P2)

Owns `tui/src/ui/transcript/notice_card.rs` (readers become the fallback),
`tui/src/ui/transcript/call.rs` (`drawn_as` from the answer), `tui/src/app/cards_plugin.rs` (new),
`plugins/builtin/cards/**`. Done when:
`a_reworded_notice_keeps_its_card`, `a_card_is_asked_once_per_item`,
`the_builtin_cards_layer_draws_what_the_readers_drew` (the readers' existing fixtures).

### K7 - `adjudicate_` floor (S, independent)

Owns `tools/src/adjudicate.rs` (one call after `baseline_for`), `plugin-sdk/src/adjudicate.rs`,
`plugins/src/stack.rs` (`FAIL_CLOSED`), `plugins/src/manifest.rs` (session scope refused). Done when:
`a_floor_layer_cannot_lower_a_tier`, `a_floor_layer_cannot_admit`,
`a_trapping_floor_layer_refuses_the_call`, `a_session_scope_adjudicate_layer_is_refused_at_admit`,
`the_unattended_path_still_runs_the_floor` (the :1854 path).

### K8 - reserved prefixes get callers (S, independent)

Owns `tools/src/runtime.rs` (`digest_` before spill), `tui/src/ui/screen.rs` (`link_` in
`url_of`), `harnessd/src/plugins/tool.rs` (exec look, backend-view reads). Done when:
`a_digest_layer_shortens_before_the_spill_budgets`, `link_words_are_batched_per_frame`,
`a_pinned_tool_layer_runs_an_exec_look_through_admit_unasked`,
`a_tool_read_look_in_a_confined_session_reads_the_view_not_the_host`.

### K9 - config keys (S, independent) - **landed 2026-11**

Owns `leticode-config/src/lib.rs` (`[queue]`, `[nag]`, `[room] tie_breaker`), `tui/src/prefs.rs`
(`[warnings]`, `[keys]`, `[verbosity.*]`), `tui/src/app/keys/bindings.rs` (read the overrides).
Done when: `a_muted_warning_kind_is_never_drawn`, `a_rebound_key_is_honoured_and_a_global_chord_is_not_rebindable`,
`a_verbosity_profile_is_a_list_of_shown_things`, `queue_constants_come_from_config_with_todays_defaults`.

As built: the six tables are read at BOTH levels in `leticode-config` - `[queue]
stuck_after_seconds`/`stuck_repeat_seconds`, `[nag] after_seconds`/`floor_seconds`/
`repeat_cap_seconds`, `[room] tie_breaker`, `[warnings] mute`/`coalesce`, `[keys]`,
`[verbosity.<name>] show = [...]` - and the project's value wins key by key. The two exceptions to
"the project wins" are both suppressions and both union, `[skills] pin`'s rule: `mute` (a project
must not unsay what the operator muted) and a profile of the same name (replaced). Every refusal
names its file and line. The runtime threads what it owns: `[nag]` and `[queue] stuck_repeat_seconds`
become one `NagTimings` (the four numbers' one home is still `crates/tools/src/builtins/todo/nag.rs`),
`[queue] stuck_after_seconds` reaches the census through `stuck::after_ms` (`STUCK_AFTER_MS` for
`None`), and `[room] tie_breaker` replaces the `DEEPSEEK` literal the coordinator hardcoded. The
head's three are read here and RESOLVED there (the surface names, the warning codes and the switches
are its vocabulary), and `[keys]`' global-chord rule is W8's: a binding is rebound within its
surface, never the escape - the refusal lists the chords.

The other numbers §4.3 points at are NOT keyed and are not touched: `serve.rs:26-47` and
`capacity.rs:11-52` (`REVIEW_DEADLINE_MS`, `MAX_REVIEWING`, `PER_HOST_MAX`, `CORES_PER_REVIEWER`, …)
stay constants, since the operator's ask for the queue was *the census's thresholds*.

### K10 - `command_` everywhere, `lifecycle_`, self verbs (M, after H0)

Owns `tui/src/app/commands.rs` (`other =>` arm), `harnessd/src/slash.rs` (fallback),
`harnessd/src/harness/open.rs` and `harnessd/src/restart.rs` (lifecycle call sites),
`tools/src/builtins/session_self.rs` (new: `reseat_self`, `compact_self`), `plugin-sdk/src/lifecycle.rs`.
Done when: `an_unknown_head_verb_is_offered_to_its_command_layer`,
`a_layer_cannot_shadow_a_host_verb`, `a_lifecycle_layer_cannot_block_a_reseat`,
`a_manifest_asking_for_an_operator_verb_is_refused`, `the_model_reseats_itself_without_tmux`.

### K11 - the bridge (M, after K3 and R144's design)

Owns `bridge/src/tools.rs` (plugin tools listed after the 23), `bridge/src/push.rs` (`push_`),
`bridge/Cargo.toml` (`letibot-plugins`), `plugins/builtin/push/**`. Done when:
`a_user_tool_layer_is_listed_as_an_mcp_tool`, `a_push_layer_cannot_suppress_an_ask`,
`addressed_only_is_a_push_rule`.

## 7. Open questions, with defaults

1. Which layers does a child inherit (K1)? Default: built-in, user, project, and the parent's pinned
   session layers. Unpinned ones stay the parent's.
2. Should `on_tool_call`'s `answer` be allowed for exec and write tools, given it skips the gate?
   Default: keep. It performs nothing; the `[note]` line is the disclosure. Restrict it if a layer
   is ever seen answering `bash`.
3. May a nag layer be quiet? Default: yes, up to `NAG_REPEAT_CAP`, never for queue rows.
4. May a session layer answer `prompt_` or `adjudicate_`? Default: no to both. Prompt bytes are the
   cached prefix, and the floor is reviewed policy.
5. G3, agents typing operator verbs through tmux: native self-scoped verbs (X24), and operator verbs
   stay the operator's? Default: yes.
6. Warnings, keys, verbosity profiles, queue constants: config, not plugins (§4.3)? Default: yes.
7. leticl's JSON dashboards and the wasm pane language (G10): one contract? Default: the pane
   language is the contract; a built-in pane layer reads the JSON files with
   `read:~/.config/letibot/dashboards`, and leticl hosts the same pane language.
8. The butler (`docs/plugins-plan.md` §6): when? Default: after K3, when there are decision hooks
   worth fitting.
9. The first review's questions 2-6, against both reviews. Defaults: the `chat` feed for built-ins
   and pinned layers only (and the same rule for any feed of other sessions' text); cursor mode;
   `exec` looks for pinned panes AND pinned `tool_` layers (K8); `dock` as the first step of
   windows; no automatic pin on rebuild.

## 8. Waiting on the operator, both reviews

Status 2026-11-07: both reviews are written; nothing of them is built except R151 (in flight). Every
question below has a default; a question not answered is built on its default. Answers go here, one
line each under the question, dated.

From this review (§7):

1. Which layers a child inherits. Default: built-in, user, project, and the parent's pinned session
   layers; unpinned session layers are not inherited.
2. `on_tool_call`'s `answer` skips the gate for exec and write tools. Keep it? Default: keep - it
   runs nothing and the `[note]` line discloses it; restrict it if a layer is seen answering `bash`.
3. May a `nag_` layer stay quiet? Default: yes, up to `NAG_REPEAT_CAP`, never for queue rows.
4. May session layers answer `prompt_` or `adjudicate_`? Default: no to both.
5. Agents typing operator verbs through tmux (`/queue reset`, `/reseat`). Default: native
   self-scoped verbs (`reseat_self`, `compact_self`); operator verbs stay the operator's.
6. Warning mute, `[keys]`, verbosity, the queue and nag constants: config, not plugins? Default: yes.
7. leticl's JSON dashboards (`~/.config/letibot/dashboards`) against wasm panes. Default: one
   contract, the pane language; a built-in layer reads the JSON files and leticl draws the pane
   language too.
8. When the butler comes. Default: after K3.

From the first review (`docs/plugin-extension-points.md` §6; question 1 is answered there, §7):

9. Who may read the `chat` feed, and any feed of other sessions' text. Default: built-in and pinned
   layers only.
10. Message keys: cursor mode (Tab on an empty input enters, Esc leaves) or uppercase keys while
    typing. Default: cursor mode.
11. `exec` looks for pinned panes and pinned `tool_` layers, through `admit_unasked`. Default: yes.
12. `dock` beside the conversation: the "tabs or windows" asked for, or a first step. Default: first
    step; one right-hand column, host-owned width.
13. Pin a session layer automatically when the session that built it loads it. Default: no; `load`
    names the one command to type.

Decided so far:

- 2026-11-07: the native room renderer stays only as a safety net until the built-in chat plugin has
  run without traps, then it is deleted (first review, §7).
- 2026-11-07: the first review was too room-focused; this wide review answers for the whole system,
  and its §6 plan (K1-K11 merged with H0, P1-P6 and R151) is the one plan.
