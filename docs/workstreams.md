# Work breakdown - what parallelises, and what does not

Companion to `implementation-plan.md`, whose section numbers it cites. It answers one question: what can
be built at the same time, and what is genuinely serial. The strand ids **W1-W16** are the marks
`TODO.md`, `DECISIONS.md` and several crates cite, so they are kept; each now also says where the work
landed.

## 1. Strands

Sixteen. The "contract exposed" column is the point: a strand is only genuinely parallel if what it hands
over is written down before the work starts.

| id | strand | delivers | plan §§ | landed where |
|---|---|---|---|---|
| **W1** | Measurement rig | recording proxy, scripted 30-turn session, metric extraction, opencode baseline | §14.3, §17-M0, §18.2 | `tests/fidelity` (`run_gate.py`, `serve_oracle.sh`), `experiments/falsifier-b` |
| **W2** | Prefix-invariant suite | grok port, I1/I2, C1/C3/C4/C5, NOTICE + per-file headers | §14.1, §14.2, §18.1 | `turn/src/metrics.rs` (`PrefixCheck`, `f_keep`), `turn/tests` |
| **W3** | Renderer-fidelity rig | fixture corpus + `/apply-template` diff runner (C2 / I3) | §7.2, §7.4 | `tests/fidelity` + `fixtures` |
| **W4** | Dialects | GLM renderer + parser; then Qwen; then 27B dense | §7 | `crates/dialect`, `dialect-glm`, `dialect-qwen` |
| **W5** | Token core | vocab-only FFI, tokenizer, TokenLedger, memfd region, transcript types, SQLite store | §3.1, §4.1-§4.4 | `crates/tokencore`, `crates/llama`, `crates/transcript` |
| **W6** | Turn engine | submit on the `/completion` fallback, stream decode, tool-call parse, LengthPolicy, steering, post-flight assertions | §5, §8.5 | `crates/turn` (`engine`, `stream`, `length`, `guards`, `steering`) |
| **W7** | Session log + head protocol | event log with `seq`, snapshot, ATTACH/RESYNC, fan-out, scrub | §4.5, §13.2, §13.2b | `crates/sessionlog` (`hub`, `server`, `wire`, `protocol`) |
| **W8** | Heads | TUI + incremental markdown; remote WS; ACP adapter | §13.3, §13.4 | `crates/tui`, `crates/rano`-based rendering, `crates/bridge`, `crates/claude` |
| **W9** | Tool runtime + built-ins | the six-clause contract, spill, roles and budgets, `read_spill` | §8 | `crates/tools` (`runtime`, `spill`, `builtins`) |
| **W10** | Execution substrate | session-to-run mapping, exec and file channels, boundary adjudicator | §3.9, §11.4, §11.8 | `crates/tools` (`exec/{confine,seatbelt,shell,jobs}`, `firecode`) |
| **W11** | Adjudication core | request/decision objects, class derivation, policy table, audit, timeouts, ack | §11.2-§11.6, §11.9 | `tools/src/{adjudicate,authorise,grant,permission}.rs`, `harnessd/src/answers` |
| **W12** | flowy connector | subscription or degraded reader, chat-as-user-item, per-room levels, decisions on the same channel | §3.7, §12 | `crates/flowy` |
| **W13** | Segments + leveled compaction | SegmentMark, segment table, levels, warm summarizer, soft/hard, scheduler | §10, §19 L1-L7 | `turn/src/compaction.rs`, `transcript` `SegmentMark` |
| **W14** | EXPLAIN | `ExplainPlan`, per-stage counters, formatter, `explain --deep` | §6 | metrics exist (`turn/src/metrics.rs`); **`ExplainPlan` has no field list** |
| **W15** | Server track | S0 control channel + shm; S1, S1b, S2b, S3b, S4b, S5b | §3.3, §3.4, §17 | **not landed**; the tree runs the `/completion` fallback |
| **W16** | Experiments & settlements | Falsifier B; UNVERIFIED-6/11/12/13/15/16; the 27B preset | §9.3, §20.1 | `experiments/falsifier-b` (run, scored); UNVERIFIED registry in the plan |

