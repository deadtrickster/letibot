# agents.md

Orientation for an agent working in this repo: the shape of the code, the operator's rules, the
rakes already stepped on. Design writing lives in `docs/`.

## What this is

A local coding-agent harness in two processes. `harnessd` (crates/harnessd) owns sessions, the
hash-chained ledger, the model connection and the permission gate. The TUI (crates/tui) is one head
that attaches to a daemon over a unix socket, so closing it mid-turn does not stop the turn. The
wire contract is `crates/sessionlog/src/protocol.rs`; `PROTOCOL_VERSION` there is the compatibility
number.

## Layout

| crate | role |
|---|---|
| vt | terminal screen model: bytes applied to a grid |
| transcript | item model (user/assistant/tool rows), media types, citation-fence rule |
| tokencore | vocabulary trait, byte vocabulary, ledger, token region, store, `apparatus` |
| llama | GGUF vocabulary backend (llama.cpp FFI); the only crate linking llama.cpp |
| dialect, dialect-glm, dialect-qwen | prompt templates per model family |
| http | blocking HTTP/1.1 client and the one retry rule |
| provider, backend | model endpoints, and the abstraction the daemon talks to |
| embeddings | embedding endpoint client |
| websearch, webfetch | tool backends; webfetch is the curl-subprocess reader-mode fetcher |
| turn | agent loop: engine, tool loop, compaction |
| sessionlog | hub, store, wire protocol, events, `testing.rs` fixtures |
| tools | the tool union: builtins, exec and confinement, adjudication |
| code | code-intelligence helpers |
| github | GitHub reviewer seat: pull requests behind one trait |
| metrics | event table and the one way to send an event |
| flowy, room | fabric seat as a monitor, per-tag rooms |
| plugins, plugin-sdk, builtins | wasm panes and hooks, guest SDK, layers compiled into the binary |
| tui, ui | the head (`app/` is the brain), editor and drawing pieces |
| leticode-config, claude, opencode | instruction files a seat reads, and the two conversation importers |
| bridge, letibot | Claude Code bridge tools, the `letibot` CLI |
| harnessd | the daemon binary |

Elsewhere: `docs/`, `scripts/` (source of the `letibot` and `leticode` wrappers), `tests/`,
`config/`, `plugins/`.

## How it runs on this box

`~/bin/letibot` and `~/bin/leticode` are wrappers (copies in `scripts/`) that start or attach to
the folder's daemon and exec the binaries from `target/release`.

- `cargo build` does not change what the operator runs. After a TUI or daemon change, also
  `cargo build --release`.
- A rebuilt binary does not replace a running process. `/proc/<pid>/exe` showing `(deleted)` marks
  a stale process.
- One daemon per folder. `letibot --status` and `letibot --stop` act on this folder's daemon.
- The model server on 127.0.0.1:8080 is protected: never kill or restart it. Tests named `*_live.rs`
  do real I/O against it or the network. Scope runs to the crate you touched (`cargo test -p
  letibot-tui`), not the workspace.

## Merge gate

The merge queue runs these steps in its own landing worktree after rebasing an approved branch onto
the target's tip. The first failure parks the branch. A step running over thirty minutes is killed
(`timed_out`). `install-gate-build` runs only after the landing was pushed. The same steps without
the install gate the target's tip when it moves. The section is read from `main`, so a branch cannot
change its own gate. The queue reads the first fenced block under this heading, one command per
line. It runs on this box or the host `[queue] gate_host` names, never on GitHub Actions.
`docs/gate.md` covers gating through the queue and by hand.

```sh
sh scripts/check-fmt.sh main
sh scripts/check-test-attrs.sh
sh scripts/clippy-gate.sh --all-targets -- -D warnings -A clippy::result_large_err -A clippy::type_complexity -A clippy::large_enum_variant -A clippy::too_many_arguments
LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture
cargo build --release --bins
sh scripts/install-gate-build.sh
```

