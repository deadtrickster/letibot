# VM boot time against host load (R234)

Measured on lab2x1 (32 threads), 2026-11-15.

## Sources

- firecode's per-run boot logs (`/build/firecode/runs/up.*.log`, 110 runs from 2026-11-12 00:13 to
  2026-11-13 15:48). Start is the run id's timestamp, the VMM start is firecracker's first ISO
  timestamp, and guest ready is the guest's `exec channel ready` line (guest uptime). Preparation is
  start to VMM start: the config drive (2 GB) and packing the copy into its drive.
- Load is sysstat's 10-minute sample (`sadf -q`) after the start, 5-minute average.
- One fresh boot of an empty project (`firecode up`, nice 10) at load 22.

## Results

| load5 | runs | ready | prep p50 / p90 / max (s) | guest boot p50 / max (s) | total p50 / p90 / max (s) |
|---|---|---|---|---|---|
| 0-10 | 13 | 13 | 10 / 12 / 12 | 6 / 6 | 16 / 17 / 18 |
| 10-32 | 52 | 52 | 32 / 56 / 121 | 6 / 25 | 38 / 67 / 145 |
| 64-100 | 5 | 5 | 26 / 27 / 27 | 16 / 17 | 42 / 43 / 43 |
| 100-150 | 14 | 14 | 70 / 81 / 104 | 14 / 16 | 84 / 95 / 119 |
| 200-300 | 4 | 3 | 76 / 419 / 598+ | 14 / 15 | 89 / 434 / killed |
| 300-600 | 1 | 1 | 87 | 15 | 102 |

The fresh boot at load 22 took 24.3 s: 13 s preparation, 9.6 s to guest ready.

Runs that never became ready at low load were firecode's own test suite (dry runs, deliberate
failures). The one at high load is the finding.

## Reading

- The guest's own boot hardly moves with load: 6 s idle, 14-16 s at load 100-335.
- Preparation is what grows. Most boots at load 200-335 still prepared in 70-90 s.
- The two outliers are two boots that prepared at the same time, at load 202 on 2026-11-13:
  `firecode-20261113-154312` spent 598 s before the VMM started and was killed at letibot's flat
  600 s deadline with the guest 1 s into its boot. `firecode-20261113-154841`, started 5 minutes
  later, prepared for 419 s and booted. Concurrency at high load is the failure, not load alone.
- letibot's other deadlines on the same path: `layer inherit` 300 s (not fatal), the first guest
  command 120 s. firecode's own readiness wait counts loop iterations, not seconds, and gives up
  after about 14 minutes plus the iterations' own cost.

## What letibot does about it

`crates/tools/src/firecode/boot.rs`:

- A boot queue shared by every daemon of the user: `LETIBOT_VM_BOOT_SLOTS` (default 2) file-locked
  slots in `$XDG_RUNTIME_DIR/letibot/vm-boot`, held through the copy, `firecode up` and the first
  command. A boot with no free slot waits, up to 30 minutes, then is refused naming the load and
  who holds each slot.
- The `firecode up` deadline is 600 s up to 4 per core and grows in proportion past it, capped at
  1800 s (load 202 on 32 cores: 947 s). The first guest command scales the same way from 120 s.
- A boot that queued or got a longer deadline says so as a `vm_boot` row on the session and in the
  backend's description. Every boot failure names the load it was given.
