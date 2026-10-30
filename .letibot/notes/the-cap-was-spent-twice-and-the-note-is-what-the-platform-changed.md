<!-- abstract: MEASURED 2026-10-30: `gate::excerpt` was inside its 4 KiB cap on both boxes (4 045 bytes); the row was over only because `judge` appends `StepMemory::evidence` after it, and that sentence names the shared cgroup on Linux (~370 B) and stops at an errno on a Mac (~90 B) — so the same fixture read 4 397 on lab2x1 and passed here. -->

# The gate's cap was spent twice, and the platform changed the note, not the excerpt

MEASURED 2026-10-30, branch `agent/a-cap-that-was-only-aimed-at` (`main` at `4fc0b173`), on the
operator's Mac and on `lab2x1`.

## The report said the excerpt was 4 434; the excerpt was 4 045 on both boxes

`gate::excerpt` never returned more than its cap here or there. What was over is the ROW, which is
two writers: `read_excerpt` → `excerpt(text, EVIDENCE_BYTES)` and then `judge`'s
`format!("{out}\n{line}")` around `StepMemory::evidence`. So the number the failing test printed
(`out.len()`) was excerpt + 1 + note, and the note is where the platform lives:

- **macOS**: `delegate_memory()` fails at `/proc/self/cgroup: No such file or directory (os error
  2)` → `[gate memory: uncapped — …]` ≈ 90 bytes. Row ≈ 4 196, under `EVIDENCE_BYTES + 256`, green.
- **lab2x1**: `own_cgroup()` succeeds and the refusal is *"this daemon shares its cgroup
  `/sys/fs/cgroup/user.slice/user-1000.slice/session-80945.scope` with 4 other process(es) — it was
  not started in a scope of its own …"* → ≈ 370 bytes. Row 4 397 (4 434 in the gate's own run; the
  number moves with the session id and the process count).

So "why 4 434 here and ≤ 4 096 there" is not arithmetic and not the tail: it is a sentence that
names a path on one platform and an errno on the other.

## Two smaller over-cap returns in `excerpt` itself, both latent

- `tail(text, cap)` / `end_of` returned `cap` bytes **plus** its marker (~43–78), so the
  no-failure-section path was always over by the sentence that admitted the cut.
- `words()`'s `clip(&out, budget)` + `"\n[gate excerpt: the failure section itself was cut here]"`
  was `budget` + 52 for the same reason.

Both are now `cut_to(s, room, marker)`: the marker is inside the room, never added to it.

## What the fix is, and the shape worth keeping

`EVIDENCE_BYTES` is the ROW's cap. `NOTE_BYTES` (512) is taken out of it *before* the tail is cut
(`read_excerpt` hands `excerpt` `EXCERPT_BYTES`), and `judge` clips the note to `NOTE_BYTES` — so
the room is spent once, by the one function that builds the row, and the invariant holds whatever
the sentence says. The excerpt is *not* "cap + a bit": the two `EVIDENCE_BYTES + 256` assertions
are now `<= EVIDENCE_BYTES`.

## The rake

**A cap that two writers spend separately is not a cap, and a fudge factor is where the platform
hides.** If a length assertion in this tree carries `+ 256`, `+ 1 KiB` or `+ a bit`, that number is
standing in for a second writer — find it before tightening the bound, or the bound will pass on
the box you test on and fail on the one that names a path.
