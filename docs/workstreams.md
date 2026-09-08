# Work breakdown — what parallelises, and what does not

Companion to `/home/dead/harness-implementation-plan.md`. Written 2026-09-09.

This document does not restate the plan, revise it, or argue with it. It answers one question:
**what can be built at the same time, and what is genuinely serial.** Section references are to the
plan unless marked *brief* (`harness-design-brief.md`).

Sizes are rough, in developer-days, for one experienced person working in Rust (or Python for the
suite). They are for ordering work, not for scheduling it.

---

## 0. The claim everything hangs on, checked

The plan says M1 needs **no C++ change** (§3.6 practice 2, §5.6, §17-M1). This is the single largest
parallelism enabler in the plan, so it was verified against the running fork
(`/home/dead/Projects/llama.cpp`, branch `glm-all`) rather than taken on the plan's word.

| what M1 needs | verified at | holds? |
|---|---|---|
| `prompt` as a token array | `tools/server/server-common.cpp:807` (`json_is_array_of_numbers`), consumed in `tokenize_input_subprompt` at `:989` | yes |
| `cache_prompt`, `return_tokens`, `return_progress`, `timings_per_token` | `server-schema.cpp:21-38`, all four are declared request fields | yes |
| generated **token ids** back, not text | `server-context.cpp:4061` accumulates `slot.generated_tokens` under `return_tokens`; serialized as `tokens` on both the partial (`server-task.cpp:1064`) and final (`:354`) non-OAI results | yes, **with a caveat below** |
| live prefill progress | `server-context.cpp:5953`, `:6392` under `return_progress`; `prompt_progress` on the partial | yes |
| checkpoint placement | `message_delimiters` is read from the request body on the *generic* completion handler, `server-context.cpp:6990` — not only the chat path | yes |
| renderer oracle | `POST /apply-template`, `server-context.cpp:7745` | yes, see the router caveat |

**Verdict: the claim holds. Treat it as load-bearing.** Every daemon strand can proceed against the
production server, unmodified, with no C++ in the loop. The entire server track (W15) is off the
critical path by construction.

Two things the plan does not say that a parallel worker will otherwise get wrong:

- **In stream mode the final chunk carries empty `content` and `tokens`** (`server-context.cpp:4322-4331`:
  *"in stream mode, content and tokens are already in last partial chunk"*). The generated token ids
  must be accumulated from the per-chunk partials. An implementation that reads only the final chunk
  finds no tokens and will quietly fall back to re-tokenizing the assistant text — which is exactly
  the silent-divergence class §4.3 exists to abolish. Write this into W6's contract now.
- **`/apply-template` is proxied in router mode.** `server.cpp:239` maps `post_apply_template` to
  `models_routes->proxy_post` (as are `/tokenize` and `/detokenize`). That is UNVERIFIED-6, and it is
  real. The day-one workaround costs nothing: run the W3 fidelity gate against a **directly launched
  single-model server**, not through the router. Then UNVERIFIED-6 stops gating anything and becomes
  a curiosity to settle later.

One more finding that changes a size estimate: `llama_model_params.vocab_only` exists
(`include/llama.h:344`) and `llama_tokenize` takes a `const llama_vocab *` (`:1205`). **The harness's
tokenizer needs neither the 199.7 GB of weights nor a GPU nor a running server.** W5's tokenizer half
is a self-contained, CI-runnable component. That is worth knowing before someone plans it around
server availability.

---

## 1. Strands

Sixteen strands. The "contract exposed" column is the important one: a strand is only genuinely
parallel if what it hands over is written down before the work starts.

