<!-- abstract: MEASURED 2026-10-27 in wt-rh9 on lab2x1 (tools inside a bwrap boundary): /tmp is PRIVATE TO EACH bash CALL — a file one call writes is gone in the next — and COMMIT_MSG.txt at the repo root is a TRACKED file holding a past commit message, so writing yours there overwrites a tracked file. -->

# Two places that look like scratch and are not — MEASURED 2026-10-27, wt-rh9 (RH9)

## `/tmp` is per-call inside this boundary

```sh
$ echo hello > /tmp/rh9-probe.txt   # one bash call
wrote
-rw-rw-r-- 1 dead dead 6 Oct 27 02:07 /tmp/rh9-probe.txt
$ cat /tmp/rh9-probe.txt            # THE NEXT call
cat: /tmp/rh9-probe.txt: No such file or directory
```

`ls -d /tmp` succeeds and `TMPDIR` is empty, so nothing announces it. The cost
that found it: `cargo clippy ... > /tmp/clippy.log 2>&1` in one call, then a
`sed` over that file in the next — *file not found*, which reads like a broken
redirection and is the boundary.

**So: a file that must outlive a call lives in the workspace**, and a JSON/text
report another call will read belongs at a path under the checkout (and removed
before committing, or it arrives tracked-looking). Two calls that must share
state share it through the tree or through stdout, never through `/tmp`.

The first symptom of this shape is the `2>&1 > file` mistake looking innocent:
that spelling writes stdout to the file and leaves stderr on the terminal, so
the console shows the warnings the file is supposed to hold. Both halves were
true here, and the second masked the first.

## `COMMIT_MSG.txt` at the repo root is TRACKED

`git ls-files COMMIT_MSG.txt` answers `COMMIT_MSG.txt`: it holds a past commit
message of this tree (*"the background trial failed: the theme's slots are
bands, not tints"*) and it is version-controlled. A commit message written
there lands as a modification of somebody's committed file.
`git checkout -- COMMIT_MSG.txt` restores it.

The safe spelling for a message that needs care: several `-m` flags (git joins
them with blank lines) rather than a file in the tree.