Sizes are gone on purpose: they were developer-day estimates for ordering, and the ordering is now
history. What remains is the ids and what each one owns.

### The contracts, named

A strand's contract is what it hands the next one. Where the plan left a gap, the answer is in the tree
now, and this is the list a new strand should read before inventing a second shape:

- **W1/W16 - the scripted-session format.** Not specified in the plan; the rig is `tests/fidelity` plus
  `experiments/falsifier-b/tasks.py`, and a new measurement should reuse them rather than grow a third.
- **W3 - the fixture corpus.** A fixture is messages plus tools plus an expected render, run by
  `tests/fidelity/run_gate.py` against a directly launched single-model server (not the router, §20.1
  UNVERIFIED-6).
- **W4 - the dialect types.** `RenderSpan`, `ParsedSpan`, `ControlTokens` and `Guard` live in
  `crates/dialect`. The plan's §7.1 named the trait and not its vocabulary; that is settled, and the two
  invariants (`render_incremental ≡ render`, `parse ∘ render ≡ id`) are the tests.
- **W5 - the store schema and the region.** §4.4 lists the tables; the memfd region and ledger live in
  `tokencore/{region,ledger,store}.rs`. A restart replays spans, never re-renders.
- **W7 - the head frames.** ATTACH, snapshot, `RESYNC`, the event envelope, `expected_seq` and
  `client_request_id` are in `sessionlog/{wire,protocol}.rs`; the plan's §13.2 is the prose and the code
  is the format.
- **W9 - the execution-backend seam.** `crates/tools/src/backend.rs` plus the `exec` module: this is what
  keeps W9 from depending on W10, since firecode is a backend swap.
- **W11 - the adjudicator interface.** §11.2's two objects are the interface; `Adjudicator` is not a
  named trait in the plan, and in the tree the gate is `AdjudicatedGate` with `ModelBrief` for the model
  arm.
- **W14 - `ExplainPlan` is still untyped.** This is the one contract the plan promised and the tree has
  not got: counters exist per stage, and a field list invented before them would have been guesses.
- **W15 - the control-channel wire format.** §3.3 names the messages and the transport; framing, encoding
  and the handshake are unbuilt, and S0 cannot start without them.

## 2. The dependency graph

Three edge kinds, and most claimed edges are the third: **H** a hard build dependency (B cannot compile or
run without A); **S** a semantic one (B builds and unit-tests, but cannot be *verified* without A); **C**
convenience.

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

- **W5 → W6 (H)**: the engine appends to the ledger and submits a span of it.
- **W4 → W6 (H)**: the engine needs *a* dialect. The hard edge is on the trait, not on the GLM
  implementation, which is why fixing the trait's types early splits it in two.
- **W3 → W4 (S)**: a renderer can be written the day the fixture format exists; it cannot be believed
  until the diff is green. This is §7.2's gate and the most valuable semantic edge in the graph.
- **W7 → W8, W7 → W12 (H)**: heads speak the frame format, and the connector is a head (§13.4).
- **W9 → W11 (H)**: the action class is derived from `tool.access`, which lives in the tool schema.
- **W11 → W12 (H)**: the connector delivers decision objects.
- **W5 → W13 (H)**: segments are spans in the ledger; compaction has nothing to address without them.
- **W6 → W2 (S)**, **W6 → W14 (S)**: the invariant suite verifies us only once a turn happens, and
  EXPLAIN's numbers exist only once turns produce them.
- **W1 → W16 (H)**: Falsifier B and UNVERIFIED-15 both need the scripted session as their instrument.
- **W10 → W11 (S)**: the boundary adjudicator decides on facts the substrate supplies, and a
  `NullBoundary` that denies everything lets W11 be built and tested first.