| id | strand | delivers | plan §§ | size |
|---|---|---|---|---|
| **W1** | Measurement rig | recording proxy, scripted 30-turn session, metric extraction, opencode baseline | §14.3, §17-M0, §18.2 | 3 d |
| **W2** | Prefix-invariant suite | grok port, I1/I2, C1/C3/C4/C5, NOTICE + per-file headers | §14.1, §14.2, §18.1 | 4 d |
| **W3** | Renderer-fidelity rig | fixture corpus + `/apply-template` diff runner (C2 / I3) | §7.2, §7.4 | 3 d |
| **W4** | Dialects | GLM renderer + parser; then Qwen; then 27B dense | §7 | 8 d GLM, +3 d each |
| **W5** | Token core | vocab-only FFI, tokenizer, TokenLedger, memfd region, transcript types, SQLite store | §3.1, §4.1–4.4 | 10 d |
| **W6** | Turn engine | submit on the `/completion` fallback, stream decode, tool-call parse, LengthPolicy, steering, post-flight assertions | §5, §8.5 | 10 d |
| **W7** | Session log + head protocol | event log with `seq`, snapshot, ATTACH/RESYNC, fan-out, scrub | §4.5, §13.2, §13.2b | 7 d |
| **W8** | Heads | TUI + incremental markdown; remote WS; ACP adapter | §13.3, §13.4 | 5 d TUI, 3 d ACP |
| **W9** | Tool runtime + built-ins | the six-clause contract, spill, roles and budgets, `read_spill` | §8 | 7 d |
| **W10** | firecode substrate | session↔run mapping, exec and file channels, boundary adjudicator | §3.9, §11.4, §11.8 | 5 d + firecode changes |
| **W11** | Adjudication core | request/decision objects, class derivation, policy table, audit, timeouts, ack | §11.2–11.6, §11.9 | 7 d (+5 d auto mode) |
| **W12** | flowy connector | subscription or degraded reader, chat-as-user-item, per-room levels, decisions on the same channel | §3.7, §12 | 7 d |
| **W13** | Segments + leveled compaction | SegmentMark, segment table, levels, warm summarizer, soft/hard, scheduler | §10, §19 L1–L7 | 10 d |
| **W14** | EXPLAIN | `ExplainPlan`, per-stage counters, formatter, `explain --deep` | §6 | 5 d (see §6 of this doc) |
| **W15** | Server track | S0 control channel + shm; S1, S1b, S2b, S3b, S4b, S5b | §3.3, §3.4, §17 | 5 d S0, 1–3 d each after |
| **W16** | Experiments & settlements | Falsifier B; UNVERIFIED-6/11/12/13/15/16; the 27B preset | §9.3, §20.1 | varies |

### The contracts, named

| strand | what it hands over | specified in the plan? |
|---|---|---|
| W1 | the **scripted-session format** (turn list + tool script + assertions) and the **metrics table** it emits | **no.** §18.2 says "a fixed 30-turn conversation with a fixed tool script" and lists what it must include, but no file format and no driver interface. This blocks W1↔W16 and W1↔M1-exit. |
| W2 | `assert_prefix_stable_pair` re-expressed over token ids; the C-test pass/fail record | yes, §14.1 and §18.1-I1/I2 are precise enough to build against |
| W3 | the **fixture corpus format** (messages + tools + expected render) and the diff runner CLI | **partial.** §7.2 lists the shapes the corpus must cover; nothing says how a fixture is stored or how a dialect is invoked from Python. |
| W4 | `trait Dialect` — and, critically, **`RenderSpan`, `ParsedSpan`, `ControlTokens`, `Guard`** | **no.** §7.1 gives the trait's method signatures and nothing about the four types they traffic in. This is the highest-value missing interface in the plan (see §7 of this doc). |
| W5 | `TranscriptItem`, `ToolOutcome`, `TokenLedger` rows, the memfd region layout, the SQLite schema | **mixed.** `TranscriptItem` and `ToolOutcome` are exact (§4.2). The ledger row is exact (§4.3). The **memfd layout and lifecycle** (who creates, how it grows, remap, restart recovery) and the **table columns** are not. |
| W6 | the `Submit` call and the `TurnFinished`/`turn_metrics` record; the fallback request body | yes for the body (§5.6 table); `turn_metrics` fields are listed in §4.4 |
| W7 | the head **frame format**: ATTACH, RESYNC, snapshot, event envelope, `expected_seq`, `client_request_id` | **partial.** The event *list* is exact (§4.5); the frames and the snapshot format are prose only. |
| W8 | nothing downstream — it is a leaf | n/a |
| W9 | the tool schema (`access: read\|write\|exec\|network`), `ToolResult`, spill notice format, `read_spill(hash, range)`, and the **execution-backend interface** | **mixed.** The schema field and outcomes are exact. The backend interface (host vs firecode) is **not named anywhere**, which is what couples W9 to W10 unnecessarily. |
| W10 | the run lifecycle and the boundary facts fed to §11.3's class derivation | partial; §11.4 lists the facts, nothing types them |
| W11 | `AdjudicationRequest` / `AdjudicationDecision`, the policy table syntax | yes, §11.2 and §11.3 are buildable as written. The **`Adjudicator` trait itself is not written down**, though its shape is implied by "same object in, same object out". |
| W12 | the flowy asks document (§12.2) and the degraded text protocol (§12.3) | yes — §12.3 is unusually complete, and it is what keeps this strand unblocked |
| W13 | `SegmentMark` placement rules, the `segment` table, summarizer construction | **partial.** §19.2-L5 says a segment opens at a user boundary and how to label it; nothing says who **closes** one, which is what L1/L2 depend on. |
| W14 | `ExplainPlan` as a type | **no.** §6.2 gives a rendered example and §6.3 gives the sources. There is no field list. |
| W15 | the control-channel **wire encoding** and version handshake | **no.** §3.3 names the messages and says "length-prefixed, versioned binary over a Unix socket". Message *names* are not a wire format. |
| W16 | numbers, and written decisions | the methods are specified (§3.10-C, §9.3); **Falsifier B's scoring method is not** |

