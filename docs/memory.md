# Passive and active memory

Written 2026-09-09 from a distinction the operator drew, after a failure in the
session that produced most of this repo. It is a concept the plan gestures at —
`SegmentMark` exists for it, §19 names its futures — but never states.

Note on scope: **"harness" here means the whole system backend**, daemon and server
together. That is not incidental. The central claim below is that active memory
cannot be built on either side alone.

---

## 1. The distinction

**Passive memory is a notebook on the table.** You must know to look, and you must
know roughly what you are looking for. Retrieval is *pull*, and it depends on the
asker already suspecting the thing exists. A filesystem, a database, a transcript,
the session log in `crates/sessionlog`.

**Active memory is the a-ha.** Something from weeks ago arrives *unbidden* because it
is relevant to what you are doing now. Retrieval is *push*, keyed on the current
context rather than on a query.

Passive memory that contains the answer and never offers it has failed, even though
nothing was lost.

## 2. The worked example, from this repo's own session

The PFN/stroppy work — three measured ceilings, a trained model, an honest negative
on the classifier — was done in the same conversation that produced this harness. It
was lost across a compaction cycle.

Asked about it directly, the assistant said it was not in context and not in the
summary. It was recovered **only because the operator said it was in the transcript**.
Without that, the answer would have been "we did not do that here" — about work that
session had itself produced.

The transcript is 77 MB of append-only jsonl. Nothing was missing. Recovery required
guessing that the word `stroppy` would appear; `pfn` alone returns 1,579 hits of
noise. That is passive memory working exactly as designed and being useless anyway.

## 3. Why this needs the whole backend

This is the part that decides where it can be built.

| | has it | lacks it |
|---|---|---|
| **the server** | the KV blocks, the cache, eviction, what is resident | any notion of what is *relevant* to this turn |
| **the daemon** | the transcript, the segments, the task at hand, relevance | any way to place a block into a prompt without re-prefilling it |

Neither side can do this alone, and the interface between them in every existing
harness is a list of messages — which can express "here is more text" and cannot
express "compose in the block you already hold".

So active memory is a **fused-seam feature**, and a stronger argument for fusion than
the ones in §3.4: semantic eviction and pre-warming are optimisations, and this is a
capability that does not otherwise exist.

## 4. Surface it as a suggestion, not an injection

The obvious design — retrieve a block and splice it into the conversation — is
destructive, and its cost is what has kept active memory out of harnesses:
**injecting into history invalidates the prefix**, measured here at 144.6 s to
re-prefill 150k tokens against 0.8 s for a hit. At that price you would only fire it
when already confident it mattered, which is when you did not need the help.

**Do not inject. Append a suggestion to the user turn.**

New tokens at the end of the prompt leave the prefix untouched, so the cost is the
suggestion's own tokens and nothing else. And the suggestion should carry a **handle,
not content**:

```
[recall] earlier this session: three measured ceilings for stroppy,
         noop driver vs pg-noop vs PostgreSQL — segment 7f3a91
```

Twenty tokens. The model then decides whether to pull it, through a
`recall(segment_id)` tool.

Four properties follow, and three of them are things §5's trap otherwise demands
work to get:

- **Non-destructive.** The prefix is intact; nothing is rewritten.
- **Refusable.** A bad suggestion costs twenty tokens the model ignores. Injected
  content cannot be declined — it consumes context and anchors regardless. That is
  most of "a wrong recall is worse than silence" removed, and it means retrieval
  *precision* matters far less than it would otherwise.
- **Structurally distinguishable for free.** It rides with the user turn as metadata,
  so it cannot be mistaken for something the model itself concluded. §5's third
  requirement is satisfied by construction rather than by designing an envelope.
- **Model-directed.** We nominate; it selects.

### This decouples active memory from composable KV

Composition stops being a prerequisite and becomes an optimisation of the **pull**.
Today `recall` returns text and costs an ordinary tool result. Later it composes a
block and costs almost nothing. **UNVERIFIED-16 no longer gates the feature, only its
efficiency** — which is a much better place for a hard open question to sit.

### The cost, accepted deliberately

A suggestion appended at turn N is part of the prefix at turn N+1 and **cannot be
retracted** without invalidating everything after it. Suggestions therefore
accumulate — twenty tokens each, permanently.

The operator has accepted this. It is worth noting that it is also a useful forcing
function: the budget disciplines the policy, so "few and short" is enforced by
arithmetic rather than by a relevance threshold nobody can pick correctly. At ~20
tokens a suggestion, a hundred turns costs ~2k tokens of a 262k window.

## 5. The trap, already measured on this fleet

The oracle work found that **an embedder measures resemblance, not truth**: a bats
passage scored 0.762 against the correct answer at 0.471.

Falsifier B then measured that a long prior context **anchors** this model rather than
distracting it — pass rates 0.83 at zero depth against 1.00 at 60k, with the model
reasoning ~4× longer when given nothing. Anchoring is the mechanism that makes active
memory valuable, and it is the same mechanism that makes a wrong recall *worse than
silence*: it costs context and it biases, and the model has no way to know the
recalled material was surfaced rather than concluded.

Two consequences:

- **Structural association must carry at least as much weight as similarity.** Same
  file, same symbol, same row id, same error string, same commit — hard links, not
  fuzzy ones. Similarity ranks; structure qualifies.
- **Recalled material must be structurally distinguishable in the prompt** from the
  model's own prior reasoning. Otherwise a surfaced fragment reads as something the
  model already concluded, which is §8.2's abstention failure wearing a new costume:
  the system reporting as grounded something that merely resembles grounding.
  **§4's suggestion form gives this for free** — a handle riding with the user turn
  is not in the assistant's voice and never was.

## 6. What exists to build on

- **`SegmentMark`** (§4.2) renders to nothing and carries
  `{segment_id, label, kind, created_at, last_touched}`. §19 says it is in v1
  *"solely so that per-segment decay, user-assisted compaction and agentic memory
  have something to address."* That is this. The addressing scheme was reserved before
  there was a name for it.
- **bge-m3** is running on this box, idle, dim 1024, and pinned so its vectors are
  reproducible (`~/bin/bge-m3-ollama`).
- **`crates/tokencore`** already content-addresses: the ledger chains
  `h_k = H(h_{k-1} ‖ tokens(item_k))`, so every item already has a stable identity.

## 7. What is missing

Three things, none of which exist:

1. **A write-time association.** What a segment is *about*, not merely when it
   happened. No producer writes `SegmentMark` today.
2. **A turn-time surfacing policy** — what fires, how much, and a threshold. This is
   where over-eagerness pollutes context, which the operator has objected to
   explicitly. It is a policy, not a mechanism, and it wants measuring rather than
   choosing.
3. **A distinguishable envelope** for recalled material, per §5 above.

## 8. Status

A concept note, not a plan — but no longer a blocked one. §4's suggestion form means
this is buildable on what exists: it needs a `SegmentMark` producer, a retrieval
policy, and a `recall` tool, and none of those wait on UNVERIFIED-16.

What still depends on composable KV is only how much the *pull* costs. If partial
recompute cannot converge on this stack's recurrent layers, `recall` returns text
forever and active memory still works — it is simply priced as a tool call rather
than as a composition.
