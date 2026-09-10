# TODO

Work queue. **The first section is everything that can be finished without the
operator**; nothing in it waits on a decision, and each entry says how to check it
is still open before starting.

Settled work is in `TODO-settled.md` and is not repeated here.

## If you are an agent picking work from this file

1. **Take one item from READY. Nothing else.** Sections 2 and 3 are blocked on a
   human; starting one produces work that cannot land.
2. **Re-run the item's `still open?` check first.** Statuses go stale. On 2026-09-10
   three items in this file — T12, T20.4, T21.1/2 — were found already implemented
   while still filed as open, and would have been re-done from scratch.
3. **Verify with the item's `done when`, not by reading your own diff.** If a check
   needs a live model, `~/bin/model status` says what is serving; several test files
   require it and will otherwise pass vacuously.
4. **If you find it already done**, move it to `TODO-settled.md` with the evidence
   and stop. That is a complete, valuable outcome.
5. **If it turns out to need a decision**, move it to section 3 with the question
   stated. Do not guess the answer.

## Repo facts you will need

    cargo check --workspace          # clean as of 2026-09-10
    cargo test -p <crate>            # crates/: transcript turn dialect{,-glm,-qwen}
                                     #   tools harnessd sessionlog tui ui code
                                     #   backend tokencore
    python3 tests/fidelity/run_gate.py    # template fidelity gate, must print GATE PASS

Tests needing a live model server on 127.0.0.1:8080: `sessionlog/{live_e2e,late_head,
sessions,resume_frames}`, `harnessd/{loop_closes,wired}`, `turn/{live_qwen,
engine_decisions}`, `tools/{exec,background,confine}`.

---

# 1. READY — no operator needed

*Verified open by inspection on 2026-09-10; each says how to re-check.*


## R1 — `TranscriptItem::Assistant` has no `truncated` field

*(was T10.1)*

**Why it matters.** §5.7 and §5.8 both require it. It is currently tracked on the
turn record instead, and that is why **one piece of steering is unbuilt** — the only
T10 item that blocks a feature rather than costing elegance.

**Still open?** `sed -n '39,43p' crates/transcript/src/lib.rs` — the variant has
`text` and `tool_calls` and nothing else.

**Where.** `crates/transcript/src/lib.rs`, the `Assistant` variant. Adding a field to
a serialised enum: give it `#[serde(default, skip_serializing_if = ...)]` like
`tool_calls` has, so old rows still load.

**Done when.** The field exists, `cargo test -p letibot-transcript -p letibot-turn`
passes, previously-written session rows still deserialise, and the §5.8 steering that
wanted it is either built or filed as a follow-up naming what is left.

---

## R2 — `ParsedSpan` carries no token offsets

*(was T10.2)*

**Why it matters.** A `Parser` cannot say which ids an item owns. Working around that
cost an entire module — `crates/turn/src/items.rs`, 22 KB — which adding a span to
`ParsedSpan` would **delete**.

**Still open?** `ls -l crates/turn/src/items.rs` — still present.

**Where.** `crates/dialect` for the type; `crates/turn/src/items.rs` is what should
shrink or vanish.

**Done when.** `ParsedSpan` carries the offsets, `items.rs` is deleted or reduced to
what genuinely is not parser work, and `cargo test -p letibot-turn` passes including
`engine_decisions.rs`. Deleting the module is the point — if it survives intact, the
change did not pay and should be reported that way rather than merged.

---

## R3 — `DialectSpec` has no `ReasoningField`

*(was T10.3)*

**Why it matters.** A per-model fact sitting in config instead of in the crate that
exists to model per-model facts as data. The engine takes it as config and works; the
fact is in the wrong place.

**Still open?** `grep -rn 'ReasoningField' crates/dialect/src/` — absent from the spec.

**Done when.** The spec carries it, both dialects declare it, the engine reads it from
there rather than from config, and `cargo test -p letibot-turn -p letibot-dialect-glm`
passes.

---

## R4 — `cargo:rustc-link-arg` does not propagate across crates

*(was T10.5)*

**Why it matters.** Every crate linking `libllama` needs its own `build.rs` to bake
the rpath. Without it, test binaries **link fine and fail at exec** looking like a
missing library — a failure that reads as an environment problem. Fixed in
`crates/turn`; this is so the third crate does not rediscover it.

**Still open?** This is a documentation task; check `docs/` for an existing note.

**Done when.** It is written down where someone adding a crate will meet it — a note
in `docs/` and a comment in `crates/turn/build.rs` pointing at it. No code change.

---

## R5 — §18.1-I1's observable form is wrong in the plan text

*(was T11)*

**Why it matters.** Resolved in code; the prose still states an invariant that
**cannot pass** on this box, for a reason that is not a violation: Qwen3-Next is
hybrid/recurrent, so llama.cpp resumes from a context checkpoint and snaps `n_past`
back to it. Anyone checking the plan against reality concludes the harness is broken.

**Still open?** Read T11 below for the measurement; then check whether the plan text
has been corrected.

**Done when.** The plan states the invariant in a form checkable on a hybrid model,
and says why the naive form is not. Prose only.

---

## R6 — Verify what opencode actually sends

*(was T4)*

