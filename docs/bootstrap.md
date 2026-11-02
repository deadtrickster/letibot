# Bootstrapping: the install, and what a first run does

What `install.sh` needs, what it puts on disk, what the first `letibot` creates and says, and
which gaps are still open. Everything here is read off the code or measured on a scratch
prefix; where a claim is measured it says so.

## 1. What the installer needs, and what it does

Two paths, and which one a run takes is decided by what it can reach:

| path | needs | when |
|---|---|---|
| release asset | `curl`, `tar`, `mktemp`, `uname`, `cp`/`chmod`/`mkdir`/`rm`/`dirname`/`head`/`grep`, and a glibc loader | the default, when a platform asset exists |
| source build | the above, plus `git`, `cargo`, a C compiler, and a built llama.cpp checkout for local models | `LETIBOT_FROM_SOURCE=1`, a platform with no asset, or a checkout |

A source build with no `LETIBOT_LLAMA_DIR`/`LETIBOT_LLAMA_LIB` is **cloud-only**: it links no
llama library, so there is none to copy.

Before it downloads or copies anything, it refuses by name for what this run will actually
need:

- **No glibc loader** - the published binaries are built for glibc, and a musl system such as
  Alpine cannot run them whatever else is installed. The message says so and points at
  `LETIBOT_FROM_SOURCE=1`.
- **Host libraries** - `libstdc++.so.6 libgomp.so.1 libsqlite3.so.0` for a build that carries
  llama, `libsqlite3.so.0` alone for a cloud-only one, because the first two are llama.cpp's.
  `host_has_lib` asks `ldconfig -p` and falls back to walking `/lib`, `/lib64`, `/usr/lib`,
  `/usr/lib64`. The refusal names the packages for exactly what is missing, so a line can be
  pasted as it stands.
- **macOS needs nothing from that list** - everything the binaries link outside the archive is
  part of the OS. What a Mac can lack is the Command Line Tools, and only the launcher feels
  it: it writes its daemon record with `python3`.

What it installs: five programs - `harnessd`, `letibot-tui`, `letibot-askpass`, `letibot`,
`leticode` - and the four llama libraries. `letibot` and `leticode` must be **scripts**: an ELF
at that name is the multicall, which does not implement the launcher, and installing it
silently replaces the command a person types with one that does not answer. A release old
enough to predate the launcher installs everything else and says so.

Then it proves the install runs, which is also the first real proof that the libraries landed:
`harnessd --version`, `letibot-tui --version`, and `letibot --help` (the launcher has no
`--version`; see B6). A failure prints the loader's own words, because the four libraries are
beside the binary and what is missing is almost always a host library the preflight could not
see.

It ends with the directory, a `PATH` line when that directory is not on `PATH`, and what the
next step is - `letibot`, which asks for a model on the first message. It does not name the
directories a run will create (B5).

### The checkout inversion

`local_checkout()` asks whether the directory holding `$0` has a `Cargo.toml` with
`[workspace]` and `"crates/harnessd"`. If it does, `install.sh` takes the **source** branch and
never looks at the release asset. So the documented one-liner, run from a clone, silently
becomes a `cargo build` needing Rust, a C compiler and a built llama.cpp - and the person who
ran it to *avoid* building is the one who gets a build. That is deliberate (it is how CI's
`Installer` job gets a warm build) and nothing announces it.

## 2. Runtime dependencies the installer does not check

`install.sh` checks host *libraries* and no host *commands*. Two are load-bearing at run time
and are named in neither the README nor the installer's header:

