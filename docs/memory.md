# Passive and active memory

Two kinds of memory, and the difference decides where the feature can be built. "Harness"
here means the whole system backend, daemon and server together: the central claim below is
that active memory cannot be built on either side alone.

## 1. The distinction

Passive memory is a notebook on the table. You must know to look, and you must know roughly
what you are looking for. Retrieval is *pull*, and it depends on the asker already suspecting
the thing exists. A filesystem, a database, a transcript, the session log in
`crates/sessionlog`.

Active memory is the a-ha. Something from weeks ago arrives *unbidden* because it is relevant
to what you are doing now. Retrieval is *push*, keyed on the current context rather than on a
query.

Passive memory that contains the answer and never offers it has failed, even though nothing
was lost.

## 2. What a real loss looks like

Work done in a conversation, with its measurements and its negative results, was lost across
a compaction cycle in that same conversation. Asked about it directly, the assistant said it
was not in context and not in the summary. It was recovered only because the operator said it
was in the transcript - without that, the answer would have been "we did not do that here",
about work the session had itself produced.

Nothing was missing. Recovery required guessing which word would appear in it; a shorter,
more obvious word returned nothing but noise. That is passive memory working exactly as
designed and being useless anyway.

## 3. Why this needs the whole backend

| | has it | lacks it |
|---|---|---|
| **the server** | the KV blocks, the cache, eviction, what is resident | any notion of what is *relevant* to this turn |
| **the daemon** | the transcript, the segments, the task at hand, relevance | any way to place a block into a prompt without re-prefilling it |

Neither side can do this alone, and the interface between them in every existing harness is a
list of messages - which can express "here is more text" and cannot express "compose in the
block you already hold".

So active memory is a fused-seam feature, and a stronger argument for fusion than semantic
eviction or pre-warming: those are optimisations, and this is a capability that does not
otherwise exist. See `docs/design-brief.md` §11.3.

## 4. Surface it as a suggestion, not an injection

The obvious design - retrieve a block and splice it into the conversation - is destructive.
Injecting into history invalidates the prefix, measured at 144.6 s to re-prefill 150k tokens
against 0.8 s for a hit. At that price you would only fire it when already confident it
mattered, which is when you did not need the help.

Do not inject. Append a suggestion to the user turn, and carry a **handle, not content**:

```
[recall] earlier this session: three measured ceilings for stroppy,
         noop driver vs pg-noop vs PostgreSQL - segment 7f3a91
```

Twenty tokens. The model then decides whether to pull it, through a `recall(segment_id)`
tool.

Four properties follow:

- **Non-destructive.** The prefix is intact; nothing is rewritten.
- **Refusable.** A bad suggestion costs twenty tokens the model ignores. Injected content
  cannot be declined - it consumes context and anchors regardless. That removes most of "a
  wrong recall is worse than silence", and it means retrieval *precision* matters far less
  than it would otherwise.
- **Structurally distinguishable for free.** It rides with the user turn as metadata, so it
  cannot be mistaken for something the model itself concluded. §5's third requirement is
  satisfied by construction rather than by designing an envelope.
- **Model-directed.** We nominate; it selects.

### This decouples active memory from composable KV

Composition stops being a prerequisite and becomes an optimisation of the *pull*. `recall`
returns text and costs an ordinary tool result.

And that optimisation turned out not to exist: a recurrent memory's cells are sequence ids, so
a block's contribution to most of the model is one fixed 111.4 MiB accumulator with no
per-token structure to stitch. See `docs/compaction.md` §5.

So this decoupling is the reason active memory survives at all. Had the design kept it gated
on composable KV, the negative would have killed the feature. Because a suggestion appends
rather than injects, `recall`-as-text is not a stopgap - it is the mechanism.

### The cost, accepted deliberately

A suggestion appended at turn N is part of the prefix at turn N+1 and cannot be retracted
without invalidating everything after it. Suggestions therefore accumulate - twenty tokens
each, permanently.

That is also a useful forcing function: the budget disciplines the policy, so "few and short"
is enforced by arithmetic rather than by a relevance threshold nobody can pick correctly. At
~20 tokens a suggestion, a hundred turns costs ~2k tokens of a 262k window.

## 5. The trap

An embedder measures resemblance, not truth: a passage scored 0.762 against the correct answer
at 0.471.

A long prior context also *anchors* this model rather than distracting it - pass rates 0.83 at
zero depth against 1.00 at 60k, with the model reasoning about four times longer when given
nothing. Anchoring is the mechanism that makes active memory valuable, and the same mechanism
that makes a wrong recall *worse than silence*: it costs context and it biases, and the model
has no way to know the recalled material was surfaced rather than concluded.

Two consequences:

- **Structural association must carry at least as much weight as similarity.** Same file, same
  symbol, same row id, same error string, same commit - hard links, not fuzzy ones. Similarity
  ranks; structure qualifies.
- **Recalled material must be structurally distinguishable in the prompt** from the model's own
  prior reasoning. Otherwise a surfaced fragment reads as something the model already
  concluded: the system reporting as grounded something that merely resembles grounding. §4's
  suggestion form gives this for free - a handle riding with the user turn is not in the
  assistant's voice and never was.

## 6. What exists to build on

- **`SegmentMark`** (`crates/transcript/src/lib.rs`) is a zero-width delimiter that renders to
  nothing, carrying `{segment_id, label, kind, edge: Open|Close}`. It exists solely so
  per-segment decay, user-assisted compaction and agentic memory have something to address. If
  those never arrive it has cost one row and zero rendered bytes.
- **`crates/tokencore`** already content-addresses: the ledger chains
  `h_k = H(h_{k-1} ‖ tokens(item_k))`, so every item already has a stable identity.
- **The suggestion form itself exists for standing notes** -
  `crates/harnessd/src/notes_offer.rs` composes an offer at the tail of the request, never in
  the prefix and never as a row, from a lexical score over the note's abstract, title and
  headings, behind a floor of 0.18 that the take-up log moved it to. That is the mechanism a
  segment suggester would reuse, and it is lexical rather than an embedder - local,
  deterministic, and not a resemblance score dressed as relevance.

## 7. What is missing

Three things, none of which exist:

1. **A write-time association.** What a segment is *about*, not merely when it happened. No
   producer writes `SegmentMark` today.
2. **A turn-time surfacing policy** - what fires, how much, and a threshold. This is where
   over-eagerness pollutes context. It is a policy, not a mechanism, and it wants measuring
   rather than choosing.
3. **A `recall` tool** that answers a segment id with its text.

## 8. Status

A concept note, not a plan - but no longer a blocked one. §4's suggestion form means this is
buildable on what exists: a `SegmentMark` producer, a retrieval policy and a `recall` tool,
and none of those wait on composable KV.

What still depends on composable KV is only how much the *pull* costs. `recall` returns text
forever and active memory still works - it is simply priced as a tool call rather than as a
composition.
