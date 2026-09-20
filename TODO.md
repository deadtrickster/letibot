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


---

## R18 — every hand-rolled lexer replaced by rano + tree-sitter — **given 2026-09-20**

> lets extend todo with this task - completely replace handrolled code with rano and
> treesitter. I really want us to rely on rano as much as possible, you are free to go
> and patch/extend it too

Rano is the hub: `~/Projects/rano/rano`, a path dependency of `crates/ui` and
`crates/tui`. It owns the tree-sitter engine — 28 languages, one capture walk, the
palette left to the caller. The rule stated in `crates/ui/Cargo.toml:27-34` is that a
**second tree-sitter integration in this process is the thing rano exists to prevent**.
There are three. Surveyed 2026-09-20:

| hand-rolled | where | size | what rano has instead |
|---|---|---|---|
| `StreamingCode` | `crates/ui/src/highlight.rs` | 699 lines, **10 languages** | `Stream` + `captures`, 28 languages |
| its own grammar set | `crates/code/Cargo.toml:20-26` | tree-sitter 0.27 + **6 grammars** | the same tree-sitter, 28 grammars |
| `scan_script` | `crates/tools/src/intent.rs:3390` | a hand-written shell scan | `crates/code::shell::shape` |

Everything below `crates/tui/src/markdown.rs`'s projection is already rano's (the block
pass, the inline pass, the `Node` tree) — `docs/streaming-markdown-plan.md` is the
record. What that file keeps by hand is **not** in scope: `fences_in`, `mask`,
`cut_point`, `stable_boundary_with`, `list_kind`, `unescape`. Each exists because the
grammar gets something wrong (the closing fence is not line-anchored; a loose list
cannot be settled by the tree) and finding it in the text *is* the fix. Deleting those
would reintroduce the bugs of 2026-09-20.

### R18.1 — `StreamingCode` goes, and the conversation gets 28 languages — **DONE 2026-09-20 (`50b005c` + rano `e58600c`)**

The visible win: a code fence is coloured by a 10-language hand-written lexer today, so a
fence tagged `tsx`, `lua`, `ruby`, `diff` and eighteen others renders plain. Rano knows
them all.

Done: `crates/ui/src/highlight.rs` 699 → 118 lines, holding `role_for_capture` and
nothing else; `sidediff` shares that table instead of keeping its own; `CodePaint` is a
rano `Stream` + `Stream::spans`; `BlockCache`'s instrument is `parses` rather than
`bytes_highlighted`. Three rano additions landed with it — `Lang::from_token`,
`Lang::name`, `Stream::spans` — and the walk got 1.5–2.5× cheaper on the way
(`for_each_capture` no longer builds a `Vec<char>` and a char→byte map per line).
Measured: ~470 µs per push at the end of a 5.4 KB Rust fence, a frame's budget. What is
still open is in rano's `TODO.md` §9: the walk is O(text), which is a fence's budget and
not an editor's whole-file repaint.

**The trade, and measure it before deleting anything.** `StreamingCode`'s guarantee is
that *a complete line is highlighted exactly once, ever* — rano cannot promise that, and
rano measured why: markdown's push re-lexes the whole document (~106 ns/byte) and rust's
reuses ~97% (~3.5 ns/byte). At the last figure a 10 KB fence costs ~35 µs per push, which
is a frame's worth of nothing; at markdown's it would not be. So the number to take is
rano's `bytes_reparsed`-equivalent for a rust/go/ts fence grown one token at a time, and
the decision follows it. If it is not flat enough, the fence gets the same window
discipline the conversation has.

**Where.** `CodePaint` (`crates/tui/src/render.rs:176`) holds the `StreamingCode` and the
byte offset it has been fed; `render_block_with`'s `Block::Code` arm draws what it
returns. It becomes a rano `Stream` plus a capture walk over the fence's text. Delete
`highlight.rs`'s lexer (`Syntax`, `State`, `StreamingCode`, the 10-language table) once
nothing calls it, and its `bytes_highlighted` instrument with it — the number it exists
for has a replacement in rano.

### R18.2 — `crates/code` stops carrying its own tree-sitter — **DONE 2026-09-20**

`crates/code` was a second integration: its own `tree-sitter = "0.27"` and six grammars
(rust, python, go, c, bash, json), used for `outline` (`crates/tools`) and the shell shape
(`crates/harnessd`'s etalon map). All six are rano's already.

Done: `Cargo.toml` drops the seven dependencies for one `rano` path dep; `outline` and
`shell::normalise` drive `Stream` and walk rano's `Node`; `examples/dump.rs` — the tool
that prints a grammar's node skeleton, and how every kind string in `classify` was chosen
— reads the same `Node`, so what it prints is what the outline sees.

**What it is not**: a merge of the two crates. `crates/code` exists because the grammars
are C and pull a `cc` build, and `crates/tools` deliberately has no `build.rs` — that
reason stands, and this crate is still where it lives. What went was the second engine
underneath it.

**Rano grew four things to make it possible**, all of them "the hub's node should carry
what its consumers need": `Node::field` (which field a child sits in — 17
`child_by_field_name` call sites here), `Node::start_point`/`end_point` as rano's own
`Point`, `Node::id` (two nodes can share a byte range, so identity is not a coordinate),
and rano's `Point` no longer being tree-sitter's in its public API.

**One bug this found, in this crate and not in the port.** `shell.rs`'s `named_children`
was misnamed — it collected *every* child, anonymous tokens included. A reader taking the
name at its word filtered on `Node::named` and lost the `&&`, `|` and `;` the module
decides *by*: `test -f x && rm x` came back `Certainty::Always` for `rm` instead of
`Conditional`. Renamed `kids` with the misnomer written down, because an operator token is
exactly the kind of child a grammar leaves anonymous.

### R18.3 — the intent scanner's shell scan — **CLOSED 2026-09-20: it stays, and not as a duplicate**

`crates/tools/src/intent.rs::scan_script` is read at the call sites rather than guessed at,
and it does not read shell at all: `ScriptLang::Shell` bodies already go through
`shell::normalise` and the grammar. This handles `ScriptLang::Other` — a **script in
another language**, named by its interpreter (`python`, `node`, `perl`, `ruby`, `php`),
which a bash grammar would read wrongly and no single rano grammar covers.

