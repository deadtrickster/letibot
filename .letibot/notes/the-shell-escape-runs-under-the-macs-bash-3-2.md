<!-- abstract: A head `!` line runs under `/bin/bash -ic`, so `-i` sources the operator's rc chain - and on a Mac `/bin/bash` is 3.2, where a bash-4 `declare -A` dies before anything runs. -->

# A `!` line is `/bin/bash -ic`, and the Mac's `/bin/bash` is 3.2 — MEASURED 2026-10-29

The chain, so nobody re-derives it: `crates/tui/src/app/composer.rs` (`!` →
`Action::OperatorShell`) → `driver.rs` → `harness.rs`'s `run_operator_shell` → `runtime.rs`'s
`invoke_operator` → `builtins/bash.rs` → `exec/host.rs` (`if req.tty { console_shell }`) →
`exec/console.rs`'s `/bin/bash -ic`. `-i` is load-bearing: it is what makes bash read the rc
chain where the aliases are, and that is what pulls in a file this repo does not own.

`crates/tools/src/console_bash.rs` probes the program - `$SHELL` if it is a bash 4+, then `bash`
on PATH, then `/bin/bash` - and is wired in `HostBackend::executable` only, because a confined
view's `$HOME` is a tmpfs and the rc there is nobody's.

A 3.2 `declare -A` fails at RUNTIME, so `/bin/bash -n` passes it: the grep is the only static
check. Under `set -u` the death is one step later and looks unrelated, because 3.2 evaluates the
rejected subscripts as arithmetic and the script exits with `name: unbound variable`. `+=` on an
array is bash 3.1, not 4; the real 3.2 hazard in the launcher scripts is expanding an EMPTY
array under `set -u`, which no grep can see.
