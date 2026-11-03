# Embeddings

A second similarity tier: a text as a vector, two vectors as a cosine. The tree's own similarity
scores two texts on the words they share, and its measurement says where that stops: a paraphrase of
a note scored 0.126, under 0.133 that an unrelated note scored against a docker prompt, so no floor
separates them. This tier answers for meaning instead, and every caller that ranks text gets it at
once.

It is off until the operator writes a section. With it off, the lexical score is the whole score and
everything behaves exactly as it did.

Nothing is compiled into the binary. `mode = "internal"` with no `model` is a checkpoint that is
either already in the data directory or fetched into it once, at the commit and the sha-256 per file
this build pins.

## One key decides the mode

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

The project file's section is read before the operator's, and a level whose section leaves the tier
off (no section at all, or `mode = "off"`) leaves the other's standing. The section is read once,
when the daemon starts, so a change to it takes effect at the next start of the daemon (`letibot
--stop`, then start again), not at the next session.

| key | |
|---|---|
| `mode` | `off`, `internal`, or an `http://` URL. Anything else is refused by name |
| `model` | under `internal`: a directory. Under a URL: the model name, default `text-embedding-3-small`, and an empty one is refused. Under `off`: refused |
| `download` | `true` unless written: a box with `mode = "internal"`, no `model` and no checkpoint on disk may fetch the built-in one. `false` never fetches |
| `builtin_repo` | the `owner/name` on Hugging Face the built-in checkpoint is fetched from, default `sentence-transformers/all-MiniLM-L6-v2` |
| `builtin_revision` | the commit the three files are pinned at, default `1110a243fdf4706b3f48f1d95db1a4f5529b4d41` |
| `builtin_sha256` | a table of the three file names to the sha-256 each must have, default the shipped table below |

A key this build does not know is refused rather than ignored, because a typo in a section that
silently did nothing is the config defect nobody finds.

## `internal`: a model in this process

`model = "<dir>"` runs your own checkpoint, which is how to have a bigger or a multilingual model:
a directory with `config.json`, `tokenizer.json` and `model.safetensors`. This build runs a
BERT-family encoder: `model_type` `bert` or `nomic_bert`. Anything else is refused at open, not at
the first round:

```text
<dir>/config.json says model_type = "qwen3", which this build does not run; it runs a
BERT-family encoder (bert, nomic_bert)
```

Two limits worth knowing before choosing a model: a text is cut at 256 tokens, because a note is a
paragraph and the tail of a long one is not worth paying for every round; and a vector is
mean-pooled over the tokens and normalised to unit length, which is what a cosine wants.

## The built-in model

`mode = "internal"` with no `model` is `all-MiniLM-L6-v2`: six layers, 384 wide, English, from a
commit on Hugging Face that this build pins. A pin is a commit rather than a branch, and a sha-256
per file rather than a length, so the bytes a box ends up with are the bytes somebody reviewed, and
any other answer is refused.

| file | bytes | sha-256 |
|---|---|---|
| `config.json` | 612 | `953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41` |
| `tokenizer.json` | 466 247 | `be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037` |
| `model.safetensors` | 90 868 376 | `53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db` |

**Where it comes from.** `https://huggingface.co/<repo>/resolve/<commit>/<file>`, with the
repository and the commit from the three pin keys. The shipped repository is
`sentence-transformers/all-MiniLM-L6-v2`.

**When.** On the daemon's start path, where the section is read, before any session's first round: a
fetch never runs inside a turn. Nothing is fetched when the three files are already in the pin's
directory, so a box that paid once starts without a request at all and says nothing.

**Where it lands.** One directory per commit, so a moved pin is a new directory and never a
half-replaced one:

```text
<data dir>/models/all-MiniLM-L6-v2/<commit>/config.json
<data dir>/models/all-MiniLM-L6-v2/<commit>/tokenizer.json
<data dir>/models/all-MiniLM-L6-v2/<commit>/model.safetensors
```

`<data dir>` is `$XDG_DATA_HOME/letibot` or `~/.local/share/letibot`.

**How it is verified.** Each file is hashed as its bytes land, written under `<file>.part`, and
renamed to its real name only when its sha-256 is the pin's. A mismatch leaves the temp removed and
the model absent, so a directory never holds a checkpoint made of one good file and one bad. A file
that is already in place was verified when it landed and is not fetched again, so a fetch that
stopped halfway resumes instead of starting over.

**What it says first.** One sentence, on the daemon's stderr, once, and only when a fetch is about
to happen. It is handed out after the fetch finishes, so a fetch that failed prints nothing and
keeps its refusal for the next session's open instead:

```text
embeddings: the built-in embedding model all-MiniLM-L6-v2 at commit 1110a243fdf4 is not in
<data dir>/models/all-MiniLM-L6-v2/1110a243fdf4706b3f48f1d95db1a4f5529b4d41, so it is being fetched
from https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve (3 files, 87 MiB): this
happens once, and every score this box makes is the words alone until it lands
```

The size is said only for bytes measured here, so a section that pins its own hashes gets `3 files`
and no size. The fetch is synchronous on the start path, so a first start that fetches 87 MiB is a
first start that takes as long as the fetch does.

### `download = false`

