# Adversarial review: plugin extension points (R150), 2026-11-10

Snapshot: `5ff5707e` (R150 built through K11, after the comments-only deslop).
Panel: three positions, two rounds, models rotated between rounds. GLM was planned for one seat
and was out of balance (error 1113), so Sonnet took it.

| Position | Round 1 | Round 2 |
|---|---|---|
| 1. Against: cosmetic or unsafe | DeepSeek | Opus |
| 2. For: useful as built | Sonnet | DeepSeek |
| 3. Against both: poor implementation | Opus | Sonnet |

The reports below are verbatim. Synthesis and decisions are at the end.

## Round 1

### Position 1 (DeepSeek)

## Report — reviewer 1

**Thesis.** The parsing, clamping and fail-direction are mostly right; the *scope* is not. Of the hooks that really decide, six change behaviour and clamp correctly — I concede that to reviewer 2. But the one guard hook is open to the agent it guards, the one hook K10 exists for cannot do anything, and `admit_for` — the design's second door — is dead code. That is not "cosmetic"; it is inert where it matters and unsafe where it counts.

**Evidence.**

1. *Four hooks.* `steer_turn` decides: `turns.rs:2085-2106` (a `Continue` takes a round, `Stop` overrules the detectors), clamped in `plugin-sdk/src/steer.rs:186-205`. `compact_summary` decides: `harness/wall.rs:1355-1359` (`after: wait` really stops the auto-continue). `adjudicate_floor` decides and is tighten-only: `tools/src/adjudicate.rs:2003-2041`, `tools/src/gatekeeper.rs:410-417`. `command_` **cannot**: the head performs `add_todo` and `say` only (`tui/src/app/commands.rs:1150-1170`), the daemon `say` only (`harnessd/src/slash.rs:544-580`); X23's `monitor_arm`/`job_kill`/`queue_op` are unbuilt and the operator verbs G3 needed are refused at admit (`plugins/src/manifest.rs:402-425`). It is an `/echo`.

2. *The guard is reachable by the guarded.* `on_tool_call`'s `answer` stands in for **any** tool's result, before the gate (`tools/src/runtime.rs:1852-1863`, `:2037`, gate skipped `:2077`); only a `[note]` line says so (`harnessd/src/plugins.rs:2850-2862`). The module header claims "a layer cannot answer admitted" (`plugin-sdk/src/tool_call.rs:1-3`) and its test checks field *names*, not effect (`:182`). A session layer reaches it: `load` applies `Scope::Session` (`plugins.rs:1511`), `NOT_A_SESSIONS` omits it (`plugins/src/stack.rs:127`), and `Manifest::admit_for` (`manifest.rs:528`) is called by nothing but tests. The `plugin` tool scaffolds `on_tool_call`; one card later, `bash` can answer "1614 passed" and the gate never sees the call — the harness's own principle ("reviewed policy is not the governed agent's to write", `stack.rs:124`) applied to the floor but not the guard.

3. *A result can be grown.* `on_tool_result` rewrites the payload **after** the spill (`harness/turns.rs:2305` → `harness.rs:1465-1478`), with no length check (`plugin-sdk/src/tool_result.rs:121-129`) — unlike `digest_`'s `Out::checked`. One call can hand the model megabytes and force a compaction, declaring no capability.

4. *Fail direction.* `FAIL_CLOSED = [on_tool_call, adjudicate_floor]` (`stack.rs:122`) is the right set; everything else falls to native, which is honest — except that a session layer may drop its own `nudge`/`stall`/`context_claim`/`nag` (`plugin-sdk/src/notice.rs:117`) and reword a project's must-see sentence, because resolution is top-down (`stack.rs:263-270`).

5. *Two hosts, two answers.* The daemon adjudicates a `tool_` layer (`plugins/tool.rs:236-260`, caps `:539-560`); the bridge runs the same stack with no trust decision and no gate (`bridge/src/plugins.rs:117-121`, `:206-260`). Looks are refused, so no read — but the weaker host is the one an MCP client talks to.

**Proposed changes.** (a) add `on_tool_call` and `notice_` to `NOT_A_SESSIONS`, or restrict `answer` to unattended calls; (b) bound `on_tool_result`'s payload (reuse `digest_`'s `checked`, or re-spill after it); (c) call `admit_for` from `Stack::apply`/`discover_scope`, and give `plugin load`'s card the layer's hooks and capabilities so the operator sees what they authorise; (d) grow `command_`'s acts or strike X23's promise; (e) one `admit` shared by daemon and bridge.

