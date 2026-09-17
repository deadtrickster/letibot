# agents.md

What an agent working in this repo needs: the shape of the code, the rules the
operator works by, and the rakes already stepped on once so nobody steps on them
twice. The deep design writing lives in `docs/`; settled calls with their dates
live in `DECISIONS.md`; this file is the orientation layer.

## What this is

letibot is a local coding-agent harness in two processes, and the split is the
design, not an accident: `harnessd` (crates/harnessd) owns the sessions, the
hash-chained ledger, the model connection and the permission gate; the TUI
(crates/tui) is one head among several that may attach to a daemon over a unix
socket. Close the TUI mid-turn and the turn keeps running. The wire contract
between them is `crates/sessionlog/src/protocol.rs` — `PROTOCOL_VERSION` at the
top of that file is the number every compatibility sentence in the codebase
refers to.

## Layout

Workspace crates, roughly bottom-up:

| crate | is |
|---|---|
| transcript | the item model (user/assistant/tool rows) |
| tokencore | tokenisers, llama.cpp FFI |
| dialect, dialect-glm, dialect-qwen | prompt templates per model family (jinja in `template/`) |
| http, provider | model endpoints |
| websearch, webfetch | tool backends; webfetch is the curl-subprocess reader-mode fetcher |
| backend | model abstraction the daemon talks to |
| turn | the agent loop: engine, tool loop, compaction salvage |
| sessionlog | hub, store, wire protocol, event types, `testing.rs` fixtures |
| tools | the tool union: builtins, exec, adjudication/intent, files |
| code | code-intelligence helpers |
| tui | the head: `app.rs` is the brain (~11k lines), plus driver/term/render/markdown/prefs |
| ui | the editor and shared drawing pieces the TUI builds on |
| harnessd | the daemon binary |

Elsewhere in the tree: `docs/` (design brief, per-subsystem notes, `tui-testing.md`
is worth reading before touching TUI tests), `scripts/` (the `letibot`/`leticode`
wrappers' source of truth), `tests/fidelity`, `scratch/` (untracked; see gotchas),
`config/`.

## How it runs on this box

`~/bin/letibot` and `~/bin/leticode` are installed wrappers (repo copies in
`scripts/`) that start or attach to the folder's daemon and exec the TUI from
**`target/release`**. Consequences:

- `cargo build` alone does not change what the operator runs. After merging a
  TUI or daemon change, `cargo build --release` too, or the restarted
  leticode silently runs the old binary. (Paid for once: merged, rebuilt
  debug, operator restarted, "no picker".)
- A running process keeps the old inode after a rebuild — `/proc/<pid>/exe`
  showing `(deleted)` is how you tell a stale TUI from a broken feature.
- One daemon per folder; socket and a pid record under
  `$XDG_RUNTIME_DIR/letibot/`. `letibot --status` / `--stop` act on this
  folder's daemon by pid.
- The model server on 127.0.0.1:8080 is **protected**. Never kill it, never
  restart it, and do not let a broad test run talk to it: every test named
  `*_live.rs` (`turn/tests/live_qwen.rs`, `sessionlog/tests/live_e2e.rs`,
  `harnessd/tests/*_live.rs`, `tools/tests/firecode_live.rs`,
  `websearch/tests/brave_live.rs`, `flowy/tests/live.rs`) does real I/O —
  generations against the model server or network calls. Scope test runs to
  the crate you touched (`cargo test -p letibot-tui`), not the workspace,
  unless the operator asked for the live ones.

## The operator's standing rules

- Work in git worktrees `~/Projects/letibot-<topic>` on a topic branch; merge
  to main only with explicit approval, merge commits with a descriptive body.
- Commit style: lowercase sentence subject, long explanatory body that says
  why, no `Co-Authored-By` / session trailers.
- Network egress (crates.io, web) — ask first, every time.
- Never stage the operator's WIP in other checkouts (e.g. `~/Projects/rano`).
- The shell adjudicator refuses compound commands with parameter expansion
  (`$var`, `$(...)`); write literal spellings chained with `;`. Inline
  `git -c` config angers the operator.
- `origin/main` runs stale; pushes need the credential-helper ask.

## Patterns that recur

**A second copy of a list drifts.** The head once kept its own `const` of mode
names; it offered a mode that did not exist and could not reach one that did.
Protocol 18 added `SettingRow::choices` so whoever owns a closed set of names
ships it with the setting. When the daemon owns a set, the head renders the
daemon's — never a local fallback list.

**Screens and cards are different shapes.** Full-body screens (help, stats,
the session picker, the panes) replace the transcript and are dispatched in
`App::screen`'s body chain. Cards (the permission ask, the sudo prompt, the
mode card) ride in the ask card's slot at the front of the chrome, transcript
visible above, and share its fit-loop protection. One list on the screen at a
time: every opener closes the others, and a card steps aside while a decision
is up because a second cursor under the ladder would be a cursor nothing
moves.

**Key ownership has a fixed precedence**, all gated on an empty composer so a
half-typed line always means the line: decision ladder, then session picker,
then other cards/panes, then the composer. New screens join the Esc-close
block and (if full-body) the scroll guard; a bottom card does not join the
scroll guard — the wheel scrolls the transcript behind it.

**Click arithmetic is recorded by the frame, not recomputed.** A click
arrives without a repaint, so the render stores what the click needs:
`picker_rows_drawn` for the session picker, `mode_first_row` +
`mode_rows_drawn` for the mode card — and the card trusts clicks only when
the whole card survived the frame, because a click into a list nobody saw
whole would pick a row nobody saw.

**The fit loop drops rows in a fixed order** (completions, hint, notice,
composer height, stuck line, box, then the card): the most expendable first,
the thing that must stay answerable last. New chrome joins that ladder
deliberately, not at the end by default.

**TUI tests are in-file** (`mod tests` in app.rs) and assert on rendered
screens: `app()`, `hello()`, `brief()`, `typed()` are the helpers;
`a.screen(w, h)` both returns the lines and records the click facts;
refusals are checked via `a.notice`. Mirror an existing test's shape before
inventing one.

**webfetch's curl transport** resolves then pins (`--resolve`), refuses
private addresses (the model server on loopback is why), walks redirects
manually, caps the body, and splits curl's metadata by a `-w` sentinel with
`rfind` so a forged body cannot lie about headers.

## Rakes, already stepped on

- `scratch/` is untracked and **not** in worktrees; `crates/tools/src/intent.rs`
  `include_str!`s `scratch/man-scrape/eval/answer-key.tsv`, so tools tests fail
  in a fresh worktree until that file is copied over.
- htmd keeps `<script>` content by default; webfetch passes `skip_tags`
  (script/style/noscript/template) and a test pins it.
- Rust `format!` needs `%{{http_code}}` to emit curl's `%{http_code}`.
- The curl option is `--dump-header` (singular), not `--dump-headers`.
- Pre-existing warnings not worth chasing: `tokencore/ffi.rs` dead
  `llama_detokenize`, `tools/intent.rs` unread `provenance`,
  `harnessd/harness.rs` `supplies`, `tui/render.rs` `unused_mut`.
- readable-rs is rejected as a dependency (yanked `kuchikikiki`); webfetch
  uses dom_smoothie + htmd + html2text.
