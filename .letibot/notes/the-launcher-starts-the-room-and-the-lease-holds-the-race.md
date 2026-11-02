<!-- abstract: Two `harnessd --room` started at once both took a `Fresh` lease and the loser's release deleted the winner's file, so the decision is taken under a lock on a sibling `lease.lock`. -->

# The launcher starts the box's room — and four things measured doing it

`CoordinatorLease::acquire` was read-then-write, so two `harnessd --room` started in the same instant
both answered `Acquired::Fresh`, and the loser's `release()` then removed the lease file the WINNER had
written. The fix is a lock for the length of the decision: `File::try_lock`
on a sibling `room-<key>.lease.lock`, never unlinked, because the lease itself is written by a rename
and a lock on that inode would follow the file that was just replaced
(`crates/room/src/lease.rs:209`). A filesystem that cannot lock falls through to the unlocked decision:
refusing to start a room is worse than missing a second one.

`--stop --all` decides liveness by `pid_argv "$pid" | head -1 | grep -q harnessd`
(`scripts/letibot:2118`), so a fake coordinator started as a `#!/usr/bin/env python3` script is called
stale while it is alive; a fixture says `exec -a harnessd python3 …`. And `rm -f` on the room's socket
does not stop a coordinator - it serves on the nameless inode - so a test that wants "no room here"
must kill the process. And `scripts/letibot` is not shfmt-formatted anywhere (1317 changed lines at
`-i 2`), so `shfmt -w` on it is a whole-file reformat; new code in it is written shfmt-clean.