---

## 2. The dependency graph

Three edge kinds, and the whole point of the exercise is that most claimed edges are the third.

- **H — hard build dependency.** B cannot compile or run without A.
- **S — semantic dependency.** B can be built and unit-tested, but cannot be *verified* without A.
- **C — convenience.** It would be tidier in this order. It is not a dependency.

```
        W3 ──S──> W4 ──H──> W6
                    ^         ^
        W5 ──H──────┘         │            W15 (server track) ── C ──> everything
         └──H──────────────> W6 ──S──> W2/W1 (M1 exit measurement)
                              │
        W7 ──H──> W8          └──S──> W14
         └──H──> W12
        W9 ──H──> W11 ──H──> W12
         └──C──> W10 ──S──> W11(boundary)
        W5 ──H──> W13 ──S──> W6, W16(Falsifier B)
```

### Edges that are real

| from → to | kind | why |
|---|---|---|
| W5 → W6 | **H** | the turn engine appends to the ledger and submits a span of it; there is nothing to submit without it |
| W4 → W6 | **H** | the engine needs *a* dialect. A stub dialect that emits plain text against a real vocab satisfies the compile; only GLM satisfies the milestone. So the hard edge is on *the trait*, not on *the GLM implementation* — which is why fixing the trait's types early splits this edge in two. |
| W3 → W4 | **S** | a renderer can be written the day the fixture format exists; it cannot be *believed* until the diff is green. This is the plan's own gate (§7.2, I3) and it is the most valuable semantic edge in the graph. |
| W7 → W8 | **H** | heads speak the frame format |
| W7 → W12 | **H** | the flowy connector is a head (§13.4) |
| W9 → W11 | **H** | the action class is derived from `tool.access`, which lives in the tool schema (§11.3) |
| W11 → W12 | **H** | the connector delivers decision objects; it needs the object |
| W5 → W13 | **H** | segments are spans in the ledger (L6); compaction has nothing to address without the span map |
| W6 → W2 | **S** | the invariant suite compiles and runs against opencode today; it verifies *us* only once a turn happens |
| W1 → W16 | **H** | Falsifier B and UNVERIFIED-15 both need the scripted session as their instrument |
| W6 → W14 | **S** | EXPLAIN's numbers exist only once turns produce them; the formatter is writable before |
| W10 → W11 | **S** | the boundary adjudicator decides on facts firecode supplies. A `NullBoundary` that denies everything lets W11 be built and tested first. |

### Edges that look real and are not

This is where the time is.