| command | used for | what its absence does |
|---|---|---|
| `ss` (iproute2) | `scripts/letibot:247`, `:1239` - is a daemon listening | the substitution is empty, so `socket_is_listening` answers *false for a live daemon*: a second `letibot` starts a second daemon over the first |
| `python3` | `scripts/letibot:1318` (`record_write`), `:1627` (the room's record) | under `set -euo pipefail` a box with no `python3` starts a daemon and then reports failure, with the daemon running |

`python3` is the cheap one to remove: the record is one flat JSON object, and the *reading*
side already parses it in bash. `sha256sum` already falls back to `shasum -a 256`, and `awk`,
`curl` and `git` are used too, but none of them silently inverts an answer.

## 3. What a first run creates

| path | created by |
|---|---|
| `~/.local/share/letibot/` and `sessions.db` | the launcher's `mkdir -p` (`:2483`), then the daemon on open |
| `~/logs/harnessd-<folder key>.log` | the launcher's `mkdir -p`; one file per folder, so a boot failure's reason is not among other daemons' lines |
| `~/logs/harnessd.log` | one line per start, naming where that daemon logs |
| `$XDG_RUNTIME_DIR/letibot/` | `mkdir -p "$RUNDIR"` (`:358`) |
| `$XDG_RUNTIME_DIR/letibot/<sha256(workspace)[0..12]>.{sock,json}` | the daemon binds the socket; the launcher writes the record |
| `$XDG_RUNTIME_DIR/letibot/room-<host key>.{sock,lease,json}` | `room_ensure`, before any session opens |
| `~/logs/room-<host key>.log` | the room coordinator |
| `$XDG_RUNTIME_DIR/leticode-state.json` | the daemon's task journal (`crates/harnessd/src/tasks.rs`) |
| `~/.config/letibot/permission.json` | the daemon, installed from the repository's seed the first time no file is there |
| `~/.config/letibot/head.toml` | the head, only when it saves a preference |
| `$XDG_RUNTIME_DIR/letibot/subagent-<id>.log` | the head, when a child's output spills |

The `mkdir -p` for the store and the logs is load-bearing rather than tidy: `Store::open` is
`Connection::open(path)` and SQLite does not create a parent directory. Run
`harnessd --store ~/.local/share/letibot/sessions.db` on a box where that directory does not
exist and the open fails.

Read but never created by a first run - a fresh box simply runs without them:

- `~/.config/letibot/providers.toml` - keys, prices, `[default]`, `[gatekeeper]`.
- `~/.config/letibot/prompts.toml` - per-model system prompts, beside it.
- the per-project and per-session modes - tables in `sessions.db`; a legacy
  `~/.config/letibot/modes.tsv` is imported once.
- `~/.config/letibot/skills/*/SKILL.md` and `~/.claude/skills/*/SKILL.md`.
- `~/.local/share/opencode/auth.json` - opencode's key store, the third place a key is looked
  for.

Three things about that list are worth stating in one place, because nothing else does:

1. **`~/logs/` is not a letibot directory, and `leticode-state.json` is on a tmpfs.** The task
   journal is state - it should survive a logout - written into `$XDG_RUNTIME_DIR`. It survives
   a daemon restart because the launcher does not clean it, and does not survive a reboot (B4).
2. **The config directory is never created as a directory.** `install_seed` does
   `create_dir_all` on the parent, so `~/.config/letibot/` appears the first time a daemon
   starts and not before, and `--help` never mentions it.
3. **Nothing tells the user these paths exist.** The installer's closing block names the model
   gap and nothing else (B5).

## 4. What the first `letibot` does

The ordering is deliberate and the refusals are good.

1. **The box's room is ensured** before any session can look for it: a session's `room` tool is
   seated at open from the socket that exists then. A room that fails to start prints the
   coordinator's own sentence and the run goes on - a session with no room is a session that
   works.
2. **No model is not a refusal.** A run starts whatever answers or does not, and the head asks:
   the first message sent while the session's model cannot be reached opens the model chooser
   with the reason on it, and a cloud row with no key goes through the key card and becomes
   `[default]` in `providers.toml`.
3. **The vocabulary goes with the same question.** `--vocab` is passed when it names a file
   that is there, or when `LETIBOT_VOCAB` asked for it by name. A provider needs none: the
   daemon keeps its ledger in the byte vocabulary. A GGUF named by default and missing is not
   passed, and `Parts::load` falls back to `Vocab::bytes` rather than refusing.
4. **A cloud provider needs a key** from `--api-key`, the provider's env var,
   `~/.config/letibot/providers.toml`, or opencode's store; a miss is refused by name with all
   of the places.
5. **The daemon starts in the background** with output appended to its log, the launcher waits
   up to 30 s, and on failure prints the last 15 lines of that log. The file is per folder, so
   that tail is this folder's runs; the shared `harnessd.log` names each start.
6. **`letibot --version` does not exist.** The launcher is a shell script and answers `--help`
   only; `harnessd --version` and `letibot-tui --version` answer for their own halves (B6).

## 5. The gaps that are still open

**B2 - `install.sh` must say which path it took, and the README must agree with it.** A
one-line *"found a checkout at …, building from source (this needs Rust, a C compiler and a
built llama.cpp); set LETIBOT_FROM_SOURCE=0 to take the release asset instead"* before the
build. And `try_prebuilt` must stop discarding curl's stderr, so a 404 or a missing asset is a
line rather than a silent fallback to a ten-minute build. *Done when* the documented one-liner
run from a clone prints which path it is on before it spends the time, and
`LETIBOT_VERSION=v9.9.9` says *"no release asset at … - is the tag spelled right?"* instead of
`git`'s `Remote branch not found`.

