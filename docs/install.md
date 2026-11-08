# Installing letibot

letibot installs as two launchers, four binaries and the four llama.cpp libraries `harnessd` links.
It is a harness, a daemon and a head. Nothing in the archive is a model, and no turn is answered
until one is reachable.

## From the published archive

```sh
curl -fsSL https://raw.githubusercontent.com/deadtrickster/letibot/main/install.sh | sh
```

That puts ten files in `~/.local/bin`; `LETIBOT_INSTALL_DIR` changes it. They are the launchers
`letibot` and `leticode`, the daemon `harnessd`, the head `letibot-tui`, `letibot-askpass`, the
Claude Code bridge `letibot-mcp` (the MCP server the `leticode` plugin runs,
[claude-plugin/](../claude-plugin/README.md)), and the llama.cpp libraries `libllama.so.0`,
`libggml.so.0`, `libggml-cpu.so.0` and `libggml-base.so.0`.

The libraries sit beside the binaries because that is what `$ORIGIN` resolves, so an install needs
no `LD_LIBRARY_PATH` and no llama.cpp checkout of its own. On macOS the same four are `.0.dylib`
files and `@loader_path` does the same job.

Published for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and `aarch64-apple-darwin`.
Other platforms take the source path.

`LETIBOT_VERSION` picks a tag or branch; `LETIBOT_FROM_SOURCE` set to anything skips the download
and builds instead.

## Three libraries come from the host

The archive carries the llama.cpp four and nothing else. A minimal container has none of these, so
`install.sh` checks for them and refuses by name before it copies anything, rather than letting you
find out from an exit 127 afterwards:

| needed | why | install it with |
|---|---|---|
| `libstdc++.so.6` | the C++ runtime `libllama` and `libggml*` link | `libstdc++6` (dnf/apk: `libstdc++`) |
| `libgomp.so.1` | OpenMP, from `libggml-base` and `libggml-cpu` | `libgomp1` (dnf, apk: `libgomp`) |
| `libsqlite3.so.0` | SQLite, which `harnessd` links | `libsqlite3-0` (dnf, apk: `sqlite-libs`) |

They are named rather than bundled on purpose: a shipped `libstdc++` is a compatibility claim
nobody has measured, since it has to match the host's libc, and one older than the host's fails
worse and more mysteriously than a missing one.

The published binaries are glibc. On musl - Alpine and its relatives - they cannot run at all,
whatever is installed, and `install.sh` says so rather than offering an `apk add` that would not
help. Use the source build there.

The one-liner itself needs `curl` to fetch the script and `tar` to unpack the asset. `wget` is not
a substitute. On a box with neither `curl` nor a toolchain, install `curl` first.

## From source

Without `curl`, `install.sh` builds from source, which needs more rather than less:

```sh
LETIBOT_FROM_SOURCE=1 sh install.sh
```

That needs `git`, Rust 1.98 (edition 2024), a C compiler, and a built llama.cpp checkout, because
`harnessd` links its tokenizer for local models. Point it at one with `LETIBOT_LLAMA_DIR` (holding
`include/llama.h`) and `LETIBOT_LLAMA_LIB` (holding `libllama.so.0`).

On a Mac, llama.cpp must be built with `@loader_path` rather than `$ORIGIN`:

```sh
cmake -S llama.cpp -B llama.cpp/build -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_INSTALL_RPATH=@loader_path
cmake --build llama.cpp/build -j "$(sysctl -n hw.ncpu)"
LETIBOT_LLAMA_DIR=$PWD/llama.cpp LETIBOT_LLAMA_LIB=$PWD/llama.cpp/build/bin cargo build --release
```

## Cloud only, without llama.cpp

If the turns go to DeepSeek, GLM or Grok and never to a local model:

```sh
cargo build --release --no-default-features --bins
cargo test --workspace --exclude letibot-llama --no-default-features
```

That drops the `local` feature: nothing links libllama, the daemon serves cloud providers, and
pointing it at a local model is refused with a sentence saying this build has no llama.cpp. Tests
that need a model vocabulary skip and say so; `LETIBOT_REQUIRE_APPARATUS=1` turns a skip into a
failure. CI's `no-llama` job builds and tests exactly this.

The daemon still keeps its hash-chained ledger in tokens, because that is what makes a session
resumable and tamper-evident. With no GGUF those tokens are the byte vocabulary (ids 0-255 are
bytes, the dialect's control literals get reserved ids above), which never leaves the machine. The
context arithmetic is rescaled by the provider's own token counts after the first turn. Such a
session cannot be switched to a local model - its ids mean nothing to a llama-server - and says so.

## What you do not get

A model. Nothing answers a turn until one is reachable, and that gap is a model rather than a
package: no amount of installing closes it.

A model chosen. `letibot` brings up a daemon and attaches the head to whatever is serving. On the
first message, if the session's model cannot be reached, the head opens the `/models` chooser with
the reason. A cloud key is asked for once on a masked card (the one sudo's password uses, so it
never enters the session log) and saved to `~/.config/letibot/providers.toml`, mode 0600, as this
box's default. A local `llama-server` is the other door: give it a `[model."local.NAME"]` block
with its `url` in `~/.config/letibot/providers.toml`. `--provider deepseek|glm|grok` is a shortcut
past the chooser, not a requirement. `$DEEPSEEK_API_KEY` and the others, or a hand-written
`[deepseek] key = "..."`, still work and are never asked about.

A local model needs its vocabulary. `--vocab` names the GGUF it serves, and every control token the
dialect uses (`<|im_start|>`, `<think>`, `<tool_call>`, ...) must be one entry in it - checked at
start, refused by name. None of the small `ggml-vocab-*` files llama.cpp ships passes for Qwen 3.x.

## On macOS

The same tree builds and runs natively on Apple silicon. What a Mac answers differently, and where
it is weaker than Linux, is written down rather than smoothed over:

- Process lifetime is process groups, not cgroups. Each command leads its own group and ending a
  scope is `killpg`, with the same presence, kill, absence record. A program that deliberately
  leaves its group (`setsid`, a double-forking daemon) escapes the scope, which a cgroup does not
  allow. `crates/tools/src/exec/scope.rs` has the rest.
- There is no namespace boundary, so the confined seats (`runner`, `coder`) refuse to start and say
  so. `leticode`, the launcher's default, runs on the host and is unaffected; for a confined
  session put it in a firecode VM with `--vm` once firecode's macOS build is installed.
  [firecode-on-macos.md](firecode-on-macos.md) is the standalone guide to that.
- "Is this run waiting for input?" cannot be read; Linux reads it from `wchan`. An operator's `!`
  run says once that it could not tell.
- The runtime dir is the per-user temp dir (`getconf DARWIN_USER_TEMP_DIR`), since there is no
  `$XDG_RUNTIME_DIR` or `/run/user`.
- The launcher needs `python3`, which comes with the Command Line Tools (`xcode-select --install`).

## Running from a checkout

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

The model endpoint is any OpenAI-shaped server. The dialects exist because the local ones disagree
about how a tool call is spelled.
