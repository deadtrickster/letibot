<!-- abstract: MEASURED 2026-10-26 building `letibot --migrate` (RH4): `~` inside double quotes is a literal character, so a root of "~/letibot-remote" resolved to a directory named `~` in the CWD; BSD `base64` takes no file argument; a unix socket path is capped at SUN_LEN; and a Mac's /var is a symlink so a host's `pwd -P` answer never equals the test's spelling. -->

# A remote command line, and the four things a Mac does differently

MEASURED 2026-10-26, branch `session-migration` (package RH4, `crates/letibot/src/migrate.rs`),
while making `letibot --migrate SESSION --to HOST` run end to end against a fake ssh.

## `~` inside DOUBLE quotes is a literal character

The first host resolution ran `d="$root"; cd "$d" && pwd -P` with `root` interpolated as
`"~/letibot-remote"`. It does not expand — so `cd "~/letibot-remote"` created a directory
literally named `~` **under the process's cwd**, which was `crates/letibot/` in the worktree:
`git status` then carried `crates/letibot/~/letibot-remote/.letibot/{bin,src}` and a
`source.bundle`. The fix is to expand it in Rust (`"$HOME/{rest}"`), not to hope: after that
first call every path is absolute and the tilde never has to survive another quoting layer.

**Rake:** a `~` interpolated into a shell string expands only when it is UNQUOTED and first, and
that is exactly the case quoting discipline removes. Resolve it once, explicitly.

## BSD `base64` takes no file argument

`base64 FILE` is GNU's spelling. A Mac's base64 (and a Mac is one end of every move here) prints
its usage and exits 2. `base64 < FILE` works on both. Symmetrically `base64 -d` is fine on both,
so only the read direction differs.

## A unix socket path is capped by `SUN_LEN` (104 on this Mac)

`$TMPDIR` under `/var/folders/dd/…` is ~70 characters before `letibot/migrated-<s-id>.sock` is
appended, and `Registry::serve` then answered `path must be shorter than SUN_LEN` — which reads
like a launcher bug and is a kernel limit. The fixture's `XDG_RUNTIME_DIR` is now `/tmp/rh4-<pid>-<name>`.

## `/var` vs `/private/var`: a host's `pwd -P` never matches the test's spelling

`pwd -P` on a Mac resolves `/var/folders/…` to `/private/var/folders/…`, so a path the fixture
built and a path the "host" answered compare unequal while naming one directory. The fixture
canonicalizes its own directories BEFORE it writes the session's workspace into the store.

## And one that is not a Mac: a read into the SAME path truncates the source

`Endpoint::read_to(path, local)` created `local` first and then ran `base64 < path` — fine when
the two are on different machines, and a 0-byte file when a fake ssh makes "remote" mean this
box. It writes to `<local>.part` and renames now, which is the right shape regardless: a
transfer must never depend on the two ends being distinct paths.

## And one more, RH5 (2026-10-26): a tilde handed to a QUOTER is a literal too

A per-host `path = ["~/.cargo/bin"]` list is put in front of the remote `PATH` for every command a
remote session runs. The first implementation passed each entry through `quote()` — which exists to
stop the shell expanding anything — so `~/.cargo/bin` became `'~/.cargo/bin'`: a directory literally
named `~` on the far side, and nothing on `PATH` at all. Silent, and exactly the same failure as
`cd "~/letibot-remote"` above, one layer along.

The fix is to expand the tilde **into the shell's own words**: a `~/x` entry becomes
`"$HOME/x"` (quoted only after the expansion point), and an entry that is not a tilde path is
quoted as any other word is. The general rule, and it is the rule for any config value that
names a path on ANOTHER machine: **decide which side expands it, then quote everything except
that**. Quoting the whole value pulls the expansion point inside the quotes.
