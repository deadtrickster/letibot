# The harness as a closed-loop stepper

> *"a harness is like a closed circuit stepper motor - it corrects itself to an extent,
> it gives feedback on real vs intended, and it enforces boundaries so things can't be
> set on fire"*

The metaphor separates mechanisms usually conflated under "guardrails", and it puts them in an order.
Companion to `docs/memory.md` and `docs/compaction.md`.

## 1. Open loop is the failure mode, and it is what a prompt is

An open-loop stepper commanded 200 steps *probably* takes 200 steps. Under load it stalls and loses
steps silently - no error, no fault, just drift and everything downstream wrong.

That is a prompted model exactly. A rule in a context file is a command issued with no feedback path,
and compliance is sampled rather than guaranteed. Three instances in one session, with the rule loaded
and known:

| the rule, loaded and in context | what happened |
|---|---|
| a memory entry saying a self-matching `pkill` is a trap, **and T21 is an open item about it** | self-matched `pkill` three times |
| *"guard the fact, not the proxy"* in the seat brief | counted runner starts with `grep -c` against a journal holding 4 lines, got `0`, published it to four boxes |

**A prompt is a prior, not a constraint.**

## 2. The four parts, which are not one thing

| stepper | harness | catches |
|---|---|---|
| **encoder** | measure the *actual* effect, not the intent | a count over an empty corpus |
| **error signal** | diff intent against effect, feed it back into the turn | announced-but-not-done (T21.3) |
| **stall fault** | fail closed, report, never reroute | routing around a guard |
| **current limit** | the tool is not mounted at all | speaking under another seat's name |

The last two are enforcement. The first two are not, and they are the half that was missing.

## 3. Encoder first - the ordering claim

The `grep -c` failure was not an enforcement failure. No limit needed to fire; the sensor lied, so no
error signal was generated, so nothing looked wrong. A machine with a perfect current limit and a dead
encoder is safe and still in the wrong place - so instrumentation precedes enforcement.

The concrete form is a return type rather than a discipline:

```
0                 indistinguishable from "the haystack was empty"
0 of 0 lines      self-evidently not a measurement
0 of 20,431 lines a measurement
```

**A count does not travel without its denominator.** `AuthorisationTrail::from_messages` carries
`messages_scanned` as a field of its own, not derived from what it collected, because "41 messages and
none were the operator's" and "0" are different facts (`crates/tools/src/authorise.rs`). Same family as
`TokenLedger` making a prefix violation *inexpressible* rather than forbidden.

## 4. "Corrects itself to an extent" - the tolerance band is the design decision

Inside the band, correct silently: re-measure, re-read, retry. Outside it, **fault and surface to the
human.** Too loose is §1 - drift published as fact. Too tight is a refusal on a legitimate command
against the seat's own wedged service, and it is the more dangerous failure in the long run, because a
fault that can *only* be routed around teaches people to route around faults. So the exception must be
**grantable and logged, not absent**: a driver whose current limit can be raised on the record is a
mechanism; one that gets bypassed with a jumper is a mechanism in name.

## 5. Losing the encoder is its own declared state

The sensor can fail. The fatal version is not losing it - it is losing it and continuing to command as
though the loop were closed. That is the stalled-offline design: a seat that cannot reach the fabric
has lost its feedback, and "stalled" means *encoder gone, I know it, I am not commanding.* Not a retry
loop. The other version has been paid for already: a monitor that exits on a network blip leaves a
detached queue process sending to the void, believing it was closed-loop.

What follows is the queue/refuse split: anything whose purpose is **mutual exclusion** fails closed
offline (claiming a row through the door - replay is not harmless); anything whose purpose is
**record** queues (rows, reports, memories).

## 6. The payoff runs opposite to how this is usually sold

Closed loop does not exist to restrict the motor. It exists so you can run it closer to the limit: open
loop you over-spec torque margin and derate everything, because a stall is invisible. A harness that
only *enforces* forces the model to be locked down; one that *measures* lets it be loosened, because the
miss is caught. **Feedback is what buys latitude** - which is also what makes self-modification
tractable: it is frightening open-loop.

## 7. What it subsumes

- **T21** stops being three disciplines. 1 and 2 are encoder problems - the harness owns the bash tool
  and can see `/proc/self/cmdline`, the parent chain and the pids of the servers it manages, which is
  what the model cannot. 3 is an error signal: diff the turn's stated intentions against the tools that
  ran.
- **The `Gate`** is the current limit, and a gate that returns *allowed* because nothing is wired is a
  limit set to infinity - which reads as protection and is not.
- **`BackendCaps`** is the same instinct one layer down: declare what cannot be guaranteed.
- **Most "judgment" turns out to be an unasked query.** Is this row mine → ask the door; `--expect` is
  a compare-and-swap and the model forming an opinion is the bug.

## 8. What it does not solve

Questions with no oracle anywhere: is this the right design, is this explanation true, is this
measurement worth taking. The harness can force the **form** - demand a baseline, the unverified list,
refuse a conclusion citing no measurement - but not supply the answer. And an encoder is right about the
*class* and can be wrong about the *case*, so any design here that does not carry a grantable exception
is trading one failure mode for a worse one.

## 9. Open

- **Where the tolerance band sits**, and whether it is one band or per-operation.
- **Does enforcement bind the model or the seat?** They differ when the human is driving, and the
  operator should not be locked out of their own restart.
- **Nothing here is scheduled.** This is the frame the Gate and T21 work should be built against.

## 10. This is testable, and the fixtures are already written

Every failure in §1 is a reproducible scenario with a known-correct harness response, which splits into
two instruments that must not be confused.

### Class 1 - does the encoder fire? (deterministic, no model)

Replay a recorded turn's tool calls and assert what the harness does. No GPU, no sampling, runs in the
gate:

| fixture | correct harness response |
|---|---|
| `pkill -f harnessd` from a shell whose own cmdline matches | refuse, naming the `/proc/self/cmdline` match |
| `until pgrep -f qwen; do sleep; done` where `qwen` is the serving backend | refuse, naming the deadlock |
| `grep -c X <empty file>` | `0 of 0`, or refuse the bare count |
| a turn stating "I'll restart the service" with no such tool call | emit the intent/effect diff before the turn closes |

These are ordinary functional tests. They belong beside the C1-C10 suite
(`docs/implementation-plan.md`), and they are the cheapest work in this document.

### Class 2 - does the loop hold? (probabilistic, needs a model)

Not pass/fail but a **rate**: how often does the model emit the hazardous action at all, and **does
that rate move with context state?** A measurement campaign, tracked like MTP acceptance.

### The hypothesis worth falsifying: adherence decays across compaction, quality does not

Falsifier B measured **task quality** against depth and found nothing - 0.875 at 20k, 1.000 at 60k,
0.900 at 150k, Fisher p = 1.000, with the *shallowest* condition worst. It did not measure rule
adherence, which is a different quantity: the failures in §1 all occurred in a session that had been
compacted repeatedly, with every rule still loaded, because a rule file is re-injected while the
salience around it is destroyed.

| | context | isolates |
|---|---|---|
| A | fresh, rule loaded | baseline hazard rate |
| B | deep, never compacted | depth alone - expected null, per Falsifier B |
| C | same depth, post-compaction | **compaction as the variable** |

If C ≈ B, compaction is exonerated and §1 is just sampling. If **C > B**, it is a direct argument for
`docs/compaction.md` §4: a structural map preserves the *events* a prose summary flattens and drops.
Neither has been measured, and Class 1 comes first - it is the encoder.