| claimed | actually | note |
|---|---|---|
| W15 (server track) → anything | **C** | verified in §0. The `/completion` fallback carries token arrays, token ids back, progress, timings and `message_delimiters`. S0 is an optimisation that lands, never a gate. |
| W4 → W5 | **C, if `RenderSpan` is text-plus-control-token** | a renderer that emits `[Text(&str) | Control(ControlToken)]` needs no tokenizer, no vocab and no daemon. It is a pure function testable against `/apply-template` from Python. Left unspecified, someone will make `render` return `Vec<llama_token>` and this becomes **H** for no reason. |
| W8 (TUI) → W6 | **C** | the TUI renders an event stream. Give it a recorded event log as a fixture and it is built and demoed before a turn engine exists. |
| W9 → W10 | **C** | tools are algorithms — grep-that-reports-where-the-term-does-occur, find-that-returns-the-surrounding-listing, spill arithmetic. All of it is host-local and unit-testable. firecode is a **backend swap**, not a prerequisite, provided the execution interface is named. |
| W12 → flowy's `/api/inbox/stream` | **C** | §12.3's degraded mode is complete enough to build against. The connector should be written to the degraded protocol *first* and the stream added as a transport. Do not let this strand wait on another team. |
| W13 → W16 (Falsifier B) | **C for the mechanism, H for the thresholds** | warm summarizer construction, levels and the fork mechanics do not depend on the quality-vs-depth answer. Only *how aggressively to compact* does. Build the mechanism; leave the high-water constant a config value. |
| W11 → W12 | claimed **H**, but only for the *object* | the policy table, class derivation, audit rows and timeouts are testable with a `Console` adjudicator that prompts on stdin. Only end-to-end decision delivery needs flowy. |
| W2 → W5 | **C** | the ported grok suite runs black-box against opencode on day one. Re-expressing it over token ids comes later. |
| W6 → W7 | **C, both ways** | the engine emits events into a channel; the log consumes them. Fix `§4.5`'s enum (already exact) and these two never meet until integration. |

---

## 3. The critical path

```
[decide RenderSpan]  →  W4 GLM renderer  ─┐
                                          ├→  W6 turn engine  →  M1 exit (C1–C10 vs harnessd)
[vocab FFI]  →  W5 ledger + store  ───────┘        ↑
                                          W8 TUI ──┘ (M1 needs one head)
                                          W9 read-only tools ──┘
```

Longest chain to M1: **W5 → W6 → integration**, roughly 20–25 developer-days, with W4 running
alongside it if and only if `RenderSpan` is fixed first. W3 must be green before M1 exits but does
not gate W4's writing.

W1/W2/W3 (M0) are **not** on the critical path to M1 — except W3, and only as an exit gate. They are
on the critical path to *knowing whether M1 worked*, which is a different thing and is why the plan
puts them first.

### What can be taken off the critical path by specifying an interface earlier

Ranked by how much they buy:

1. **`RenderSpan` / `ParsedSpan` / `ControlTokens`.** Define them as text-and-control-token pieces
   with no vocab dependency and W4 leaves the critical path entirely — it becomes a pure function
   with a Python-driven acceptance test, buildable by a second worker from hour one. Leave them
   undefined and W4 waits on W5's FFI. *This is the single highest-leverage half-hour in the whole
   plan.*
2. **The head frame format** (ATTACH / RESYNC / snapshot / event envelope). Fixing it makes W8 a leaf
   that can start immediately against a recorded log, and takes the TUI — a real chunk of M1 — off
   the chain.
3. **The execution-backend interface** for tools (`run(cmd, cwd, env) -> (stdout, stderr, exit)`
   plus read/write/list). Names the seam between W9 and W10, so tools ship host-local in M1 and
   firecode becomes a swap in M2 rather than a rewrite.
4. **The scripted-session format.** Without it W1 and W16 are the same person's work serialised; with
   it Falsifier B can be run by someone who never touched the proxy.
5. **The control-channel wire encoding.** Not on the critical path, but writing it down early lets
   W15's S0 be built in parallel with M1 instead of after it, and it is the thing that keeps §3.5's
   "named module" exit discipline honest.
6. **`ExplainPlan` as a type.** Every strand that produces a number then fills in its own field as it
   goes, instead of W14 archaeologising them all at the end.

---

## 4. Day one versus blocked

### Startable tonight, no further decisions

