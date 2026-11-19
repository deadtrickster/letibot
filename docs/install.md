# Installing letibot

letibot installs as two launchers and six binaries. **The default install is cloud-only**: the
daemon serves a provider and links no llama.cpp, so nothing here needs a C++ runtime, OpenMP or a
llama.cpp checkout. Local models are opt-in, and the section with that name below is how. It is a
harness, a daemon and a head. Nothing in the archive is a model, and no turn is answered until one
is reachable.

## From the published archive

```sh
curl -fsSL https://raw.githubusercontent.com/deadtrickster/letibot/main/install.sh | sh
```

That puts eight files in `~/.local/bin`; `LETIBOT_INSTALL_DIR` changes it. They are the launchers
`letibot` and `leticode`, the daemon `harnessd`, the head `letibot-tui`, the multicall
`letibot-multi` (every other role, and the verbs the launcher hands on) with `letibot-askpass` a
link to it, the
Claude Code bridge `letibot-mcp` (the MCP server the `leticode` plugin runs,
[claude-plugin/](../claude-plugin/README.md)), and the ACP adapter `letibot-acp`, which is what Zed
and Neovim launch ([acp.md](acp.md)).

`LETIBOT_LOCAL=1` adds llama.cpp's four libraries and the binaries that link them; `install.sh
--plan` prints which of the two this machine would get, and changes nothing. An archive published
before the adapter exists installs the rest and says so, the way an archive without the bridge does.

Publishing is two assets per platform: `letibot-<triple>.tar.gz`, which the default install asks
for, and `letibot-<triple>-local.tar.gz`, which `LETIBOT_LOCAL=1` asks for. The first carries no
library at all and no binary of it may link llama.cpp; the release job fails if either is untrue.

Published for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and `aarch64-apple-darwin`.
Other platforms take the source path.

`LETIBOT_VERSION` picks a tag or branch; `LETIBOT_FROM_SOURCE` set to anything skips the download
and builds instead.

## One library comes from the host

A cloud install links nothing of llama.cpp's, so the only runtime library the archive does not
carry is SQLite. A minimal container does not have it, so `install.sh` checks for it and refuses by
name before it copies anything, rather than letting you find out from an exit 127 afterwards:

| needed | why | install it with |
|---|---|---|
| `libsqlite3.so.0` | SQLite, which `harnessd` links | `libsqlite3-0` (dnf, apk: `sqlite-libs`) |

A **local** install links llama.cpp, and those binaries need two more:

| needed | why | install it with |
|---|---|---|
| `libstdc++.so.6` | the C++ runtime `libllama` and `libggml*` link | `libstdc++6` (dnf/apk: `libstdc++`) |
| `libgomp.so.1` | OpenMP, from `libggml-base` and `libggml-cpu` | `libgomp1` (dnf, apk: `libgomp`) |

They are named rather than bundled on purpose: a shipped `libstdc++` is a compatibility claim
nobody has measured, since it has to match the host's libc, and one older than the host's fails
worse and more mysteriously than a missing one.

The published binaries are glibc. On musl - Alpine and its relatives - they cannot run at all,
whatever is installed, and `install.sh` says so rather than offering an `apk add` that would not
help. Use the source build there.

The one-liner itself needs `curl` to fetch the script and `tar` to unpack the asset. `wget` is not
a substitute. On a box with neither `curl` nor a toolchain, install `curl` first.

## From source

Without `curl`, `install.sh` builds from source, which needs `git`, Rust 1.98 (edition 2024) and a
C compiler (the tree-sitter grammars are C). It is **cloud-only unless a llama.cpp is named**:

```sh
LETIBOT_FROM_SOURCE=1 sh install.sh
```

The same build by hand is `cargo build --release --bins`, because `local` is off by default. CI's
`no-llama` job builds and tests exactly that.

## Local models

A daemon that serves a local `llama-server` needs llama.cpp's tokenizer, so it is the one thing an
install has to be asked for:

```sh
LETIBOT_LOCAL=1 sh install.sh      # the -local release asset
```

Or, from source, against a built llama.cpp checkout:

```sh
LETIBOT_LLAMA_DIR=$PWD/llama.cpp LETIBOT_LLAMA_LIB=$PWD/llama.cpp/build/bin \
    cargo build --release --bins --features local
