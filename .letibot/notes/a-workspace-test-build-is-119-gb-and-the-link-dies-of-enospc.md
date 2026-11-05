<!-- abstract: `cargo test --workspace` here links ~119 GB of debug test binaries; on lab2x1's 1.8 TB disk (94-100% used) the LINK step dies of ENOSPC as `ld terminated with signal 7 [Bus error]`, which reads like a compiler bug and is not a test failure. -->

# The workspace's test build is 119 GB, and a full disk kills `ld` with SIGBUS

MEASURED 2026-11-05 in `wt-z-r93` on lab2x1, after `cargo clean && cargo test --workspace`:

- `du -sh target` after one whole-workspace test build: **119 GB** (`target/debug/deps` 96 GB, 6694
  files; `target/debug/incremental` 17 GB).
- The host filesystem is 1.8 TB at 94-100% used, so those builds sit within ~1 GB of the wall.
- At the wall the failure is NOT a test failure and not a compile error:
  `error: linking with cc failed` / `collect2: fatal error: ld terminated with signal 7 [Bus error],
  core dumped`, plus `failed to write query cache ...: No space left on device (os error 28)`.
  `ld` mmaps its output, so a full disk surfaces as SIGBUS - which reads like an lld bug.
- `cargo clean` in the worktree is the way back: it removed 58,925 files and 132.8 GiB.
- What fits: `CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --workspace -j 8`. The
  tests are the same tests; only the debug info and the incremental cache are gone.

So when a gate run reports several unrelated test targets failing to link at once, look at `df`
before the code: the step that died is the linker, not the test.