**Concessions.** Reviewer 2 is right about six hooks — the tests do prove the clamps end to end. Reviewer 3's cost point is real (12.9M fuel for one chat window), but a cheaper wrong door is still wrong, so fix scope first.

### Position 2 (Sonnet)

THESIS
The layers change real decisions, and the guardrails fit each hook's risk. Failure rules are chosen per hook, not one blanket rule. Native behaviour is the explicit fallback. Gaps exist, but they are in coverage and docs, not in the design.

EVIDENCE
1. Layer changes the outcome / native unchanged, four hooks
- nag: `crates/harnessd/src/plugins/tests.rs:4953` `a_layer_naming_every_free_head_is_r127` shows a layer naming all free heads with its own text and `next_ms`. The same test then shows the no-layer host returns `None` (the native one-row nag). `:4868` shows a layer cannot name a row outside `eligible`. `:4989` shows a trap falls to native and is logged as `plugin_layer`.
- push: `crates/bridge/src/push.rs:744` shows a layer may reword an ask or failure but never silence it. `:817` `addressed_only_is_a_push_rule` runs the real compiled built-in wasm: a line to somebody else is not pushed, and mine and @-mentions are. `:779` shows pass, garbage and `{}` all give the bridge's own push.
- steer: `crates/harnessd/src/harness/tests/steer_door.rs:161` shows a layer turns "stop" into "continue" only after a compaction. `:115` and `:142` show the host refuses a continue past budget or on an interrupted turn.
- card: `crates/tui/src/app/tests/cards.rs:74` shows a reworded notice keeps its card, and the reader-only and no-stack paths give the generic or facts line. `:119` shows one ask per row id.
- adjudicate: `crates/tools/src/adjudicate/tests/floor.rs:219` shows a trap refuses the call. `crates/plugins/src/manifest.rs:1151` shows a session-scope floor layer is refused at admit.

2. Built-ins against the native code
- cards: `cards.rs:143` `the_builtin_cards_layer_draws_what_the_readers_drew` walks every reader FIXTURE. It checks the layer's own answer, the drawn row, and `session_notice` all equal the expected line. Where the layer says nothing, it asserts the readers know no such shape. Both paths come from the same functions (`SKILL.md:380`).
- push: a no-layer bridge and the built-in give the same answer (`push.rs:813-` comment and `:817`).
- review: `plugins/tests.rs:2786` `the_builtin_review_layer_briefs_as_the_native_prompt_did`.
- nag todo: `plugins/builtin/todo/src/lib.rs:208` shows the layer returns exactly the text and name it was handed, so it cannot differ from native.

3. Could a project ship `push_channel` or `card_item` from `SKILL.md` alone?
Mostly yes. `SKILL.md:325-375` gives the toml, input JSON, output JSON, refusal rules, and the positional-answer rule for cards. The loop (scaffold, build, test, load) is at `:380-410`, and `plugin test` uses a synthetic input.
Gaps:
- `:407` says scaffold has no template for `push_` or `card_`. You must scaffold the nearest and rename the export.
- `:10` and `:12` say "eight kinds" while the table has 15.
- Cards are asked only when a layer is on disk (`:357`), which is documented.

4. Failure rules are per hook
- Fallback is "layer below, then native, said once" by default (`docs/plugin-extension-points-wide.md:293`).
- `adjudicate_floor` fails to the strictest tier (`SKILL.md:54`). A floor that cannot answer has not said the call is fine.
- `push_` falls to the bridge's own push (`SKILL.md:348`). A silent channel is the failure to avoid.
- `nag_` quiet is overridden at the repeat cap (`plugins/tests.rs:4936`).
- `card_` and `digest_` refuse the whole answer rather than zip a shifted one (`SKILL.md:202`, `:395`).
- Each is justified by what a wrong fail costs.

PROPOSED CHANGES
- Add `push_`/`card_` scaffold templates, or an explicit worked example. Fix "eight kinds" in `SKILL.md`.
- Add a built-in nag test that passes the real wasm through a host and compares against the native decision. Today the nag built-in is an echo with a unit test only.
- The steer tests use closures, not wasm, and `a_harness` skips without a gguf. Add one wasm-backed steer layer test.
- Add a timeout test per hook. I found a floor trap test but not a per-hook timeout table; the doc says fuel only.

