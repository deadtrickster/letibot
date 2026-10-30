<!-- abstract: MEASURED 2026-10-30 on ten wedged branches (eight `conflict`, two `failed`): `git rebase <main>` replays every commit between the branch's merge-base with the NEW main and the branch, which after a main rewrite is the whole of the OLD main — so the conflict is main against itself, and `git rebase --onto <main> <the branch's own fork in the old main>` replays only the branch's commits (five tui branches: 0 conflicts). -->

# The queue's rebase replays the old main, so every branch cut before a rewrite conflicts

MEASURED 2026-10-30, worktree `agent-rebase-the-wedged-batch`, on the ten rows the queue had
parked after `main` was rebased onto GitHub's main (`75b66024`): **eight `conflict` and two
`failed`**. What each branch became is in
`the-wedged-batch-rebased-tips-and-verification.md`; this note is only the mechanism.

## What the queue does

`mergequeue/git.rs::rebase_in` (lines 414-421) is `git checkout --detach --force <head>` then
`git rebase <onto>` — the plain form. Its base is `merge-base(branch, onto)`, and after a main
rewrite that merge base is the last commit the two histories share **before** the rewrite.
Everything the old main had after it is then replayed onto the new main: commits whose content is
already there under different shas.

For `agent/a-streamed-reply-is-drawn-twice` (ONE commit of its own, `tui` only) the replay was
**six commits**, and it stopped on the first of them — `8da70e18` "mergequeue: the gate runs on a
declared host" — with **seven conflicted paths**, none of them about the branch:

```
AA crates/harnessd/examples/gate_on_host.rs
UU crates/harnessd/src/cli.rs
UU crates/harnessd/src/mergequeue/gate.rs
AA crates/harnessd/src/mergequeue/gate_host.rs
AA crates/harnessd/src/mergequeue/gate_host/tests.rs
UU crates/harnessd/tests/gate_host.rs
UU docs/remote-hosts.md
```

That is the old main fighting the new one, in files a one-commit `tui` branch never touched.

## The operation that is right

```
git rebase --onto 75b66024 <merge-base of the branch with the OLD main> <branch>
```

i.e. replay only the commits the branch authored. The old fork point is
`git merge-base <branch> backup/main-pre-rebase` (the box keeps `backup/main-pre-rebase`, the tip
before the rewrite) — or, equivalently, the commit the branch's own log stops at.

With that, five `tui` branches and three others rebased with **zero** conflicts, and
`git range-diff <old base>..<old tip> <new base>..<new tip>` reported every patch `=`.

Under `--onto` **two** branches still conflicted, and for two different reasons — the count and
the causes do not collapse into one sentence:

* `agent/a-gate-excerpt-keeps-the-failure` — a REAL conflict: the branch replaced
  `gate.rs`'s tail-only excerpt with a whole-stream one, and main (GitHub's side) had rewritten
  the same region around it. Four hunks, resolved in the verification note.
* `agent/an-apparatus-the-host-has-not-got` — not a conflict at all: its three commits are already
  in `main` as `73ce93fe`/`b57657f0`/`b5b0cb7f`, rewritten during the rebase, so the replay
  conflicts with work that IS itself.

## Two rakes under it

* **A stacked branch must be rebased onto its rebased parent, not onto main.** With
  `--onto main <old fork>` a child replays its parents' commits too, giving them new shas that
  the parent's own branch does not have — two branches, two histories for one patch. Rebase the
  parent first, then `--onto <the parent's new tip> <the parent's old tip> <child>`.
* **"Already landed" looks exactly like "conflicts".** The test that settles it is a patch
  comparison, not a rebase:
  `diff <(git diff <old main> <branch>) <(git diff <the commit the rewrite landed at> <its tip>)`
  — for `an-apparatus` byte-identical, 1402 lines both sides. Nothing to land; the row should be
  withdrawn.

## And one that is a queue defect, not a branch's

`rebase_in` could do the right thing by itself: the branch's fork point is knowable. What it
cannot see is that the target's history was *rewritten* — the daemon has no record of the
target's previous tip, so nothing tells it the commits it is about to replay are already there.
That is the piece a fix would need: a per-repository record of the target tip the last gate ran
against.
