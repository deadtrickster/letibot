<!-- abstract: The record of the 2026-10-30 batch: eight `conflict` rows and two `failed` rows, the rebased tip of each of the ten branch refs with its old tip, the conflicts each met and how each was resolved, the command that verified it and the result line, and the two `failed` rows' verdicts (one fixed by a cherry-pick, one fixed by the rebase). -->

# The wedged batch: ten branch refs put back on `main`, and what verified each

MEASURED 2026-10-30, worktree `agent-rebase-the-wedged-batch`. `main` was `75b66024` throughout
and was never written to. **This commit is the RECORD; the work is ten branch refs**, listed
below with their tips — `git show-ref` in a clone that has only this branch cannot see them, so
every sha is here in full.

The operation, per branch, was `git rebase --onto 75b66024 <the branch's fork in the old main>
<branch>` (see `the-queues-rebase-replays-the-old-main.md` for why not the plain
`git rebase 75b66024`). Each was done in its own throwaway worktree under `/tmp`, which is gone;
the branch ref was moved with `git update-ref refs/heads/<b> <new> <old>`.

## The table

| branch | old tip | **rebased tip** | conflicts met | how resolved | verification |
|---|---|---|---|---|---|
| `agent/a-streamed-reply-is-drawn-twice` | `0eed7127` | **`c45926443e5b439ef9f915fc3e9e39d833e61905`** | none | — | `cargo check -p letibot-tui` → `Finished dev profile`; `cargo test -p letibot-tui --lib` → **1051 passed; 0 failed; 5 ignored** |
| `agent/landed-thinking-is-drawn-once` (onto ①) | `d81a32a3` | **`35b9d8648e2e0e60c862efb4116aec92c47bc197`** | none | — | `cargo test -p letibot-tui --lib` → **1055 passed; 0 failed; 5 ignored** |
| `agent/a-click-that-cannot-be-undone` (onto ②) | `152ce1d0` | **`66dfd6ba4eda67a31d78f72b4e86c58c3dbd4ac1`** | none | — | `cargo test -p letibot-tui --lib` → **1069 passed; 0 failed; 5 ignored** |
| `agent/ctrl-n-is-a-watermark-not-a-hide` | `2a641d20` | **`bc08e32da56e2a855580704e744ccc4384f796f4`** | none | — | `cargo test -p letibot-tui --lib` → **1052 passed; 0 failed; 5 ignored** |
| `agent/a-question-that-outlives-its-row` (onto ④) | `aed31e37` | **`19fa0c2d75acd36ed19d344f174774c23757bd28`** | none | — | `cargo test -p letibot-tui --lib` → **1056 passed; 0 failed; 5 ignored** |
| `agent/a-gate-excerpt-keeps-the-failure` | `1181b74d` | **`222c7ba16c797c374d5f126962a309f4a8d9cbb6`** | 4 hunks in `crates/harnessd/src/mergequeue/gate.rs` (below) | main's body kept, the branch's excerpt re-applied on top (below) | `cargo check -p letibot-harnessd` → `Finished dev profile`; `cargo test -p letibot-harnessd --lib mergequeue::gate` → **33 passed; 0 failed** |
| `agent/an-apparatus-the-host-has-not-got` | `f0277bf6` | **not rebased — already in `main`** | — | — | nothing to land (below) |
| `agent/one-retry-row-with-a-counter-again` | `03e46490` | **`ab23ec1055989ba6ec9b88865034796e5a53b10d`** | none | — | `cargo test -p letibot-harnessd --test model_outage` → **1 passed; 0 failed**, 13.40 s |
| `agent/the-sweep-cannot-delete-unmerged-work` | `9c6e6ab3` | **`210ef6fdc8f4bba5809d48c072edb535e5283c43`** | none | — | `cargo test -p letibot-harnessd --lib` → **795 passed; 0 failed; 1 ignored** (its own files alone: `mergequeue::git` 15, `mergequeue::serve` 68) |
| `agent/a-verdict-waiting-is-not-a-dead-reviewer` | `41f2bb5c` | **`cc00b3a004305e4503de78414e890aa73709f567`** = rebase `dd2b3d04`+`c8452bbf`, then cherry-pick `4a5920bf` | none in the rebase; its TESTS were red (below) | the fixture fix cherry-picked (below) | 3 targeted tests green; `cargo test -p letibot-harnessd --lib` → **795 passed; 0 failed; 1 ignored** |

The two stacks were rebased parent-first, as the brief asked: `a-streamed-reply-is-drawn-twice`
→ `landed-thinking-is-drawn-once` → `a-click-that-cannot-be-undone`, and
`ctrl-n-is-a-watermark-not-a-hide` → `a-question-that-outlives-its-row`, each child
`--onto <the parent's rebased tip> <the parent's old tip>`. Checked after the fact:
`git merge-base --is-ancestor agent/landed-thinking-is-drawn-once
agent/a-click-that-cannot-be-undone` and the same for `ctrl-n` → `a-question` both hold.

Every patch was compared with `git range-diff <old base>..<old tip> <new base>..<new tip>`:
`=` on all of them except `a-gate-excerpt`'s first commit, whose four differences are the
adaptations named below.

`sh scripts/check-fmt.sh 75b66024` was run in each of the nine rebase worktrees and in this one:
**all formatted** (streamed 3 Rust files, landed 6, click 20, ctrl-n 6, question 17, excerpt 3,
retry 3, sweep 4, verdict 6; this branch touches none).

## `a-gate-excerpt-keeps-the-failure`: the four hunks, and why they were not side-picked