```

`LETIBOT_LLAMA_DIR` holds `include/llama.h` and `LETIBOT_LLAMA_LIB` the built `libllama.so.0`. Both
are required; one without the other is refused by name rather than quietly built cloud. On a Mac,
llama.cpp must be built with `@loader_path` rather than `$ORIGIN`:

```sh
cmake -S llama.cpp -B llama.cpp/build -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_INSTALL_RPATH=@loader_path
cmake --build llama.cpp/build -j "$(sysctl -n hw.ncpu)"
```

A local install puts four libraries beside the binaries, because that is what `$ORIGIN` resolves,
so it needs no `LD_LIBRARY_PATH` and no llama.cpp checkout of its own. On macOS the same four are
`.0.dylib` files and `@loader_path` does the same job. A **cloud install removes those four** if an
older local install left them there, and says so: they sit where the loader looks, so leaving them
would claim a local model this build has not.

A cloud daemon still keeps its hash-chained ledger in tokens, because that is what makes a session
resumable and tamper-evident. With no GGUF those tokens are the byte vocabulary (ids 0-255 are
bytes, the dialect's control literals get reserved ids above), which never leaves the machine. The
context arithmetic is rescaled by the provider's own token counts after the first turn. Such a
session cannot be switched to a local model - its ids mean nothing to a llama-server - and says so.

`cargo test --workspace --exclude letibot-llama --no-default-features` is the no-llama test run.
`--no-default-features` is still needed there because `letibot-sessionlog` and `letibot-turn` keep
`local` on by default for their own vocabulary tests. Tests that need a model vocabulary skip and
say so; `LETIBOT_REQUIRE_APPARATUS=1` turns a skip into a failure.

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

## On Linux

letibot puts every tool call and job in its own cgroup (v2). It caps memory and CPU there, counts IO, and freezes or kills a whole job tree at once. The cgroups live under your user's systemd manager, so letibot can only use the controllers that manager hands down to it.

**What the stock setup gives.** On most distributions the user manager hands down `cpu memory pids`. That is enough for memory caps and job lifetimes. To check:

```sh
cat /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers
```

**IO accounting, throttling and the disk floor need `io` as well.** Without it, a job's bytes written are sampled from `/proc` and undercount short-lived children, `io.max` throttling is off, and the daemon says so at startup. The setup is done once, as root:

```sh
sudo mkdir -p /etc/systemd/system/user@.service.d
printf '[Service]\nDelegate=cpu memory pids io\n' | sudo tee /etc/systemd/system/user@.service.d/delegate-io.conf
sudo systemctl daemon-reload
```

It takes effect when your user manager next starts, which is at a login after all your sessions have ended, or at a reboot. Restarting the user manager (`systemctl restart user@$(id -u)`) kills every user process, tmux included, so do not use it on a box with work running.

**To turn it on now, without a restart.** The manager's own subtree file belongs to you:

```sh
echo +io > /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.subtree_control
```

This is done behind systemd's back, and a systemd reload may undo it. Repeat it after a reload, or rely on the drop-in at the next fresh start. Daemons that were already running keep the sampled fallback until they restart.

**To check that it worked:**

```sh
systemctl show user@$(id -u).service -p DelegateControllers   # cpu io memory pids
cat /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.subtree_control
```

Then start a daemon and read its startup lines: they say whether io accounting is live or sampled.

`letibot doctor` checks all of this, and the rest of the box, in one run.

**To undo it:** `echo -io > …/cgroup.subtree_control`, then remove `delegate-io.conf` and run `sudo systemctl daemon-reload`.

The keys that use io are in `docs/config/leticode.toml.md` under `[exec]`: `io_wbps`, `io_wiops`, `disk_floor`, `disk_floor_action`, `disk_watch` and `io_pressure_full`.

### The rest of the host setup

Each step below is done once per box, except the firecode taps, which a reboot removes. The root steps are one-time setup, not something a session asks for on each run. `letibot doctor` measures every one of them and prints the fix for the rows that fail. [troubleshooting.md](troubleshooting.md) has what each failure looks like.

**Mask drkonqi's coredump socket** on a KDE desktop. A gate test killed for file size dumps core. drkonqi's coredump launcher is socket-activated per crash, crashes on letibot's dumps and loops. The user manager collected about 128k failed units, `systemd-run --user --scope` stopped answering, and every daemon came up without its scope and with memory caps off. No root needed:

```sh
systemctl --user mask --now drkonqi-coredump-launcher.socket
```

Doctor rows: `drkonqi coredump socket masked`, `user manager failed units`. A manager already wedged clears at a logout of every session or a reboot.

**firecode taps.** firecode VMs need tap devices that belong to you. They are made as root and last until a reboot:

```sh
sudo "$(command -v firecode)" net-setup --count 14
```

Doctor row: `firecode taps`.

**kvm group.** A VM started as you, without the jailer (`--no-jail`, and firecode's own tests), needs read and write on `/dev/kvm`, which is `root:kvm 0660`:

```sh
sudo usermod -aG kvm $USER
```

Groups are read at login, so log out of every session and back in. A tmux server started before that keeps the old groups. Doctor row: `kvm usable (/dev/kvm)`, which says whether the group is missing or only this login lacks it.

**Keep `/tmp` off a large tmpfs** on a box that runs gates and builds. A tmpfs is RAM: test runs and builds writing there compete with the jobs and the model server for memory, and a full one fails writes. On systemd distributions that mount `/tmp` from `tmp.mount`, `sudo systemctl mask tmp.mount` and a reboot put it on the root fs. A tmpfs capped below half of RAM (`size=` in `fstab`) is fine too. Doctor row: `/tmp not a large tmpfs`.

**`LETIBOT_STORE` names a directory.** Since the store split the store is `~/.local/share/letibot`, with `shared.db` and `projects/<key>.db` in it. Leave the variable unset. An old `export LETIBOT_STORE=~/.local/share/letibot/sessions.db` in a shell rc or a tmux environment broke `--continue` on builds before the launcher learnt to resolve it. Remove it from both:

```sh
unset LETIBOT_STORE
tmux set-environment -gu LETIBOT_STORE
```

Doctor row: `LETIBOT_STORE`.

### Installing from a checkout

`letibot` and `leticode` on `PATH` must be the shell launchers from `scripts/`, not binaries. The Rust multicall is `letibot-multi`, the launcher hands it the verbs it does not handle itself, and `letibot-askpass` is a copy (or a hard link) of it. Never install a binary under a launcher's name: `leticode --continue` then answers `letibot: no such role: --continue`.

The release install loop, run in the main checkout. This box serves local models, so its build
names the `local` feature; a box that does not can drop the flag and install the cloud build:

```sh
# Every binary a launcher, the plugin or `letibot-askpass` names. The bridge's PACKAGE is
# `letibot-bridge` and its binary is `letibot-mcp`, which is the name the plugin runs from PATH.
BINS="harnessd letibot-tui letibot-mcp letibot-multi letibot-acp"

