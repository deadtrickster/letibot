# Embeddings

A second similarity tier: a text as a vector, two vectors as a cosine. The tree's own similarity
scores two texts on the words they share, and its measurement says where that stops: a paraphrase of
a note scored 0.126, under 0.133 that an unrelated note scored against a docker prompt, so no floor
separates them. This tier answers for meaning instead, and every caller that ranks text gets it at
once.

It is off until the operator writes a section. With it off, the lexical score is the whole score and
everything behaves exactly as it did.

## One key, three answers

```toml
# leticode.toml, in the project, or ~/.config/letibot/leticode.toml
[embeddings]
mode = "off"                        # the default: words only

[embeddings]
mode = "internal"                   # candle, in the daemon, with the built-in model

[embeddings]
mode = "internal"
model = "/data/models/nomic-embed-text-v1.5"   # ... or a directory of your own

[embeddings]
mode = "http://127.0.0.1:8080"      # an OpenAI-compatible /v1/embeddings
model = "bge-m3"                    # the name that endpoint answers to
```

The project file's section wins over the operator's, and a level that wrote no section leaves the
other's standing. The section is read once, when the daemon starts, so a change to it takes effect at
the next start of the daemon (`letibot --stop`, then start again), not at the next session.

| key | |
|---|---|
| `mode` | `off`, `internal`, or an `http://` URL. Anything else is refused by name |
| `model` | under `internal`: a directory. Under a URL: the model name, default `text-embedding-3-small`. Under `off`: refused |

A key this build does not know is refused rather than ignored, because a typo in a section that
silently did nothing is the config defect nobody finds.

## `internal`: the model in this process

Nothing is compiled into the binary and nothing is fetched for you. The built-in model is
`all-MiniLM-L6-v2`, and its three files go in the data directory:

```text
<data dir>/models/all-MiniLM-L6-v2/config.json
<data dir>/models/all-MiniLM-L6-v2/tokenizer.json
<data dir>/models/all-MiniLM-L6-v2/model.safetensors
```

`<data dir>` is `$XDG_DATA_HOME/letibot` or `~/.local/share/letibot`. Put a checkpoint there and
`mode = "internal"` finds it. Leave it out and the refusal names the directory it looked in, which is
the line to act on:

```text
~/.local/share/letibot/models/all-MiniLM-L6-v2 is not a model directory: it has no config.json,
no tokenizer.json, no model.safetensors
```

`model = "<dir>"` is the same machinery over your own checkpoint, which is how to have a bigger or a
multilingual model: a directory with those three files. This build runs a BERT-family encoder:
`model_type` `bert` or `nomic_bert`. Anything else is refused at open, not at the first round:

```text
<dir>/config.json says model_type = "qwen3", which this build does not run; it runs a
BERT-family encoder (bert, nomic_bert)
```

Two limits worth knowing before choosing a model: a text is cut at 256 tokens, because a note is a
paragraph and the tail of a long one is not worth paying for every round; and a vector is mean-pooled
over the tokens and normalised to unit length, which is what a cosine wants.

## A URL: an endpoint that embeds

An OpenAI-compatible endpoint. The client is HTTP/1.1 with no TLS, so an `https://` URL is refused by
name and the reachable spelling is `http://host:port` with an optional prefix:

```text
mode = "http://127.0.0.1:8080"          -> POST http://127.0.0.1:8080/v1/embeddings
mode = "http://host:9000/openai/v1"     -> POST http://host:9000/openai/v1/embeddings
```

A URL is only ever the operator's explicit setting, because it sends the text of every note and every
skill's frontmatter to whoever answers it. Nothing is retried: a failure falls back to the lexical
score and the session says so once.

## The cache

Vectors live in `<data dir>/embeddings.db`, a file of its own so the session store's schema is not
touched by a cache. The key is `(model, sha256 of the text)`, which needs no invalidation: an edited
note is a new key, a changed model or mode is a new model name in the same table. Rows unused for
thirty days are dropped when the file is opened.

It is a cache and nothing else. Deleting it costs time and loses no fact.

## The blend

One function, for every caller: half the lexical score and half the cosine.

```text
score = 0.5 * lexical + 0.5 * cosine
```

`off` is not a weight of zero, it is no second term at all, so the score a box without the section
computes is bit for bit the score it computed before the tier existed.

Each caller keeps its own floor, because offering a note and refusing a duplicate want different
thresholds:

| caller | what a score does | floor |
|---|---|---|
| the note offer | offers a note that looks relevant, in the request's tail | 0.18 |
| the skill offer | offers a skill that looks relevant, at most two a round | 0.18 |
| the note near-copy gate | refuses a note write that is a paraphrase of another note | 0.30 |
| the context claim check | finds the claim a session already made, so it is not made twice | 0.25 |
| the waits offer | says a brief looks like a row somebody already holds | 0.35 |
| the done offer | says a brief looks like work a commit or a settled entry already did | 0.35 |
| compaction relevance | keeps an old exchange whole because it is about a row on the board | 0.35 |

A caller that cannot answer with meaning falls back to its own words, and the tier is emptied rather
than left holding the last corpus's vectors: a stale vector would score a text that has since been
edited.

## Cost

One model per box, loaded once and shared, and one vector per text the cache has not seen. A corpus
of notes or skills is embedded once and then read from disk; in a round only the round's own text is
embedded. A few thousand vectors of 256 to 1024 dimensions are a few megabytes in memory.

## What it says when it fails

- A section this build cannot read is refused at load, by name: the unknown key, the unknown word in
  `mode`, the `https://` URL, a `model` written under `off`. The refusal is the sentence that says
  which line to change.
- A section that was written and did not produce an embedder (a model directory that is not there, a
  cache that will not open) is said once when the next session opens: `the [embeddings] section did
  not produce an embedder, so every score this session makes is the words alone: <why>`.
- A tier that stops answering mid-session is said once, not once a round: `the second similarity tier
  could not answer, so every score this session makes is the words alone, until it answers again:
  <why>`. The warning's code is `embeddings_unavailable`.
- An offer made with meaning in the score says so in its own line (`lexical over its frontmatter, and
  its meaning`), so a line never claims an arithmetic it did not use.

See [skills.md](skills.md), [compaction.md](compaction.md) and [memory.md](memory.md) for the callers.