**Why it matters.** Decides whether the residual prompt-cache divergence after the
`interleaved` fix is one mechanism or two, and whether T3 affects opencode at all.
Filed as needing the W1 recording proxy, but **`~/bin/qwen-proxy` already sits in that
path** and can log — this does not have to wait for W1.

**Still open?** `docs/chat-templates.md` §3 lists the two mechanisms; the second is
marked unverified.

**Done when.** A capture of real opencode traffic shows whether reasoning arrives as
its own message or fused onto the assistant message, written into
`docs/chat-templates.md` §3 with the raw evidence kept.

**Care.** `qwen-proxy` is production for opencode. Do not restart it mid-session
without saying so; log alongside rather than replacing.

---


# 2. NEEDS A NOD — small question first, then unblocked

## N1 — Put content on `TranscriptAppended` (was T13.1)

**This is the structural one.** T14 settled that composable KV is impossible on hybrid
models and an addressable record is the fallback — and a log whose transcript events
carry no content **cannot be that record**. A head cannot reconstruct a conversation
from the log at all; the daemon reconciles out of band via `Hub::record_item`.

**Still open?** `grep -n 'TranscriptAppended' crates/harnessd/src/harness.rs` — the
comment at line 19 states the gap.

**Why it is not in section 1.** W8 kept §4.5's event exactly as specified and made the
gap explicit **rather than widening it unilaterally**, which was the right call: this
changes the head protocol. The question for the operator is one line — *does the event
carry the content, or a handle the head resolves?* — and after that it is code.

## N2 — `Dialect` split, rendering-as-data (T2), and structural eviction (T15)

Both arguments are finished and written down. Neither needs an answer so much as a
decision to spend the time; each is large enough that starting one is a scheduling
choice, not a task pickup.

---

# 3. BLOCKED ON THE OPERATOR — do not start

- **T5, T25** — 17 open decisions. Several gate everything else.
- **T22 and T17** — both wait on **D11**. "Until it is decided, M1 remains formally
  unexited."