Its question is also a different one: not what the shell will *do*, but which
**capabilities** the text names — a network import, a shell-out, a host, a secret path —
deliberately **body-wide rather than adjacency-based**, because `host = "192.0.2.10"` three
lines above `urlopen(f"http://{host}/")` is a real shape. A parse does not change that.

What a parse *would* improve is precision (`import urllib.request` names its module in a
node, where this matches a name list against a whole line) — noted in the function's own
doc rather than done, because it changes what a gate decides and that is not a refactor.
So: no grammar here, and the reason is recorded where the next reader asks.

### Rano patches this needed — **all three landed** (`e58600c`)

- **`Lang::from_token(&str)`** — done, with its own alias table rather than `detect`'s
  (`mk` is Make as an extension and not a token; `sh` is bash as a token and `/bin/sh` as
  a path).
- **A route from `Lang` to its highlight query** — done as `Stream::spans`, which is
  better than making `query()` public: the embedder hands over *nothing* and gets capture
  names back, so no query text crosses the boundary at all. `classes()` was written first
  and removed — see rano's commit.
- **`Stream` over a growing text** — it already existed; the fence feeds it the delta.

**Still open?** Nothing — all three subtasks are closed. What *was* the check, for the
record, is now the answer:

    grep -c 'name: "' crates/ui/src/highlight.rs        # 0  (was 10)
    grep -c '^tree-sitter-' crates/code/Cargo.toml      # 0  (was 6)
    ls crates/tools/build.rs                             # absent, as it must stay

**Done when.** A fence tagged `tsx` (or `lua`, `ruby`, `diff`) renders coloured in a live
answer — the rendering is pinned by tests in `crates/tui/src/render.rs`, and the one thing
a test cannot assert is a live answer; `crates/ui/src/highlight.rs` holds no lexer;
`crates/code` declares no `tree-sitter-*` of its own and `crates/tools` still has no
`build.rs`; the conversation's per-push cost is measured against the old one and the number
is in rano's `TODO.md` §9; 2026-09-20's markdown fixtures pass, since they are the record of
what the grammar gets wrong.

### R18.4 — attaching to a big session takes seconds, and it is the markdown parse

**Reported 2026-09-20**, after the migration shipped: *"startup time skyrocketed.
literally seconds"*, and then *"I do `leticode --continue` and it just does nothing, then
chrome appears with empty conversation history and then after a while it renders
history"*.

**Measured**, release, on this workspace's own stored transcripts:

| | |
|---|---|
| markdown block grammar | 133 ns/byte (555 KB → 74 ms) |
| markdown inline grammar | 250 ns/byte (450 KB → 133 ms) |
| Rust, for scale | ~3 ns/byte |
| a stored session | **4.1 MB and 5.9 MB** (`s-…838#t18`, `s-…813#t11`) |
| so one attach | **1.6-2.7 s** of lexing, which is the "after a while" |

Both markdown grammars run external scanners and that is the constant; tree-sitter is
not the problem. A row is lexed once (the walk is monotonic — see
`rendering_the_history_does_not_grow_with_the_session_either`), so this is one linear
pass, not a repeat.

**Fixed (`5884ddf`, rano)**: the query cache. `Query::new` is 8.7-10.2 ms and was paid
per *code fence* and per *diff excerpt*; it is compiled once per process per language
now. This session alone holds 132 fenced messages, so that was ~1.2 s of pure query
compilation per attach — our bug, and it is gone.

**Chrome first (done 2026-09-20).** The head used to draw its first frame *after*
the whole attach — and `HeadClient::attach` blocks on the daemon's `Hello`, which
**carries the snapshot**. So on a big session the operator's previous screen stayed
up for the entire round trip and then the transcript appeared at once, which is
exactly the reported *"does nothing, then chrome appears with empty conversation
history and then after a while it renders history"*.

Measured on this daemon: 0.01 s of process start, 0.17 s of attach (the `Hello`,
snapshot included), 0.08 s of first frame. Only the first of those was ever visible
as *letibot*.

Now the terminal is taken over and a frame is drawn **before** the attach, so the
composer and the hint bar are on screen in the time the process takes to start, and
the history fills in when the daemon answers. The frame drawn in the meantime must
not lie: the empty-transcript banner says *"this session has said nothing yet"*,
which is false when the truth is *"nobody has told this head yet"*, so `App` carries
an `attaching` flag that suppresses it — `the_frame_before_the_attach_claims_nothing_about_the_session`.

`Terminal`'s `Drop` restores the termios and leaves the alternate screen, and it does
so before printing its own reports, so entering the screen before a refused attach
still lands the error on a restored terminal.

**The design, and it is the operator's rather than mine.** Stated twice, and I said
yes to it both times and then planned as though I had not:

> so in a way we are looking back right, instead of looking in the future, interesting
> challenge for treesitter  — 2026-09-20, on the tail problem

> render chrome asap, then render the very current frame of text, then some scroll up
> buffer if needed — mind the bounds of blocks, etc  — earlier the same day

So: **stream backwards.** The document is finished and the head wants its *end* — the
current frame, then scrollback above it, fetched as it is looked at. That is a viewport
over a buffer the daemon already owns, which is what an editor has, and it is *not* the
three-patch list a first pass at this item produced (lex less, cache the render, fill
over frames). Those were written after `tail_cut` was built from the operator's own
insight and then not wired up — the mechanism for this design already exists in
`crates/tui/src/markdown.rs` and every one of those three would have worked around it.

**Stage 1 — the window is a window (no protocol change).**
`tail_cut(src, min_bytes)` returns the offset near the end from which the suffix parses
standalone. Committed and tested (`815200e`, `ad0a10a`): `lex(tail)[1..]` is the
document's blocks, the first may be ragged and goes off the top, and a 4 KB tail of
152 KB measures **3.0%** of the whole's cost. What is missing is only the caller: the
walk in `body_window` still lexes each row whole.