- `check-fmt.sh` format-checks only the Rust files the change touches. The tree is not rustfmt-clean.
- `check-test-attrs.sh` rejects a `#[test]` not on a function of its own. Two attributes separated
  only by comments attach to one function and one test stops running.
- `clippy-gate.sh` touches every workspace member's root first. A warm target dir otherwise answers
  `-D warnings` from a fresh plain-clippy fingerprint and lints nothing.
- The four design-shaped lints are exempt on the clippy line instead of by `#[allow]`. A plain
  `cargo clippy --all-targets` still reports them.
- `LETIBOT_REQUIRE_APPARATUS=1` turns a test's missing-apparatus skip (no vocabulary GGUF, no cgroup
  v2 subtree, no store fixture) into a failure. Without it a skipping test returns and counts as
  passed. Apparatus the platform can never have goes through `apparatus::applies` and is not
  refusable.
- Each of the `build.rs` scripts bakes the llama.cpp it first found into an rpath, and cargo does
  not see that input change. If vocabulary targets are red on a box with a working llama.cpp, set
  `LETIBOT_LLAMA_DIR` and `LETIBOT_LLAMA_LIB` on the build steps or delete that worktree's `target/`.
- `crates/turn/tests/restore.rs` real-store tests read the operator's `sessions.db` and run only
  with `LETIBOT_CHECK_REAL_STORE=1`. Run them by hand on the box that has the store:
  `LETIBOT_CHECK_REAL_STORE=1 cargo test -p letibot-turn --test restore -- --nocapture`.
- The queue builds with `CARGO_TARGET_DIR=<landing worktree>/target` (`GATE_TARGET_DIR`).
  `install-gate-build.sh` copies the built binaries into the main tree's `target/release` beside the
  name and renames, since a running binary rejects `cp` with `Text file busy`. A running daemon keeps
  the old binary until `letibot --stop && letibot`.

## The operator's standing rules

- Work in a worktree `~/Projects/letibot/wt-<topic>` beside the main checkout
  (`~/Projects/letibot/letibot`), on a topic branch. Merge to main only with explicit approval.
- Commits: lowercase subject saying what changed, short optional body, no `Co-Authored-By` or any
  attribution trailer.
- Never `git -c` inline config. Never bypass hooks.
- Build machines lab2x1, lubuntu1 and lubuntu2 may fetch build dependencies without asking. Any
  other network egress asks first. lubuntu3 is never used.
- Gates run remotely: `scripts/rrun NAME WORKTREE --host H [--target DIR] --gate` (header of
  `scripts/rrun` has the full usage; default host lab2x1). The Mac runs only platform-touching tests.
- Never stage the operator's WIP in other checkouts (e.g. `~/Projects/rano`).
- The shell adjudicator refuses compound commands with parameter expansion (`$var`, `$(...)`). Write
  literal spellings chained with `;`.
- Pushing: a top-level seat gets `GH_TOKEN` and a credential helper (`crates/tools/src/exec/seat.rs`).
  A child session cannot push. Push only when asked.
- Comments are 2-3 lines and say why. `scripts/comment-blocks.py count|list FILE...` finds longer
  blocks. `scripts/same-code.py BASE [PATH...]` proves a comment-only change leaves the code tokens
  identical.

## Patterns that recur

A test asserts what it can prove on the machine it runs on. A panic because the box lacks a GGUF,
a cgroup v2 subtree or a session store reports absent apparatus as a failed assertion. Use
`letibot_tokencore::apparatus`:

```rust
let Some(_) = letibot_llama::apparatus::present_gguf() else { return };
let Some(_) = letibot_tokencore::apparatus::present(
    "a process-lifetime tree (cgroup v2; process groups on macOS)",
    letibot_tools::host_tree().is_ok(),
) else { return };
```

The skip is loud and says `THIS IS NOT A PASS`. `LETIBOT_REQUIRE_APPARATUS=1` makes it a failure.
`LETIBOT_REQUIRE_MODEL=1` does the same for the model server skip. A configured endpoint that
refuses is not apparatus: with `LETIBOT_COMPLETION_URL` set, silence is a failure. Unset, it is a
default nobody asked for.