- **T19** — rewrite-or-not is the decision itself.
- **T24** — needs **D3** (firecode's two asks).
- **T16** — several items explicitly want an operator or a strand owner.
- **T3** — the minja bug report is written and ready, but `llama.cpp/AGENTS.md` says
  reports and PR text are the operator's to write.
- **T6** — not started; `pytest` is still not installed, which `docs/workstreams.md`
  calls blocking for W1/W2.

---

# 4. Reference — the full text of everything above



## T5 — Operator decisions still open

Carried from `DECISIONS.md`; see there for the full statement of each.

- **D3** — firecode's two asks (parent cgroup, persistent shell). A written "no" is a
  complete answer and unblocks the work; it selects which of two tool runtimes gets
  built.
- **D5** — Falsifier B scoring rubric.
- **D6** — `max_inline_bytes` (deliberately has no default).
- **D7** — the 27B preset.
- **D8** — the harness licence. The workspace currently declares Apache-2.0 and there
  is no GitHub remote yet; the repo is intended to be public, so this should not stay
  open.

---

---

## T25 — Everything waiting on the operator

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

**D13 — `Access::Session`, a new access class.** Declaring the intent tools `Read` would
have reproduced the gap the survey names in grok-build (§1.4). The wire carries access as
a String so nothing breaks, but no head has rendered one.

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

**D15 — 13 stale worktrees and one 30-hour orphan tmux** (`nano_test`). Safe to prune the
finished ones; two belong to live agents, so not a blanket sweep. T24 is the mechanism,
this is the backlog it would have prevented.

**D16 — `~/bin/letibot` is not version-controlled**, and two real bugs were found in it
tonight (a `--continue` that showed an empty screen, and an `up()` that would have
orphaned a version-skewed daemon holding every session). It belongs in the repo.

**D17 — Group 2, subagents and admission.** Held all evening because it lands in
`crates/harnessd/src/sessions.rs`, which session-resume was rewriting. **Now unblocked.**
Its hard requirement is already written: `Fits{ceiling, cost_per_id, pool_free}`, because
436.7 MiB per sequence id charged at allocation makes N subagents an OOM path — and two of
the five surveyed harnesses ship subagents with no ceiling at all.

**D18 — LSP.** The one gap in the survey's ten that no group covers. Four of five
harnesses have it, the most complete is 14 actions, and it needs a language server per
language. Its own decision, not a group.

**D20 — `never_hit` now sees network arguments.** A `web_search` or `github` string that
merely *mentions* `.config/gh` or `.password-store` is denied by the never-write list.
Fail-closed, and a real false positive.

**D21 — Enforcement binds the model, or the seat?** From `docs/closed-loop.md` §9. They
differ when a human is driving, and the operator should not be locked out of their own
restart because the model cannot be trusted with it. Tonight's blocked
`systemctl --user restart` is the worked example.

**D22 — The tolerance band: one, or per-operation?** Also §9. How large a deviation is
corrected silently versus faulted to a human is the whole design decision, and both ends
have measured costs. **One data point now exists**: T21.3's nudge is capped at one per
user turn, chosen at its narrowest for want of a measurement rather than because one is
right.

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

**D26 — A wake spends a generation, and nothing bounds a chatty monitor.** T24's wake
turns a firing into a user item and runs a turn. A monitor watching a path that changes
every second would spend the box's slots on itself until its TTL expires. The TTL is the
only bound and it is up to an hour. A firing that should be *noted* rather than *acted
on* has no spelling — and inventing one is a decision about what a monitor is for, not a
fix.

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

**D28 — Text selection in the TUI, still undiagnosed and needing one answer from
the operator.** The repaint hypothesis is REFUTED by measurement: before/after
frames byte-identical over 1,256,038 bytes, and 30 s idle wrote **zero bytes**.
Two candidates remain and one question splits them: *can you select while it is
idle, between turns?* Yes -> the complaint is mid-turn and is content change under
the selection, not gratuitous repaint. No -> it is not this code at all, it is the
TUI holding SGR mouse mode so the terminal hands drags to the application instead
of doing native selection, and the fix is a mode toggle.

### Not ours, tracked because we caused or found them

**D23 — The 702 long chunks.** 702 of 43,967,653 exceed ~2048 tokens and already hold
ollama vectors of *truncated* text. Routing them to TEI makes new ≠ old for exactly those
documents. Small enough to re-embed rather than manage; lubuntu3's corpus, our capacity.

**D24 — The chunker length assertion.** lubuntu1's close, unowned: while chunks stay under
~2048 every backend agrees and none of the truncation split matters. Nobody enforces it.



---

## T22 — `f_keep` names two different quantities, and C4 applies one's threshold to the other

Found while answering "how is `f_keep` computed". The answer is: **two ways, and the
plan uses both under one name.**

### The two

`server-task.cpp:2603,2614` — a cache **selection** heuristic, with a hard floor
refusing any entry under `f_keep < 0.25`:

```
f_keep = lcp / cached_entry.size()      denominator = the CACHED entry
f_sim  = lcp / new_prompt.size()        denominator = the NEW prompt
```

`lcp` is the longest common prefix — how many leading tokens of the new prompt match
the cached entry, token for token, via `server_tokens::get_common_prefix`.

§18.2's **C4** defines it as `usage.prompt_tokens_details.cached_tokens /
prompt_tokens`. **That is `f_sim`, under `f_keep`'s name.**

### Why it matters — one turn of the M1 run

```
cached entry (turn N)     9,000 tokens
new prompt  (turn N+1)   10,093 tokens   ← appended a 1,093-token tool result
lcp                       9,000 tokens   ← the whole entry is a prefix; nothing rewritten
```

That is a **perfect** turn — full reuse, nothing recomputed.

| | formula | value |
|---|---|---|
| llama's `f_keep` | lcp / cached | **1.000** |
| llama's `f_sim` | lcp / new prompt | 0.892 |
| **C4's `f_keep`** | cached_tokens / prompt_tokens | **0.892** |

The gap is not a cache miss. **It is the tool result that was just appended.** C4's
number falls purely as a function of how much the conversation grew.

### The defect

C4 says it is *"directly comparable to the 0.000 → 0.999 measurement"*. **It is not.**
That 0.999 was read off the server's trace lines, so it was llama's `f_keep` —
denominator the cached entry, and therefore **indifferent to growth**. The plan even
notes at line 64 that the two are "a hit-ratio pair", then migrates a threshold from
one to the other.

So a bar measured on metric A is applied to metric B. On A, 0.99 is reachable and was
reached. On B it is arithmetically impossible for any session that returns tool
output.

### SETTLED 2026-09-09 — option 1, see D11

`f_keep = lcp / cached_entry`, computed as
`cached_tokens(N+1) / (prompt_tokens(N) + committed_generated(N))` — **no server change
needed**, since the denominator is what we left in the cache and the numerator is what
the server already returns. It is the ratio form of C3's inequality, over C3's own
quantities.

**M1 must be re-measured.** 0.8771 was a correct measurement of the wrong metric.

### The choice as it stood — operator's

1. **Restate C4 as `lcp / cached_entry`** — directly comparable to the 0.999 that
   motivated it, indifferent to growth, and what the append-only invariant actually
   claims. Needs the server's `lcp`, which is S1 or `-lv 4`.
2. **Keep `cached_tokens / prompt_tokens`** and set a bar reachable for tool-using
   sessions. The harness-only version of this is T17's C4b, which measured p10 1.0000.

Until it is decided, **M1 remains formally unexited** and the run stands as recorded.

This is the seventh instance in one day of a number whose meaning was not checked
before use, and unlike the others it is **ours** — it is in the plan, not in something
we inherited.

---



---

## T17 — harnessd — **BUILT 2026-09-09. The loop runs; M1's exit criterion is NOT met as written.**

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

**C3's shortfalls have an exact signature** — every one equalled `previous generation +
3`: the server reused to the end of the last committed item and discarded the 4-token
generation prompt *and* the whole generation. It is intermittent (14/55 on an earlier
run of the same script), so it is a server-side cache event, not structural. Settling it
needs `-lv 4` and `glm-why-no-cache` — a server-log question the harness cannot answer
from its own metrics.

---

---

## T19 — Rewrite or not, on the KV representation — **the angle UNVERIFIED-16 should be read from**

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

---



---

## T24 — The harness owns what a subagent leaves behind, and monitors need the same owner

Operator, 2026-09-09: *"you guys like to accumulate monitors and shells"* — and, on
being shown the measurement, *"you are fine, others nt"*. That correction is the
requirement. The leak is not the top-level agent's own housekeeping; it is **everything
its children spawn and do not clean up.**

Measured on this box the same evening, mid-session:

| | |
|---|---|
| git worktrees in `.claude/worktrees/` | **13**, most from agents that finished hours ago |
| orphan tmux sessions | `nano_test`, **30 hours** old, from a finished agent |
| the top-level agent's own leak | 1 listener, 1 poll loop — correct |

`docs/tui-testing.md` already says *"Kill your sessions. An orphaned tmux session holds a
`harnessd` and its socket, and the next run then attaches to a daemon it did not start."*
Written here, loaded in context, and it did not bind. That is `docs/closed-loop.md` §1
again and the fix is the same: not a better rule.

### The mechanism was already chosen

D4, answered by the operator earlier the same day: *"just do byobu, which connects to
cgroups — use dependent. if a vm is temporary then it is a session cgroup otherwise not.
still must be clearly reapable."*

So: **every process a turn spawns lands in a cgroup owned by a scope.** Three scopes and
no fourth — `turn`, `session`, and `explicit` (survives the session because somebody said
so, and is listed as such). Reaping is `cgroup.kill`, and a subagent's cgroup is a child
of its parent's, so a parent that ends reaps its children by construction rather than by
remembering to.

### The part that pays for itself immediately

**Cgroups retire pattern matching for both reaping and liveness**, which is T21.1 and
T21.2 dissolved rather than guarded:

- `pkill -f X` becomes "kill this cgroup" — no pattern, so nothing to self-match.
- `until pgrep -f X` becomes "is this cgroup non-empty" — no predicate that can match its
  own waiter.

The evidence that a guard is not enough: on 2026-09-09 a process check self-matched
**five times in one session**, with `process-checks-that-self-match` loaded in memory and
T21 open in this file. The fifth was `grep -E '[h]arnessd'` — the bracket trick defeats
`pgrep`, but the shell wrapper echoes the expanded pattern back into its own command line,
so the literal string was there to be found. A hazard with that many spellings is not
one you check for; it is one you make unspellable.

### Monitors, which are the same problem wearing a different hat

Operator, same message: *"btw we need monitors in letibot"*. A monitor is a condition
watched **between** turns that wakes the loop when it fires — the encoder running while
the model is not, in `docs/closed-loop.md`'s terms, and the only correct shape for
listening. The seat brief pays for that distinction: *a Stop hook fires when a session
goes idle, and a seat that is rate limited, has crashed, or never started is not running
a session, so no stop event ever fires and the silence looks exactly like a quiet room.*

A monitor is also, structurally, a long-lived process. **Adding monitors before the
lifetime work multiplies the leak this entry exists to stop**, so they land together:

1. **Scoped at creation.** No monitor without an owner; the default is the session.
2. **Listable and attributable.** The head can show what is watching and for whom, the
   way it now shows sessions. An invisible watcher is an unreapable one.
3. **Reports why it fired**, not just that it did.
4. **One waiter per name**, enforced. The fleet already learned this: two processes under
   one reader means the roster shows a seat attached while the real one hears nothing.
5. **Bounded.** A TTL or an explicit renewal, so a monitor whose reason has passed dies
   without anybody remembering it.

### What the fleet answered, 2026-09-09 — and what it ruled out

Asked in `Lab/#general` with the counts stated first so replies were comparable.

| box | worktrees | orphan tmux | listeners |
|---|---|---|---|
| **.79 (here)** | **13**, from finished subagents | 1, 30 h | 1 + 1, correct |
| .78 (lubuntu2) | 4, **deliberate** — four llama.cpp checkouts, 3 live | 1, **14 days** | 1 + 1, correct |
| .76 (lubuntu1) | 0 | 0 (the one present is the operator's login) | 1 Monitor + 1 unit |

**lubuntu1's reading is the finding, and it is better than the counts:**

> *"the leak scales with children spawned, not with seat uptime. .76 has been up as long
> as you and leaked nothing. Reaping belongs wherever subagents are created, not in a
> periodic sweep on each box."*

That **rules out a design this entry left open**: a per-box reaper on a timer. A sweeper
cannot tell debris from a deliberate long-lived resource, and it runs on boxes with
nothing to sweep while the box doing fan-out is the only one that needs it. Reaping is a
property of the *creation site*, which is what the parent/child cgroup shape already
gives — and it means the mechanism ships with subagents (S8/M6) rather than as fleet
housekeeping.

**lubuntu2 supplies the case a sweeper would get wrong**: four worktrees that are not a
leak at all — three are live llama.cpp variants under active comparison. A long-lived
resource with a declared owner is correct. That is the `explicit` scope above, and .78 is
the reason it must exist rather than be a convenience.

**And lubuntu1's caveat is the most useful sentence in the thread**, because it is about
how the measurement lies:

> *"I killed a stray llama-server earlier tonight after a benchmark, and I only noticed
> because I went looking. Had I not, it would be in this count. My zero is partly
> attention, not only design."*

A zero produced by vigilance and a zero produced by a mechanism are the same number and
different facts — `docs/closed-loop.md` §3 exactly, one layer up. So the acceptance test
for this work is **not** "the counts are low". It is that the counts stay low when nobody
is watching, which means the reaper has to be observable: a scope that ended must record
what it killed, or its zero is unfalsifiable too.

### Blocks on

Nothing. This is substrate for M6 subagents (S8) and it is **cheaper to build before them
than after**: the leak measured above came from subagents the harness does not yet have,
run by an agent that does. The requirement was discovered before the feature, which is the
rare ordering.

### Landed, 2026-09-10 — monitors and background promotion

`crates/tools/src/exec/monitor.rs`, `crates/tools/src/builtins/monitor.rs`,
`crates/tools/tests/background.rs`. All five requirements above are mechanised and each
has a test. What is worth recording is the three decisions that were **not** obvious.

**The promotion threshold is 15 s and it is reactive.** Measured, warm, on `.79` at load
1.4: the whole routine set — `find`, `grep -rn`, `cargo check -p`, `cargo check
--workspace`, `cargo clippy --workspace --all-targets`, `cargo test -p letibot-tools`, the
fidelity gate — tops out at **6.5 s**. 15 s clears the slowest by 2.3×, and is 6× under the
`sleep 90` the operator named as the case that must promote. The previous default was 120 s,
which would have let `sleep 90` block the turn for a minute and a half and never promote.
The threshold fires on **elapsed time and never on the command text**: `cargo build` is 3 s
warm and 4 minutes cold and the string is the same both times, so a predictive rule is
`docs/closed-loop.md`'s open-loop stepper in a new costume.

**`m2_runner` is nine tools, one over §8.4's ceiling, and it says so in its own
`max_tools`.** The ceiling's evidence is about confusion between *similar* choices, so the
monitor surface was cut twice before it was allowed to cost a seat: declare/renew/retire are
one tool taking an `action` because all three act on one named handle, and **listing is not a
tool at all** — monitors are in `job_list` beside the jobs, the scopes, the promotions and
the reap log, because "what is running and what is watching" is one question and a second
listing nobody opens is how a watcher becomes invisible without anybody hiding it. What is
left cannot fold into `job_wait`: that blocks *inside* the turn, and a flag switching between
the two would make "I believed I had waited" spellable. A ceiling quietly raised for
everybody is not a ceiling; one role declaring its own number with the trade written down is
a decision somebody can reverse, and a test pins both numbers.

> **2026-09-10: the role is seatable, and it seats eight of the nine.** `--role runner`
> over `HostBackend::confined`. `bash` needs `--bash` on top, because
> `docs/boundary-and-adjudication.md` §5's transcript choke point does not exist: the
> boundary keeps secret bytes out of the process's **view** — `~/.ssh` is *absent* from
> the mount namespace, not denied — and nothing yet stops a tool result carrying bytes
> from inside that view into the transcript. `bash` is the tool whose result is an
> arbitrary byte stream; the job verbs and `monitor` are shaped by the tool. So the
> common case is eight tools, which is `DEFAULT_MAX_TOOLS` exactly, and the ninth seat is
> spent only when somebody asks for it. That is a nicer answer than the entry above
> expected and it is a **consequence**, not a plan: the ceiling was not what kept `bash`
> off.
>
> Measured on `.79`: `HostBackend::confined` builds — delegated cgroup v2 subtree and
> `bwrap` both present — so the runner seat is real here rather than a constructor that
> always errors. A box without either gets a refusal naming which half is missing, and
> never a silent fall back to `HostBackend::executable`, which buys cgroup lifetime and
> no view at all.

**A port watch has no `host` argument, and that omission is load-bearing.** A monitor that
could reach an arbitrary address would have to declare `Access::Network` on *every* call,
including the ones watching a cgroup — which is D12's shape (a network declaration making an
unrelated capability refuse entirely). Loopback-only keeps the whole tool at `Access::Exec`,
which is also the honest class: it leaves something watching after the turn ends.

### Still open — the head cannot promote a job yet

Requirement 3 of the operator's three (*"the operator promotes it, mid-flight, from the
head"*) is **half done**: the daemon-side verb is `ProcessHost::promote`, it is tested with
`Backgrounding::Operator`, and the frame the head must send is specified verbatim in that
method's doc comment — `ClientFrame::PromoteJob { client_request_id, expected_seq, job,
identity }`, queued, idempotent, answered by calling `promote`.

It is **not wired**, deliberately, and the reason belongs with D11. A new `ClientFrame`
variant is a wire change, so `PROTOCOL_VERSION` goes 5 → 6; both sides refuse a mismatch by
name; and landing a bump costs the running daemon its live session. Against that, the verb
buys nothing in production today: `harnessd` seats `m1_orchestrator`, which has no `bash`,
no jobs, and therefore no job to promote. The constant is also the one line every
wire-touching branch edits, and 5 was taken deliberately *"to coordinate rather than race"*
(D10). So: one variant and one integer, whenever a head is ready to send it.

### ~~Still open — nothing calls the monitor wake seam~~ — **CLOSED 2026-09-10**

`Monitors::wait_for_any` now has a caller, and the interesting part is what had to be
added before it could have one.

**`Bell::ring` was not the door.** `Registry::next_work` skips a session whose command
queue is empty, so ringing the existing bell for a firing would have been a no-op that
*read* like a wake — worse than the poll it was replacing, because the poll was honest.
So `Bell::ring_wake` and `Work::Woken` are new: a third queue, drained **last**, because
a head that pressed enter is waiting and a monitor is not. A third `Work` variant rather
than a synthetic `Command`, because a command has an issuing head, an identity and a
`client_request_id`, and inventing three of those would put a head's name on something no
head did.

**Two cursors, and they answer different questions.** The waiter thread keeps *"have I
rung for this?"*; the harness and its steering source share *"has the model seen this?"*.
That is what makes a firing arrive exactly once whether it is picked up mid-turn (through
steering, at the next step boundary) or between turns (through the wake). A wake that
raced a mid-turn pickup returns `Outcome::Ignored` rather than running a turn about
something already delivered.

**No monitors, no thread**, the same rule `Monitors`' own poller keeps and the same rule
this entry exists to enforce: the waiter is armed after a turn in which something is
actually watching, one per session, tracked so a second cannot start. Its `wait_for_any`
deadline is **not** delivery — the condvar is — it is the only thing a thread blocked in
that condvar can do about `Bell::close`, which notifies a different one. Named here
because "no timer, no poll loop" (§18.1-I12) is a property this daemon states about
itself, and a re-check that went undescribed would read as a violation of it.

What is **not** closed: the wake runs a full turn with the firing as a user item. A
session with a chatty monitor therefore spends generations on it, and nothing bounds that
except the monitor's own TTL. A firing that should be noted and not acted on has no
spelling yet.

### Still open — the disclosure can say `POLL ONLY` and the operator cannot tell why

`GateWiring::monitor_wake` is stamped by `Sessions::arm_wake`, so a `Harness` driven
directly by a test or by `letibot-m1` honestly reports `false`. What it does not
distinguish is *"no daemon armed one"* from *"the thread failed to start"* — the second
publishes a `Warning`, the first is silent, and both render the same line. Small, and the
kind of thing that costs an hour once.

---

---

## T16 — W9's open questions — **several want an operator or a strand owner, not me**

From the tool runtime, in descending order of consequence.

1. **§8.1 clause 3 gives the outcome vocabulary but never the mapping.** Which outcome
   is an empty `grep`? W9 decided: matches in scope or elsewhere ⇒ `Ok`; nothing
   anywhere under any relaxation ⇒ `Abstained`; a nonexistent path ⇒ `Failed`. That
   choice determines how often `propagate()` blocks a caller and should be recorded
   rather than inherited.
2. **D6 answered means M1 ships with clause 5 switched off.** `NoBudget` is correct per
   D6 — unset is a genuine no-op — so **nothing spills until a budget is configured**.
   A session-config gap, not a code one, and invisible unless said.
3. **`Gate` will collide with W11.** W9 defined the smallest adjudication seam the
   runtime needs (`admit(name, access, args)`). W11 should absorb it rather than build
   a parallel one.
4. **§8.1 clause 1 and §9.4 pull against each other.** Relaxing a pattern *is*
   rewriting the query, which §9.4 forbids. The reconciling word is **visible**: every
   relaxation appears in the result. Worth stating in §9.4 so nobody deletes one of
   the two.
5. **§8.4's `orchestrator` role has no `read_spill`** though its `read`/`grep` can
   spill — so for that role the omission notice is advisory, which §8.3 says it must
   not be. W9 shipped `read_spill` in its place.
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
7. **Call ids are positional per turn** (`call_0`; GLM carries none), so an id-derived
   mark repeats across turns. Fine now, wrong once a head correlates across a session.

---

---

## T3 — Report the minja scoping bug upstream — **ready, not filed**

`{% set %}` inside `{% for %}` is not scoped per iteration in llama.cpp's minja.
Repro needs no GPU: a template, four messages, one `/apply-template` call.
Full writeup and both engines' output in `docs/chat-templates.md` §2.

Affects any llama.cpp user serving GLM with `messages` — the model is conditioned on
reasoning it never produced. We are insulated because we submit token ids.

---

---

## T6 — Not started, from `docs/workstreams.md`

W1 measurement rig · W2 prefix-invariant suite (grok port, Apache-2.0) · W6 turn
engine (**critical path**) · W7 session log and head protocol · W8 heads · W9 tool
runtime · W10 firecode substrate · W11 adjudication · W12 flowy connector · W13
segments and compaction · W14 EXPLAIN · W15 server track · W16 experiments.

`pytest` is still not installed; `docs/workstreams.md` calls that blocking for W1/W2.

---

---

## T2 — Revise the `Dialect` contract — **UNBLOCKED by T1, re-scored**

Under the template-driven design, four of the eight defects stop existing or move.
Re-scored by the T1 experiment:

| # | defect | fate under T1 |
|---|---|---|
| 1 | `render_incremental` cannot be pure | **gone** — Jinja has no incremental mode, so there is no boundary state to carry |
| 3 | no home for the generation prompt | **gone** — it is a template argument |
| 5 | resolution must key on literal not role | **gone** — rendering no longer asks for roles |
| 4 | `ControlRole` closed and too small | **moves** to `parse`'s problem |
| 7 | `ControlTokens` forces `&'static str` | **now mandatory** — a runtime-loaded template makes `&'static` impossible |
| 2 | `parse(&[u32])` unimplementable | survives, `parse` side |
| 6 | `stop_tokens()` bare literals | survives, `parse` side |
| 8 | `ControlRole` has no `Ord` | survives, trivial |

So `Dialect` splits: **rendering becomes data** (template source, control-token set,
quirks) and **parsing stays code**. The three survivors are all on the parsing side,
which T1 does not touch.

**One thing gets harder and needs deciding.** The `server-bug-compatible` profile
cannot be produced by a template-driven renderer without deliberately reintroducing
minja's bug, and minijinja has no knob for it. My call: **drop it.** Its purpose was
to prove we understood the template well enough to reproduce minja exactly, which
earned the `faithful` profile the right to declare divergences. Running the real
template through a correct engine is stronger evidence than reproducing a wrong one,
and the INTEROP phase against `/apply-template` still reports how the server differs.

1. **`render_incremental(prev_end, new_items)` cannot be a pure function.** GLM needs
   boundary state — turn open, `<think>` open, previous item a tool result — none of
   it derivable from a `usize`. Worked around with `GlmDialect::for_conversation`;
   `new()` panics rather than guessing. The signature must carry history or a state
   token.
2. **`parse(&[u32])` is unimplementable as specified** — no vocab, yet it must return
   `Content(String)`. Currently takes a caller-supplied `TokenDecoder`.
3. **No home for the generation prompt.** Folding `<|assistant|><think>` into
   `render` breaks the stated invariant for any conversation whose next item is a
   user message. Currently an inherent `generation_prompt()`.
4. **`ControlRole` is closed and too small** — no `<arg_key>`, `<arg_value>`,
   `<sop>`, or the image triplet, all single vocab entries, so all must be `Control`.
   They currently sit under `TurnEnd` as a "no role" bucket.
5. **Resolution must key on literal, not role.** One role with many literals is the
   case that bites, and `ControlTokens::get(role)` silently returns whichever comes
   first. `GLM_TOKENS` is ordered so the canonical entry wins — a convention, not a
   guarantee.
6. **`stop_tokens()` returns bare `&'static str`** with no role, so a failure cannot
   be reported honestly. Correctness argument: a stop token that is silently a
   *sequence* never fires, and the turn runs to `n_ctx`.