CONCESSIONS
- `nag_todo` built-in is an identity, so "reproduces native exactly" is trivially true there and proves little.
- Steer/compact end-to-end coverage with real wasm is thinner than for push and cards. I did not find a compact layer test beyond the host entry at `plugins.rs:809`.
- Timeouts are not separately justified. Fuel and memory are one global (`HOOK_FUEL` 50M, 64 MiB, `docs:293`).
- Cost and duplicated stacks (reviewer 3) are unrebutted here. I did not measure them.
- I did not run any tests. This is a read-only review.

### Position 3 (Opus)

Reviewer 3, round 1. Read-only, nothing run beyond grep and reads.

**Thesis.** The layering idea is sound. The code that carries it is not. Plugin cost lands on paths that used to be free. One wasm host is copied three times, and the copies already disagree. Errors go to stderr or to a single slot that later errors overwrite. The daemon side is one 3,217-line god object.

**Evidence**

1. Compile and call cost.
   - rano builds a new `Engine` and runs Cranelift `Module::new` on every load (rano c12f672 `src/plugin.rs:138-139`). Every hook call then builds a new `Linker`, a new `Store` and a full instantiate (`plugin.rs:504-506`). That is per call, not per session.
   - Only the daemon caches compiled built-ins (`harnessd/src/plugins/rano.rs:51-75`), and its own comment says a compile "costs seconds of CPU".
   - The head and bridge loaders do not override `load_builtin` (`tui/src/app/sessions_pane.rs:1490`, `bridge/src/plugins.rs:16`). So `letibot_builtins::install` compiles all 13 built-ins (80-233 KB each) on every `Host::build`.
   - The head builds a new host whenever the workspace changes. It does this inside `plugin_host()` (`sessions_pane.rs:715`), which `sessions_list(w)` calls while drawing.
   - The bridge compiles all 13 to serve two hooks (`push_`, `tool_`), and does it while holding the `plugins` mutex (`bridge/src/tools.rs:301`).
   - User and project layers go through uncached `load()`. So every session and every child (`for_child` -> `production`, `plugins.rs:369`) recompiles them.

2. A session with no layers still pays. `production()` always installs the nag and decider built-ins (`plugins.rs:361-363`). Every turn end then instantiates `steer_turn` (`turns.rs:3216`), and every nag pass instantiates `nag_todo` (`nags.rs:282`). The bridge installs `push_channel`, so every room line and event is a wasm instantiate (`push.rs:229`). The doc comment at `plugins.rs:601` ("as cheap as it was") is false.

3. Wasm runs under the session lock. `ask` holds `inner` across `stack.call` (`plugins.rs:614-625`). Pane renders and turn hooks for one session therefore queue behind each other. `stack.rs:1044` also reserializes the input for every layer it walks.

4. `card_` caching. Answers are keyed by `hook\nrow_id` only (`sessions_pane.rs:594`), so the row's content is not in the key. At `MAX_CARDS` the whole cache is cleared (`:609`), which is a cliff, not LRU. The built-in `cards` layer is never asked unless a user layer exists (`cards_plugin.rs:243-257`). It is 13th-module dead weight that copies the native readers.

5. Three hosts, three copies.
   - `RanoLoader` and `RanoModule` are pasted almost word for word in the daemon, head and bridge. The comments call this "deliberate" (`bridge/src/plugins.rs:11`, `sessions_pane.rs:1485`). A `letibot-plugins-rano` crate would keep the JIT out of `letibot_plugins` just as well.
   - There are four config-dir resolvers. Daemon (`plugins.rs:2523`) and bridge (`:134`) filter an empty `XDG_CONFIG_HOME`. Head (`sessions_pane.rs:695`) and trust book (`trust.rs:749`) do not. If the variable is set but empty, the head reads layers and pins from the relative `letibot/`, which differs from the daemon. That is a real divergence bug.
   - The session dir has the same split: `letibot_data_dir` filters an empty value (`git.rs:427`) and the head's `:682` does not.
   - The "not pinned any more" sentence exists in four copies.
   - Inside the daemon, `install_builtin_for`, `_tools`, `_nags`, `_deciders` and `ensure_builtin_panes` (`plugins.rs:416-590`) are five copies of one loop: walk table, parse manifest, filter, check answered, register, `eprintln`.