**Stage 2 — a row is a logical string, and the head holds a viewport.**
The operator's own framing, and it is the right one: a tool result of 418 KB is a
*logical* string that wraps to thousands of *display* lines, of which 40 are on screen.
Wrapping exists; paging does not, so an unfolded row is unreachable past the fold. The
fix is the editor's, not a cap: the daemon owns the buffer, the head holds the window,
and a row fetched on demand when it is unfolded. `Peek` already fetches a stored
conversation for the session picker, so the mechanism is built — it needs a row-shaped
form rather than a new protocol.

**Stage 3 — bound the snapshot in bytes (`ViewBounds`).**
`items: 2_000` bounds *count*, and the operator has sessions of **160 MB and thousands
of turns** where a row may be 418 KB — so the snapshot can be most of a gigabyte. This
is the first half of the viewport rather than an optimisation: without it the window has
nothing to be a window *over*. Do it first only in the sense that it is small and needs
no protocol change; on its own it just drops old rows and still ships the fat one.

**Not a fix**: making the grammars cheaper. Both external scanners are upstream, and the
markdown half costing ~20x the Rust half per byte is a fact about those scanners rather
than about rano. Nor is capping a row's payload at the source: that cuts the logical
string to fix a display problem, which the operator named as the editor mistake.

**Not a fix**: making the grammars cheaper. Both external scanners are upstream, and the
markdown half costing ~20x the Rust half per byte is a fact about those scanners rather
than about rano.

**The one piece left, and it is rano's.** The capture walk is O(text) per repaint. A code
fence is a frame's budget at fence sizes — measured, and why this shipped — but an editor
repainting a 200 KB file per keystroke is not, and that is exactly the consumer rano's
README says it is for. rano's `TODO.md` §9 has it as open with the shape of the fix
(a range-limited `QueryCursor` widened back to any overlapping node, which is where the
correctness lives). It is not filed here as well, because it is one item and it belongs to
the engine.

---

## R12 — the firecode backend for subagents, and the cookbook — **SETTLED 2026-09-14 (claude-lab2x1)**