| # | work | strand | note |
|---|---|---|---|
| 1 | install pytest; build the recording proxy | W1 | §14.4 calls this blocking and it is one command plus a proxy |
| 2 | fixture corpus + `/apply-template` diff runner | W3 | **point it at a directly launched single-model server**, not the router (§0). UNVERIFIED-6 then gates nothing. |
| 3 | port grok's prefix suite | W2 | Apache-2.0, no licence question, no harness needed |
| 4 | run C1–C10 against opencode v1.18.29 | W1 | the baseline; §17-M0's exit |
| 5 | vocab-only `libllama` FFI + tokenizer | W5 | verified: no weights, no GPU, no server |
| 6 | GLM renderer as a pure function | W4 | needs only the `RenderSpan` decision, which is an engineering call, not an operator one |
| 7 | built-in read-only tools to §8.1's six clauses | W9 | fully standalone; the miss-is-self-correcting behaviour is where the value is and it is pure logic |
| 8 | event log + head frames | W7 | §4.5's enum is exact |
| 9 | settle UNVERIFIED-13 | W16 | grep the server startup log for the `--cache-reuse` refusal line. Minutes. The plan says do it before M4; do it tonight, it is free. |
| 10 | settle UNVERIFIED-11 / -12 | W16 | time 50 `firecode in` calls and one tar round trip. Decides whether W10's exec path needs a persistent channel. |

### Blocked on an operator decision

Stated as questions. Each one blocks a *start*, not merely a finish.

| # | question | blocks | why it cannot be deferred |
|---|---|---|---|
| **D1** | **Where does the code live?** §3.3 and §11.6 say one repo, one release, one version number. Does the Rust workspace go *inside* `/home/dead/Projects/llama.cpp` on `glm-all`, or beside it with a shared version file? | the first commit of W5/W6, and the whole of W15 | "one release" is a property of a repository layout, and changing it later rewrites every path in CI |
| **D2** | **Which flowy reader does harnessd use?** §12.2 ask 5 offers `(seat, purpose)` readers or a seat per agent, recommends the latter, and UNVERIFIED-17 says only flowy's owner can answer. Today `claude-lab2x1`'s single reader is held by the interactive session — one waiter per name, enforced. | W12 entirely, including the degraded mode | there is no flowy work at all until harnessd has a name it may hold a reader under |
| **D3** | **Will firecode get the parent-cgroup hook (§11.8 ask 1) and the persistent shell channel (ask 2), or does harnessd reap its own runs and maintain cwd/env itself?** The plan explicitly offers "a documented statement that it will not exist" as an acceptable answer. | W10's design, and M2's exit criteria | these are two different tool runtimes, not two settings |
| **D4** | **What is flowy's licence?** UNVERIFIED-19. | the flowy delivery tests in W2 and the suite's `NOTICE` | it is a question of intent, and only the operator has it |
| **D5** | **How is Falsifier B scored?** §9.3/M3.5 specify the depths (20k / 60k / 150k) and that it is answer quality, not speed. Nothing says who or what grades it. | W16's largest item, and M4's aggressiveness | a measurement without a stated rubric produces a number nobody will act on, and §9's whole position rests on this one |
| **D6** | **What is `max_inline_bytes`?** §8.3 deliberately ships no default: unset means the spill policy is a no-op. | W9's spill path, weakly | small, but it must be a number before spill is testable. A placeholder is fine if it is recorded as one. |
| **D7** | **Is the 27B dense preset going into `router-presets.ini`?** §7.3 calls it a prerequisite; §11.7 recommends the same model for auto mode; UNVERIFIED-7 and -8 both hang off it. | W16, M7, M8 | it is the operator's launcher |
| **D8** | **Harness licence** (§14.2). The plan recommends Apache-2.0 for the *suite* regardless; the harness's own is open. | nothing, today | listed only so it is not discovered at ship time |

### Looks open, is already resolved — do not re-litigate