6. Errors nobody sees.
   - Built-in load failures go to daemon stderr (`plugins.rs:431,461,488,524,587`).
   - The bridge writes to stderr of an MCP stdio child (`plugins.rs:124`, `push.rs:250`). Its `told` is one `AtomicBool` per process, so only the first distinct refusal is ever printed.
   - The head keeps one `problem` slot that is overwritten (`sessions_pane.rs:603,627`). It is shown only when the session picker opens (`surface.rs:250`), so a card-layer refusal turns up as a message attached to the session list.

7. Size.
   - `PluginHost` is spread over four `impl` blocks (`plugins.rs:203-3183`) with 54 public methods.
   - Its longest functions: `accepted` 161 lines, `load` 157, `render` 124, `stack_doc` 109. Also `stack.rs::ask` 121, `scaffold::answer` 132, `tool::answer` 120, head `session_list` 165.
   - The SDK is 6,506 lines with 338 public items across 15 hook kinds, plus a 967-line design doc and a 456-line skill. Nobody can keep that in their head.

**Proposed changes**
1. One shared rano crate with a process-wide compiled-module cache keyed by sha256, for every load path, plus one shared `paths` module.
2. One `Engine` per process. Pre-instantiate with `InstancePre`, or pool stores.
3. Install a built-in only on the first ask of its hook, as `install_builtin_for` already does. Skip the wasm call when only a built-in that copies native code answers.
4. Call modules outside the session lock: snapshot the `Arc<dyn Module>` list, then release.
5. Key cards by row id plus a content hash, and evict LRU.
6. Send failures to a per-host queue that is drained into the head's notice line, and dedupe per message, not per process.
7. Split `PluginHost` into a loader/installer, a hook dispatcher and the pane driver. Replace the five install loops with one `install_where(pred)`.

**Concessions.** The daemon's built-in cache fixes the worst case for sessions. Daemon-side refusals reach the model through `say_once` plus `hub.warn`. The bridge's `tools/list` reads manifests only (`bridge/src/plugins.rs:142`). The head checks for card layers through manifests at most once a minute without compiling (`sessions_pane.rs:291`).

Against reviewer 2: of 100 tests in `plugins/tests.rs`, about 6 touch the real engine. The rest use fakes with zero compile cost, so they prove the logic is correct, not that the cost is acceptable. Against reviewer 1: the safety gates are real. My objection is the cost and the upkeep.

## Round 2

### Position 1 (Opus)

Reviewer 1, round 2 (position: cosmetic or unsafe). Claims 2 and 3 both hold at 5ff5707e.

Claim 2 holds. `NOT_A_SESSIONS` (`crates/plugins/src/stack.rs:127-131`) lists only `adjudicate_floor`, the review brief and the review verdict. `on_tool_call` is not in it, so the session skip at `stack.rs:996` never fires for it. In `runtime.rs:2037-2043`, `Patch::Done` returns before the gate at `:2077`. An `Answer` becomes `ToolOutcome::Ok` plus a `[note]` (`:1852-1863`). The header claim "a layer cannot answer admitted" (`plugin-sdk/src/tool_call.rs:2`) is false in effect. `admit_for` (`manifest.rs:528`) has no caller outside tests. One correction: `plugin load` is `Access::Exec` (`tools/src/builtins/plugin.rs:194`), so loading a layer takes a gate card. The card does not list the hooks the layer exports.

Claim 3 holds. The spill is inside `invoke` (`runtime.rs:2278`). `plugin_result` replaces `*payload` afterwards (`harnessd/src/harness/turns.rs:2305`, `harness.rs:1471-1473`). Nothing bounds the length: `Said` has no check (`tool_result.rs:121-129`) and I found no output cap in `plugins/rano.rs`.

Claims 1, 4 and 5 I did not recheck.

Changes, in priority order:
1. Add `on_tool_call` to `NOT_A_SESSIONS`, or make `answer` from a session layer read as `Refuse`.
2. Re-spill or cap the payload after `plugin_result`. Reuse `digest_`'s `checked`.
3. Call `admit_for` from `Stack::apply`, and put the layer's hooks on the load card.
4. Fix the `tool_call.rs:2` header, and add a test that checks the effect rather than field names.
5. Give `command_` more verbs, or remove X23's promise.
6. Share one admission path between the bridge and the daemon.

