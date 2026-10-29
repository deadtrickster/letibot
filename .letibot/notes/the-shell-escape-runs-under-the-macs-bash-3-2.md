<!-- abstract: MEASURED 2026-10-29: a head `!` line is `/bin/bash -ic` (exec/console.rs), so `-i` reads the operator's `~/.bashrc` chain — which is bash 4 and died with `declare -A: invalid option`; the failing file was the operator's emacs integration, NOT scripts/etalon-snapshot; and a `declare -A` under `set -u` on 3.2 dies as `name: unbound variable`, not at the declare. -->

# A `!` line is `/bin/bash -ic`, and the Mac's `/bin/bash` is 3.2 — MEASURED 2026-10-29

The operator typed `! cp …` in the head and got, before anything ran:

```
bash: declare: -A: invalid option
declare: usage: declare [-afFirtx] [-p] [name[=value] ...]
```

## The chain, file by file (grep these, do not re-derive it)

`crates/tui/src/app/composer.rs:166` (`!` → `Action::OperatorShell`) → `crates/tui/src/driver.rs:1013`
(`ClientFrame::OperatorShell`) → `crates/harnessd/src/harness.rs:6778` (`run_operator_shell`, which
mints a `bash` ToolCall) → `crates/tools/src/runtime.rs:2302` `invoke_operator` (ungated) with
`tty: !gated` (`runtime.rs:2533`) → `crates/tools/src/builtins/bash.rs:702` (`tty: ctx.tty`) →
`crates/tools/src/exec/host.rs:1636` (`if req.tty { console_shell }`) whose default is
`exec::console::shell()` (`host.rs:1126`) = **`/bin/bash -ic`** (`exec/console.rs:127`).

So the escape does **not** inherit the operator's interactive shell — it gets the daemon's
environment and a fixed `/bin/bash`. `-i` is load-bearing (it is what makes bash read
`~/.bashrc`, which is where the aliases are), and that is what pulls in the rc chain.

## The failing file was NOT in this repo

`~/.bashrc` (185 bytes) sources `~/.emacs.d/shell/integration.bash`; **line 118 is
`declare -A __BB_WINDOW_CMD=(`**. It also has `local -A` (180, 242) and `mapfile` (233). The
operator's interactive shell is zsh (`$SHELL=/bin/zsh`, `.zshrc` + mise), so the bash rc was
never exercised by them — and `bash` on `PATH` is 5.3.20 (homebrew) while `/bin/bash` is
3.2.57. `scripts/etalon-snapshot` has a `declare -A` too, but it is a hand-run script nothing
sources; it was never on the escape's path.

## Two facts worth keeping

- **A 3.2 `declare -A` fails at RUNTIME, so `/bin/bash -n` passes it.** The grep is the only
  static check; the guard that does it now lives at the top of `tests/launcher.sh`.
- **Under `set -u` the death is one step later and looks unrelated**: bash 3.2 evaluates the
  rejected `[name]=` subscripts as arithmetic, so `declare -A CHECKS=([openssh_key_body]=…)`
  answers `openssh_key_body: unbound variable` and the script exits — before any of the work.
  Measured on `scripts/etalon-snapshot`'s own block.
- `+=` on an array is **bash 3.1**, not 4 — `scripts/letibot`/`leticode` use it deliberately.
  The real 3.2 hazard there is *expanding* an empty array under `set -u`
  (`EXTRA_ARGS[@]: unbound variable`), which no grep can see.

## What fixed it

`crates/tools/src/console_bash.rs` (new): the flags stay `exec::console`'s, the program is
probed — `$SHELL` if it is a bash 4+, then `bash` on `PATH`, then `/bin/bash`. Wired in
`HostBackend::executable` only (the unconfined path, which is leticode and the Mac's fallback):
a confined view's `$HOME` is a tmpfs, so the rc there is nobody's. `exec/` was off-limits on
this branch, which is why the choice lives one layer up.