git fetch -q && git merge --ff-only -q origin/main
for o in target/release/build/letibot-builtins-*/output; do
  grep -q "rerun-if-changed=$PWD" "$o" || rm -f "$o"
done
nice -n 10 cargo build --release --bins --features local
for b in $BINS; do
  install -m755 "target/release/$b" ~/.local/bin/$b.new && mv -f ~/.local/bin/$b.new ~/.local/bin/$b
done
install -m755 target/release/letibot-multi ~/.local/bin/letibot-askpass.new &&
  mv -f ~/.local/bin/letibot-askpass.new ~/.local/bin/letibot-askpass
ln -sfn "$PWD/scripts/letibot" ~/.local/bin/letibot
ln -sfn "$PWD/scripts/leticode" ~/.local/bin/leticode
hash -r; type letibot   # must be the symlink to scripts/letibot
```

The same list on macOS, where the loop is the same because the binaries are: `BINS` names every
file `install.sh` copies, and `scripts/check-dist-names.sh` holds the three lists (installer,
archive, launcher names) to each other. A binary missing from it is a plugin whose server is not
on `PATH` - Claude Code answers `command not found: letibot-mcp` and the session has no rooms.

The builtins loop drops build output another worktree left in a shared target dir. `install` then `mv` replaces a binary that is running, which a plain `cp` refuses with `Text file busy`. A running daemon or bridge keeps the old binary until it restarts. A `target/release/letibot` left by an older build is not used: ignore it.

Doctor rows: `letibot is the launcher script`, `letibot-askpass is the multicall`, `versions agree`, `daemons run the installed binary`, `bridges run an installed binary`.

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

A Mac installs the same binaries as a Linux box. From a checkout, name every one of them, so a
missing one is a build failure rather than a plugin whose server is not on `PATH`:

```sh
cargo build --release --bin harnessd --bin letibot-tui --bin letibot-mcp --bin letibot-multi \
    --bin letibot-acp
for b in harnessd letibot-tui letibot-mcp letibot-multi letibot-acp; do
  install -m755 "target/release/$b" ~/.local/bin/$b
done
ln -sfn "$PWD/scripts/letibot" ~/.local/bin/letibot
ln -sfn "$PWD/scripts/leticode" ~/.local/bin/leticode
ln -f ~/.local/bin/letibot-multi ~/.local/bin/letibot-askpass 2>/dev/null ||
  cp ~/.local/bin/letibot-multi ~/.local/bin/letibot-askpass
```

`letibot-mcp` is the binary of `crates/bridge` (package `letibot-bridge`): the MCP server the
`leticode` plugin runs from `PATH`, and the room waiter its monitors run. A Mac without it installs
fine and has no rooms, which is why this list must stay one list, shared with `install.sh` and
`scripts/make-dist.sh` by `scripts/check-dist-names.sh`.

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
