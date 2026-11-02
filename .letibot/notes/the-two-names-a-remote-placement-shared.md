<!-- abstract: `git init --bare` from three processes on one path loses two of three every round and leaves the DIRECTORY behind, and one `incoming.bundle` per mirror handed a losing placement another placement's commit with no error. -->

# Two names `SshBackend::place` shared, and neither fails the way you expect

`[ ! -d <mirror> ]` is the wrong guard. Three placements each pass it before anything exists,
the two losers of `git init --bare` leave the directory behind, and every later placement skips
the init and fails its fetch on a directory that is not a repository - reported as *handing the
bundle to the remote repository*, from a race that finished minutes earlier. The guard is now a
bounded loop on `<mirror>/objects`.

`<mirror>/incoming.bundle` was one name per mirror, so two placements of one repository on one
host wrote and fetched through the same file, and
`git fetch -f incoming.bundle +HEAD:refs/heads/letibot/<mine>` made MY ref from WHATEVER was
there. The loser was checked out at the other placement's commit and said nothing. Now
`incoming-<pid>-<counter>-<short>.bundle`, fetched once and removed.

Rule: before blaming a shared name, ask which name the two runs actually share. `place` takes
the source's toplevel, and two worktrees of one repository resolve to two different toplevels -
so three placements of three worktrees is not a collision.
