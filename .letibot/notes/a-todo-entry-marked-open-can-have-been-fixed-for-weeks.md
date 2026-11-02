<!-- abstract: A TODO entry marked OPEN can have been fixed for weeks - `git log -S '<the symbol the fix would introduce>'` is the one call that settles it, and a passing test is not evidence unless it is the entry's own check. -->

# A TODO entry marked OPEN can have been fixed for weeks

A brief built by grepping `TODO.md` for OPEN entries hands a session work that is already done: of
one five-item brief, four described a tree that no longer existed, and one was filed four days after
its own fix.

The one call that settles an entry:

```sh
git log --oneline -S '<a symbol only the fix would introduce>' -- <the file the entry names>
```

`-S` on the symbol, not the prose, found all four in one call each. `git log --grep=<the entry's
number>` does not: the fix commit does not carry the entry's number, because the entry was written
afterwards by a session that had not read the log.

Two rakes under the check. `git log -S` on a 25k-line file dies of the tool's 64 MiB default - give
it `memory: "2G"`, or `Killed` reads as "no such commit". And a test that passes is not evidence the
entry was fixed unless it is the entry's own check: a racy test passes alone on the broken tree too,
so the evidence is the same binary under the load that used to find it (see
`the-load-a-racy-test-needs-is-sixteen-copies-of-itself`).

When an entry is closed, close it in place - heading marker plus the commit, the mechanism and the
test.