**B3 - the launcher's external dependencies must be checked and named.** `ss` and `python3`
above, and the list belongs in `install.sh`'s preflight beside `host_has_lib`. *Done when*
`install.sh` refuses, before copying, on a box whose `PATH` lacks `ss` or `python3`, naming the
package.

**B4 - the paths belong to one convention.** The log under `~/logs/`, the task journal on a
tmpfs. *Done when* the log is `$XDG_STATE_HOME/letibot/harnessd.log` (or
`~/.local/state/letibot/`), the task journal is beside it rather than on a tmpfs, and both are
named in one table in the README.

**B5 - a first run should be able to say what it made.** *Done when* the closing block names
`~/.config/letibot/` (what will be created there and when), `~/.local/share/letibot/` and the
log's path, so a user who hits a failure knows where to look without asking.

**B6 - an installed `letibot` should be able to report its own version.** The multicall's
`--version` is a different thing under the same name. *Done when* the launcher answers
`--version` with the tag it was installed from, and `install.sh`'s check uses it instead of
`--help`'s substring match.

**B7 - `install.sh` should be runnable as a test, not only as a side effect of CI.** CI runs it
against a warm checkout, which is the *source* branch; the asset path is only ever exercised by
the release workflow's extraction step, which unpacks the archive itself rather than running
the installer. *Done when* a test runs `install.sh` with `LETIBOT_VERSION=<tag>` and
`LETIBOT_INSTALL_DIR=$(mktemp -d)` from a directory that is not a checkout, and asserts the
nine files and a running `harnessd`.

**B8 - an inherited `LETIBOT_SOCKET` must not let one workspace stop another's daemon.**
`SOCKET="${LETIBOT_SOCKET:-$RUNDIR/$KEY.sock}"` outranks the socket computed from the
workspace, so `--status` and `--stop` act on whatever it names while `daemon_line` prints
`harnessd for $WORKSPACE` - the *current* workspace. Measured against a live daemon: a scratch
`HOME`, workspace and `XDG_RUNTIME_DIR` reported the operator's own daemon as this workspace's,
and `--stop` acts on the pid it reads from that socket. *Done when* `--status` and `--stop`
refuse by name when `LETIBOT_SOCKET` resolves to a socket whose record names a different
workspace.

**B9 - the release workflow's `$ORIGIN` guard must cover the binaries too.** The guard iterates
the four library names, so `harnessd`, `letibot-tui` and `letibot-askpass` are never inspected
- and all three ship a runner build path in their `DT_RUNPATH`. Harmless today, because
`$ORIGIN` is searched first and `/home/runner/…` exists on no user's machine, and both binaries
are measured running with `LD_LIBRARY_PATH` emptied. *Done when* the loop runs over
`$bins $libs`, the way the extraction check already does.

One gap is closed and recorded here so nobody reopens it: **a fresh box no longer needs a GGUF
to reach a cloud provider.** `Parts::load` falls back to the byte vocabulary when no GGUF is
named, and the launcher passes `--vocab` only when the file is there.