### Edges that look real and are not

- **W15 → anything (C)**. The `/completion` fallback carries token arrays, generated token ids, progress,
  timings and `message_delimiters`, so S0 is an optimisation that lands and never a gate. This is the
  single largest parallelism enabler in the plan and it is why this tree runs without the server track.
- **W4 → W5 (C, if `RenderSpan` is text plus control tokens)**. A renderer emitting text-and-control-token
  pieces needs no tokenizer, no vocab and no daemon, and is a pure function testable against
  `/apply-template`. Left unspecified, someone makes `render` return `Vec<llama_token>` and this becomes
  H for no reason.
- **W8 → W6 (C)**: the TUI renders an event stream, so a recorded log is enough to build and demo it.
- **W9 → W10 (C)**: tools are host-local algorithms, and firecode is a backend swap once the execution
  interface is named.
- **W12 → flowy's stream (C)**: §12.3's degraded mode is complete enough to build against, so write the
  connector to the degraded protocol first and add the stream as a transport.
- **W13 → W16 (C for the mechanism, H for the thresholds)**: only *how aggressively to compact* depends on
  Falsifier B, so build the mechanism and leave the high-water constant a config value.
- **W11 → W12 (H for the object, C for everything else)**: the policy table, class derivation, audit and
  timeouts are testable with a console adjudicator.
- **W2 → W5 (C)**, **W6 → W7 (C, both ways)**: the ported suite runs black-box on day one, and the engine
  emits into a channel the log consumes, so fixing §4.5's enum means the two never meet until integration.

## 3. The critical path

