# letibot

A local-first LLM agent harness: **one authoritative reader**, a gate that answers before the
model acts, and a runtime that behaves like a normal data app.

![letibot's TUI, mid-turn](docs/screenshot.jpg)

Two things above, in the same frame: the model's work in flight — `Running "cargo build …" · 4.2s`
and a yellow `[3 tools, 2 thinking]` on the sentence it belongs to — and, on its own permanent row
above the composer, `Responded in 12.4s at 21:07`. The counters go yellow while the work is still
going and stop when it is; the row above the input never moves.

## Features

- **Daemon and heads.** Sessions live in `harnessd`; close the terminal and the turn keeps going.
  Attach several heads to one conversation, resume any session from the store.
- **Local or cloud.** llama.cpp locally, or DeepSeek, GLM, Grok. Switch per session, per subagent.
- **Gate before every tool call.** Approve once, or "always allow" a pattern; the reasoning is
  stored with the decision.
- **Subagents as real sessions.** Each one attachable and steerable mid-run (`task_message`), in
  its own git worktree (`task_start`) or a [firecode](https://github.com/deadtrickster/firecode) VM.
- **Merge queue.** A finished branch is reviewed by a gatekeeper subagent that never sees the
  author's report, rebased onto main's tip, run through the repository's own gate, then
  fast-forwarded. One queue serves every repository under the workspace.
- **Per-repository gate.** A `## Merge gate` section in `AGENTS.md`. The agent proposes one from
  what the repo has — `make test`, cargo, go, npm, pytest, `act` for `.github/workflows` — and you
  pick.
- **Background jobs and monitors.** Long commands go to the background, settle into the
  conversation when done; conditions can be watched across turns.
- **Graduated compaction.** Context is summarised in levels, each one saying what it dropped.
- **An editor inside.** [rano](https://github.com/deadtrickster/rano) as a pane: `ctrl-e` opens any
  file, a click on an edit shows the whole file with the change, `alt-s` sends your place to the
  prompt.
- **Shell in the conversation.** `!cmd` runs it, `!term` keeps a terminal pane that survives
  detaching.
- **Built for long sessions.** Scrollback pins the prompt of the turn you are reading; a held view
  does not move while the model streams; themes and colours are yours (`head.toml`).

## What it is

`harnessd` is a daemon. `letibot-tui` is one of its heads. A head speaks the session protocol over
a unix socket; **the daemon is the only reader of the session log**, so two heads can be attached to
one conversation without either of them being a second writer. That single-reader rule is §13.2 and
most of the design follows from it.

The runtime does not try to be clever about the model. It tries to be *legible* about it:

- **Every tool call can be gated before it runs.** The gate is asked first, the tool second, and
  what the gate was shown is stored as the trail — not reconstructed later.
- **Compaction is graduated.** Context is summarised in levels rather than in one blocking pass, and
  each level discloses what it cost and what it lost.
- **The wire carries facts, not summaries of facts.** A head can attach to a session that has been
  running for hours and draw it correctly, because the protocol says what happened rather than what
  a client should think about it.
- **Nothing moves that the reader did not ask to move.** A queued message keeps its place; a row
  that has been drawn is not redrawn elsewhere; a counter that stopped is drawn differently from one
  that is counting. This is the constraint the UI work is tested against, and it is why the
  screenshots in the docs are compared byte for byte.

## The crates

| crate | |
|---|---|
| `letibot-transcript` | The conversation record: what the harness stores, and what a dialect renders. |
| `letibot-dialect`, `-qwen`, `-glm` | The seam between the turn engine and whatever actually runs the model. |
| `letibot-turn` | The turn engine: one turn, from a transcript to a transcript. |
| `letibot-sessionlog` | The session log and the head protocol. |
| `letibot-tools` | The tool runtime and the built-ins. |
| `letibot-code` | The structural half of code search: what is defined in this file, where. |
| `letibot-tui` | The terminal head. |
| `letibot-harnessd` | The daemon that assembles the parts into a working harness. |

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/deadtrickster/letibot/main/install.sh | sh
```

That puts **eight files** in `~/.local/bin` (set `LETIBOT_INSTALL_DIR` to change it): the launcher
`letibot`, the daemon `harnessd`, the head `letibot-tui`, `letibot-askpass`, and the four llama.cpp
libraries the daemon links — `libllama.so.0`, `libggml.so.0`, `libggml-cpu.so.0` and
`libggml-base.so.0`. The libraries sit **beside the binaries** because that is what `$ORIGIN`
resolves, so an install needs no `LD_LIBRARY_PATH` and no llama.cpp checkout of its own. Published
for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and `aarch64-apple-darwin` (Apple
silicon); other platforms take the source path. On a Mac the libraries are the same four as
`.0.dylib` files beside the binaries (`@loader_path` is dyld's `$ORIGIN`), and see
[On macOS](#on-macos) for what differs there.

### What you do NOT get, so the next step is not a surprise

**A model.** The install is a harness, a head and the libraries they link. Nothing answers a turn
until a model is reachable, and a fresh box reaches this point and stops here — the gap is a model
rather than a package, and no amount of installing closes it.

**A running chat.** `letibot` brings up a daemon and attaches the head; with no model behind it, the
first thing you see is a connection error rather than a conversation. Start one first:

```sh
llama-server -m MODEL.gguf --port 8080      # or: --provider deepseek|glm|grok
```

**An older release may not carry the launcher.** It joined the archive after `v0.1.1`, so an install
from that release says so and points at the head directly instead of telling you to run a command it
did not install.

### What has to be on the machine already

The archive carries the four llama libraries, and nothing else. Three more come from the host, and a
minimal container has none of them — so `install.sh` checks for these and refuses *by name* before
it copies anything, rather than letting you discover it from an exit-127 afterwards:

| needed | why | install it with |
|---|---|---|
| `libstdc++.so.6` | the C++ runtime `libllama` and `libggml*` link | `libstdc++6` · `dnf`/`apk` `libstdc++` |
| `libgomp.so.1` | OpenMP, from `libggml-base` and `libggml-cpu` | `libgomp1` · `dnf` `libgomp` · `apk` `libgomp` |
| `libsqlite3.so.0` | SQLite, which `harnessd` links | `libsqlite3-0` · `dnf` `sqlite-libs` · `apk` `sqlite-libs` |

**They are named rather than bundled on purpose.** A shipped `libstdc++` is a compatibility claim
nobody has measured: it has to match the host's libc, and one older than the host's fails worse and
more mysteriously than a missing one.

**The published binaries are glibc.** On musl — Alpine and its relatives — they cannot run at all,
whatever is installed, and `install.sh` says so in its own words rather than offering an `apk add`
that could not help. Use the source build there.

### What the one-liner itself needs

`curl` to fetch the script, and `tar` to unpack the asset. **`wget` is not a substitute** — and on a
box with neither `curl` nor a toolchain, install `curl` first (`apt-get install -y curl`,
`apk add curl`, `dnf install curl`). This is the bootstrap limit rather than something the script
can work around: there is no way to download an installer without a downloader.

Without `curl`, `install.sh` falls back to building from source — which needs **more**, not less:
`git`, Rust, a C compiler, and a **built llama.cpp checkout**, because `harnessd` links the tokenizer
and that is not optional. Point it at one with `LETIBOT_LLAMA_DIR` (holding `include/llama.h`) and
`LETIBOT_LLAMA_LIB` (holding `libllama.so.0`).

`LETIBOT_VERSION` picks a tag (e.g. `v0.1.1`); `LETIBOT_FROM_SOURCE` forces the source path.

## On macOS

The same tree builds and runs natively on macOS (Apple silicon, measured on macOS 26). What a Mac
answers differently, and where it is weaker than Linux, is written down rather than smoothed over:

- **Process lifetime is process groups, not cgroups.** Each command leads its own group, and
  ending a scope is `killpg`, with the same presence → kill → absence record. A program that
  deliberately leaves its group (`setsid`, a double-forking daemon) escapes the scope, which a
  cgroup does not allow. `crates/tools/src/exec/scope.rs` (`ProcessGroups`) has the rest.
- **There is no namespace boundary**, so the confined seats (`runner`, `coder`) refuse to start
  and say so. `leticode` — the launcher's default — runs on the host and is unaffected; for a
  confined session put it in a firecode VM with `--vm` once firecode's macOS build is installed.
- **"Is this run waiting for input?" cannot be read** (Linux reads it from `wchan`); an
  operator's `!` run says once that it could not tell.
- **The runtime dir** is the per-user temp dir (`getconf DARWIN_USER_TEMP_DIR`), since there is
  no `$XDG_RUNTIME_DIR` or `/run/user`.
- **The launcher needs `python3`**, which on macOS comes with the Command Line Tools
  (`xcode-select --install`).

Building from source needs llama.cpp built with `@loader_path` rather than `$ORIGIN`:

```sh
cmake -S llama.cpp -B llama.cpp/build -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_INSTALL_RPATH=@loader_path
cmake --build llama.cpp/build -j "$(sysctl -n hw.ncpu)"
LETIBOT_LLAMA_DIR=$PWD/llama.cpp LETIBOT_LLAMA_LIB=$PWD/llama.cpp/build/bin cargo build --release
```

**A local model needs its vocabulary; a cloud provider does not.** For a local model `--vocab`
names the GGUF it serves, and every control token the dialect uses (`<|im_start|>`, `<think>`,
`<tool_call>`, …) must be one entry in it — checked at start, refused by name. None of the small
`ggml-vocab-*` files llama.cpp ships passes for Qwen 3.x. Under a provider no GGUF is needed: see
[Cloud only](#cloud-only).

## Cloud only

If the turns go to a cloud provider, nothing about llama.cpp is needed — not to build, not to run:

```sh
sh install.sh            # from a checkout, with no LETIBOT_LLAMA_* set: a cloud-only build
letibot                  # nothing local and no provider yet: it asks which one
```

**The key is asked for, not configured.** With no key on the box, the first message opens a
masked card (the one sudo's password uses, so the key never enters the session log) and the
key is saved to `~/.config/letibot/providers.toml`, mode 0600, with that provider as the
default — so the next `letibot` just starts. A key the provider refuses (401/403) is asked for
again on the same card. `$DEEPSEEK_API_KEY` (and the others) or a hand-written
`[deepseek] key = "…"` still work and are never asked about.

The daemon still keeps its hash-chained ledger in tokens, because that is what makes a session
resumable and tamper-evident; with no GGUF those tokens are the **byte vocabulary** (ids 0–255 are
bytes, the dialect's control literals get reserved ids above), which never leaves the machine. The
context arithmetic is rescaled by the provider's own token counts after the first turn, as it
already was. Such a session cannot be switched to a local model — its ids mean nothing to a
llama-server — and says so. llama.cpp is linked only by `crates/llama` (`letibot-llama`), behind
harnessd's `local` feature; `cargo build --no-default-features -p letibot-harnessd` leaves it out.

## Build and run

```sh
cargo build --release

# the daemon; it prints the socket a head attaches to
./target/release/harnessd \
    --dialect qwen --model qwen-3.8-27b --endpoint 127.0.0.1:8080 \
    --vocab path/to/model.gguf \
    --workspace . --store ~/.local/share/letibot/sessions.db

# a head, in another terminal
./target/release/letibot-tui
```

Requires Rust 1.98 and edition 2024. The model endpoint is any OpenAI-shaped server; the dialects
exist because the local ones disagree about how a tool call is spelled.

## Documentation

`docs/` holds the design documents the code argues against — `design-brief.md` for the thesis,
`compaction.md` and `closed-loop.md` for the two subsystems with the most measurement behind them,
and `boundary-and-adjudication.md` for the gate. `agents.md` states the conventions a contribution
is held to.

## Licence

Apache-2.0. `crates/ui` contains material ported from upstream projects; see `NOTICE` for the
attribution and for the files that carry modification notices.