7. **`ControlTokens` as `&'static [ControlToken]`** forces `&'static str` through
   every error type. Fine while dialects are compile-time constants; impossible for
   a dialect loaded from a config file or a downloaded template. `Cow<'static, str>`
   costs nothing today.
8. **`ControlRole` has no `Ord`**, so deterministic error listings must preserve
   declaration order rather than sort. Trivial; a derive would do.

---

---

## T15 — Replace §10's compaction design with structural eviction — **design written, not scheduled**

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

---

---

## T13 — §4.5's event enum is insufficient for a real head — **five gaps; gap 1 is a priority, see T14**

Found by building one, then by fixing T12. Listed with what was done about each.

**Gap 1 is not a footnote.** T14 argues that an addressable record is the fallback if
composable KV proves impossible on hybrid models — and a log whose transcript events
carry no content cannot be that record. Treat it as blocking for the "sessions that
do not forget" goal, not as a schema nicety.

1. **`TranscriptAppended{item_id, kind, ledger_head}` carries no content**, and
   `EventSink` has no channel for it — so **a head cannot reconstruct a conversation
   from the log at all**. The daemon must reconcile items into the view out of band
   (`Hub::record_item`). W8 kept the event exactly as specified and made the gap
   explicit rather than widening it unilaterally. This is the structural one and it
   needs a decision: either the event carries content, or the log is formally not a
   sufficient record of a session.
