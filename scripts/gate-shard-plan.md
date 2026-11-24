# Sharding the gate's test step

Plan only. Today step 4 is one `cargo nextest run --workspace` on the gate machine, and one box has
to finish the slowest binary. Sharding runs the same set of tests on two or more hosts at once.

## The shapes that make it possible
- `cargo nextest archive --workspace --features local --archive-file <dir>/tests.tar.zst` builds
  every test binary once, in the gate's own `CARGO_TARGET_DIR`, and writes one file the tests run
  from with no source tree and no compiler.
- `cargo nextest run --archive-file <dir>/tests.tar.zst --partition hash:k/N --profile gate` runs
  shard `k` of `N`. `hash` splits by a stable hash of each test id, so every binary's tests land on
  both shards; `slice` would hand one host the whole `letibot-harnessd` lib binary, the one unit
  worth splitting. Both hosts need the same rustc/cargo and the same apparatus
  (`LETIBOT_REQUIRE_APPARATUS=1`: vocabulary GGUF, delegated cgroup v2, `bwrap`), because a skip
  that is a failure here has to be a failure there.

## Which hosts, and when it does not happen

- The list is `[queue] gate_shards` in `leticode.toml` (or `LETIBOT_GATE_SHARDS`): names from
  `~/.config/letibot/hosts.toml`, in shard order. **Empty, the default, means no sharding**: step 4
  runs exactly as it does today, locally.
- Each host is probed before the archive is built: reachable, and with spare cores
  (`cores - load - reserve`). A busy or unreachable host is dropped and the shards are renumbered
  over the hosts that answered; nothing answering leaves one shard, local. A host that dies
  mid-shard makes its shard `Infra`, not red: it is re-run locally inside the remaining deadline.

## What changes in `scripts/gate-at.sh`

1. Recognise the step: one naming `cargo nextest run` with shards configured is run this way; every
   other step keeps the single `sh -c` it has now.
2. Build the archive once locally (the step's `CARGO_TARGET_DIR`, `--features local`), then run `N`
   copies of `sh scripts/gate-env.sh … cargo nextest run --archive-file … --partition hash:k/N
   --profile gate` — locally or over ssh — each into `<name>.step4.shard<k>.log`, concurrently,
   under the gate's one deadline and the per-shard memory cap.
3. The verdict is every shard's exit status. The step's log is the shard logs concatenated, one
   `# shard k/N on HOST` header each, so a reader still sees nextest's `FAIL` and `Summary` lines.
4. `crates/harnessd/src/mergequeue/gate.rs`'s nextest reader must SUM the shard summaries instead of
   taking the last, and keep one `FailingTarget` per shard (binary ids repeat across shards). The
   gate lock, the leak check and the install step do not move.
