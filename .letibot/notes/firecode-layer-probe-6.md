<!-- abstract: firecode.layer probe 6: a CHANGED layer file still costs ~38s, not 16 — the `chown -R` forces a full overlayfs copy-up (metacopy=N) of the 1.9 GB toolchain into the per-VM upper even when the image already owns it. -->

<!-- abstract: firecode.layer probe 6: a CHANGED layer file still costs ~38s, not 16 — the `chown -R` forces a full overlayfs copy-up (metacopy=N) of the 1.9 GB toolchain into the per-VM upper even when the image already owns it. -->

# firecode.layer probe 6 — MEASURED 2026-10-10, guest `firecode` (kernel 14:00:41)

Third VM, this one on a CHANGED `firecode.layer` (a comment edit), so every RUN line re-ran.

## The claim "~16s because the image now owns the toolchain" is FALSE — ~38s, same as probe 5
Timeline from the guest's own mtimes (the layer's stamp is written after its LAST line, so its
mtime IS layer-done):
- boot (`uptime -s`) 14:00:41 · line 35 profile file 14:00:47.8 · `/var/lib/apt/lists` 14:00:51.8 ·
  `build-glm` touched 14:00:53.5 · `/usr/local/cargo/registry` 14:00:57.0 · stamp **14:01:18.9**
Boot→layer-done **37.9s** (probe 5: 40.6s). The claim was 16s: missed by ~22s.

## Why the image's ownership buys nothing: `chown -R` copies the toolchain UP
`/proc/mounts`: `overlay / … lowerdir=/base/1/upper:/base/2/upper:/base/3,upperdir=/upper/upper`,
and `/sys/module/overlay/parameters/metacopy` is **N** — so a metadata change on a lower inode
copies the whole FILE up. The chown fires whether or not the owner already matches.
Evidence, all from this boot: `/firecode/upper/upper/usr/local/cargo` = **419M** and
`/usr/local/rustup` = **1.5G** in the upper (the whole toolchain); `/dev/vdd` (the upper's fs) was
fresh at 14:00 with 2.0G used; `/usr/local/cargo` ctime 14:01:00.27 → `/usr/local/rustup` ctime
14:01:18.11 → **~18s of chown**, ~1.9 GB at ~105 MB/s. That is the entire difference between 16s
and 38s. Ownership in the image is not the lever; the chown line is.
(`/var/lib/firecode/upper/{claude,opencode}` are the harness's own overlays — 1.1G of the 1.2G
that `du` reports there; do not read them as toolchain copy-up.)

## Guards held, and each is visible in an mtime
- `/usr/local/bin/{cargo,rustc,rustup}` still **12:54** → the `[ ! -x /usr/local/bin/cargo ]` guard
  skipped install+relink. `command -v cargo rustc` → `/usr/local/bin/…`; `cargo --version` →
  `cargo 1.99.0`, **rc=0**. Profile file one line, 65 bytes.
- `/home/dead/Projects/llama.cpp/.git` still 12:59 and `build-glm/bin/libllama.so*` still 12:59 →
  clone guard held, cmake re-ran configure and ninja had nothing to relink.
- Lines that DID work: profile write (0.0s), apt (4.0s: lists rebuilt then `rm -rf`'d — the lists
  dir is empty and dated 14:00:51.8), cargo fetch (~5.9s, warm registry), chown (~18s).
- `target/` did NOT exist in this VM, so `cargo check -p letibot-llama` was a full graph compile
  (artifacts 14:03:33.5, job started ~14:03:04 → ~29s); the incremental re-run says
  `Finished dev profile … in 0.02s`, rc 0.

## Trap, again, and it cost real data
No `job_output` tool and no completion notice for a backgrounded job: the first `cargo check`'s
stdout — including its seconds — was lost. Redirect with `> file` (not a pipe) and read the file.
