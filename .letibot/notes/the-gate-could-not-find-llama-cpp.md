<!-- abstract: main went red at 0f9ddad8 because five build scripts hardcoded the Linux llama.cpp path and the gate is a child of a Mac daemon with neither LETIBOT_LLAMA_* set — clippy died on one crate's assertion, and 70 test targets then aborted at exec on the other four's rpath. The full step log is in target_health.gate_steps_json. -->

# `main` was red on the gate because five build scripts only knew the Linux llama.cpp path

MEASURED 2026-10-26, `main` at `0f9ddad8`: gate step 2 of 4, `cargo clippy --all-targets`,
exit 101 — **and no lint caused it**. `crates/llama/build.rs` panicked:

```
thread 'main' panicked at crates/llama/build.rs:27:5:
llama.h not found at /home/dead/Projects/llama.cpp/include/llama.h
```

`DEFAULT_LLAMA_DIR` is a **Linux** path. The merge queue's steps run as children of the
daemon (`run_step` in `crates/harnessd/src/mergequeue/gate.rs` sets only
`CARGO_TARGET_DIR`), and the daemon serving the queue ran on the operator's Mac — `ps eww`
on it shows no `LETIBOT_LLAMA_DIR` and no `LETIBOT_LLAMA_LIB`. So the header was never
looked for where it is, every gate run on this box would have failed there, and nothing
about `main`'s history was at fault.

## The tail the queue shows hid the cause — read `gate_steps_json` instead

`queue status` prints the last ~4 KiB of the step log. Cargo keeps printing diagnostics
from in-flight units after the failure, so the tail began and ended with clippy warnings
(`large_enum_variant`, `empty_line_after_outer_attr`) and the `error:` sat in the middle.
The **whole** step output is stored per repository, keyed by target:

```sh
sqlite3 -readonly ~/.local/share/letibot/sessions.db \
  "select gate_steps_json from target_health where repository='/Users/dead/Projects/letibot/letibot';"
```

That is a JSON array of `{command, outcome, elapsed_ms, output}` — `output` is the full
`read_tail` text, not the condensed tail `queue status` renders. Reach for it before
believing a lint caused an exit code.

## How to learn what env a build actually used — the fingerprint says

`target/debug/.fingerprint/letibot-llama-*/run-build-script-build-script-build.json` records
`RerunIfEnvChanged` with the **value** the last successful run had. On this Mac every one
of them says:

```
LETIBOT_LLAMA_DIR = /Users/dead/Projects/llama.cpp
LETIBOT_LLAMA_LIB = /Users/dead/Projects/llama.cpp/build/bin
```

while the Linux VM's is `…/llama.cpp/build-glm/bin` — the two boxes' build trees are
different subdirectories, which is why a single default could not serve both. A build
script whose inputs are "whatever the human exported" is invisible to a queue; the
fingerprint is where that becomes visible.

## Fixing step 2 only moved the redness to step 3 — FIVE build scripts, one constant

`docs/build-notes.md` told each new crate to "copy `crates/turn/build.rs`", because
`cargo:rustc-link-arg` applies to the emitting package's own targets and nothing
downstream. So five build scripts each carried their own

```
const DEFAULT_LLAMA_LIB: &str = "/home/dead/Projects/llama.cpp/build-glm/bin";
```

in `llama`, `turn`, `sessionlog`, `harnessd` and `letibot`. Teaching only `llama` to
search made step 2 green and step 3 fail on **70 targets**, every one of them with

```
dyld: Library not loaded: @rpath/libllama.0.dylib
  Reason: tried: '/home/dead/Projects/llama.cpp/build-glm/bin/libllama.0.dylib' (no such file)
```

— the search found the library so the link succeeded, and the rpath the other four baked
was still the Linux one, so each binary aborted before `main` ran. The release `harnessd`
was the same: links fine, `otool -l` shows the wrong LC_RPATH.

**The fix**: `crates/llama/build-support.rs`, `include!`d by all five (an `include!` in a
build script resolves relative to the including file, so four of them say
`include!("../llama/build-support.rs")`). It searches `LETIBOT_LLAMA_DIR` →
`$HOME/Projects/llama.cpp` → the box path, then `build-glm/bin` → `build/bin`, first hit
wins; `llama_lib_dir()` never panics so the four that only need an rpath stay
unconditional, and `crates/llama`'s own script asserts with the full tried-list. The
shared file has no `use` and qualifies everything, because a duplicate `use` inside an
`include!` is a compile error in a build script. `docs/build-notes.md` now says include,
do not copy.

## UPDATE 2026-10-26 — with the llama.cpp path fixed, step 3 goes red on a SECOND absent thing

MEASURED against `main` at `f35d19c066fc`, reproduced in `wt-qa`: step 3 (the tests) failed **48
targets**, and 47 of them are one absent FILE, not code:

```
LETIBOT_REQUIRE_APPARATUS=1 cargo test --offline -p letibot-turn --test engine_decisions
test result: FAILED. 0 passed; 22 failed
panicked at crates/tokencore/src/apparatus.rs:151:9:
LETIBOT_REQUIRE_APPARATUS is set, and a vocabulary GGUF
(/Users/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf)
is not on this machine.
```

`~/models` does not exist on this Mac, and there is no `.gguf` under `/Users/dead` at depth 3. So the
sentence `AGENTS.md` relies on — *"The default vocabulary path does resolve on this box … so the
prefix changes nothing here today"* — is true of the Linux box and FALSE of the Mac the queue's
daemon runs on, and the prefix then makes ~46 targets red every run. The 48th target is
`crates/turn/tests/restore.rs` on the operator's own corrupted `s-gatekeeper#t0`.

**The llama.cpp test vocabularies are not a substitute — measured.** With
`LETIBOT_VOCAB_GGUF=$HOME/Projects/llama.cpp/models/ggml-vocab-qwen35.gguf` the same 22 tests still
fail, differently: `ControlFailure { role: ThinkOpen, literal: "<think>", cause: NotSingleToken {
ids: [13708, 766, 29] } }` and `one token decodes: BadTokenId { id: 248069 }`. The MODEL GGUF is
load-bearing for the control literals being one token and for the ids a fixture's canned frames name.

**And the evidence for the earlier of two causes is gone afterwards.** `gate.rs` writes each step to
`$TMPDIR/letibot-gate/<pid>-<ms>-<n>-<repo>.log` and `remove_file`s it when the step ends
(gate.rs:532/584); `target_health.gate_steps_json` keeps only `read_tail`'s 4 KiB
(`… [397250 byte(s) dropped from the front]`). Step 3's tail began mid-restore-panic, so the
vocabulary panics in 47 other targets were invisible from the row — reproduce a red target by target
instead of reading the row's output.
