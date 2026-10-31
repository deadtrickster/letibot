<!-- abstract: MEASURED 2026-10-31: the launcher's room autostart, the two-`Fresh` race the lease could not settle (and the loser deleting the winner's lease), the host key derived in bash (64-bit FNV's LOW HALF of the prime, 435 — not the 32-bit FNV prime, which gives a different key), `--stop --all` deciding liveness by argv[0] so a python stub is invisible, and `scripts/letibot` being shfmt-clean nowhere. -->

# The launcher starts the box's room — and four things measured doing it

MEASURED 2026-10-31, branch `agent/the-launcher-starts-the-room`, from `docs/room.md`'s
*Still owed* item 4.

## The lease could not settle a race, and the loser deleted the winner's lease

`CoordinatorLease::acquire` was read-then-write. Two `harnessd --room` started in the same
instant (which the launcher now makes ordinary) both read "no lease" and both answered
`Acquired::Fresh` — MEASURED with 8 threads and a `Barrier`: **two winners**. The loser then
failed at the SOCKET BIND, so the operator was told *"binding the room's socket: Address already
in use"* instead of the holder's name, and its `release()` (on the bind-error path) removed the
lease file the WINNER had written — the winner served unclaimed until its next beat put one back.

The fix is a lock for the length of the decision: `File::try_lock` on a **sibling** file
(`room-<key>.lease.lock`, never unlinked), because `write` renames a temporary OVER the lease and
a lock on that inode would follow the file that was just replaced. A filesystem that cannot lock,
and a turn that does not come in 2 s, fall through to the unlocked decision — refusing to start a
room is worse than failing to catch a second one, and the bind is still behind it. The test is
`a_race_for_the_lease_gives_one_coordinator_and_names_the_holder`, and it is red on the base tree.

## The host key in bash: the 64-bit FNV's LOW HALF of the prime, and the 32-bit variant is WRONG

The launcher has to name `room-<host key>.sock` *before* it starts anything, so
`lease::host_key_of` is mirrored in shell. The Rust hashes with the **64-bit** basis and prime
(`0xcbf29ce484222325`, `0x100000001b3`) and truncates to `u32`.

Truncation to 32 bits does commute with the ring operations, so the low half can be computed in
32-bit arithmetic — **but only with the 64-bit prime's low half (435, `0x1b3`)**, not with the
32-bit FNV variant's own prime (`16777619`, `0x1000193`). Those are different numbers, and the
difference is a silently different key: MEASURED, `MacBook-Pro` answers **`271c727b`** with the
32-bit variant and **`45a1c8db`** with the right one (which is what the Rust answers). The
starting value is likewise `0xcbf29ce484222325 & 0xffffffff = 0x84222325`. Both constants are then
small enough for bash 3.2 to spell.

A key that drifts from the Rust's does not fail loudly: the launcher checks a socket no
coordinator binds, starts a second one on every run, and is refused by the lease every time.

## `--stop --all` decides liveness by argv[0]

The loop asks `pid_argv "$pid" | head -1 | grep -q harnessd`. A **python stub's** argv[0] is
`python3`, so a test that starts its fake coordinator as a `#!/usr/bin/env python3` script gets
its record called *"stale … (pid N gone)"* while the process is alive and listening. The harness
works around it with `exec -a harnessd python3 …` (bash 3.2 has `exec -a`), which also makes
`room_up`'s `pid_argv | grep -qx -- '--room'` work.

## The room is not a session socket, and `interrupt` says so

`letibot-tui --interrupt-all` speaks the session protocol. Pointed at the room's socket it comes
back a refusal, and `--stop` reports that as *"its turns were not interrupted"* — a true sentence
about the wrong thing. Both `--stop` and `--stop --all` therefore skip the interrupt for a record
whose `role` is `room`; the coordinator's own `Drop` gives the lease back.

## A room's socket outlives its FILE

`rm -f` on the socket does not stop a coordinator: it keeps serving on the nameless inode and
`ss -lx` still names the path — the property `live` is written for, one daemon down. A test that
wants "no room here" must kill the process, not delete the file (`tests/launcher.sh` writes the
binder's pid beside its socket for exactly that).

## `scripts/letibot` is not shfmt-formatted anywhere

`shfmt -d` on the base file: **1317 changed lines at `-i 2`**, **403 at `-i 2 -ci`**, and defaults
(tabs) would rewrite every indented line. The file is hand-formatted — `{ echo x; return 0; }`
one-liners, `cmd ;;` case arms, hand-wrapped flag lists — so `shfmt -w` on it is a whole-file
reformat and a conflict magnet in a file three branches touched in a fortnight. What was done
instead: the new code is written shfmt-clean (`shfmt -i 2 -ci -d` over the extracted room block
is empty; the case arms keep the file's `cmd ;;` idiom, which shfmt would split for all ~50 arms),
and the file-wide reformat is left as its own decision.