### Position 2 (DeepSeek)

Everything above is from reading the code and tests; I executed no test. The offline build stops before compiling (Cargo.lock pins rano `c12f672b`, which this box's cargo cache does not hold), and I did not fetch.

**Round 1 holds where I checked it**: a nag cannot name a row outside `eligible` (crates/harnessd/src/plugins/tests.rs:4868) or outrun the repeat cap (:4925), a push cannot silence an ask (crates/bridge/src/push.rs:744), a broken layer loses no push (:779), a trapped floor refuses (crates/tools/src/adjudicate/tests/floor.rs:219), a reworded notice keeps its card (crates/tui/src/app/tests/cards.rs:74). Conceded: `admit_for` has no production caller; the three live admit sites call `admit` (crates/plugins/src/stack.rs:793, gate.rs:193, stack.rs:515).

**Two of its proposed tests already exist, wasm-backed**: crates/harnessd/tests/builtin_nag.rs:86 runs the real engine and module over four plan shapes against `unfinished_plan_for`; builtin_layers.rs:237 does steer, :182 and :205 compact; push.rs:817 compares the built-in against no layer. `nag_todo` being an identity is correct content: the bottom layer must move no sentence.

**P1 claim 2.** Real but bounded. `Patch::Done` returns before the gate (crates/tools/src/runtime.rs:2039) and `NOT_A_SESSIONS` omits `on_tool_call` (stack.rs:127), so a session layer may substitute a result. Nothing runs, no field admits (crates/plugin-sdk/src/tool_call.rs:182), and the payload is prefixed "answered by plugin X; the tool did not run" (crates/harnessd/src/plugins.rs:2859). Residual: it dodges its own gate card and corpus row.

**P3 coverage.** True of plugins/tests.rs alone. The fake swaps only `Loader`/`Module` (crates/plugins/src/testing.rs:1); the walk, merge, FAIL_CLOSED and scope rules tested there are production. Engine and ABI are covered by nine tui test files, four harnessd integration files and the bridge.

**Changes**
1. Call `admit_for` at the three admit sites, or stop claiming two doors (skills/plugin-hooks/SKILL.md:300).
2. A Session layer's `answer` becomes a refusal; one test.
3. SKILL.md: "Eight kinds" is 15 plus 3 surfaces; :54's "timeout" is fuel.

### Position 3 (Sonnet)

Reviewer 3, round 2. Read-only. Nothing run beyond grep and reads.

Confirmed:
- Rano rebuilds per call. Load runs `Engine::new` and `Module::new` (rano c12f672 `src/plugin.rs:138-139`). Every hook call runs `store`, `linker` and `instantiate` (`:504-506`).
- The head and bridge pay full compile. `register_builtin_bytes` calls `loader.load_builtin` eagerly (`stack.rs:547`). The default `load_builtin` is `load_bytes` (`plugins/src/lib.rs:64-72`), and only the daemon overrides it. So `letibot_builtins::install` (`sessions_pane.rs:657`, `bridge/plugins.rs:121`) compiles all 13 per Host.
- The empty `XDG_CONFIG_HOME` divergence is real. Daemon (`harnessd/plugins.rs:2525`) and bridge (`:135`) filter empty. Head (`sessions_pane.rs:696`) and trust (`trust.rs:750`) do not. With the variable set empty, the head reads layers and pins from the relative `letibot/`.

Corrected:
- "Head recompiles while drawing" is overstated. `plugin_host()` rebuilds only when the workspace changes (`sessions_pane.rs:709-715`). The first call per workspace stalls the UI thread on 13 Cranelift compiles. It is called from chat, editor, commands and scroll. Reload is by `reload_if_due`. So the cost is a first-call freeze plus every workspace switch, not per frame. The card gate (`any_card_layer`, `:286`) avoids the compile only for cards.

Not re-checked, standing from round 1: the `ask`-under-lock claim, card cache key and eviction, the five install loops, and the other sentence and line counts.

Final changes, in priority order:
1. Fix the head and trust empty-XDG filter. Add one shared `paths` module. This is a bug fix.
2. Make `load_builtin` cache by sha256 in the head and bridge. Share one `Engine` per process. Better, move the Rano loader into one crate.
3. Build the head Host off the UI thread, or compile built-ins lazily on first ask.
4. Skip the wasm call when the only answerer is a built-in identity (`nag_todo`).
5. Call modules outside the session lock.
6. Send refusals to a per-message queue.

## Synthesis

### Agreed by all three, after round 2

1. A session-scope layer can answer `on_tool_call` and the gate never sees the call.
   `NOT_A_SESSIONS` (`crates/plugins/src/stack.rs:127`) omits it and `Patch::Done` returns before
   the gate (`crates/tools/src/runtime.rs:2039`). Position 2 calls it bounded (nothing runs, the
   payload says a plugin answered), but agrees the layer skips its own gate card and corpus row.
   The model can write such a layer with the `plugin` tool, and the load card does not list the
   hooks it answers.
2. `Manifest::admit_for` has no production caller (`crates/plugins/src/manifest.rs:528`). The live
   admit sites call `admit`.
3. `skills/plugin-hooks/SKILL.md` is wrong in two places: "eight kinds" (there are 15 plus 3
   surfaces), and "timeout" at `:54` where the limit is fuel.

### Confirmed by one position, not disputed

4. `on_tool_result` can grow a payload after the spill, with no bound (position 1, both rounds).
5. An empty `XDG_CONFIG_HOME` resolves differently in the head and the trust book than in the
   daemon and the bridge (position 3, both rounds). The head then reads layers and pins from a
   relative `letibot/`. A real bug.
6. rano compiles a module on every load and instantiates on every hook call. The head and the
   bridge compile all 13 built-ins on every host build. In the head that is the first call per
   workspace and each workspace switch, on the UI thread (position 3, corrected in round 2: not
   per frame).

### Disagreements and decisions

- Test coverage. Position 3: about 6 of 100 tests in `plugins/tests.rs` touch the real engine.
  Position 2, round 2: the fakes swap only `Loader`/`Module`, and the engine is covered by nine tui
  test files, four harnessd integration files (`builtin_nag.rs:86`, `builtin_layers.rs:182,205,237`)
  and the bridge. Decision: coverage is adequate. New tests come with each fix below, not as a
  separate effort. Position 2's round-1 ask for wasm-backed nag and steer tests is withdrawn, since
  they exist.
- `nag_todo` is an identity layer. Position 2: correct, the bottom layer must change nothing.
  Position 3: then do not pay a wasm call for it. Decision: keep the layer; skip the call when the
  only answerer is a built-in that reproduces native. Part of the cost work.
- `command_` can only `say` and add a todo (position 1). Decision: the design doc stops promising
  `monitor_arm`/`job_kill`/`queue_op` until they are built. Growing the verbs is deferred.
- The bridge runs `tool_` layers without the daemon's trust decision or gate (position 1, round 1;
  looks are refused there). Decision: one admission path shared by daemon and bridge, done with
  the shared loader below.
