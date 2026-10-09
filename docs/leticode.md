# leticode — opencode-shaped agent on letibot primitives

Direction (2026-09-13): a letibot session that models itself after opencode —
the tool union plus a verbatim port of opencode's permission model — and a
serenedash-style dashboard over the pieces that are new.

## Done

- `crates/tools/src/permission.rs` — verbatim opencode permission model
  (`Action`, `Rule`, `evaluate`, wildcard, `config_to_ruleset`), wired into
  `AdjudicatedGate` before the mode, with an `allow_always` reply feeding
  `(permission, pattern)` back into the ruleset.
- `crates/flowy` — the seat on the fabric, as a monitor: `harnessd --flowy`
  holds one persistent seat per daemon, a root session attaches with its own
  per-room attention table and gets a continuous `flowy` monitor whose firings
  are the messages; `flowy` is the sixteenth tool (`status`, `attention`,
  `subscribe`, `say`, …). Subagents route through the parent. See
  `docs/flowy-monitor.md`.
- **Downgradable subagents.** `task` takes `access` — `read-only`, or any of
  `no-write`, `no-exec`, `no-network` — and `where` (`host`, or `firecode`).
  A subagent inherits the parent's ruleset and the downgrade only removes: it
  is the union of the parent's own downgrade and the one asked for, so a
  downgraded session cannot spawn a wider child by naming a wider role. Three
  readers of one fact, and a test that they agree: the tools of a denied class
  are not seated (`Registry::without_access`), the backend is opened without
  them (read-only view without `Write`, no process host without `Exec`), and
  the ruleset carries a `deny` per denied tool — seated or well-known — that
  wins by last-rule. `role` now accepts any seat this build knows and refuses
  an unknown one rather than seating it as coder. `where: firecode` is a
  declared seam: refused by name until the backend exists, never run on the
  host in its place. The disclosure names the downgrade.
- **`where: firecode` filled** — `crates/tools/src/firecode.rs`: tools in a
  VM over vsock, model on the host, the child on a copy of the workspace, the
  VM the boundary (allow-all inside). `docs/subagents.md`; `docs/cookbook.md`
  (filed on the fabric as skill `01M2FRW9D3A8K8V9660BJQX5PZ`).
- **Cloud providers** — `--provider deepseek|glm|grok`: the transcript as
  messages over a `MessagesBackend`, the token ledger kept as the record,
  METERED and disclosed. `docs/providers.md`.
- **`pkill` and a process watcher that cannot self-match** —
  `crates/tools/src/exec/procs.rs` is an in-process `/proc` finder that removes
  this daemon, its ancestors and its protected pids before matching; `pkill`
  lists by pattern and kills by (pid, start time) only; `monitor process=` /
  `pid=` resolve to handles once and watch those. The daemon declares its model
  server protected. leticode is 17 tools with it.
- **One daemon per folder; `/flowy` and `/models`** — `scripts/letibot`
  connects-or-starts per git toplevel (`--attach`, `--daemons`, `--stop` by pid).
  `ClientFrame::Slash` (protocol 11) carries `/flowy status|login|logout` and
  `/models …` to the daemon; `crates/harnessd/src/slash.rs` answers on the
  session log, naming the next command when something is missing. A seat can
  attach to a running daemon (the `flowy` tool is always a door); `/models`
  switches the provider underneath the session and records the standing choice
  in `~/.config/letibot/providers.toml`, which a new daemon starts on.
- **The fabric block** — the shelf's skills as summaries and the memories as
  titles in the system prompt, refreshed after a compaction as a system update,
  cached for when the node is away. `docs/flowy-monitor.md` §5c.
- **`ask_user_question` is seated, and a person can answer it** — the operator's
  ruling, 2026-10-09: *"yeah i want you to be able to ask me for a choice. each
  choice can have my note, and i can abstain or type my answer"*. The tool existed
  since D10 and lived in the `intent` bundle, which is the planner's, so a leticode
  session had no such verb at all and a model could only ask in prose. Now the seat
  carries it, `crates/harnessd/src/answers.rs`'s `HeadQuestioner` poses it as a
  `DecisionRequested` with `kind: "question"` and its `choices`, and the answer comes
  back through the frame the head already had. Four shapes, all first-class: a choice,
  a choice **with the person's own note on it** (`<choice> -- <note>` at the composer),
  words of their own, and an **abstention** (`abstain`, alone) — which is an answer,
  not silence and not an empty reply, and which the model receives as `Abstained`
  rather than `not_run`. No `PROTOCOL_VERSION` bump: every field and frame this needs
  was already on the wire.

## The three tools, and what each is

| tool | opencode's | what it is | hard part |
|---|---|---|---|
| `task` | subagent | spawn a child turn with a role + tool subset, run to completion, return its result | delegation + a second turn loop + budget/lifetime |
| `lsp` | LSP | run a language server, read diagnostics/hover/definition for the file at hand | spawning + talking LSP, one server per language |
| `skill` | skill | a named, loadable prompt/capability; list them, load one into context | almost none — a registry + a list/load tool |

## Reading an image