Checked against the plan before listing above: **language** (Rust daemon + Python suite, §16);
**the seam** (two processes, private control channel, shm, §3.3-c); **whether M1 needs C++** (no,
verified in §0); **steering policy** (inject at the next step boundary, §5.8); **flowy vs the
firecode room** (separate, harnessd bridges, §12.4); **cache-ladder sharing** (independent policies,
shared vocabulary, §10.5); **block-sharing granularity** (two ladders at two tiers, §3.10-A);
**segment swapping in v1** (no — append, §19.3); **ACP** (an adapter, not the native protocol,
§13.4); **reasoning replay** (verbatim, non-negotiable while llama.cpp caches generated tokens,
§9.1); **compaction default** (soft; hard at 80% of `n_ctx`, §10.4); **system-update mode**
(user-turn envelope until a per-model eval says otherwise, §5.3); **the adjudication policy table's
initial contents** (§11.3); **tool budgets and roles** (§8.4).

---

## 5. Synchronisation points

Where strands must meet, what must be true when they do, and what breaks if it is not.

| # | meeting | must be true | if it is not |
|---|---|---|---|
| **S1** | W4 + W5 — the first token appended | `render_incremental ≡ render` and `parse ∘ render ≡ id` both pass as property tests (I3b), and the `/apply-template` diff is green for GLM (I3) | a renderer bug becomes a ledger that is internally consistent and wrong. The hash chain will agree with itself forever. This is the 612 GB failure with better instrumentation. |
| **S2** | W5 + W6 — the first submit | the memfd region's lifecycle is agreed: who creates it, how it grows, what a daemon restart rebuilds from (§4.3 clause 6 says replay the ledger's spans) | two owners of one region, or a restart that re-renders instead of replaying, which reintroduces exactly the non-determinism I4 exists to catch |
| **S3** | W6 + W1/W2 — M1's exit | the rig can **drive harnessd**, not only observe it. Today the scripted session drives an HTTP client; harnessd is a daemon behind a Unix socket. | M1's stated exit criterion ("C1–C10 pass against harnessd where opencode failed") is unmeasurable, and the milestone is declared on a feeling |
| **S4** | W7 + W12 — the flowy head attaches | the snapshot handed to a late head is the **scrubbed** projection (§13.2b): settled decisions render as outcomes, not as open prompts; and the read cursor advances over everything *read*, not everything *kept* | a decision is answered twice, or a filtering consumer rereads its own output forever. Both are bugs flowy has already paid for, in that repo, with tests. |
| **S5** | W9 + W11 — the first gated call | every tool schema carries `access`, and every action derives exactly one class (§18.3's third property test: a policy that does not match every action exactly once is rejected at load) | everything falls through to a default nobody chose, and the policy table becomes decorative |
| **S6** | W6 + W15 — S0 lands | the submit path is behind a named module (§3.5's exit discipline), and both sides refuse a protocol-version mismatch loudly | S0 becomes a turn-engine rewrite instead of a backend swap, and the fallback path rots because nothing exercises it |
| **S7** | W13 + W5 + W14 — compaction meets the ledger | segments are **contiguous spans** (L6), labelled at open (L5), with a stated close rule | §19's three futures are foreclosed, and re-labelling 200 old segments later means running a model over the whole history — the stop-the-world pass §10 exists to abolish |
| **S8** | M6 subagents + admission | a concurrency ceiling exists as a configured number *before* concurrent dispatch, whether or not `Admit` (S2b) has landed | the box's first real concurrent client meets its own OOM path. §3.4.4's whole point is that today the loser of a blind negotiation is an OOM kill. |
| **S9** | W4 + a model file update | `template_sha` is recorded per dialect and the diff re-runs on mismatch (§7.2) | the one hazard self-rendering introduces goes silent, which §20.2 names as the most dangerous risk in the plan |

---

## 6. Parallelism honesty

Where splitting the work costs more than it buys.

**The token pipeline is one person's work, end to end.** Render → tokenize → ledger → submit →
parse. It is presented above as W4/W5/W6 because the *interfaces* between them are worth naming, but
the parse side of a dialect is defined by its render side (`parse ∘ render ≡ id`), and the ledger's
correctness is a property of what the renderer emitted. Two people either agree constantly or ship a
consistent-and-wrong pipeline. Assign W4, W5 and W6 to one mind and use the interfaces as *notes to
self*, not as a handover — except for the second and third dialects (W4's Qwen and 27B work), which
genuinely are separable once the trait is real and the diff is green.

**EXPLAIN cannot be built by an observability strand.** §6.4 asks each stage for its own counters,
and only the person who built a stage knows which of its numbers is a proxy and which is the fact
(the memory this fleet already paid for: publication age is not progress; a poll counter is not
reading). W14 as a standalone strand produces a beautiful formatter over numbers nobody validated.
The right split: **the `ExplainPlan` type and its printer** are a strand; **the fields** are a tax
every other strand pays as it goes.

**Compaction policy and the segment model are one design.** §10's levels, §19.1's decay rates and
§19.2's labelling are the same decision viewed three times. Splitting "segments" from "compaction"
produces a segment model that cannot express what compaction needs, discovered at S7.

**The flowy connector is one person's work.** Its hard parts interlock: the per-room level table
(§12.2b), the read-cursor rule, the `Skipped` disclosure, and the human-broadcast clause that has
failed in *both* directions. Each is a small thing that becomes a lie in combination with the wrong
neighbour.

**Physical limits on this box, which no interface fixes.** With `--no-op-offload` the CPU is on the
inference path, so a `-j 24` Rust build steals prefill and decode from production (§18.2, and the
plan's own risk list). Two workers compiling concurrently is a measurable production regression, not
a scheduling detail. The realistic ceiling here is **two or three strands actually building at once**
— nice them, use a quiet window, or build in a firecode run (§3.6 practice 3, which is also the
tidiest early use of the substrate). Strand parallelism is bounded by the box, not by the graph.

**And the honest framing of who the parallel workers are.** This is one operator with agents, not a
team. That makes the contracts *more* load-bearing than they would be for humans, because a human
who finds `RenderSpan` undefined asks; an agent invents one, and two agents invent two.

---

## 7. Underspecified enough to be a risk to parallel work

Ranked by blast radius. Each of these is a place where two workers will produce two incompatible
answers, and neither will be wrong.

1. **`RenderSpan`, `ParsedSpan`, `ControlTokens`, `Guard`** (§7.1). The trait is given; its
   vocabulary is not. Decides whether W4 is on the critical path. Fix first.
2. **The control-channel wire format** (§3.3). Message names, a transport and a versioning *policy*
   are given; framing, encoding and the handshake are not. W15's S0 cannot start without it, and the
   plan's "refuse a peer whose version is not exactly ours" is unimplementable as written.
3. **The memfd region: layout, growth, ownership, restart recovery** (§4.3). "One region per
   transcript, appended by the harness, mapped read-only by the server" is a policy, not a format.
   S2 fails here.
4. **The head frame format and the snapshot** (§13.2). The event enum is exact; the envelope is not.
   Blocks the cheapest strand-lift available (W8).
5. **The execution-backend interface** for tools. Not named anywhere. Creates a false W9→W10 edge.
6. **`ExplainPlan`** (§6.2). An example rendering is not a type.
7. **The scripted-session format and how the rig drives harnessd** (§18.2, §17-M1 exit). S3.
8. **Fixture format for the `/apply-template` corpus** (§7.2). The *shapes* to cover are listed
   precisely; the storage and invocation are not. Two people will build two corpora.
9. **Segment close rules** (§19.2-L5 gives open and label; nothing gives close). S7.
10. **The `Adjudicator` trait** (§11.2 gives the objects, not the interface). Minor — the objects
    constrain it heavily — but two adjudicators will otherwise be written to two shapes.
11. **The SQLite schema.** §4.4 names eight tables and their contents in prose. Whoever writes W5
    writes the schema; anyone else who touches it will guess columns.
12. **The Journal `Part` type** (§4.1, §15: vendored from opencode). Named as a thing to take, never
    defined here.

Two small documentary defects noticed in passing, neither structural: §12.2 item 7 has a duplicated
opening clause (`**Streaming/updatable message content.**7. **Streaming/updatable…`), and §17's
server-track list numbers items `S1, S1b, S2b, S3b…` with `S2`–`S7` appearing separately below, which
reads as two overlapping schemes for one track.
