# agents.md

What an agent working in this repo needs: the shape of the code, the rules the
operator works by, and the rakes already stepped on once. The design writing lives in
`docs/`; this file is the orientation layer.

## What this is

letibot is a local coding-agent harness in two processes, and the split is the design:
`harnessd` (crates/harnessd) owns the sessions, the hash-chained ledger, the model connection
and the permission gate; the TUI (crates/tui) is one head among several that may attach to a
daemon over a unix socket. Close the TUI mid-turn and the turn keeps running. The wire contract
is `crates/sessionlog/src/protocol.rs`, and `PROTOCOL_VERSION` at the top of it is the number
every compatibility sentence refers to.

## Layout

Workspace crates, roughly bottom-up:

| crate | is |
|---|---|
| vt | a terminal screen, modelled: the bytes a full-screen program writes, applied to a grid |
| transcript | the item model (user/assistant/tool rows), media types, and the citation-fence rule |
| tokencore | the vocabulary (a backend trait, plus the byte vocabulary), ledger, token region, store |
| llama | the GGUF vocabulary backend: llama.cpp FFI and the C shim; the only crate linking llama.cpp, behind harnessd's `local` feature |
| dialect, dialect-glm, dialect-qwen | prompt templates per model family (jinja in `template/`) |
| http | a hand-written blocking HTTP/1.1 client, and the one retry rule |
| provider, backend | model endpoints, and the abstraction the daemon talks to |
| websearch, webfetch | tool backends; webfetch is the curl-subprocess reader-mode fetcher |
| turn | the agent loop: engine, tool loop, compaction salvage |
| sessionlog | hub, store, wire protocol, event types, `testing.rs` fixtures |
| tools | the tool union: builtins, exec, adjudication/intent, files |
| code | code-intelligence helpers |
| metrics | the event table, and the one way to send an event |
| flowy, room | the fabric seat as a monitor, and the per-tag room sessions coordinate through |
| plugins, plugin-sdk, builtins | wasm panes and hooks, the guest SDK, and the layers that ship in the binary |
| tui, ui | the head (`app.rs` is the brain), and the editor and drawing pieces it builds on |
| leticode-config, claude, opencode | the instruction files a seat reads, and the two conversation importers |
| bridge, letibot | the Claude Code bridge tools, and the `letibot` CLI |
| harnessd | the daemon binary |