Done: `crates/tools/src/firecode.rs` + the harness placement; live test
`crates/tools/tests/firecode_live.rs` (up 6.9 s, read/write/list/stat/run/job,
down landing the guest's writes); an end-to-end run where a leticode session
spawned `task(role: researcher, access: read-only, where: firecode)` and the child
listed the workspace inside the VM and had no shell; `docs/cookbook.md`, filed as
skill `01M2FRW9D3A8K8V9660BJQX5PZ`. Left open, in `docs/subagents.md` §4:
checkpoints, `--cwd` made after boot, the child's own voice.

**Given by the operator 2026-09-14**, leaving for the day: *"1. firecode + cookbook"*.
Design and measured numbers in `docs/subagents.md` §3. The `task` tool's `where:
firecode` is a declared seam that refuses by name (`4f3d1d7`); this fills it.

**Where.** `crates/tools/src/firecode.rs` (`FirecodeBackend: ExecBackend`, a
`FirecodeConfinement` wrapping every job as `firecode in`), and `harness.rs`
placement: `Placement::Firecode` opens that backend for the child, `backend_confined
= true`, mode allow-all inside. The child works on a **copy** of the parent's
workspace under `~/.cache/letibot/firecode/` (firecode's shared-tree guard refuses a
main checkout with worktrees and uncommitted tracked changes — exactly when a parent
spawns; and `/tmp` scratch evaporates). Cold boots only: checkpoint/restore is not
dependable on the host yet (claude-host-lab's note `01M2FR5A1VAJJ13M7S2XRRZK33`). The
operator's direction for startup: **hierarchical image caches**, so a boot is
milliseconds — firecode's side of the seam; recorded in the cookbook, not built here.

**Done when.** A `task(where: "firecode")` from a leticode session boots a VM, runs
its tools inside it (read/write/list/run over vsock), ends with `down`, and returns
its answer naming the sibling directory where its writes landed; a live test behind
`FIRECODE_LIVE=1` holds it; `docs/cookbook.md` exists, is filed on the fabric as
`kind=skill`, and covers firecode, flowy, subagents.

---

## R13 — cloud GLM, DeepSeek and Grok as turn backends — **SETTLED 2026-09-14 (claude-lab2x1), one knob open**

Done: `crates/provider`, `MessagesBackend` in `letibot-backend`,
`TurnEngine::run_turn_messages`, `--provider/--api-key/--thinking`, the `provider`
disclosure, cost in the footer; fake-provider tests and an end-to-end harness run
(`docs/providers.md`). **Not verified live** — no key on lab2x1. Open, per the
operator's note: an eager-compaction budget under a meter (compaction is manual
today), and the providers' prompt caches are relied on through the stable prefix.

*"2. i want cloud glm and deepseek and grok to work in letibot/code."* D10 reserved
the seam (`crates/backend`: `TurnRequest` holds the transcript, `BackendCaps` states
facts, `Meter::Money`, `PrefixGuarantee::None`). All three speak an OpenAI-compatible
chat-completions API with tool calling; one `messages` backend with three endpoint
presets. Keys from the environment (`ZHIPU_API_KEY`, `DEEPSEEK_API_KEY`, `XAI_API_KEY`)
or `~/.config/letibot/providers.toml`. D10's rule holds: the invariant suites skip
loudly under a provider — `skip_reason()` says the prefix check did not run.

**Done when.** `harnessd --provider deepseek --model deepseek-chat` (and `glm`,
`grok`) runs a turn with tool calls through the same engine; a fake-server test
covers the message conversion, streaming and tool-call parsing; live tests behind the
key being set; cost reported as `TurnCost.micros_usd`.

---

## R17 — Brave behind `web_search` — **SETTLED 2026-09-14 (claude-lab2x1), not yet run live**

*"I bought brave search api key, I want web search tool"* (2026-09-14). The seam
was already there: `web_search` has shipped as a refusing tool since the external
module was written, precisely so the schema could be fixed before anything cached
against it. This fills it.

- New crate `letibot-websearch` — `Brave` implements
  `letibot_tools::…::web::SearchProvider` over ureq+rustls. Its own crate because
  `letibot-tools` is deliberately *a function of a call and a filesystem*; the
  trait is the seam that keeps TLS out of it.
- Key: `--brave-key` → `$BRAVE_API_KEY` / `$BRAVE_SEARCH_API_KEY` → `[brave] key`
  in `~/.config/letibot/providers.toml`. **Refuses at attach**, naming all three.
  The key is held in a `OnceLock` in the websearch crate, never in `Config`,
  which derives `Debug`.
- Seated **only when attached** (`role_for_seat`, like `flowy`'s door), so a
  session without `--web-search` is byte-identical to yesterday's and nothing
  re-prefills. `max_tools` rises by one when it is.
- Only `web.results` is read; `<strong>` stripped; a URL-less hit dropped;
  `considered` carries the denominator; Brave's `query.altered` is surfaced as
  `rewritten_query` because a silently rewritten query is a rewritten query.
- Tests: 3 unit (key order and refusal text, the `[brave]` section read, markup),
  4 against a stand-in Brave (the key rides in `X-Subscription-Token`, `site:`
  goes out as Brave's operator, 401/429/transport each say which they are, an
  empty result is empty rather than invented).

**Verified live 2026-09-15.** The operator's key landed and
`crates/websearch/tests/brave_live.rs` (BRAVE_LIVE=1) ran a real query:
`Brave Search, key from ~/.config/letibot/providers.toml`, 3 hits of 3
considered, titles/URLs/snippets all parsed, no `<strong>` reaching the model.
The field names written from the documented shape are the ones Brave sends, so
the remaining unknown named here is closed.

Two notes from doing it. The key was pasted onto the commented placeholder line
and stayed a comment, which reads as "no key" — correctly, and the refusal named
all three places, but the operator's belief was that it was set; the placeholder
being *inside* the section is what made that easy. And the end-to-end through a
local-model session could not be run: `ggml-cuda.cu:108` on model load, the
standing CUDA fragility, not anything to do with search. `--provider deepseek
--web-search brave` is the path that avoids the local GPU.

---

## R16 — the preapproved list, Always allow, `ps`, a shell by default, and sudo-to-the-head — **SETTLED 2026-09-14 (claude-lab2x1)**

The operator, 2026-09-14: *"we badly need a list of preapproved globs, like all
read only git and gh commands, cargo, go, and other tests. and good old Allow
Always from opencode and Claude Code"*; *"when i do leticode --bash must be a
default"*; and, from the two transcript scans on this box (`PS_USE.md`: 294
`ps` pipelines in 22 days; `SUDO_USE.md`: 40 `sudo` attempts, 0 succeeded):
*"we need tools/skills for ps - i dont want models to reinvent the same commands
and i want a solution for sudo"*.

Done:

- **The preapproved list** — `permission::DEFAULT_ALLOW`, 142 prefix rules for
  `bash`: read-only git and gh, cargo/go/npm/pytest build-and-test verbs, the
  shell's read-only utilities. Deliberately absent: `find` (`-delete`), `awk`
  (`system()`), `sed` without `-n`, `env`, `gh api`, `cargo run`, `git branch
  -d`. A compound command is tested **one simple command at a time** — every
  segment must match — and a substitution, a redirection to a file, a group, a
  here-doc or a leading assignment is never matched (`bash_segments`). That is
  a parser for the purpose of NOT admitting, the one job `docs/tool-survey.md`
  allows a shell parser in a gate. Precedence: shipped < `~/.config/letibot/
  permission.json` < `$LETIBOT_PERMISSION`, last match wins; a `deny` row
  outranks everything. The `preapproved` disclosure counts them.
- **Always allow** — offered on every prompt whose tier is not always-ask, exec
  included (the 2026-09-11 rule *"exec asks every time"* is revised: a session
  GRANT is still never offered for exec; a durable RULE, in a file the operator
  reads and edits, is the operator's own preapproval). For `bash` the rule is
  the program and its verb (`cargo run --bin x` → `cargo run*`); for a file
  tool, the path. Written to `permission.json` through a sink; the row says
  when it could not be.
- **`ps`** — read-only; `pattern` / `pid` / `children_of` / `top cpu|mem`;
  pid, ppid, age, state, CPU%, RSS, command line; never lists this process;
  `PROTECTED` and `job` marks as `pkill`. Seated with the shell.
- **`leticode` seats the shell by default** (`--no-bash` to refuse it), the
  way opencode's coder has bash. What it runs unasked is the list above.

**Sudo — SETTLED 2026-09-14, shape 1.** The operator's ask: *"if model wants a
sudo i must be able to enter password safely and let it run … some other shim …
i honestly dont care now"*. Built as `SUDO_ASKPASS` routed to the head:

- Every command of a host session runs with `SUDO_ASKPASS` pointing at
  `letibot-askpass` (a fourth binary in the harnessd crate) and a `sudo` **shim**
  ahead of `PATH` (`crate::sudo`, written once to `$XDG_RUNTIME_DIR/letibot/
  shims`) that execs the real sudo with `-A` — except for `sudo -n`, left alone,
  so a probe still answers *no* honestly. The daemon also puts `LETIBOT_SOCKET`,
  `LETIBOT_SESSION` and `LETIBOT_COMMAND` in every command's env (a new
  `HostProcesses::set_standing_env` / `set_path_prefix`).
- The command itself passed the gate first — `sudo …` is privilege escalation,
  always-ask — so the operator saw and admitted it before any password.
- The helper attaches as an `askpass` head and sends one `Askpass` frame
  (PROTOCOL_VERSION 12). The daemon raises `SecretRequested` to every head; the
  TUI shows a card naming the command and sudo's prompt with a **masked field
  that owns the keyboard** (dots on screen, never the composer, never its
  history); the head answers with a `Secret` frame; the password goes head →
  daemon → helper → sudo's stdin and **nowhere else** — no log, no view, no
  transcript, no `CommandIssued`. The log gets `SecretSettled { given, by }`,
  the record without the secret. Deadline two minutes; Esc refuses; a late or
  duplicate answer is a `secret_late` warning.
- Tests: `letibot-sessionlog`'s `askpass` socket test (the secret reaches only
  the helper; refusal and late answer are honest; the hub state and log never
  hold it); the TUI `password_field` test (dots, keyboard ownership, no history);
  `letibot-tools`… the `sudo::install` shim test (adds `-A`, leaves `-n`); and
  the live `sudo_live` (SUDO_LIVE=1) that runs the box's real sudo through the
  shim and gets its wrong-password refusal, not a tty error.

Shape 2 (`Defaults timestamp_type=global`) and shape 3 (a detached privileged
shell) were the alternatives; shape 1 is the one that keeps the secret off the
log and scopes to the session. Not yet done: the remote/ACP heads have no
password card (only the TUI does), so a session driven only by one of those
still cannot answer a sudo — the frames are there, the UI is not.

The behaviour when no head answers is the one the scan shows Claude Code
already has: probe `sudo -n true`, announce the refusal, write the root half as
a script for the operator, and stop — never `script -q` or `echo '' | sudo -S`.

---

## R15 — a leticode session started at `/`, and searched it — **SETTLED 2026-09-14 (claude-lab2x1)**

Measured twice, driving local GLM as a one-shot coder on this repository: `read
crates/flowy/src/context.rs` → `no file`, because the whole-host backend joined the
relative path to its root and the root is `/`. The model then did the reasonable
thing — `glob` and `grep` from `/home` down for the file — and the daemon read a
27 GB model shard into memory and spent twelve minutes in the kernel with no head
able to reach it. Four fixes, all with tests:

- `HostBackend` has a `cwd`; relative paths and a command's `cwd` start there
  (`with_cwd`, `ExecBackend::workdir`); the harness sets it to the workspace for
  the `/`-rooted backend; the disclosure says `relative paths start at DIR`.
- `grep` opens no file over 16 MiB, stops a rung after 512 MiB read, and the
  result counts what it did not open. `read` refuses a file over 256 MiB by size
  before opening it. A walk never enters `/proc`, `/sys`, `/dev`, `/run`.
- The VM copy leaves out `.git/worktrees`: firecode's shared-tree guard counted
  the source's 28 and refused the copy.
- A NEW session's open-time notes reach the banner and the head (`open_note`):
  the project store choosing `writes allowed` over `--mode allow-all` used to go
  into a report only a resumed session printed.

After the fix the same GLM one-shot read the file, made the edit, added the two
tests (5/5 pass) and, having no shell, said it could not run them rather than
inventing a result line. DeepSeek, live, did the same on `grep.rs` for ≈ $0.02
(`docs/providers.md` "Verified").

**Then, the same afternoon: both models in a VM each.** `--where firecode` /
`letibot --vm` place a ROOT session in a VM (the placement a subagent already
had), `--vm-arg` hands `firecode up` its options, one-shots get their own
socket. DeepSeek and local GLM each wrote the `read` ceiling test and ran
`cargo test` inside — 10 min / $0.075 and 35 min respectively — after both
discovered the guest lacks llama.cpp and the sqlite dev symlink and stubbed them
outside the tree. `docs/cookbook.md` §4a. Then, the operator's rule — *"allow-all should be the
true allow-all"* — `Mode` gained a boundary axis: `allow-all` is `Structural`
and admits the always-ask list (`sudo -n id -u` in a VM: `0`, no ask); the
flow rule (a secret across the boundary) still refuses. Layers follow the copy
(`firecode layer inherit`, in the firecode tree, uncommitted). **Open:** the
layer itself for this project (toolchain, llama.cpp fork, `libsqlite3-dev`) —
needs docker, which lab2x1 lacks.

**Open, small.** `Mode::ALLOW_ALL` on a bare host is refused by its confinement
prerequisite and the project store overrides `--mode` — both by design — but a
one-shot with no adjudicator then has a `bash` that fails closed on every call.
A one-shot wanting a shell needs `--bash` and an adjudicator, or a VM. Whether
`--mode` on the command line should beat the store row is the operator's call.

---

## R14 — inject the fabric's skills and memories into the session — **SETTLED 2026-09-14 (claude-lab2x1)**

Done: `letibot_flowy::context` + `Sessions::with_fabric` / `refresh_fabric`, the
`fabric` disclosure, live test (8 skills, 42 memories; cache served with the node
away). Not run through a `--flowy` daemon end to end on this box: the seat's reader
is held by another session's listener and `Seat::open` refuses, as it should.
`docs/flowy-monitor.md` §5c.

*"3. I want flowy skills, memories etc to be injected. how? skills are summaries of
full pages, memories are titles."* At session open (and after a compaction), when
the daemon holds a seat: the shelf's skills as one line each (title + first
paragraph as the summary), memories as titles, into a `fabric` block of the system
prompt, so the model knows what exists and loads a body through `skill` /
`flowy get` when it needs it. **Offline mode** (`docs/tool-design-brief.md` §3b,
`docs/closed-loop.md` §5): a node that is away is a declared state, the block is
served from the last cached copy on disk and says its age; no seat, no block, said
so in the disclosure.

**Done when.** A session's system prompt carries the block; a compaction refreshes
it; a stalled seat serves the cached block labelled with its age; a session with no
seat has no block and the disclosure says `fabric: OFF`.

---

## R7 — A turn that ends inside its own reasoning is reported as success — **SETTLED 2026-09-13, see TODO-settled.md (R7) — a6b970e**

---

## R8 — `edit`'s near-miss recovery is defeated by exactly the error this model makes — **SETTLED 2026-09-12, see TODO-settled.md (R8) — ad23f39**

**Measured 2026-09-11**, in the live letibot session, twice in one turn.

The model sent `old_string` beginning `                if!text.is_empty() {`. The file
holds `                if !text.is_empty() {` at line 2543 — **one missing space** after
`if`. That is a reproduction artefact of this model (` !` versus `!`), not a typo, and
it is the same family as R7: GLM emitting something that is not quite the bytes it read.

The edit failed, which is correct. What is wrong is that **both** recovery paths in
`builtins/edit.rs::no_match` are defeated by that single space:

- `probe()` normalises whitespace, indentation and case — but normalising cannot
  **re-insert a deleted** character, so it finds nothing.
- `anchor_lines()` anchors on *"the longest whitespace-delimited token"* of the first
  line. Deleting the space **merges two tokens into one** — `if!text.is_empty()` —
  producing a token that occurs nowhere in the file.

So the tool honestly reports *"nothing in `…` matches that text, and no part of your
first line occurs anywhere in it either"* and then prints the file's **first 20 lines**,
which for `crates/tui/src/app.rs` is its module doc comment: pure waste, and no closer
to the answer. Two rounds burned, and the operator watching.

`text.is_empty() {` **does** occur, at 2543. One shorter anchor would have landed on it
and the model would have seen its own missing space.

**Still open?** `grep -n -A20 'fn no_match' crates/tools/src/builtins/edit.rs` — the
`_ =>` arm that dumps `file.lf.lines().take(20)`.

**Where.** `anchor_lines`, in the same file. When the longest token does not occur, fall
back to progressively shorter ones — next-longest, then any token over a few characters —
before giving up. Print the candidate lines, not the top of the file.

**Done when.** An `old_string` differing from a real line by one inserted or deleted
whitespace character comes back with that line and its number, and a test in this file
asserts it on the `if !text` case verbatim. §2.1's rule is the bar: the miss is
self-correcting **in the same call**.

---

## R9 — a refusal says `host_other` about a path inside the workspace — **SETTLED 2026-09-12, see TODO-settled.md (R9) — 545d9d7**

**Seen 2026-09-10** in the live session, on an `edit` of `crates/tui/src/app.rs`:

    reading: ask — intents [write_file] over [host_other]

The path is plainly inside the workspace, and the classifier that **decides** agrees:
`GateCall::path_is_inside` returns true for a relative path with no `..`, so
`ActionClass::host(access, inside, creates)` yields `EffectScope::HostProject`. The
grant key is right.

What is wrong is the **reporting**. The `host_other` in that line comes from
`crate::intent`'s `Region`, a different classifier written for shell-argument analysis,
whose `region_of` falls back to `Region::HostOther` for anything it cannot place. It
reaches the operator through the baseline string.

**Why it is worth fixing rather than tolerating.** It cost real time: the wrong scope in
that message was the first hypothesis for why `allow_session` did not stick, and it was
wrong — the cause was the mode's grant scope. A misleading fact in a refusal is worse
than no fact, because a refusal is what somebody reads when they are already confused.

**Still open?** `grep -rn 'region_of' crates/tools/src/intent.rs` and read the
`Region::HostOther` fallback at the end.

**Where.** Either give `region_of` the workspace so a file path resolves the way
`path_is_inside` does, or keep the baseline silent about region for a call whose class
was decided from a path. The second is smaller and loses nothing.

**Done when.** A gated `edit` inside the workspace reports a project-scoped region, or
none, and never `host_other`. A test on the rendered refusal payload, not on the class.

---

## R10 — layer 1's two env-hygiene lines, which `bash` waits on — **SETTLED 2026-09-12, see TODO-settled.md (R10) — 61afc57**

`docs/boundary-and-adjudication.md` §5 states these as **requirements rather than
assumptions**, *"because a requirement crosses a merge where an assumption does not"* —
and they are what stands between the exec substrate and
`Surroundings::with_pinned_shell` being honestly callable:

1. **`env_clear()` before the explicit `env` pairs**, so `PATH` is *pinned* rather than
   inherited.
2. **Unset `BASH_ENV`, `ENV`, `SHELLOPTS`, `BASHOPTS`.** `BASH_ENV` **is** sourced by bash
   for non-interactive shells, so a distribution where `/bin/sh` is bash has a real
   injection point this box does not — which is exactly the kind of thing that is true
   here and false one machine over.

**Why this is the prerequisite and not caution.** *"A resolved parse is not a resolved
meaning."* A grammar reads text; a shell resolves a bare command name through aliases,
functions and `PATH`, none of which are **in** the text — which is how the survey's best
parser is defeated. So `intent::ShellTrust` defaults to `Unknown`: a bare name is
unresolved and the command is `not_run`, while an absolute path is not shadowable and
passes. Layer A can only mean something if layer 1 pins what the words resolve to.

The gap is small because `exec/host.rs` already spawns `/bin/sh -c`, non-interactive and
non-login, so no rc file is read, and `/bin/sh` here is `dash`, which reads `$ENV` only
when interactive.

**Still open?** `grep -rn 'env_clear\|BASH_ENV' crates/tools/src/exec/host.rs`

**Where.** The spawn path in `crates/tools/src/exec/host.rs`. `Prereq::Confinement`
already exists as the seam to assert it against.

**Done when.** The spawn clears the environment before setting its own pairs, the four
variables are unset, a test asserts a `BASH_ENV` planted in the parent does not reach the
child, and `Surroundings::with_pinned_shell` is called where it was previously only
described. Note what this does **not** do: it does not seat `bash` — see N4 and the
transcript-edge choke point, which is the other half.

---

## R11 — the audit rows are written and never read — **SETTLED 2026-09-13, see TODO-settled.md (R11) — 74abade**

---

# 2. NEEDS A NOD — small question first, then unblocked

## N5 — the agent should know the view is hermetic, and propose the grant it needs

Operator, 2026-09-11: *"i guess the agent should know it is hermetic and prompt me with
his idea of shared data which i can allow."*

**Measured 2026-09-11, and it is why this matters now.** The confined backend builds on
this box — `bubblewrap 0.11.1`, cgroup entered, nested-namespaces held, no-new-privs
held — and it is `writable + EXEC`, so confinement costs no file access. But its view is:

    project /home/dead/Projects/letibot (rw); 19 read-only system paths;
    $HOME is a FRESH TMPFS at /run/letibot/home
      (so a build cache under $HOME is empty every run); no grants
    egress: DENIED — no interfaces, no routes, no DNS

So `cargo test` cannot run: `~/.cargo` is absent and there is no network to refetch it.
That is the boundary working — *"a secret outside it is ABSENT, not denied"* — pointed at
a toolchain instead of a secret.

### Most of this exists

`exec/confine.rs` already has the mechanism, and its discipline is the interesting part:

- `Grant::{ReadOnly, ReadWrite, AgentSocket}`, each carrying a **`why`**, because *"a
  grant nobody can explain is a grant nobody can revoke"*
- `ViewSpec::granting()`, and `Boundary::describe` prints **the consequence next to the
  grant** — `ReadOnly` means readable *into the transcript*, since §3's second half is not
  enforced by that module
- the `agent_from_env` precedent: when a key is needed there is no grant that binds it.
  The socket is bound, the key is not, and *"the error path is the important half"* — the
  refusal names the mechanism nobody built rather than quietly widening

### What is missing is the loop, and it is three things

1. **The model does not know it is hermetic.** The boundary description is a startup
   banner for the operator; nothing puts it in the model's context, so a failure inside
   the view looks like a broken toolchain rather than an absent one.
2. **An absence does not say what would fix it.** `cargo: command not found` or a missing
   registry should come back as *absent because outside the view*, with the grant that
   would change it — the same shape as `edit`'s read-before-write refusal and R8's
   near-miss, self-correcting in the same call.
3. **Grants are construction-time.** `ViewSpec` is built before the spawn; nothing adds
   one for the rest of a session after an operator approves it.

### The questions, which is why this is a nod

**Does a grant go through the gate like any other decision?** It should — the prompt is
*"the model wants `~/.cargo` read-only, because: to run the workspace's tests"*, and
`grants session` then makes it stick for the session, which is the machinery that already
exists. The consequence line has to be in the prompt, not only in the banner: binding
`~/.cargo` read-only means its contents can reach the transcript.

**And which asks must be refused rather than prompted?** The ssh case is the precedent
and the flow rule (§3) is the test: a grant that would make secret bytes *readable* is
inexpressible, not adjudicable, however politely the model asks for it. A model that can
propose grants must not be able to propose that one and have it arrive as an ordinary
prompt. `NEVER_WRITE` is the first precheck and the tier mints no `Adjudicable` for an
inexpressible action — so the pieces are there; what is undecided is that a
model-proposed grant is routed through them rather than around them.

**Smallest version that closes the loop for tests:** the view learns one grant shape
(`ReadOnly` on a path), the model is told the view is hermetic and what it holds, an exec
failure names the absence, and the grant prompt carries its `why` and its consequence.
That is N4's answer too — see there for why a shaped runner is wanted rather than `bash`.

---

## N4 — a coder can write a test and cannot run it

**Observed 2026-09-11**: letibot wrote a regression test and then had no way to execute
it. Not a bug — a consequence, and the role table shows it plainly:

    coder()       write,edit + bash       <- §8.4's spec
    m2_coder()    write,edit, NO bash     <- what the daemon seats
    m2_runner()   bash, NO write,edit     <- the only role with a shell

**No implemented role can both change the code and run it.** This is the same shape as
`todo` before it was seated, one level up and costlier: there the encoder was missing, here
it is the *verifier*. A model that writes and cannot check is the closed loop of
`docs/closed-loop.md` §2 left open at the point where it would have paid.

**Why `bash` is not the answer, and the reason is good.** From the daemon's own
disclosure: *"`bash` is the tool whose result is an **arbitrary byte stream**. The job
verbs and `monitor` are seated and shaped."* §5's choke point — the single place every
tool result would pass through — does not exist, so nothing stops a result carrying bytes
from inside the view into the transcript. Every other tool's result is shaped: `read`
returns numbered lines, `grep` returns matches, the job verbs return structured state.
Seating `bash` on `coder` would widen the hole and add a capability by side effect, which
is exactly what `m2_coder`'s comment refuses.

**Nor does role switching help yet.** The mechanism exists (branch `role-switch`: the
registry holds the union, `ToolRuntime::active` gates admission, so a switch moves no
prompt byte) — but switching to `runner` still needs `bash` to be seated somewhere, so it
is blocked on the same §5 hole.

### The question, which is why this is a nod and not a task

**Is a shaped test verb acceptable where a shell is not?**

A `test` tool that takes **no command from the model**, runs the workspace's test command,
and returns a *shaped* result — passed, failed, and per-failure the test name and its
assertion — is not an arbitrary byte stream. `cargo test`'s output is parseable into
exactly those fields.

It narrows the hole rather than closing it: a failing assertion can still print whatever
the test printed, so the result is bounded to *test-framework output* instead of *anything
at all*. That is a real reduction and not zero, which is precisely why it wants a decision
rather than an implementation. Per-failure spill and truncation are available if the
answer is "yes, but bounded".

### Measured 2026-09-11, after this was filed — the blocker is not the choke point

A confined backend **builds on this box** (bwrap 0.11.1, cgroup entered, nested-namespaces
held) and is `writable + EXEC`, so it costs no file access. `needs_exec_backend()` is
`matches!(self, Seat::Runner)` — one line from covering `Coder`.

Two things stand in the way and neither is §5's transcript edge:

1. **Nothing starts a process.** `m2_runner` seats `job_list`, `job_output`, `job_wait`,
   `job_kill` and `monitor` — the verbs that *manage* a job. `bash` is the only one that
   *starts* one, and it is off. A shaped starter is the gap.
2. **`cargo` cannot run inside.** `$HOME` is a fresh tmpfs, so `~/.cargo` is absent, and
   egress is denied so it cannot be refetched. See **N5** — the boundary already has a
   grant mechanism and what is missing is the loop that proposes one.

**If the answer is no**, the honest consequence should be written into the disclosure: a
`coder` session states that it can write tests and not run them, so the operator runs them
and nobody is surprised.

---

## N3 — a refusal claims "the operator has been told" without knowing it

From the same refusal, verbatim:

    The operator has been told, with this: grant `adj-…-0001` (edit) for this session,
    or answer the pending decision.

The operator had **not** been told: they were away from the keyboard, the 300-second
window expired, and when they came back they asked *"what decision"*. The prompt was
gone and the question was unrecoverable — the ask is not a transcript row, only the
refusal is.

This is the fleet's own rule inverted. *Guard the fact, not the proxy*: "a prompt was
emitted" is not "the operator saw it", and the sentence states the second while knowing
only the first.

**Why this needs a nod rather than a fix.** The minimal change is honest and small —
say what is true (*"a decision was raised and timed out unanswered"*) instead of
asserting delivery. The larger change is the one that would actually help: make a
pending or expired decision **recoverable**, so `what decision` has an answer after the
window closes. That is a question about what the transcript records, which is N1's
territory, and it should be decided with N1 rather than beside it.

**Still open?** `grep -rn 'has been told' crates/tools/src/` — the payload builder.

**The question:** does an ask become a transcript row, or does the head keep a durable
list of expired decisions? Either answers *"what decision"*; they differ in whether the
model sees it too.

---

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

**2026-09-13 (D13):** two of the six are now wired — *mode does not persist per
project* and the mode side of *`/mode` and `--mode` are unwired*. The per-project
`ModeStore` is threaded into `Parts` and consulted in `open_with` after the
workspace is resolved, so a session's point is its project's row (longest ancestor
wins), not the daemon's `--mode` default. A new named point `allow-all` (opencode's
`bypassPermissions`) ships, and `Mode::parse` accepts opencode's four permission-mode
names. Still open: the `/mode` head command to *move* a project at runtime, the
`todo_write` rename completion, and the remaining four D27 items.

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

## T10 — Contract gaps found by W6 — **items 1, 2, 3 and 5 SETTLED 2026-09-11, see TODO-settled.md (R1, R2, R3, R4)**

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

## T11 — §18.1-I1's observable form is not checkable on a hybrid model — **SETTLED 2026-09-11, plan text corrected, see TODO-settled.md (R5)**

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

## T4 — Verify what opencode actually sends — **SETTLED 2026-09-11, mechanism 2 refuted, see TODO-settled.md (R6)**

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

**A second cost, measured on a DENSE model, 2026-09-14 by `lubuntu1-lab` on .76
(Qwen3.8-27B Q6, llama.cpp, stock).** Different mechanism from the recurrent
accumulator above and it compounds with it:

- A saved prompt-cache state costs roughly the **full `-c` allocation, not the
  tokens actually used** — ~1.5 GiB each at `-c 8192`. So capacity is
  `cache-ram / per-saved-state`, and the 8 GiB default holds about five
  conversations. At 40 GiB: 16 of 16. Two levers, and only two: raise
  `--cache-ram`, or lower `-c`.
- Eviction is **capacity, not conflict**: interleaving A,B,A,B,A,B hits 94–95 %
  once each has been seen. So the number to size against is *how many distinct
  briefs are live*, not how they are ordered.
- Warm 644 ms median against cold 1986 ms; the first touch of a new conversation
  is always cold.
- **`--cache-reuse` is silently disabled on that model** — `cache_reuse is not
  supported by this context, it will be disabled`, logged with `--kv-unified`
  both true and false, so unified KV is not the gate. The 94 % hits are the
  slot's own longest-common-prefix match, not `cache_reuse`. Advice to rely on
  that flag is therefore **model-dependent**, and a measurement taken on one
  model does not port. Nobody has yet established what makes a context
  unsupported; the suspicion is an attention property rather than a flag.

Mitigation used, and worth generalising: the sweep harness refuses to start a condition
below 40 GiB `MemAvailable`. Any future experiment that prefills many distinct prompts
needs the same guard.

---

---

### 2026-09-11 — the sixth OOM, and why the disk tier did not save the turn

GLM was **killed by the kernel OOM killer** at 01:35:34 (`status=9/KILL`), taking a live
letibot turn with it: `malformed http response: connection closed mid-chunk: the server
went away before the terminating 0-length chunk`. Host RAM, not VRAM.

**The good news first, because it corrects a standing fear.** The spills survive a hard
kill and are re-used. 957 GB across 506 files, and the restarted server indexed them:

    L2: indexed 386 cache entries from disk (23,128,678 tokens, 931,079 MiB)

and then served **6,427,050 cached prompt tokens against 240,831 prefilled** — 26.7×
reuse, resuming a 171,901-token conversation rather than re-prefilling it. Critically,
`forcing full prompt re-processing` appears **zero** times, so the plan's "Root cause B"
— a disk restore that loaded the KV and then discarded it because `checkpoints` was
empty, which is exactly the hybrid/recurrent case — is **fixed** in this build. That is
the first evidence of it holding across a `SIGKILL`.

**What is wrong is that the tier is read-only in practice.** Every idle cycle logs

    flush: 0 sequence(s) with tokens, 0 saved, 0 already cached, 0 offloaded (0 saved),
           cache now 386 entries / 0.0 MiB

`0 sequence(s) with tokens` — by flush time no slot holds anything to save, which is
`spill-only-writes-cached-prompts` in the memory dir: *prompt_save is false for an
already-cached prompt, so live slots never spill*. So it serves brilliantly from what was
written before the kill and **adds nothing**. Any conversation that grows from here has
no disk entry, and that one really will prefill from zero after the next kill.

**And the shape of it is perverse.** The RAM tier is **write-back** — entries reach disk
on eviction or graceful shutdown — and write-back is precisely what a `SIGKILL` defeats.
`--cache-ram 81920` therefore both raises the chance of the kill and maximises what the
kill destroys. `/data` is a 7.3 GB/s NVMe with 5.3 T free; **write-through survives OOM
by construction** and the read side is already proven fast enough at 26.7× reuse.

**Also costing something:** 48 `index_disk` calls in one log, ~20 minutes apart, each
re-reading metadata for 931 GB across 386 entries — the sleep/wake cycle
(`--sleep-idle-seconds 600`) paying the index once per wake rather than once per start.

**The decision, and it is the operator's:** lower `--cache-ram` (less pressure, less
lost, more disk reads), or make the RAM tier write-through (survives the kill, costs
NVMe bandwidth that is measurably spare). Not a letibot change either way — it is
`~/bin/glm-flash-server` and llama.cpp's `server_prompt_cache`.

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

