<!-- abstract: firecode.layer probe 5: the three fixes hold; the recipe re-ran EVERY line because the stamp is per FILE, and mtimes are how you tell a guard held. -->

# firecode.layer probe 5 — MEASURED 2026-10-10, guest `firecode`

Second VM after the three fixes (toolchain guard, `>` not `>>`, `chown -R 1000:1000`). Nothing
broke: the guest both boots and builds.

## The recipe re-ran EVERY line, and that is the stamp working
`/var/lib/firecode-layer.sha` is a **per-FILE** stamp, so editing `firecode.layer` re-runs the
whole file in the next VM even though the imported image already did all of it. Evidence, this
boot (kernel 13:28:08):
- `/etc/profile.d/10-letibot-rust.sh` 13:28:15 · `/var/lib/apt/lists` 13:28:18.9 ·
  `build-glm/CMakeFiles/cmake.check_cache` 13:28:20.7 · `/usr/local/cargo/registry` 13:28:24 ·
  stamp 13:28:48.6
So boot→layer-done ≈ **40s**, of which ~33s is the recipe — and ~24s of THAT is the
`chown -R` over the 1.5 GB toolchain (registry written 13:28:24, stamp 13:28:48). A VM on an
UNCHANGED layer file skips all of it (~14s). Any edit to `firecode.layer` buys a 30-second boot.

## How to tell a guard HELD, from inside the guest
A guard that skips writes nothing, so mtimes are the evidence — and they are unambiguous:
- `/usr/local/bin/{cargo,rustc,rustup}` still **12:54** (the image's), not 13:28 → the
  `[ ! -x /usr/local/bin/cargo ]` guard skipped the relink. A self-link would read 13:28.
- `/home/dead/Projects/llama.cpp/.git` still **12:59** → the clone guard held.
- `build-glm/bin/libllama.so*` still **12:59** → cmake re-ran configure, ninja had nothing to do.
(`/usr/local/bin` itself reads 13:28 for another reason: the harness links `claude` and `opencode`
into it at 13:28:13. Do not read that as the layer touching the toolchain.)

## The three fixes, verified
- `command -v cargo rustc` → `/usr/local/bin/…`; `cargo --version` → `cargo 1.99.0`, **rc=0**
  (the last probe's self-loop answered rc=127).
- The three symlinks point at `/usr/local/cargo/bin/…`, not at themselves.
- `/etc/profile.d/10-letibot-rust.sh` is **one line**, 65 bytes.
- `/usr/local/cargo` and `/usr/local/cargo/registry` are `dead dead` (1000:1000); uid 1000 can
  `touch` inside — including `/usr/local/cargo/git/db/rano-e5ff04637e4ffcdb`, the exact directory
  the last probe could not create.

## Cold timings (target/ wiped; registry warm) — same shape as probe 4
- `cargo check -p letibot-llama` — **2.54s**, rc 0 (`Finished dev profile … in 2.54s`)
- `cargo test -p letibot-llama --no-run` — **2.31s**, rc 0, two executables linked.
  `crates/llama/build.rs` asserts BOTH `include/llama.h` and `libllama.so` exist in the fork's
  build tree, so a green build proves the layer's clone + cmake output are present and load-bearing.

## Trap
No `job_output` tool and no completion notice for a backgrounded job (same as probe 4). Redirect
with `> file` — not a pipe — and read the file; `{ time cmd ; } > log 2>&1` gives the wall clock.
