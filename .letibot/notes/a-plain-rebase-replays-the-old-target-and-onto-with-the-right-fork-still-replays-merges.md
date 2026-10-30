<!-- abstract: MEASURED 2026-10-30: `git rebase <onto>` bases itself on merge-base(branch, onto) — a commit on the OLD target once the target is rewritten — so the queue replayed the old main onto the new one; `--onto` with the right fork fixes the linear case but still replays what merges of the old target brought in, and the three git facts that decide how (first-parent list, range-diff's defaults, an empty cherry-pick). -->

# A plain rebase replays the old target, and `--onto` with the right fork still replays merges

MEASURED 2026-10-30, branch `agent/the-queue-replays-the-old-main` (`crates/harnessd/src/mergequeue/git.rs::rebase_in`), from the operator's report that nine rows sat in `conflict` after `main` was rewritten and nine branches merged into it.

## The defect, in one line

`git rebase <onto>` takes `merge-base(branch, onto)` for its base. After the target is rewritten that merge base is a commit on the **old** target, so the rebase replays the old target onto the new one and the branch's own commit rides along. Measured on `agent/a-streamed-reply-is-drawn-twice`: one `crates/tui` commit replayed six and conflicted in five files it had never touched. Re-measured against `8c9d08fb` in a fixture: `Rebasing (1/3)` → `could not apply … old main: a.txt says one`.

## Finding the fork when the old base is on nobody's target any more

The branch's own commits are the commits over **the base it was cut from**, and that base has to be *remembered* — no property of the current graph names it (that is what `merge-base` is, and it is wrong). Three things remember it:

1. the row's `onto_sha` (the tip it was last rebased onto),
2. the base the review row recorded (`placement.base_sha`, the branch's cut point),
3. **the target ref's reflog** — `git rev-list -g refs/heads/<target>`, every commit it has pointed at. This is the only source that needs no record, and it survives a rewrite because a rewrite is an update to that ref.

The fork is the *first* of those on the branch's **first-parent spine** (`git rev-list --first-parent <head>`). First-parent and not plain ancestry: a branch that merged the target into itself has newer target commits in its ancestry than its own fork, and taking one of those replays only the commits above the merge.

There must be **no fallback to `merge-base`** — that is the defect. A branch whose base nobody remembers gets a sentence, not a guess: *"could not find the branch's own base; rebase it by hand"*.

## The trap: `--onto` with the right fork still replays the merges

A branch that merged the old target into itself has, in `fork..head`, the commits those merges brought in. `git rebase --onto <onto> <fork>` replays them too (measured: it conflicts in a file the branch never touched), because rebase's todo list is `rev-list --no-merges fork..head`, not the first-parent list. So the two lists are compared — `rev-list --no-merges --first-parent --reverse fork..head` against `rev-list --no-merges fork..head` — and when they differ the own commits are cherry-picked one at a time. When they are equal (every branch the queue has ever handled) plain `--onto` is kept, which keeps git's own dropping of a patch the target already has.

## Three git facts under that

- **`git range-diff` excludes merges by default** and prints one line per patch: `1:  7c361a6 = 1:  20e25b1 subject`, the third field being the comparison (`=` same, `<` dropped from the after series, `!` changed, `>` invented). `--no-merges` is not an option it accepts.
- **`git range-diff` refuses an empty range**: `A..B C..C` is `fatal: need two commit ranges`, so "every commit came out empty" has to be special-cased rather than measured.
- **`git rev-list -g <ref>` answers nothing and exits 0** for a ref with no reflog (measured by `rm -rf .git/logs`), so *no reflog* needs no error handling — it just leaves fewer candidates.
- **An empty cherry-pick is detectable**: the sequencer stops with `CHERRY_PICK_HEAD` set, no unmerged paths and an index equal to `HEAD`, and `git cherry-pick --skip` clears it — that is `git rebase`'s `--empty=drop` in cherry-pick's dialect.

## The rake

**A rebase's base is a claim about history, and a rewrite breaks the claim without breaking anything git reports.** If a queue rebases onto a moving target, it must record (or be able to recover) *the base each branch was cut from* and replay from that, and it must prove the replay was a replay: `range-diff` before and after, with `!`/`>` refused. Also: `git rebase --onto X fork branch` is not "the branch's commits" — it is "everything in `fork..branch` that is not a merge".
