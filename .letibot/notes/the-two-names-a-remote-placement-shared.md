<!-- abstract: MEASURED 2026-10-31: `git init --bare` three at once on one path exits 128 for two of the three in 30/30 rounds (and a loser leaves the DIRECTORY, so `[ ! -d ]` is the wrong guard), and `{mirror}/incoming.bundle` being one name per mirror means a placement that loses the race is handed another placement's COMMIT with no error at all. -->

# Two names `SshBackend::place` shared, and neither fails the way you expect

MEASURED 2026-10-31 on lab2x1, branch `agent/the-placement-and-bridge-cluster` (R101), while
chasing "three placements at once on one host: two failed the bundle hand-off".

## `git init --bare` on one path from three processes: 2 of 3 exit 128, every time

Thirty rounds, three concurrent `git init --bare m`, one path, a fresh `m` each round: **every
round had two non-zero exits (128), 30 of 30.** And a loser has already made the directory by
then, which is what makes the guard wrong:

```sh
if [ ! -d {mirror} ]; then git init --bare {mirror}; fi   # a DIRECTORY is not a repository
```

Three placements each pass `[ ! -d ]` before any creates anything; the two losers leave the
directory behind; every later placement's guard then skips the init and the fetch that follows
fails on a directory that is not a repository — reported as *handing the bundle to the remote
repository*, from a race that finished minutes earlier. The guard is now a bounded loop on
`{mirror}/objects` (the directory is the answer, not `git init`'s exit status).

## `{mirror}/incoming.bundle` is one name per MIRROR, and losing that race is SILENT

The far-side copy was named by the mirror, so two placements of one repository on one host wrote
and fetched through the same file. Nothing fails: `git fetch -f incoming.bundle
+HEAD:refs/heads/letibot/<mine>` creates MY ref from WHATEVER bundle is there, so the placement
that loses is checked out at **the other placement's commit** and says nothing. Measured in the
forcing test (`two_placements_at_once_never_read_each_others_bundle`): left `e08287cf`, right
`3937ad49`, both "successful" placements. It also left 19 MB per placement in the bare mirror for
ever. Now `incoming-<pid>-<counter>-<short>.bundle`, fetched once and removed by the same call.

## The rake: two worktrees are two mirrors, so "three at once" is not automatically a collision

`place` takes `git rev-parse --show-toplevel` of the source, and **two worktrees of one repository
resolve to two different toplevels** (measured), so they name two different mirrors. Both defects
above need two placements of the SAME repository — two sessions of one worktree, a commit that
moves between two placements, or a mirror left half-made — and neither explains the original
report, whose three packages were three worktrees. Before blaming a shared name, ask which name
the two runs actually share.
