<!-- abstract: MEASURED 2026-10-30 on the operator's store: `queue status` printed 98 lines for one repository's question, and ONE rejected review row's `evidence` is up to 7 828 characters — so "one row, one line" is false; the scoped/bounded shapes and the numbers behind them. -->

# A queue row is not one line, and `status` printed 98 of them

MEASURED 2026-10-30, branch `agent/a-status-line-for-one-project` (`crates/harnessd/src/mergequeue/desk.rs`,
`crates/tools/src/builtins/queue.rs`), from the operator's two reports: *"queue status tool call should
show only status of the current project"* and *"the problem im solving is not mine but yours - it is more
than 60 rows for queue status tool call now - and you guys defocus and get confused"*.

## The numbers, on this box's store (copied read-only into a scratch store)

| shape | lines |
|---|---|
| `status` (default: the session's repository, bounded) | **11** |
| `status all` (one line per repository) | **26** (header + 25 repositories) |
| `status repository: "letibot"` (that one in full) | **11** |
| what the tool printed before this change (every repository, full) | **98** |

And the reason a bounded shape needs a character cap as well as a row cap: over 168 review rows the
longest `failure` is 399 characters but the longest `evidence` is **7 828** — a rejected verdict's
argument, on one line, ~2 000 tokens for one row. The bounded shape cuts a row's words at 200; the
named shape keeps them whole, which is what it is for.

## The three shapes, and the escape that works

`status` = the session's own repository (target + evidence, each queue counted by state, ten rows,
`+N more`). `all: true` = one line per repository (`letibot  GREEN · 2 reviewing · 2 queued · 1 gating`,
plus `· unserved` when nobody holds the lease and rows are open). `repository: "<path, tail or
directory name>"` = the full shape, which is also what `/clear` prints.

The `+N more` line points at `repository: "."` — **`.` resolves against the session's workspace through
git**, so it names the session's own repository without the model having to spell a path. A test asserts
that pointer works, because a pointer that does not is worse than no pointer.

## Rakes

**A count appended after a closing paren reads as a different number.** `format!("every repository ({})",
n)` then `push_str(", 2 red")` gives `every repository (2), 2 red`; the header is built first and
interpolated once. A test caught it, not a reading.

**The default must not be `None` when the scope cannot be resolved.** The old code did
`resolve(..).ok().map(..)` and passed `None` to a function where `None` means *every repository* — so a
session seated above several repositories got the whole box. "I could not tell" is a THIRD answer: say so
in a sentence and name both escapes.

**A folder above several repositories is `repos_of_workspace`, not `repo_of_workspace`.** The former
answers a project's every repository (children sharing an `origin` owner); the latter answers one, or
none — and none is the case the default has to handle in words.