`read` reads a picture as a picture. Same tool, same verb, same gate, same permission, same path —
the only thing that differs is what comes back: an image part with the file's bytes, and a text
payload that describes it. The ruling is the operator's, 2026-09-27: *"lets just do it opencode
way"*, *"read is read there is nothing to settle"*, *"reading an image is no different to reading a
rust file."*

**Measured against the local `llama-server`** (`qwen-3.8-27b`, `--mmproj`), because the numbers are
what a future change has to argue with rather than guess about:

| what was measured | result |
|---|---|
| an image in a `user` message | the model named a red square `Red` |
| an image in a **`tool`-role** message | the model named a green square `Green` — **media inside a tool result works here** |
| `data:image/png;base64,…` as the URL | works |
| a bare **path** as the URL | **`HTTP 400 · Failed to load image or audio file`** — the server fetches the URL, so a `data:` URI is required |
| an image above **4.19 MP (2048×2048)** | **resized, never refused**: 2048² and 4096² both cost exactly 4151 prompt tokens, one token per 1024 pixels |
| a 16.8 MB request body | accepted; no byte limit was found up to there |

**So there is no size cap on the read path.** A head that imposed one would be discarding what the
server would have taken, and an attachment dropped by the head looks exactly like a picture the
model ignored.

**And the sentence that makes it work.** If a message says an image is attached and the model has no
image, the attachment did not arrive — say that, rather than answering as though the picture were
merely uninteresting. A model that answers around a missing picture is indistinguishable from one
that saw it and had nothing to say about it, and only one of those is worth acting on.

### What an image turn costs the cache — measured

**A LOCAL-ONLY question.** leticl runs remote deepseek-flash, which never sends token ids, so none of
this applies to it and nothing here should be ported to its side: *"the whole design of sending token
ids exists on the assumption that it would be"* free, and that assumption is what was tested.

The question was whether an image turn is a cold prefill, because llama.cpp takes images only when
`prompt` is an object carrying a `prompt_string` — the server tokenizes it — while letibot's whole
design is that *the harness* renders and tokenizes and sends ids. If the two tokenizations differ,
every cached turn in the conversation dies at the first boundary.

**They do not differ, and the reason is the framing.** Tokenized as one string versus item by item:

| how the prompt was joined | per-item | one string | shared prefix |
|---|---|---|---|
| bare text, no framing | 110 | 107 | **40** |
| framed by `<|im_start|>`/`<|im_end|>` — what a renderer actually emits | 125 | 125 | **125** |

Bare text diverges at the first boundary because BPE merges across it (`cache. And` is not
`cache.` + `And`). Control tokens are atomic, so a framed prompt has no such adjacency and the two
tokenizations are identical. **The string path is free, provided the renderer frames its items** —
which is a property of the renderer and not of this decision.

**Caching, measured on the live server (slots pinned; unpinned requests bounce between slots and
each has its own cache, which made the first run unreadable):**

| request | prompt_n | cache_n |
|---|---|---|
| token ids, cold | 88 | 0 |
| token ids again (control) | 4 | 84 |
| `multimodal_data` prompt with a 512×512 image | 4 | 342 / 346 |
| the same image prompt again | 4 | 342 / 346 |
| **token ids after an image turn** | 4 | **84** — identical to its own warm control |

The image costs **258 prompt tokens** at 512×512 (256 at one token per 1024 px, plus 2 framing),
consistent with the table above. And the last row is the one that mattered: **an image turn does not
poison the cache for the ledger's ordinary path.**

**One case was cold and it is not resolved.** The *first* image turn against a prefix built by
token-ids requests returned `cache_n = 0`. The suspicion is `process_mtmd_prompt`'s hardcoded
`add_special = true` (`server-common.cpp:966-975`), which would prepend BOS to a prompt whose cached
counterpart has none — one token of difference, and a prefix match of zero. **Not confirmed:** the
server keeps a large persistent prompt cache (`--cache-ram 57344`, `--cache-disk 512000`), so
"cold" is hard to produce deliberately and I could not isolate it. What it would cost is one cold
prefill when the first image arrives, and the check is cheap for whoever builds the wire: tokenize
the ledger's prompt both ways and compare the first token.

**Open, and named rather than left to be discovered: compaction.** An image in the transcript is
re-sent on every prompt rebuild and stored inline as base64 (4/3 of the file). opencode strips media
on compaction and carries a dedicated recovery instruction for a provider that refuses oversize
media. R24/R27 own what this tree does; until then an image lives for the life of the session.

## The dashboard

A serenedash-style terminal TUI (the same palette / row grid / boxed panels /
side-by-side reflow as `llama-dash`), with one panel per piece:

- **tasks** — the subagent tree: name, role, state (queued/running/done/failed),
  tokens so far, elapsed, and a sparkline of progress. The detailed one.
- **lsp** — servers up, per-language diagnostics counts (error/warn), last
  hover/definition latency.
- **skills** — the registry: name, description, loaded/not, last-loaded.

Data comes from the daemon over the existing session socket (events for task
spawn/finish, a query for the lsp/skill registries), so the dashboard is a view
and the daemon is the record.

## Order

1. `skill` (registry + tool) — no moving parts.
2. `lsp` (one server, diagnostics only, behind a boundary) — medium.
3. `task` (subagent turn, role + subset, budget) — the real work.
4. the dashboard, tasks first.