A second copy of a list drifts. `SettingRow::choices` carries the daemon's closed set of names with
the setting. The head renders those, never a local fallback list.

Screens and cards differ. Full-body screens (help, stats, session picker, panes) replace the
transcript. Cards (permission ask, sudo prompt, mode card) ride in the ask card's slot with the
transcript visible, and share its fit-loop protection. One list on screen at a time: every opener
closes the others, and a card steps aside while a decision is up.

Key ownership: decision ladder, then session picker, then other cards and panes, then composer. An
open decision owns Enter even with a line typed. A line naming an option answers it, any other line
stays in the composer. New screens join the Esc-close block, the scroll guard if full-body, and the
wheel router. A test walks every pane to check the wheel does not reach the transcript.

Behind a running turn the operator's queue is one message. Consecutive operator prompts coalesce
(`Pending::absorb` in the engine, `submit()` in the mirror). Notices (harness injections, monitor
firings) stand alone and split the run. Up on an empty composer recalls the queue and sends
`WithdrawPrompts`. `retire_pending` retires a queued row's echo by exact match or front-strip.

Never show a button the gate would decline. The permission ladder orders its rules: tier first
(`AlwaysAsk` offers allow_once/deny/deny_and_tell), then exec class (a shell never takes a session
grant), then the mode's grant scope: `permission_options` at a `Session` point,
`once_only_options` at a `Once` point. Keep both guards in step.

Click arithmetic is recorded by the frame, not recomputed. A click arrives without a repaint, so
`App::screen` (`crates/tui/src/ui/screen.rs`) stores what a click needs. Trust a card's click only
when the whole card survived the frame.

The fit loop drops rows in a fixed order: completions, hint, notice, composer height, stuck line,
box, then the card. New chrome joins that ladder deliberately.

TUI tests live in `crates/tui/src/app/tests/` and assert on rendered screens. Helpers: `app()`,
`hello()`, `brief()`, `typed()`; `a.screen(w, h)` returns the lines and records click facts. A
refusal is checked through `a.notice`. Mirror an existing test.

A sessions-tree row is not a session. `App::session_rows()` returns `Vec<SessionRow>` mixing project,
`system`, group and session rows. `SessionRow::idx` indexes `App::sessions`, the row's position does
not. `App::placements` decides where a row goes. `sessions_feed` ships the marks to the built-in
layer. The head and the plugin layer draw rows twice, held together by
`sessions_list.rs::the_heads_list_and_the_plugins_list_draw_the_same_rows` and committed pictures
re-blessed with `LETIBOT_BLESS=1 cargo test -p letibot-tui sessions_list`. `letibot --sessions`
(`harnessd::cli`) is a third shape over the same marks.
Packages of OTHER daemons are not in `App::sessions`: `App::fleet` (read in the background by
`letibot_bridge::fleet`) draws as a `packages in other daemons` node after the projects, in both
drawings, and Enter says the attach command. The feed carries the fleet rows already worded.

webfetch's curl transport resolves then pins (`--resolve`), refuses private addresses (the model
server on loopback is why), walks redirects manually, caps the body, and splits curl's metadata by
a `-w` sentinel with `rfind`, so a forged body cannot lie about headers.

## Rakes

- `scratch/` is gitignored. `answer-key.tsv`, which a test reads, lives at
  `crates/tools/tests/data/`. Nothing in the build reads `scratch/`.
- Test targets that load the GGUF abort with a CUDA error while the model server holds the GPU.
  `grep -li gguf crates/*/tests/*.rs` finds them. Run `--lib` and named non-model `--test`
  targets instead.
- htmd keeps `<script>` content by default. webfetch passes `skip_tags`
  (script/style/noscript/template) and a test pins it.
- Rust `format!` needs `%{{http_code}}` to emit curl's `%{http_code}`.
- The curl option is `--dump-header`, singular.
- readable-rs is rejected (yanked `kuchikikiki`). webfetch uses dom_smoothie, htmd and html2text.
