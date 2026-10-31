<!-- abstract: MEASURED 2026-10-31: a 25-commit stack whose base had been rebased into main shows up as add/add conflicts on files BOTH sides added (git cannot see that they are the same content from rewritten shas), and `git rebase --onto main <stack tip>` is the operation that drops the landed commits instead of resolving them by hand. -->

# A stack that is already in main conflicts as add/add, and `--onto <stack tip>` is the fix

MEASURED 2026-10-31, branch `agent/the-bell-per-client` (worktree of the same name), asked to rebase
a 25-commit room stack onto `main` at `cc1496ab`.

## The symptom, and why it reads as a real conflict

`git rebase main` stopped at the stack's *fourth* commit with **add/add** conflicts on
`plugins/builtin/room/{plugin.toml,src/lib.rs,src/tests.rs}` and content conflicts in
`crates/sessionlog/src/pane.rs`, `docs/room.md`, `crates/harnessd/src/plugins.rs`. Every one of those
files exists on both sides with (nearly) the same content, because the stack's commits had **already
been landed** — main's `362e8fa2 room: the pane — …`, `ec68f2ed room: a tag's agent …`,
`9cd2b8b1 harnessd: a tag's bell is its own` are the branch's own `2984ffb4`, `8ae4bcf8`, `489b7ef7`
under rewritten shas. Git compares trees, not provenance, so an add/add on identical content looks
exactly like two people who wrote the same file.

## How to tell, in two commands, before touching the conflicts

```sh
git log --oneline main..HEAD          # 25 commits — is that plausible?
git log --oneline main --grep="<subject of a stack commit>"
```
The second one is the measurement: main's log carried **the same subjects** under different shas.
`git cherry main HEAD` is not enough on its own (it said 7 of 25 were absent, 5 of them landed ones —
its patch-ids differ because the landed version was rebased over a base the branch never had; the
standing note `the-whole-inventory-on-one-pin` says the same thing from the other end).

## The operation

```sh
git rebase --onto main <the stack's tip, exclusive>
```
i.e. replay **only the commits that are actually new** (here two) onto main. It completed with no
conflicts at all, and the result is `main` + the two. Resolving those add/add hunks by hand would
have produced a *worse* tree — main's copies are the ones with 125 commits of later work in them.

## Rakes

- **A rebase that goes through cleanly is not a measurement.** Mine touched `crates/harnessd/src/room/link.rs`
  and `crates/room/src/wire.rs`, both of which main had changed under the branch; only compiling and
  running the suites says the two sides agree. The check that they do not *contradict* is a grep of
  main's own copy for the mechanism you added (`missed`, `note_gap`, `wake::take`) — main had none of
  them and still spoke protocol 3, so the bump to 4 was uncontested.
- **`git status` before writing anything on a resumed branch.** A `write` that died with the previous
  turn's daemon restart had left a half-written file over a successor's committed one, and ` M` in
  `git status` was the only sign; `git checkout -- <path>` was the whole repair.
