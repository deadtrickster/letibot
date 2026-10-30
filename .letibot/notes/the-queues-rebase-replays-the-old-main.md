<!-- abstract: MEASURED 2026-10-30 on ten wedged branches: `git rebase <main>` replays every commit between the branch's merge-base with the NEW main and the branch, which after a main rewrite is the whole of the OLD main — so the conflict is main against itself, and `git rebase --onto <main> <the branch's own fork in the old main>` replays only the branch's commits (five tui branches: 0 conflicts). -->

# The queue's rebase replays the old main, so every branch cut before a rewrite conflicts

MEASURED 2026-10-30, worktree `agent-rebase-the-wedged-batch`, ten branches the queue had
parked as `conflict` after `main` was rebased onto GitHub's main (`75b66024`).

## What the queue does

`mergequeue/git.rs::rebase_in` is `git checkout --detach <head>` then `git rebase <onto>` — the
plain form. Its base is `merge-base(branch, onto)`, and after a main rewrite that merge base is
the last commit the two histories share **before** the rewrite. Everything the old main had
after it is then replayed onto the new main: commits whose content is already there under
different shas.

For `agent/a-streamed-reply-is-drawn-twice` (ONE commit of its own, `tui` only) the replay was
six commits and four conflicts, in `crates/harnessd/src/cli.rs`, `mergequeue/gate.rs`,
`mergequeue/gate_host.rs` (add/add), `tests/gate_host.rs` (add/add) and `docs/remote-hosts.md`.
None of them is about the branch. They are the old main fighting the new one.

## The operation that is right

```
git rebase --onto 75b66024 <merge-base of the branch with the OLD main> <branch>
```

i.e. replay only the commits the branch authored. The old fork point is
`git merge-base <branch> backup/main-pre-rebase` (the box keeps `backup/main-pre-rebase`, the
tip before the rewrite) — or, equivalently, the commit the branch's own log stops at.

With that, five `tui` branches and three others rebased with **zero** conflicts, and
`git range-diff <old>..<branch> <new>..<rebased>` reported every patch `=`. The two branches
that did conflict conflicted for a real reason (`gate.rs`: the branch rewrote `excerpt` while
main's GitHub side had rewritten the same file around it).

## Two rakes under it

* **A stacked branch must be rebased onto its rebased parent, not onto main.** With
  `--onto main <old fork>` a child replays its parents' commits too, giving them new shas that
  the parent's own branch does not have — two branches, two histories for one patch. Rebase the
  parent first, then `--onto <the parent's new tip> <the parent's old tip> <child>`.
* **"Already landed" looks exactly like "conflicts".** `agent/an-apparatus-the-host-has-not-got`
  conflicts on the plain replay, and with `--onto` it still conflicts — because its three commits
  are already in main as `73ce93fe`/`b57657f0`/`b5b0cb7f`, rewritten during the rebase. The test
  that settles it is a patch comparison, not a rebase:
  `diff <(git diff <old main> <branch>) <(git diff <the commit the rewrite landed at> <its tip>)`
  — here byte-identical, 1402 lines both sides. Nothing to land; the row should be withdrawn.

## And one that is a queue defect, not a branch's

`rebase_in` could do the right thing by itself: the branch's fork point is knowable
(`git merge-base --fork-point` against the old target ref, or `git rebase --onto <onto>
$(git merge-base <head> <onto>)` is what it already does). What it cannot see is that the
target's history was *rewritten* — the daemon has no record of the target's previous tip, so
nothing tells it the commits it is about to replay are already there. That is the piece a fix
would need: a per-repository record of the target tip the last gate ran against.
