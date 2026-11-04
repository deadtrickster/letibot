<!-- abstract: The Linux wide seat is `--bind / /` plus masks; the surprising parts are that a mask of an absent store would create its mount point in the operator's real home, that `/dev/null` makes a masked shell rc silent where macOS's deny is noisy, and that `--bind / /` writes more than the macOS wide profile does. -->

# A bwrap mask needs a mount point that is already there

The Linux wide seat (R123 item 1, `ViewSpec::wide`, branch `z-r123-wide`) renders as
`--bind / /` over the machine at its own paths, `--proc /proc` (still needed: the PID namespace is
unshared, so the host `/proc` bound by `/` would show pids the namespace says are not there), then one
mount per fence path from `exec::seat::SeatView::masked`, which is `seatbelt::deny_under_home(true)`
joined to the operator's home.

Four things that are not obvious:

- **A mask of an absent store is skipped, and that is deliberate.** `--tmpfs ~/.ssh` on a box with no
  `~/.ssh` makes bwrap `mkdir` the mount point — and under `--bind / /` the directory it makes is the
  real one in the operator's home. The same for `--ro-bind /dev/null`, which creates an empty FILE at
  the name. So the renderer masks only what `is_dir()`/`is_file()` says is there, and a name with
  nothing behind it discloses nothing to hide.
- **A masked file reads as empty, not as refused.** Directories get `--tmpfs` then `--remount-ro`;
  files get `--ro-bind /dev/null`. So masking `~/.bashrc` makes `bash -ic` source an empty file and
  say nothing, where the macOS profile's `(deny file-read*)` makes the same shell print
  `Operation not permitted`. The macOS list includes the shell profiles; the Linux rendering inherits
  them and is quieter for it.
- **A wide seat writes more than a macOS wide seat.** `--bind / /` gives back the user's own file
  modes and nothing else: `/etc` refuses, `~`, `/usr/local`, `/opt` do not. The macOS profile allows
  `file-write*` only under the project, the home and `TMPDIR`, so a wide command there is refused a
  write to `/usr/local/bin` that a Linux one is not. Anything that claims the two are the same
  perimeter is wrong about writes; the fence (the masked stores) is what both share, and a test holds
  that list equal.
- **`/tmp` stays the machine's, so the session scratch reaches the command as `$TMPDIR`.** The tight
  view's `--bind <session tmp> /tmp` is not emitted for a wide one; this mirrors macOS, where the wide
  profile allows the session tmp and leaves the real `/tmp` visible too. The home's own `--bind` is
  skipped for the same reason: under `--bind / /` it is a self-bind.

Fail-closed shape: `Harness::wide_seat` returns `Err`, so a `leticode` session that seats a shell
neither opens unconfined nor opens at all on a machine with no bwrap or no cgroup; the refusal goes on
the hub as `boundary_absent` and into the `Setup` error. macOS still falls back unconfined with the
same warning, and `unconfined_session` is now only for a `leticode` session that seats no shell.

Measured in this session (lab2x1, inside a letibot boundary): nested `bwrap` fails with
`Creating new namespace failed: nesting depth or /proc/sys/user/max_*_namespaces exceeded (ENOSPC)`
while `/proc/sys/user/max_user_namespaces` reads 2147483647, so it is the DEPTH of the daemon's own
namespace chain and not the count and not AppArmor. Every behaviour test of the wide seat skips here
(with `THIS IS NOT A PASS`) and only the ARGV is measurable; `crates/tools/tests/wide_seat.rs` and
`crates/harnessd/src/harness/open.rs`'s `linux_wide_seat_tests` assert both branches so the refusal is
still measured where the boundary cannot be.

The tight seat carries the fence too. `seat_view` (the tight builder) fills `SeatView::masked` with
the whole `deny_under_home(false)` list joined to the operator's home, and the renderer masks each
entry only when `classify(p) == Presence::Inside` — so a store is masked exactly when a grant has put
it in the view. That is what closes `~/.cargo/credentials.toml` (the `~/.cargo` toolchain grant is
read-only, which used to mean readable), and it also covers a `--grant-ro ~`, because `classify` reads
the caller's grants, not only the seat's. Mask the list in `seat_view` or in the renderer and one of
the two levels loses it: the wide seat has no grants to filter by, the tight one has no home of its
own (its `$HOME` is a tmpfs), so the list is absolute and the filter is the view.

The macOS profile was already right about this: its `(deny file-read* file-write*)` for the same list
is emitted after every allow, so a read-only `~/.cargo` grant never exposed the credentials file
there. The Linux rendering is what had no fence under a grant, and a test now holds the two equal.
