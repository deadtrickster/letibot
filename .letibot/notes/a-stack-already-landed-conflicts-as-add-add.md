<!-- abstract: A stack whose commits are already in main under rewritten shas conflicts as add/add on files both sides added, because git compares trees and not provenance; `git rebase --onto main <stack tip>` replays only the commits that are new. -->

# A stack that is already in main conflicts as add/add, and `--onto <stack tip>` is the fix

`git rebase main` on a 25-commit stack stopped at its fourth commit with add/add conflicts on files
both sides had, with nearly the same content, because the stack had already been landed under
rewritten shas. Every hunk looks like two people who wrote the same file.

Check before touching a conflict:

```sh
git log --oneline main..HEAD                        # 25 commits - is that plausible?
git log --oneline main --grep="<a stack commit's subject>"
```

If main's log carries the same subjects, the stack has landed. `git cherry main HEAD` is not enough
on its own: its patch-ids differ for landed commits, because the landed version was rebased over a
base the branch never had.

The operation is `git rebase --onto main <the stack's tip, exclusive>`, which replays only the
commits that are actually new. Resolving the add/add hunks by hand would have produced a worse tree,
because main's copies carry the later work.

A rebase that goes through cleanly is not a measurement: compile, run the suites, and `git status`
before writing anything on a resumed branch - a half-written file over a successor's committed one
shows only as ` M`.