Both sides had rewritten the same region of `gate.rs`: `main` (GitHub's side of the rebase) added
`GateScratch`/`GATE_TMPDIR`/`GATE_TMP_ROOT`/`build_jobs`/`GateControl::budget` and renamed
`read_excerpt` → `read_tail` + `pub(crate)` for the host gate; the branch replaced the tail-only
`read_tail` with a whole-stream `excerpt` and added four tests. Resolved as:

1. `EVIDENCE_BYTES`'s doc comment — main's "the end, which is where a failure says why" is the
   claim the branch's commit refutes; kept the branch's wording.
2. The function's name — main's `pub(crate) fn read_tail` vs the branch's
   `fn read_excerpt`. Kept **one** function, named `read_excerpt` (the body no longer returns a
   tail), `pub(crate)`, and renamed main's five call sites in
   `crates/harnessd/src/mergequeue/gate_host.rs` (its import + 4 calls). That file is the only
   one this batch touched that the branch does not own.
3. and 4. Both sides appended tests at the same anchor, and the jobs test's body was split across
   the marker by main's edit. Kept main's three tests (`a_steps_scratch_is_short…`,
   `the_builds_jobs_come_from_the_cap…`, `a_step_is_told_how_many_jobs…`) whole, then the
   branch's four whole; the branch's end-to-end fixture grew `budget: Duration::from_secs(120)`
   because `GateControl` gained that field on main.

## `an-apparatus-the-host-has-not-got`: nothing to rebase, because it already landed

Its three commits (`bf95a3f2`, `96394f1b`, `f0277bf6`) are in `main` as `73ce93fe`, `b57657f0`,
`b5b0cb7f` — rewritten during the operator's own rebase. The evidence is a patch comparison, not
a rebase (the plain replay conflicts, and `--onto` conflicts too):

```
git diff 7becd144 f0277bf6 > b.diff        # the branch's whole change, 1402 lines
git diff e3090817 b5b0cb7f > m.diff        # what main landed,            1402 lines
diff <(grep '^[+-]' b.diff) <(grep '^[+-]' m.diff)   # empty, rc 0
```

So the ref is left where it is and **the row should be withdrawn**; rebasing it would be a no-op
that still cannot apply.

## The two `failed` rows

**`agent/a-verdict-waiting-is-not-a-dead-reviewer` — rebasing alone does NOT fix it.** Measured on
the rebased base before the cherry-pick, both of its tests failed in 0.13 s:

```
test mergequeue::serve::tests::a_verdict_waiting_at_the_desk_is_not_a_dead_reviewer ... FAILED
  assertion `left == right` failed: the verdict landed: ReviewRow { … state: Waiting, …
  failure: "waiting for the reviewer's tree: … the tree it was given (``) is gone and the row is
  about d6f76f0c…" }   left: Waiting   right: Approved
```

The cause is main's `681d722f`, which made `review::verdict` refuse a verdict about a tree that is
gone; the branch's fixture opens its desk entries with `cwd: ""`, which now reads as *the tree is
gone*. **The fix is `4a5920bf`** ("the verdict fixture's reviewer is given the tree it judged"),
cherry-picked onto the rebased branch — after it, both tests and the third
(`a_reviewer_that_died_without_handing_a_verdict_in_is_still_re_seated`) pass, and the whole
`letibot-harnessd --lib` is 795/0.

`agent/why-the-verdict-branch-fails-the-gate` (read first, as the brief said) is already based on
`75b66024`, and `git range-diff 75b66024..cc00b3a0 75b66024..47d6d0bd` says it is **exactly the
same three commits** plus three more:

```
1: dd2b3d04 = 1: 1a5831e4 queue: a reviewer whose verdict is waiting is not a dead reviewer
2: c8452bbf = 2: e1f5c0d1 notes: a reviewer's last act is the verdict and its own death
3: cc00b3a0 = 3: 4a5920bf queue: the verdict fixture's reviewer is given the tree it judged
-: -------- > 4: 7bd65307 bridge: the person's head is attached before the ask is sent
-: -------- > 5: bae150a8 queue: the deadline test's clock covers the gate's setup …
-: -------- > 6: 47d6d0bd notes: the tree a verdict is about, and the load a racy test needs
```

So either land that branch (superset) or land this row and cherry-pick `7bd65307` + `bae150a8`,
which fix two of `main`'s own racy tests and are nobody's queue row. **Do not land both** — the
verdict fix would arrive twice.

**`agent/the-sweep-cannot-delete-unmerged-work` — rebasing is enough.** `range-diff` is `=` (the
patch is byte-identical to what the gate judged), so if the red had been its own test it would
reproduce here; at the rebased tip the whole `letibot-harnessd --lib` is **795 passed, 0 failed**.
I did not read the row's failure text — the store is off-limits — so I cannot name what the
original red was, and I am not going to invent one.

## The semantic conflict, in one sentence (the thing the brief asked for)

**`agent/a-verdict-waiting-is-not-a-dead-reviewer` is the only one, and it is not in a branch's
code:** both sides changed behaviour and were both right — `main`'s new rule says *a verdict about
a tree that is gone is not a verdict*, and the branch's fixture spelled "a reviewer with no tree"
as `cwd: ""`, which the new rule reads as exactly that. `4a5920bf` is the decision (the fixture is
given the tree it judged); I took it because the fix branch had already taken it and measured it,
but it is the operator's call, not mine.

Everywhere else the conflicts were textual: five `tui` branches, `one-retry-row`, `the-sweep` and
the verdict rebase met none at all, and `a-gate-excerpt`'s four hunks are mechanical (a comment, a
name, and two appends at one anchor) with no behaviour for either side to lose.