2. **`TurnFinished{usage, timings}` is lossy against `turn_metrics`.** Not carried:
   `prefix_check`, `id_slot`, `n_busy_slots`, `cost`, `dialect_template_sha` — and
   §18.2 says **`id_slot` is exactly what distinguishes a scheduling fact from a
   prefix divergence**, so a head cannot today tell those apart. W8 widened `usage`
   to carry `cached_tokens` on the argument that a harness whose point is cache reuse
   must be able to display it.
3. **No event announces who issued a command**, which §13.2 requires twice. Added as
   `CommandIssued`, labelled as an addition rather than smuggled into `Warning`.
4. `ToolStarted` / `ToolProgress` are given by name only in §4.5; their shapes are
   W8's invention and **W9 will find out whether they are right**.
5. **`DeltaTarget` is `Text | Reasoning` — there is no channel for a tool call.**
   Found while fixing T12. A tool call's argument text streams as `Text` while
   `items::produce` puts it in `ToolCall{arguments}` and excludes it from the
   Assistant row, so T12's new per-channel equality assertion **would fail on a
   tool-calling turn — correctly**, as a real live/stored disagreement rather than a
   test defect. No test exercises it yet because `live_e2e`'s prompt makes no calls.
   This is T12's defect, unfixed, in a third channel. The enum was not widened
   unilaterally.

