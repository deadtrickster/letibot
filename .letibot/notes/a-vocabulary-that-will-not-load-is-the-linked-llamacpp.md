<!-- abstract: MEASURED 2026-10-26: `~/.models/qwen3.8-flash-next/...00001-of-00006.gguf` is a SPLIT model's first shard (10 MB), so `Parts::load` dies with "libllama could not load a vocabulary" and 30 harnessd lib tests + tests/delivery.rs fail at the `expect("the vocabulary must load")` line — absent apparatus, not a code defect. -->

# The vocabulary GGUF on this box is a split model's first shard, and that is why 30 harnessd tests fail

MEASURED 2026-10-26, `~/Projects/letibot/wt-d1` (package D1), macOS, `cargo test --offline -p letibot-harnessd --lib`.

`letibot_harnessd::vocab_gguf()` finds a file — `letibot_tokencore::apparatus::present_gguf`
returns a path — so the `let Some(_) = vocab_gguf() else { return }` guards in every test open
the door. Then `Parts::load(&cfg)` fails:

```
the vocabulary must load: Setup("loading the vocabulary: libllama could not load a vocabulary from
/Users/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf")
```

The file is **10,946,560 bytes** and its name says `00001-of-00006`: it is the first shard of a
split GGUF, not a whole model, and there is a `split-shard-1.gguf.orig` beside it. `libllama`
refuses it. The consequence is a fixed list of failures that look like code and are not:

* `-p letibot-harnessd --lib`: **631 passed, 30 failed**, every one of them panicking ON the
  `Parts::load(...).expect("the vocabulary must load")` line — `harness::queue_e2e::*` (10),
  `harness::tests::{citations,store_writes,workspace}::*` (7), `sessions::review_pass::*` (7),
  `sessions::gatekeeper::tests::*` (4), `sessions::operator_run_failures::*` (1),
  `sessions::seat_slots::*` (1).
* `-p letibot-harnessd --test delivery`: all 5 fail the same way.

**How to tell this from a real breakage**: read the panic line, not the test name. If it is
`Parts::load(...).expect("the vocabulary must load")` (or a crate-local variant of the same
call), the file did not load and the test body never ran. No `git stash` is needed to prove it:
`tests/delivery.rs` fails identically and nothing in that file is related to whatever you changed.

**What a test that needs a real engine should do**: `let Ok(parts) = Parts::load(&cfg) else {
return; }` with a line saying why — `AGENTS.md`'s rule that absent apparatus is not a failed
assertion. The tests that `expect` were written when every dev box had a whole model file, and on
this box they are 30 red rows with nothing behind them.