- Cost, duplication and unseen errors (position 3) were not rebutted. Decision: do them, after the
  security and correctness fixes.

### Order of work

1. Security: a session-scope `on_tool_call` answer becomes a refusal; call `admit_for` at the admit
   sites; the load card lists the layer's hooks and capabilities; fix the `tool_call.rs:2` header
   and add a test that checks the effect, not the field names.
2. Bound `on_tool_result`'s payload with `digest_`'s `checked`, or re-spill after it.
3. One shared `paths` module; the empty `XDG_CONFIG_HOME` fix in the head and the trust book.
4. `SKILL.md`: the kind count, fuel instead of timeout, `push_`/`card_` scaffold templates.
5. One shared rano loader crate: a process-wide compiled-module cache keyed by sha256, one `Engine`
   per process, used by daemon, head and bridge, with one admission path. Build the head's host
   off the UI thread. Skip the call when only a native-copy built-in answers.
6. Refusals go to a per-host queue drained into the head's notice line; the bridge dedupes per
   message, not per process.
7. Call modules outside the session lock. Key cards by row id plus a content hash, evict LRU.
8. Split `PluginHost` (loader and installer, hook dispatcher, pane driver); one `install_where`
   for the five install loops.

Deferred: `command_` verbs beyond `say` and `add_todo`. Tracked in TODO.md R175.