---

---

## T10 — Contract gaps found by W6 — **small, concrete, one blocks a §5.8 feature**

The turn engine is the first real consumer of the crates below, and it found five
things. Listed in the order I would fix them.

1. **`TranscriptItem::Assistant` has no `truncated` field**, and both §5.7 and §5.8
   require one. Currently tracked on the turn record instead, which is why **one
   piece of steering is unbuilt**. Needs a `transcript` crate change — the only item
   here that blocks a feature rather than costing elegance.
2. **`ParsedSpan` carries no token offsets**, so a `Parser` cannot say which ids an
   item owns. Cost an entire module (`crates/turn/src/items.rs`) to work around
   without reimplementing the parser. Adding a span to `ParsedSpan` would delete it.
3. **`DialectSpec` has no `ReasoningField`** — a per-model fact of exactly the kind
   that crate exists to model as data. The engine takes it as config rather than
   guessing, which works but puts a model fact in the wrong place.
4. **`render_incremental(history, new)` replays the whole history** for boundary
   state, so building one ledger row per item is O(n²) replays. Microseconds today
   at our sizes; a resumable state token fixes it. Note T1 may delete this entirely
   when the template-driven renderer lands.
5. **`cargo:rustc-link-arg` does not propagate across crates**, so every crate that
   links `libllama` needs its own `build.rs` to bake the rpath. Without it, test
   binaries link fine and fail at exec looking like a missing library. Fixed in
   `crates/turn`; worth a note so the third crate does not rediscover it.

