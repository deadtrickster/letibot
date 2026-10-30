<!-- abstract: MEASURED 2026-10-30: rebasing the box's 7 commits onto GitHub's e136c695 stopped TWICE (not once), the conflict markers' HEAD is GitHub and `>>>>>>>` is ours, the box's `Judged`/`Done`/`assemble` are OURS that the remote never had (so "take the remote's shape" would delete gate_host's seam), the real breakage was 2xE0063 that no marker showed, and `cargo test | tail` reports tail's exit status. -->

# Rebasing the box's main onto GitHub's: two stops, one inverted brief, and a breakage no conflict marker showed

MEASURED 2026-10-30, worktree `agent-rebase-box-main-onto-github`, branch
`agent/rebase-box-main-onto-github`, `main` at `7becd144` (the box) rebased onto `e136c695` (GitHub).

## 1. In a rebase the markers are the other way round from how a brief usually reads them

`<<<<<<< HEAD` is the **commit being rebased onto** (GitHub's tree), `>>>>>>> <sha> <subject>` is
**ours**. Read the other way, block 1 looks like "the remote says two implementations" when it is
ours that says it — the remote's copy was the older one-paragraph text.

## 2. The four blocks were: 2 real, 2 not conflicts at all

| block | HEAD (GitHub) | ours (`8da70e18`) | resolution |
|---|---|---|---|
| 1, the module doc | the old "one implementation, R62 will add a second" | two implementations, `[queue] gate_host`, `set_gate_host` | ours — it supersedes the other, nothing of GitHub's is lost |
| 2, `run_with_memory`'s body | `intended_cap` + `placement(memory, cap)` + `GateScratch` + `run_step(…, scratch.path(), cap, …)` | the older body (`gate_target_dir`, `placement(memory)`) plus the extraction into `judge`/`assemble` | **GitHub's mechanics, ours' seam** — see 3 |
| 3, `could_not_run` vs `Judged`/`Done`/`assemble` | `could_not_run` | `Judged`, `Done`, `assemble` | **both** |
| 4, the tails | `could_not_run`'s tail + `enum StepEnd` | `assemble`'s tail + `run_gate` + `pub(crate) enum StepEnd` | **both**, and the wider `pub(crate)` |

**Blocks 3 and 4 were not a mistake in our copy.** `Judged`/`Done`/`assemble` are the box's own
seam: `mergequeue/gate_host.rs` imports exactly `{Done, assemble, judge, StepEnd, StepMemory}` from
`gate`, so "take the remote's shape and drop the old helpers" would have deleted the thing the host
gate is built on. The remote never had them — its body inlines the whole step-to-row mapping inside
`run_with_memory`. What the remote DID replace is the body around them, and that is what block 2
takes.

## 3. The one genuine adaptation

`run_with_memory` keeps GitHub's three new decisions (`cap = intended_cap(memory)`,
`placement(memory, cap)`, `GateScratch::for_run`) and calls them through the box's seam, so
`gate_host.rs` and `LocalGate` still judge a step with the same function. `gate_target_dir` survives
because `StepEnv::for_run` still uses it.

## 4. The rebase stopped TWICE, and the second stop was not the expensive one

`6855bab9` ("salvage: the slow-host fix") collided in `gate.rs` too: its `clause_of` and the rebased
`could_not_run` both wanted the same spot. Pure insertion — keep both.

**The real breakage had no conflict marker at all.** `6855bab9` added `GateControl.budget` and
patched the two construction sites that existed on the box's `main`. GitHub's two NEWER gate tests
(`…gate-scratch…` TMPDIR test, `…gate-jobs…` `CARGO_BUILD_JOBS` test) also construct `GateControl`,
and the box's `main` never had them — so the rebased tree failed with **2×E0063 missing field
`budget`**, in a file git called clean. Folded into the commit that introduces the field, so every
commit in the branch compiles.

## 5. Two rakes under the numbers

- **`cargo test … | tail` reports `tail`'s exit status.** A red build (2 errors, "could not
  compile") was reported to me as *exited 0 after 27.6s*. Use `set -o pipefail`, or write to a file
  and print `$?`.
- **A rebase flattens a merge, and the box's history had one.** `18bfbce5` is a merge of
  `ed540f8e` + `c4d2e934`; `git rebase` drops it. It lost nothing here — `ed540f8e` is the merge
  base, so its side is already in `e136c695` — and that is provable rather than arguable: the 18
  files GitHub's 7 commits touch and the 18 files that differ between the box's tip and the branch
  are the SAME set, 17 of them byte-identical to `e136c695`, and `crates/harnessd/src/mergequeue/gate.rs`
  is the only file both sides changed.

## 6. A rebased branch cannot be fast-forwarded from the branch it rewrites

`git merge-base --is-ancestor main HEAD` is **false** after this, by construction: the box's seven
commits exist on the branch as six rewritten ones. The operator's `main` has to be *moved*
(`reset --hard`), not fast-forwarded — and the old tip should be kept on a backup branch first.
