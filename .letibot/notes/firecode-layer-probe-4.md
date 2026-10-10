<!-- abstract: firecode.layer probe 4: every line landed; the profile file is load-bearing, not decorative; cold build timings are seconds because the registry is pre-fetched. -->

# firecode.layer probe 4 — MEASURED 2026-10-10, guest `firecode`

Every line of `firecode.layer` landed on this boot. The two defects the previous probe found
(`cc` as a package name; `RUSTUP_HOME` set only for the layer's own lines) are both fixed, and
nothing new failed.

## Timings (from mtimes — the layer ran before anyone could watch it)
- `/usr/local/rustup`, `/usr/local/cargo/bin`: 12:29
- `/home/dead/Projects/llama.cpp`, `build-glm/bin/*.so`: 12:30
- `/usr/local/cargo/registry`: 12:31
So the whole layer cost ~2 minutes of boot, not ten.

## The profile file is LOAD-BEARING, not belt-and-braces
`/usr/local/bin/cargo` is a **rustup proxy**. With `RUSTUP_HOME` unset it dies with
`error: rustup could not choose a version of cargo to run, because one wasn't specified
explicitly, and no default is configured.` — even though the 1.5 GB toolchain is installed and
`rustup toolchain list` reports `stable-x86_64-unknown-linux-gnu (active, default)`.
`/etc/profile.d/10-letibot-rust.sh` (65 bytes, the two exports) is the only thing that fixes it.
Tested with `env -i PATH=... HOME=/home/dead bash -c ...`: fails; add
`. /etc/profile.d/10-letibot-rust.sh` to that same shell: `cargo 1.99.0`, `rustc 1.99.0`.

**The harness's own exec path is a login shell** (`bash -lc`, confirmed with
`shopt -q login_shell`), so every tool call in a firecode session inherits the profile file's
exports. That is why `cargo` works in a session without anyone setting anything up.

## The payoff is real and it is seconds, not minutes
With `target/` wiped:
- `cargo check -p letibot-llama` — **2.54s**, exit 0
- `cargo test -p letibot-tokencore --no-run` — **2.74s**, exit 0 (real link, `libsqlite3.so.0`)
- `cargo test -p letibot-llama --no-run` — **0.95s**, exit 0
- `cargo fetch --locked --offline` — exit 0, no output: the registry is complete, no network
- test binaries RUN: tokencore `70 passed; 0 failed`, llama `gguf.rs` `20 passed; 0 failed`

## Traps worth keeping
- `su - dead` from uid 1000 **prompts for a password** and hangs the tool call (measured: fails in
  1.0s with stdin at /dev/null). Use `bash -lc` — you are already uid 1000.
- ~~`/usr/local/cargo` is root-owned and not writable by uid 1000, yet cargo as uid 1000 works
  fine. Do not "fix" the ownership.~~ **SUPERSEDED — this bullet was WRONG.** In a VM provisioned
  by the imported image, a root-owned `CARGO_HOME` is exactly what broke the build:
  `failed to create directory /usr/local/cargo/git/db/rano-…: Permission denied`. The layer now
  ends with `chown -R 1000:1000 /usr/local/cargo /usr/local/rustup`, and probe 5 confirms the
  build works with `1000:1000`. See `firecode-layer-probe-5`.
- Running `cargo` under `env -i` with no `RUSTUP_HOME` makes rustup **create an empty
  `$HOME/.rustup`** — a side effect of the probe, not of the layer. Remove it afterwards.
- This session has **no `job_output` tool**, so a backgrounded `bash` job's output cannot be read
  and its completion notice did not arrive. Redirect heavy builds with `tee` to a file under
  `target/` (gitignored) so the output survives and is readable.
