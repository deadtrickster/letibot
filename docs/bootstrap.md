# Bootstrapping: what a first run does today, and what it does not

**This file is a work list for the next branch, not a design document.** It exists because
the operator's read on the installer is that *"the installation and bootstrapping are
underdeveloped"*, and "underdeveloped" is not actionable until it is a list of things with
a file and a line. Everything below is either read off the code (cited) or measured on a
scratch prefix against the `v0.3.0` release; where a claim is measured rather than read, it
says so.

The release side is in good shape: `install.sh` refuses a musl box by name, names the three
host libraries a minimal container lacks before it copies anything, and verifies that what
it installed runs. What is thin is everything *after* the copy — there is no bootstrap.

**§5 records what was measured** on a scratch prefix against `v0.3.0`; everything before it
is read off the code and cited.

---

## 1. What the installer assumes, and what it names

`install.sh` runs under `/bin/sh` with `set -eu`. It uses these commands, and only these,
on the download path:

| command | where | needed for |
|---|---|---|
| `curl` | `install.sh:229` | the asset download — the one hard dependency |
| `tar` | `install.sh:231` | unpacking it |
| `mktemp` | `install.sh:282` | the staging directory |
| `uname` | `install.sh:210-213` | picking the triple |
| `cp`, `chmod`, `mkdir`, `rm`, `dirname`, `head`, `grep` | `install.sh:345-390`, `368` | copying and the launcher check |
| `ldconfig` | `install.sh:132` | the host-library probe (`host_has_lib`), with a `/lib` walk as the fallback |
| `git`, `cargo`, `cc`/`gcc`/`clang` | `install.sh:240-259` | the **source** path only |

That set is honest and it is documented (`README.md:97-107`). Two things are not:

1. **`local_checkout()` at `install.sh:194-202` inverts the install for anyone standing in
   a checkout.** It asks whether the directory holding `$0` has a `Cargo.toml` with
   `[workspace]` and `"crates/harnessd"`. If it does, `main` (`install.sh:306-309`) takes
   the **source** branch and never looks at the release asset. So the documented one-liner,
   run from a clone, silently becomes a `cargo build` needing Rust, a C compiler and a built
   llama.cpp — and the person who ran it to *avoid* building is the one who gets a build.
   That is a deliberate design (it is how CI's `Installer` job works, `ci.yml:516`) but it is
   not what the sentence in `README.md:50` says, and there is no message announcing the
   switch.

2. **A version that does not exist is reported by `git`, not by `letibot`.** MEASURED:
   `LETIBOT_VERSION=v9.9.9 sh install.sh` prints the banner, then *"Fetching
   deadtrickster/letibot..."*, then `fatal: Remote branch v9.9.9 not found in upstream
   origin`, and exits 128 having installed nothing. The asset download's own failure is
   discarded (`try_prebuilt` sends curl's stderr to `/dev/null`, `install.sh:229`), so the
   404 that actually explains it is the one line the user never sees. The same silence
   covers the ordinary case — a platform with no published asset — where the source build
   is the designed fallback (`install.sh:204-207`) and is announced nowhere.

3. **`ss` and `python3` are hard runtime dependencies of the launcher and are named
   nowhere.** `scripts/letibot:617` answers *is a daemon listening* from `ss -lxpH`
   (iproute2), and `scripts/letibot:793` and `:1189` read the daemon's pid the same way. On
   a box without iproute2 the command substitution is empty (`|| true`), so
   `socket_is_listening` returns *false for a live daemon* — a second `letibot` starts a
   second daemon over the first, which is the bricked-transcript failure the comment at
   `scripts/letibot:623-632` records as measured. And `record_write`
   (`scripts/letibot:696-703`) writes the daemon's record with `python3`; it is called on
   the start path (`:1279`) under `set -euo pipefail`, so a box with no `python3` starts a
   daemon and then reports failure — with the daemon running. The installer checks three
   host *libraries* and no host *commands*; neither README nor `install.sh`'s header names
   `ss` or `python3`.

---

## 2. What a first run creates, and where

The launcher's `mkdir -p "$(dirname "$STORE")" "$(dirname "$LOG")"`
(`scripts/letibot:1213`) is the only directory creation on the path, and it is
**load-bearing rather than tidy**: `Store::open` is `Connection::open(path)`
(`crates/tokencore/src/store.rs:1460-1462`) and SQLite does not create a parent directory.
Run `harnessd --store ~/.local/share/letibot/sessions.db` — which is exactly what
`README.md:117-120` tells a reader to do — on a box where that directory does not exist and
the open fails.

| path | created by | cited at |
|---|---|---|
| `~/.local/share/letibot/` | the launcher's `mkdir -p` | `scripts/letibot:1213` |
| `~/.local/share/letibot/sessions.db` | the daemon, on open | `scripts/letibot:125` |
| `~/logs/harnessd.log` | the launcher's `mkdir -p` + the daemon's `>>` | `scripts/letibot:1213`, `:1269` |
| `$XDG_RUNTIME_DIR/letibot/` | `mkdir -p "$RUNDIR"` | `scripts/letibot:120-121` |
| `$XDG_RUNTIME_DIR/letibot/<sha256(workspace)[0..12]>.{sock,json}` | the daemon / the launcher's record | `scripts/letibot:122-124` |
| `$XDG_RUNTIME_DIR/leticode-state.json` | the daemon's task journal | `crates/harnessd/src/tasks.rs:173-181` |
| `~/.config/letibot/permission.json` | the daemon, **installed from the seed the first time no file is there** | `crates/harnessd/src/config.rs:1190-1201`, `crates/tools/src/permission.rs:204-214` |
| `~/.config/letibot/head.toml` | the head, only when it saves a preference | `crates/tui/src/prefs.rs:162-170`, `:396` |
| `$XDG_RUNTIME_DIR/letibot/subagent-<id>.log` | the head, when a child's output spills | `crates/tui/src/app.rs:20512-20536` |

Read but never created by a first run — a fresh box simply runs without them, which is the
right default but is nowhere said in one place:

- `~/.config/letibot/providers.toml` — keys, prices, `[default]`, `[gatekeeper]`
  (`crates/provider/src/keys.rs:147-155`; the `[default]` block is resolved at
  `crates/harnessd/src/cli.rs:711-727`).
- `~/.config/letibot/prompts.toml` — per-model system prompts, beside `providers.toml`
  (`crates/harnessd/src/config.rs:882-887`, read at `cli.rs:919-936`).
- `~/.config/letibot/modes.tsv` — the per-project mode record
  (`crates/harnessd/src/modes.rs:55-66`).
- `~/.config/letibot/skills/*/SKILL.md` and `~/.claude/skills/*/SKILL.md`
  (`crates/tools/src/builtins/skill.rs:114-126`).
- `~/.local/share/opencode/auth.json` — opencode's key store, the third place a key is
  looked for (`crates/provider/src/keys.rs:1-10`).

**Three things about that table are the list:**

1. **`~/logs/` is not a letibot directory and `leticode-state.json` is in the runtime dir.**
   A log file at `$HOME/logs/harnessd.log` (`scripts/letibot:126`) is a directory in the
   user's home that belongs to no convention, and the task journal
   (`crates/harnessd/src/tasks.rs:173-181`) is state — it should survive a logout — written
   into `$XDG_RUNTIME_DIR`, which is a tmpfs. It survives a daemon restart by accident
   (the launcher does not clean it) and does not survive a reboot, which is not stated
   anywhere.
2. **The config directory is never created as a directory.** `install_seed`
   (`crates/tools/src/permission.rs:209-211`) does `create_dir_all` on the parent, so
   `~/.config/letibot/` appears the first time a daemon starts and not before. `--help`
   never mentions the directory.
3. **Nothing tells the user these paths exist.** The installer's closing block
   (`install.sh:461-485`) names the model gap and nothing else.

---

## 3. What the very first `letibot` shows

The ordering is deliberate and the refusals are good; the *defaults behind them* are the
author's box.

1. **No model server** → `letibot` dies before anything else with *"nothing serving on
   127.0.0.1:8080"* and two ways forward (`scripts/letibot:1159-1164`). This is the best
   first-run message in the tree and it is the one most users will hit.
2. **With a server** → the launcher detects the model from `/props` against a table of
   **three hardcoded `$HOME/models/...` paths** (`scripts/letibot:377-388`) and otherwise
   falls back to
   `VOCAB="${LETIBOT_VOCAB:-$HOME/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL.gguf}"`
   (`scripts/letibot:408`). That is the author's file, on the author's disk.
3. **And the vocab is required even for a cloud provider.** `Parts::load` refuses by name
   when `cfg.vocab_gguf` is not a file — *"no vocabulary GGUF at {}"*
   (`crates/harnessd/src/harness.rs:191-196`) — and the launcher always passes `--vocab`
   (`scripts/letibot:1223`, `:1264`). So `letibot --provider deepseek` on a fresh box still
   needs a local GGUF, and the failure arrives as a path the user never chose. **This is the
   single largest bootstrap gap in the tree.**
4. **A cloud provider needs a key** from `--api-key`, the provider's env var,
   `~/.config/letibot/providers.toml`, or opencode's store; a miss is refused by name with
   all three places (`crates/provider/src/keys.rs:1-10`, `:38-50`). That refusal is good.
5. **The daemon starts in the background** with output appended to
   `~/logs/harnessd.log` (`scripts/letibot:1263-1269`), the launcher waits up to 30s
   (`:1277`), and on failure prints *the last 15 lines of that log* (`:1278`). A first-run
   failure is therefore a log tail the user has to find later.
6. **`letibot --version` does not exist.** The launcher is a shell script and answers
   `--help` only (`install.sh:426-435` records the measurement). There is no way to ask an
   installed `letibot` what version it is; `harnessd --version` and `letibot-tui --version`
   answer for their own halves.

---

## 4. The list for the next branch

Ordered by what unblocks the most, each with a `done when`.

**B1 — a fresh box must not need the author's GGUF to reach a cloud provider.**
`crates/harnessd/src/harness.rs:191` and `scripts/letibot:408`. Either the vocab becomes
lazy (load it when the first local turn needs it, not at `Parts::load`), or `--vocab` is
optional whenever `--provider` is set. *Done when* `LETIBOT_VOCAB= LETIBOT_ENDPOINT=
letibot --provider deepseek --api-key …` starts a daemon on a box with no `$HOME/models`
and answers one turn.

**B2 — `install.sh` must say which path it took, and the README must agree with it.**
`install.sh:306-313`, `README.md:47-51`. A one-line *"found a checkout at …, building from
source (this needs Rust, a C compiler and a built llama.cpp); set LETIBOT_FROM_SOURCE=0 to
take the release asset instead"* before the build, and the README's one-liner must carry
the same caveat. And `try_prebuilt` (`install.sh:229`) must stop discarding curl's stderr,
so a 404 is a line rather than a silent fallback. *Done when* running the documented
one-liner from a clone prints which path it is on before it spends ten minutes on it, and
`LETIBOT_VERSION=v9.9.9` says *"no release asset at … — is the tag spelled right?"* instead
of `git`'s `Remote branch not found`.

**B3 — the launcher's external dependencies must be checked and named.**
`scripts/letibot:617`, `:793`, `:1189` (`ss`), `:696` (`python3`), `:122` (`sha256sum`),
`:544` (`awk`), `:397` (`curl`), `:109` (`git`). `install.sh` checks three host *libraries*
and no host *commands*. *Done when* `install.sh` refuses, before copying, on a box whose
PATH lacks `ss` or `python3`, naming the package; and `host_has_lib`'s sibling exists for
commands. (`python3` at `:696` is the cheap one to remove outright — the record is one flat
JSON object and the *reading* side already parses it in bash, `:705-724`.)

**B4 — the paths belong to one convention.** `scripts/letibot:126`
(`~/logs/harnessd.log`), `crates/harnessd/src/tasks.rs:173-181` (`$XDG_RUNTIME_DIR` for
state). *Done when* the log is `$XDG_STATE_HOME/letibot/harnessd.log` (or
`~/.local/state/letibot/`), the task journal is beside it rather than on a tmpfs, and both
are named in one table in the README.

**B5 — a first run should be able to say what it made.**
`install.sh:461-485` ends on the model gap and nothing else. *Done when* the closing block
names `~/.config/letibot/` (what will be created there and when), `~/.local/share/letibot/`,
and the log's path — so a user who hits a failure knows where to look without asking.

**B6 — an installed `letibot` should be able to report its own version.**
`install.sh:426-435` is the measurement that `--version` is not the launcher's flag; the
multicall binary's `--version` (`crates/letibot/src/main.rs:289`) is a different thing under
the same name. *Done when* the launcher answers `--version` with the tag it was installed
from, and `install.sh`'s check uses it instead of `--help`'s substring match
(`install.sh:441-445`).

**B7 — `install.sh` should be runnable as a test, not only as a side effect of CI.**
`ci.yml:516-525` runs it against a warm checkout, which is the *source* branch — so the
asset path (`install.sh:208-234`) is only ever exercised by the release workflow's
`Every asset must extract to what install.sh expects` step (`release.yml:220-239`), which
extracts the archive itself rather than running the installer. *Done when* a test runs
`install.sh` with `LETIBOT_VERSION=<tag>` and `LETIBOT_INSTALL_DIR=$(mktemp -d)` from a
directory that is not a checkout, and asserts the eight files and a running `harnessd`.