The longest chain to M1 was **W5 → W6 → integration**, with W3 green before M1 exits but not gating W4's
writing: W1/W2/W3 are on the path to *knowing whether M1 worked*, not to M1. The interfaces that took
work off the chain, in order of what they bought: `RenderSpan`/`ParsedSpan`/`ControlTokens` (without them
W4 waits on W5's FFI), the head frame format (W8 becomes a leaf), the execution-backend interface
(firecode becomes a swap), the scripted-session format, the control-channel encoding, and `ExplainPlan`
as a type.

## 4. Decisions

The eight that blocked a start are settled in `DECISIONS.md`, and this document no longer restates them:
**D1** the code lives in `~/Projects/letibot`, separate from the llama.cpp fork, with fusion enforced by
the contract tests rather than directory adjacency; **D2** harnessd reuses the `claude-lab2x1` seat for
tests; **D3** firecode's two asks are answered by mechanisms that already exist - cgroup membership is
use-dependent and the persistent shell is byobu's job; **D4** flowy's licence; **D5** Falsifier B's scoring
rubric, which has now been run; **D6** `max_inline_bytes` is a *decider* and not a number; **D7** the 27B
preset is derived by measurement; **D8** the harness is Apache-2.0. Later decisions (D10 cloud models, D11
`f_keep`'s denominator, D14-D17 the environment, the boundary and plugin trust) are in the same file and
matter more to the tree than these do.

### Looks open, is already resolved - do not re-litigate

Language (Rust daemon, Python suite, §16); the seam (two processes, private channel, shm, §3.3); whether
M1 needs C++ (no); steering policy (next step boundary, §5.8); flowy vs the firecode room (separate,
bridged, §12.4); cache-ladder sharing (independent policies, shared vocabulary, §10.5); block-sharing
granularity (two ladders at two tiers, §3.10-A); segment swapping in v1 (no - append, §19.3); ACP (an
adapter, not the native protocol, §13.4); reasoning replay (verbatim, §9.1); the compaction default (soft,
hard at 80% of `n_ctx`, §10.4); the system-update mode (user-turn envelope until an eval says otherwise,
§5.3); the policy table's initial contents (§11.3); and tool budgets and roles (§8.4).

## 5. Synchronisation points

Where strands must meet, what must be true when they do, and what breaks if it is not.

| # | meeting | must be true | if it is not |
|---|---|---|---|
| **S1** | W4 + W5, the first token appended | `render_incremental ≡ render` and `parse ∘ render ≡ id` pass as property tests (I3b), and the `/apply-template` diff is green for GLM (I3) | a renderer bug becomes a ledger that is internally consistent and wrong: the hash chain will agree with itself forever |
| **S2** | W5 + W6, the first submit | the region's lifecycle is agreed - who creates it, how it grows, what a restart rebuilds from (§4.3 clause 6 says replay the spans) | two owners of one region, or a restart that re-renders instead of replaying, which reintroduces the non-determinism I4 exists to catch |
| **S3** | W6 + W1/W2, M1's exit | the rig can **drive harnessd**, not only observe it | M1's exit criterion is unmeasurable and the milestone is declared on a feeling |
| **S4** | W7 + W12, the flowy head attaches | the snapshot handed to a late head is the **scrubbed** projection (§13.2b), and the read cursor advances over everything *read*, not everything kept | a decision is answered twice, or a filtering consumer rereads its own output forever |
| **S5** | W9 + W11, the first gated call | every schema carries `access` and every action derives exactly one class (§18.3's third property test) | everything falls through to a default nobody chose and the policy table becomes decorative |
| **S6** | W6 + W15, S0 lands | the submit path is behind a named module (§3.5's exit discipline) and both sides refuse a version mismatch loudly | S0 becomes a turn-engine rewrite instead of a backend swap, and the fallback path rots |
| **S7** | W13 + W5 + W14, compaction meets the ledger | segments are contiguous spans (L6), labelled at open (L5), with a stated close rule | §19's futures are foreclosed, and re-labelling 200 segments later means the stop-the-world pass §10 exists to abolish |
| **S8** | M6 subagents + admission | a concurrency ceiling exists as a configured number *before* concurrent dispatch | the box's first real concurrent client meets its own OOM path |
| **S9** | W4 + a model file update | `template_sha` is recorded per dialect and the diff re-runs on mismatch (§7.2) | the one hazard self-rendering introduces goes silent, which §20.2 names as the most dangerous risk in the plan |

## 6. Parallelism honesty

Where splitting the work costs more than it buys. **The token pipeline is one person's work end to end**,
because the parse side of a dialect is defined by its render side (`parse ∘ render ≡ id`) and the ledger's
correctness is a property of what the renderer emitted - two people either agree constantly or ship a
consistent-and-wrong pipeline; only the second and third dialects are genuinely separable. **EXPLAIN
cannot be built by an observability strand**, since only the person who built a stage knows which of its
numbers is a proxy and which is the fact: the type and printer are a strand, the fields are a tax every
other strand pays. **Compaction policy and the segment model are one design** (§10, §19.1, §19.2 are one
decision seen three times). **The flowy connector is one person's work**, since the level table, the
read-cursor rule, the `Skipped` disclosure and the broadcast clause interlock.

**Physical limits on this box**: with `--no-op-offload` the CPU is on the inference path, so a `-j 24`
build steals prefill and decode from production and the realistic ceiling is two or three strands at once,
niced or in a firecode run. And the parallel workers here are one operator with agents, not a team, which
makes the contracts *more* load-bearing: a human who finds `RenderSpan` undefined asks, and two agents
invent two.

## 7. What is still underspecified

Ranked by blast radius, restricted to what the tree has not settled:

1. **`ExplainPlan`'s field list** (§6.2). An example rendering is not a type, and every stage's counters
   are unread until it exists.
2. **The control-channel wire format** (§3.3): framing, encoding and the handshake, without which S0
   cannot start.
3. **Segment close rules** (§19.2-L5 gives open and label; nothing gives close), which S7 depends on.
4. **The 27B dense preset** (§7.3): derived by measurement (D7), not yet derived.