A box that must not reach the network writes it down in the same table. Then nothing is fetched and
the pin's own directory is opened as it stands: a box with the three files there opens them and says
nothing, and a box without them gets the refusal this tree had before the fetch existed, naming the
directory to fill:

```text
<data dir>/models/all-MiniLM-L6-v2/1110a243fdf4706b3f48f1d95db1a4f5529b4d41 is not a model
directory: it has no config.json, no tokenizer.json, no model.safetensors
```

### The offline path

The pin's directory is one an operator can fill. Copy the three files in under the commit the pin
names and the tier opens them with no network at all, whatever `download` says. That is the whole of
the air-gapped story: fetch the three files somewhere with a network, carry them in, and put them
where the pin says they belong.

The directory is the pin's and not a fenced-off one, so a section that pins another commit or
another repository places its files under that commit, which is the `builtin_revision` in the same
section. With the shipped pin this is the path above.

### The pin keys

```toml
[embeddings]
mode = "internal"
builtin_repo = "me/all-MiniLM-L6-v2"          # a mirror of the checkpoint
builtin_revision = "<40 hex>"                 # the commit the three files are read at
builtin_sha256 = { "config.json" = "<64 hex>",
                   "tokenizer.json" = "<64 hex>",
                   "model.safetensors" = "<64 hex>" }
```

- A commit is 40 hexadecimal digits and a sha-256 is 64, and the all-zero filler a table holds
  before its values are known is refused in both. A key of the wrong shape is refused by the name
  the operator wrote.
- A revision without all three hashes is refused where it is written, and so are hashes without a
  revision: the shipped hashes can never verify a new commit, so that half could only fail at the
  fetch.
- The hash table names the checkpoint's three files and nothing else, and it must name all three.
- `builtin_repo` alone is allowed, which is the shipped commit and hashes read from another
  repository.
- A section's own revision moves the directory the files land in, because the commit names it. A
  previous revision's files are kept, not adopted.

### What a fetch that fails says

The fetch walks the tree's ladder: a transport failure or a *not now* (408, 429, 500, 502, 503, 504)
is tried again after 1 s, 2 s, 4 s, 8 s and 16 s; a refusal, a hash mismatch and a disk error end it
at once. Every ending is a sentence naming what to act on:

- The shipped base is `https://`, and this build's HTTP client speaks `http://` with no TLS, so
  under the shipped pin the fetch ends at `the built-in model is at <url>, and this build's HTTP
  client cannot fetch it: the client speaks http:// only, with no TLS`. A base the client can reach
  is `http://`, which today only a test in this tree serves, so on an ordinary box the offline
  path above is what fills the directory.
- `fetching <file> from <url>: <why>`, where the why is what the ladder ended with: the class of the
  failure, how many calls were made, how long they waited, and the last thing the server said.
- `<url> served <file> with sha-256 <got> and the pin says <want>, so it was not renamed into place
  and the built-in model is still absent`.
- `<path>: <why>` when the data directory would not take a file.
- `the built-in model's pin is not a full commit and a sha-256 for every file, so nothing was
  fetched`, for a pin with no fill in it. The section parser refuses such a pin before a fetch can
  see it, so this one guards the code rather than a config.

A failed fetch is no embedder at all, and not a tier that scores the words alone with a warm cache:
the failure is kept, and said once when the next session opens, in the sentence in the last section.

## A URL: an endpoint that embeds

An OpenAI-compatible endpoint. The client is HTTP/1.1 with no TLS, so an `https://` URL is refused
by name and the reachable spelling is `http://host:port` with an optional prefix:

```text
mode = "http://127.0.0.1:8080"          -> POST http://127.0.0.1:8080/v1/embeddings
mode = "http://host:9000/openai/v1"     -> POST http://host:9000/openai/v1/embeddings
```

A URL is only ever the operator's explicit setting, because it sends the text of every note and
every skill's frontmatter to whoever answers it. Nothing is retried: a failure falls back to the
lexical score and the session says so once.

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
embedded. A few thousand vectors of 256 to 1024 dimensions are a few megabytes in memory. The
built-in checkpoint is 87 MiB on disk, once, whichever way it got there.

## What it says when it fails

- A section this build cannot read is refused when it is read, by name: the unknown key, the unknown
  word in `mode`, the `https://` URL, a `model` written under `off`, a pin written half. The refusal
  is the sentence that says which line to change, and it is carried to a person by the next bullet.
- A section that was written and did not produce an embedder (the parser refused it, a model
  directory that is not there, a built-in checkpoint that could not be fetched, a cache that will
  not open) is said once when the next session opens: `the [embeddings] section did not produce an
  embedder, so every score this session makes is the words alone: <why>`.
- A tier that stops answering mid-session is said once, not once a round: `the second similarity
  tier could not answer, so every score this session makes is the words alone, until it answers
  again: <why>`. The warning's code is `embeddings_unavailable`.
- A fetch that succeeds says its one sentence before it starts, on the daemon's stderr, and nothing
  after it. A fetch that already happened on an earlier start says nothing at all.
- An offer made with meaning in the score says so in its own line (`lexical over its frontmatter, and
  its meaning`), so a line never claims an arithmetic it did not use.

See [skills.md](skills.md), [compaction.md](compaction.md) and [memory.md](memory.md) for the callers.
