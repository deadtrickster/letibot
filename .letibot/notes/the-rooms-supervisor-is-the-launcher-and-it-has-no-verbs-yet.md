<!-- abstract: MEASURED 2026-10-31 (branch agent/the-room-asks-for-a-restart): the only thing that starts a coordinator on this box is the launcher's `room_ensure`, and its `--room`/`--room --stop` verbs live on an UNLANDED branch — so a supervisor that runs `letibot --room --stop` refuses with `unknown flag: --room` until that lands; and `write_stub`'s probe execs a stand-in with no arguments, so a fixture that logs its argv logs its own proof. -->

# The room's supervisor is the launcher, and on this branch it has no verbs

MEASURED 2026-10-31, branch `agent/the-room-asks-for-a-restart` (stacked on `agent/the-rooms-pane`,
`06583ab2`), building `docs/room.md` § *Still owed* 1 — the pane's restart control.

## Who starts a coordinator, and who can restart one

- `harnessd --room` is a whole daemon (`crates/harnessd/src/room/coordinator.rs`); nothing inside a
  daemon may fork another daemon.
- **Nothing resident watches it.** The only thing that starts one is `scripts/letibot`'s
  `room_ensure`, on the branch `agent/the-launcher-starts-the-room` (commit `88cebaf7`), which is
  **not in `main` and not in this stack**: `git merge-base --is-ancestor 88cebaf7 main` is false, and
  no branch reachable from `main` greps `room_ensure`.
- So `supervise::Launcher` runs `<launcher> --room --stop` then `<launcher> --room`, and on a box
  whose installed launcher predates that branch it refuses with `unknown flag: --room` — **said, not
  swallowed**, as `room_restart_refused`. The daemon-side design does not depend on anything else in
  that script.
- The launcher writes the record `letibot --ls`/`--stop` read, which is the argument for asking it
  rather than spawning `harnessd --room`: a coordinator the daemon started itself has no record, so
  `--stop` would not see it.

## The two test rakes, both silent

1. **`write_stub`'s probe is an EXEC of the stand-in with no arguments.** A launcher fixture that
   appends `"$*"` to a log records the probe as an empty first line, so the assertion about
   `lines[0] == "--room --stop"` fails every time — and the fix is the stub's own rule (a
   no-argument run must be harmless): `[ -n "${LETIBOT_STUB_PROBE:-}" ] && exit 0` first.
   The ETXTBSY half of the same helper is the existing note
   `a-written-then-exec39d-file-races-every-fork-in-its-binary.md`; the *probe being recorded* is not.
2. **`room::link`'s `the_feed_snapshot_carries_the_rooms_identity_and_its_items` was racy on its
   own**: it waited for `unreadable.is_none()` but not for the item, and the reader thread's FIRST
   read is taken at connect — before the test's post — so it could exit on a legitimately empty
   snapshot. Wait for the item too. (It only started failing when this branch added tests beside it;
   the race was already there.)

## What the marker is keyed on, and why not the identity sentence

`Server::identity()` is prose whose age moves every second. The restart marker's instance is the
**lease's `started_ms`** (`<socket>.lease`, written by `CoordinatorLease::write`), a number two
readers compare — so `Marker::claims(instance)` can answer *has this room already been accounted
for* without parsing a sentence. `Server::identity_at(now)` remains the pane's *how long*.