---

---

## T11 — §18.1-I1's observable form is not checkable on a hybrid model — **resolved in code, plan text still wrong**

§18.1-I1 states the prefix invariant observably as
`cached_tokens(N+1) >= prompt_tokens(N) + predicted_tokens(N)`. On this box it
**cannot pass**, and not because the invariant is violated.

Qwen3-Next is hybrid/recurrent, so llama.cpp resumes from a **context checkpoint**
and snaps `n_past` back to it (`server-context.cpp:5910`). Measured against prompts
*proven* identical over the shared span: reuse 48 of 52, and 38 of 44. The server is
reusing less than it could, correctly, for reasons of its own memory model.

W6's resolution, which I agree with: make the **exact** form the assertion — hash
turn N's prompt plus its committed generation, re-hash that span of N+1's prompt —
and demote the observable number to a *measurement of the server*, with a warning
that says which it is.

**Second defect in the same sentence:** `predicted_tokens(N)` is the wrong term for
a harness that owns its turn boundaries. A trailing stop token is stripped before
commit — keeping GLM's emitted `<|user|>` would put a second one in the next prompt —
so the witness must record **committed** generated tokens. With `predicted`, every
turn warns by one, forever.

Both need fixing in `docs/implementation-plan.md` §18.1. The code is already right.

