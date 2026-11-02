<!-- abstract: The gate's evidence row is built by two writers - the excerpt and the memory note `judge` appends - so a cap on the excerpt alone read green on a Mac and over on Linux, and `EVIDENCE_BYTES` is now the row's cap. -->

# The gate's cap was spent twice, and the platform changed the note, not the excerpt

`gate::excerpt` never returned more than its cap. What was over is the ROW, which has two writers:
`read_excerpt` → `excerpt(text, EXCERPT_BYTES)` and then `judge`'s `format!("{out}\n{line}")` around
`StepMemory::evidence`. The note is where the platform lives - on a Mac `delegate_memory()` fails at
`/proc/self/cgroup` and the sentence is ~90 bytes, on Linux `own_cgroup()` succeeds and the refusal
names the shared scope at ~370 bytes - so one fixture read under the bound here and over it there.

`EVIDENCE_BYTES` (4096) is now the ROW's cap: `NOTE_BYTES` (512) comes out of it before the tail is cut
(`EXCERPT_BYTES = EVIDENCE_BYTES - NOTE_BYTES - 1`, `crates/harnessd/src/mergequeue/gate.rs:51-60`),
`judge` clips the note, and `cut_to` puts a marker inside the room instead of adding it.

A cap that two writers spend separately is not a cap, and a fudge factor is where the platform hides: if
a length assertion carries `+ 256`, `+ 1 KiB` or `+ a bit`, that number is standing in for a second
writer.