Elsewhere: `docs/`, `scripts/` (the `letibot` and `leticode` wrappers' source of truth),
`tests/fidelity`, `config/`, `plugins/`.

## How it runs on this box

`~/bin/letibot` and `~/bin/leticode` are installed wrappers (repo copies in `scripts/`) that
start or attach to the folder's daemon and exec the TUI from `target/release`. Consequences:

- `cargo build` alone does not change what the operator runs. After merging a TUI or daemon
  change, `cargo build --release` too, or the restarted leticode silently runs the old binary.
- A running process keeps the old inode after a rebuild; `/proc/<pid>/exe` showing `(deleted)`
  is how you tell a stale TUI from a broken feature.
- One daemon per folder; socket and a pid record under `$XDG_RUNTIME_DIR/letibot/`.
  `letibot --status` and `--stop` act on this folder's daemon by pid.
- The model server on 127.0.0.1:8080 is protected. Never kill it, never restart it, and do not
  let a broad test run talk to it: every test named `*_live.rs` does real I/O, generations
  against the model server or network calls. Scope test runs to the crate you touched
  (`cargo test -p letibot-tui`), not the workspace, unless the operator asked for the live ones.

## Merge gate

The merge queue runs these in its own landing worktree (`~/.local/share/letibot/landing/<repo>-<hash>`,
never inside `.git`; a session of the repository can read it, not write it), after rebasing
an approved branch onto the target's tip and before moving the target; the first that fails parks
the branch, and a step still running after thirty minutes is killed with its process group and
the row is `timed_out` (R61). A step naming `install-gate-build` is not run with the gate: it runs
only after the landing was PUSHED (R61 decision 7), so nothing is installed that is not on the
remote. The same steps (without the install) gate the target's own tip whenever it changes. Read from `main`'s copy of this file, so a
branch cannot change its own gate. **The gate runs on this box or on the machine `[queue]
gate_host` names, never on GitHub Actions** (R62: the operator was rate limited there, the boxes
are ours, and a workflow judges a push rather than the rebased branch the queue is about to land).
`docs/gate.md` is how to gate - through the queue and by hand - and `.github/workflows` holds the
release, which no branch starts.

**The four design-shaped lints are exempt on the clippy line, and the reason is here rather than in
68 `#[allow]`s in the source.** `result_large_err` (29 findings), `type_complexity` (15),
`large_enum_variant` (13) and `too_many_arguments` (11) are each a decision about an API's shape, and
the tree does not meet them today. Suppressing them item by item would hide them where nobody looks
again; exempting them on the line keeps them **running** - a plain `cargo clippy --all-targets`
still reports every one, and the sweep still counts them - while the queue stops demanding them
tonight. Making them true is a slice of its own. Everything else stays `-D warnings`.

**The clippy step goes through `scripts/clippy-gate.sh` because a warm target dir can answer
for it.** Cargo's fingerprint for a unit does not include the arguments after `--`, so a unit
already built by a plain `cargo clippy` or a `cargo check` in the same target dir is fresh for
`-- -D warnings`, and the step can exit green having linted nothing. The script touches every
workspace member's root first, which costs the re-lint and nothing else.

**`scripts/check-test-attrs.sh` rejects a `#[test]` attribute that is not on a function of its
own.** Two test attributes separated only by comments and other attributes attach both to one
function, and the test between them silently stops running while the suite stays green. It is the
gate's second step, so a branch that arrives with the shape is red before anybody reads it.

```sh
sh scripts/check-fmt.sh main
sh scripts/check-test-attrs.sh
sh scripts/clippy-gate.sh --all-targets -- -D warnings -A clippy::result_large_err -A clippy::type_complexity -A clippy::large_enum_variant -A clippy::too_many_arguments
LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture
cargo build --release --bins
sh scripts/install-gate-build.sh
```

**The test step REQUIRES the apparatus it needs, and that env prefix is the whole of the
reason.** A test with no vocabulary GGUF SKIPS itself through
[`letibot_tokencore::apparatus::present`], which prints its reason and returns — and a test that
returns is a test that PASSED, as far as the step's status is concerned. MEASURED on this box:
`LETIBOT_VOCAB_GGUF=/tmp/definitely-not-a-gguf cargo test -p letibot-harnessd --lib review_pass`
reports **`10 passed`** for ten tests that never ran, and the same command with
`LETIBOT_REQUIRE_APPARATUS=1` reports **`10 failed`**. So without the prefix the gate can report
review coverage it never had — the same class of defect as a gate that proves a binary it never
installs. The default vocabulary path does resolve on the box this was written on
(`$HOME/models/qwen3.8-flash-next/`), so the prefix changes nothing there and is what makes a box
that lacks it go RED rather than green.

**Two things can be absent, and the prefix is only about one of them.** A vocabulary GGUF, a
delegated cgroup v2 subtree or a store fixture is NOT PROVISIONED — real on this platform, absent
on this box, and somebody can go and get it — which is what `LETIBOT_REQUIRE_APPARATUS=1` refuses,
because a skip nobody can refuse is a suite that quietly stops testing. A test whose apparatus the
PLATFORM can never have — `bwrap` and unprivileged user namespaces, cgroup v2, `systemd-run`,
`ss` over `/proc/net/unix` — goes through `letibot_tokencore::apparatus::applies` instead, which
says `NOT APPLICABLE on <os>` and is deliberately not refusable, or carries
`#[cfg(target_os = "linux")]` where the test itself only means anything there. **A test that
cannot apply on this OS is not missing apparatus.** The queue's daemon runs on the operator's Mac,
so collapsing the two made THIS step red there on four targets for a reason that was in no diff,
and a gate whose red is ignored has stopped carrying information.

**And step 3 links whichever llama.cpp the FIRST build of a worktree found.** Each of the five
`build.rs` scripts searches for it (`build-glm/bin` before `build/bin`, `LETIBOT_LLAMA_*` first)
and bakes the answer into an rpath — an input `cargo` cannot see, because `rerun-if-env-changed`
only fires when the VARIABLE changes, not when the search finds something else. MEASURED
2026-10-26: the landing worktree's `harnessd` carried `LC_RPATH …/llama.cpp/build/bin`
(llama.cpp 0.3.0, which does not know this model's architecture) on a box that had a working
0.4.0 in `build-glm/bin`, so 42 vocabulary targets stayed red where the same tests passed by
hand. Name both `LETIBOT_LLAMA_DIR` and `LETIBOT_LLAMA_LIB` on the steps that build, or remove
that worktree's `target/` once.

**The gate judges CODE, and this box's own data is not code — which is why one class of test opted
out of it.** `crates/turn/tests/restore.rs`'s real-store tests read
`~/.local/share/letibot/sessions.db`, the operator's history, and so they run only when
`LETIBOT_CHECK_REAL_STORE=1` is set: a red there is a fact about the store and not about the diff
(MEASURED 2026-10-26 — the `--workspace` step failed on one corrupted transcript,
`s-gatekeeper#t0`, and no change to the tree could have fixed it), `LETIBOT_REQUIRE_APPARATUS=1`
deliberately does NOT turn their skip into a failure, and the queue was "red most of the time
everywhere" so its result "is ignored" — a gate whose red is ignored is a constant that has stopped
carrying information. The canary those tests hold is still worth running, BY HAND, on the box that
has the store:

```sh
LETIBOT_CHECK_REAL_STORE=1 cargo test -p letibot-turn --test restore -- --nocapture
```

It is what found a stored row whose `tok_offset` did not continue its predecessor — real corruption
nothing else in this tree looks for.

**The first four are PROOF and not INSTALLATION, and the fifth is the gap.** The queue runs
every step with `CARGO_TARGET_DIR` pointed at `<worktree>/target` (`mergequeue.rs`'s
`GATE_TARGET_DIR`), so a branch is never judged with the main tree's artifacts — right, and it
means the `cargo build` above leaves the main tree's `target/release` exactly as it was. That
is the directory the launcher starts: `~/bin/letibot` is a symlink to `scripts/letibot`, which
resolves `BIN` to `<repo>/target/release` and execs `$BIN/harnessd` and `$BIN/letibot-tui`
from there, and `letibot-askpass` is looked for beside the daemon's own binary. So a gate that
only proved was a gate whose landings were never installed. MEASURED 2026-10-10: a daemon came
up at 20:55 running code from 17:44 — three hours and seventeen landings behind, with no
per-workspace queue and no queue views in it — and restarting it could not fix that, because
the file it execs had never been rebuilt.

`scripts/install-gate-build.sh` copies what the fourth step built into the main tree's
`target/release` (the artifact that passed, which is what `main` becomes one step later — see
the script for why the other shape, building the main tree, installs the tree the gate did not
judge). It copies beside the name and renames, because a running daemon holds the old inode and
a `cp` over a running binary fails with `Text file busy`; it prints what it proved, the SHA it
built and the path it installed into, so the row says where the binary came from. **A daemon
already running keeps the old one until it is restarted** — `letibot --status` says so, and
`letibot --stop && letibot` is what picks the new binary up.

*(This section is read from `main`, so a change to the gate itself lands under the gate it
replaces: the landing after this one is the first that installs.)*

## The operator's standing rules

- Work in git worktrees `~/Projects/letibot-<topic>` on a topic branch; merge to main only with
  explicit approval, merge commits with a descriptive body.
- Commit style: lowercase sentence subject, long explanatory body that says why, no
  `Co-Authored-By` or session trailers.
- Network egress (crates.io, web): ask first, every time.
- Never stage the operator's WIP in other checkouts (e.g. `~/Projects/rano`).
- The shell adjudicator refuses compound commands with parameter expansion (`$var`, `$(...)`);
  write literal spellings chained with `;`. Inline `git -c` config angers the operator.
- `origin/main` runs stale; pushes need the credential-helper ask.

## Patterns that recur

A test asserts what it can prove on the machine it is running on. A test that panics because the
box has no vocabulary GGUF, no cgroup v2 subtree or no session store is reporting absent apparatus
as a failed assertion - two findings collapsed into one, which is the mistake
`ToolOutcome::Abstained` exists to prevent in the runtime ("could not ask" is not "the answer is
no"). Use `letibot_tokencore::apparatus`:

```rust
let Some(_) = letibot_tokencore::apparatus::present_gguf() else { return };
let Some(_) = letibot_tokencore::apparatus::present(
    "a process-lifetime tree (cgroup v2; process groups on macOS)",
    letibot_tools::host_tree().is_ok(),
) else { return };
```

The skip is loud: it names what is missing and prints `THIS IS NOT A PASS`.
`LETIBOT_REQUIRE_APPARATUS=1` turns it back into a failure, so a run on a box that has the
apparatus can refuse to skip. Set `LETIBOT_REQUIRE_MODEL=1` alongside it and a run that means it
says so.

An apparatus the platform can never have is a different call, and the Merge gate section above states
it: `apparatus::applies` says `NOT APPLICABLE on <os>` and is deliberately not refusable. A test that
cannot apply on this OS is not missing apparatus.

What is not apparatus: a configured endpoint that refuses. The two look identical at the socket
and are not the same fact. `LETIBOT_COMPLETION_URL` set is a promise about where the model is, and
silence against a promise is a failure; unset, the address is a default nobody asked for and
silence is a runner, a fresh clone or a laptop. If you add a skip, ask which of the two you are
looking at.

A second copy of a list drifts. The head once kept its own `const` of mode names and offered a mode
that did not exist. Protocol 18 added `SettingRow::choices` so that whoever owns a closed set of
names ships it with the setting: when the daemon owns a set, the head renders the daemon's, never
a local fallback list.

Screens and cards are different shapes. Full-body screens (help, stats, the session picker, the
panes) replace the transcript and are dispatched in `App::screen`'s body chain. Cards (the
permission ask, the sudo prompt, the mode card) ride in the ask card's slot at the front of the
chrome, with the transcript visible above, and share its fit-loop protection. One list on the
screen at a time: every opener closes the others, and a card steps aside while a decision is up,
because a second cursor under the ladder would be a cursor nothing moves.

Key ownership has a fixed precedence: decision ladder, then session picker, then other cards and
panes, then the composer. The empty-composer gate holds for the picker and the cards, but an open
decision owns Enter even with a line typed - a line naming an option is that answer, and any other
line is held back to the composer while the marked row answers - because a permission arriving
mid-typing would otherwise turn Enter into "send the half-thought". New screens join the Esc-close
block, the scroll guard if full-body, and the wheel router: wheel and page keys move what is on
the screen, a screen on top swallows them, and the transcript scrolls only when nothing is up. A
test walks every pane, so a pane that lets the wheel reach the transcript fails there instead of
on somebody's screen.

Behind a running turn, the operator's queue is one message. Consecutive operator prompts coalesce
(engine-side in `Pending::absorb`, mirror-side in `submit()`) into one User item at the boundary;
notices (harness injections, monitor firings) stand alone and split the run, because they are not
the operator's words and must not wear the attribution. Up on an empty composer recalls the queue
for editing and sends `WithdrawPrompts` (protocol 19), so the edited resend replaces the original
instead of stacking onto it; a half-typed line keeps the editor's own Up, which is readline history
and never a take-back. When a queued row lands, `retire_pending` retires its echo by exact match or
front-strip, because a notice that split the run leaves pieces.

A button whose effect the gate would decline is never shown. The permission ladder orders its
rules: tier first (`AlwaysAsk` to allow_once/deny/deny_and_tell), then exec-class (a shell never
takes a session grant; a durable allow-always rule is offered), then the mode's grant scope -
`permission_options` at a `Session` point, `once_only_options` at a `Once` point. Two guards for
one decision, only one kept in step, is how `allow_session` "did not stick".

Click arithmetic is recorded by the frame, not recomputed. A click arrives without a repaint, so
the render stores what the click needs: `picker_rows_drawn` for the session picker,
`mode_first_row` and `mode_rows_drawn` for the mode card. The card trusts clicks only when the
whole card survived the frame, because a click into a list nobody saw whole would pick a row
nobody saw.

The fit loop drops rows in a fixed order: completions, hint, notice, composer height, stuck line,
box, then the card. The most expendable goes first and the thing that must stay answerable goes
last. New chrome joins that ladder deliberately, not at the end by default.

TUI tests live in `mod tests` beside the code (`crates/tui/src/app/`, `crates/tui/src/ui/`) and
assert on rendered screens: `app()`, `hello()`, `brief()` and `typed()` are the helpers,
`a.screen(w, h)` both returns the lines and records the click facts, and a refusal is checked
through `a.notice`. Mirror an existing test's shape before inventing one.

A row of the sessions tree is not a session. `App::session_rows()` draws a project's line, a
`system` line, a group's line and a session's row into one `Vec<SessionRow>`, and
`SessionRow::idx` is the index into `App::sessions` while a row's position is not - indexing by
position panics one past the end, which reads as a fixture with too few sessions. Every reader goes
through `idx`. Where a row GOES is one function (`App::placements`, taking the project from the root
of the parent chain, so a review clone under a temp dir is not a project), and `sessions_feed` ships
both marks to the built-in layer because a guest cannot walk a store. What a row SAYS exists twice,
because the layer is the copy a project overrides: one equality test holds the two together
(`sessions_list.rs::the_heads_list_and_the_plugins_list_draw_the_same_rows`) plus two committed
pictures re-blessed with `LETIBOT_BLESS=1`, so a change to the tree's shape is a change to those
files. `letibot --sessions` is a third shape over the same marks (`harnessd::cli::grouped`), and one
session can sit differently in the two pictures.

webfetch's curl transport resolves then pins (`--resolve`), refuses private addresses (the model
server on loopback is why), walks redirects manually, caps the body, and splits curl's metadata by
a `-w` sentinel with `rfind`, so a forged body cannot lie about headers.

## Rakes, already stepped on

- `scratch/` is gitignored. The one file a build needed from it, `answer-key.tsv`, lives at
  `crates/tools/tests/data/`, beside the test that reads it; nothing in the build reads from
  `scratch/`, and it stays that way.
- Scoping by crate is not enough to stay off the model: `harnessd`'s `compact`, `loop_closes`,
  `resume`, `slash`, `todos` and `wired`, and `turn`'s `compaction` and `engine_decisions`, load
  the GGUF through llama.cpp and abort with a CUDA error while the model server holds the GPU. The
  invocation that stays clear names every non-model target: `cargo test -p <crates> --lib --test
  askpass --test late_head --test resume_frames --test screen --test sessions --test restore`.
- htmd keeps `<script>` content by default; webfetch passes `skip_tags`
  (script/style/noscript/template) and a test pins it.
- Rust `format!` needs `%{{http_code}}` to emit curl's `%{http_code}`.
- The curl option is `--dump-header` (singular), not `--dump-headers`.
- readable-rs is rejected as a dependency (yanked `kuchikikiki`); webfetch uses dom_smoothie, htmd
  and html2text.