---



---

## T4 — Verify what opencode actually sends — **cheap, decides whether T3 affects it**

`docs/chat-templates.md` §3 lists two ways a client causes prompt-cache divergence.
The second — sending reasoning as its own message rather than fused onto the
assistant message — applies only if opencode does that. Unverified.

The W1 recording proxy answers it directly. Decides whether the residual
divergence after the `interleaved` fix is one mechanism or two.

---

---

## T18 — The prompt cache costs ~120 KiB/token on recurrent models — **operational, caused a fifth OOM**

Found while running UNVERIFIED-16, which **OOM-killed `qwen-flash-next` at 14:35 on
2026-09-09** (158.1 GiB peak; systemd restarted it, `qwen-slots-restore` put the slots
back, ~40 s down, in-flight requests from two other agents lost).

Cause, and it generalises: **every distinct prompt prefilled becomes a prompt-cache
entry at ~120 KiB/token, because 111 MiB of each entry is the recurrent accumulator** —
fixed per sequence, independent of length. A sweep over many short distinct prompts is
therefore far more expensive than its token count suggests. `server-context.cpp:4549`
records four previous occurrences; this was the fifth.

Mitigation used, and worth generalising: the sweep harness refuses to start a condition
below 40 GiB `MemAvailable`. Any future experiment that prefills many distinct prompts
needs the same guard.

---

---

## T20 — Where the assembled parts did not fit — **integration findings, several serious**

1. **Double envelope.** `ToolRuntime::transcript_item` renders §8.2's `NO_RESULT`
   envelope into `payload`, and **both** dialect renderers wrapped it again — the model
   received two. Fixed: renderers emit `payload` verbatim.
2. **`Registry::tools_json()` is not prompt-ready** and this one was nearly invisible.
   It emits `,`/`:`; both templates use HF `tojson`'s `, `/`: `. Using it directly for
   `StablePrefix::tools_json` differs in **the first bytes of the stable prefix** — a
   cold prefill every single turn, with nothing in the tree to catch it. Now routed
   through the dialect's own tool-JSON function, with a test asserting the two differ.
3. **C10 was unmeasurable as specified** — `prompt_tokens − cached_tokens` from
   `TurnMetrics` alone is a tautology. Needed the server's own frame numbers.
4. ~~**`ToolRuntime` is not `Send`** — `Gate` lacks `Send + Sync`~~ — **FIXED, verified
   2026-09-10:** `crates/tools/src/runtime.rs:275` reads `pub trait Gate: Send + Sync`.
5. **`Session::append_items` renders one item at a time**, so Qwen's consecutive tool
   results cannot merge into one user turn without a fourth trait method. Declared as a
   gate divergence.
6. **Qwen's `SystemUpdateMode` is `Envelope`, not `InHistory`** — its template *raises*
   on a mid-history system message, so §5.3's conservative default is the only
   renderable form. The model can and did argue with it.
7. **`ask_code`/`ask_corpus` return `NotRun`, not `Abstained`** — nothing ran, which is
   a different fact. The daemon's startup disclosure says so.
8. The `/apply-template` oracle caught a renderer bug **no unit test would have**: an
   `Assistant` following a `Reasoning` opened a second message.

---

---

