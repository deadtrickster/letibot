<!-- abstract: `/tmp` inside a confined session is a fresh tmpfs private to the command unless the session owns one, so a file that must outlive a call lives in the workspace - and `COMMIT_MSG.txt` at the repo root is a TRACKED file holding a past commit message. -->

# Two places that look like scratch and are not — MEASURED 2026-10-27, wt-rh9 (RH9)

`/tmp` is not scratch across calls. In the default view `confine.rs` mounts a fresh tmpfs at `/tmp`,
`/var/tmp` and `/run` for each command, so a file one `bash` call writes is gone in the next - and
nothing announces it, because `ls -d /tmp` succeeds and `TMPDIR` is empty. The cost that finds it is
`cargo clippy … > /tmp/clippy.log` in one call and a `sed` over it in the next. Where a session has a
`/tmp` of its own, `Presence::Replaced` says so in as many words
(`REPLACED_ROOTS`, `crates/tools/src/exec/confine.rs`); otherwise it starts empty.

`COMMIT_MSG.txt` at the repo root IS tracked: `git ls-files COMMIT_MSG.txt` answers it, and it holds a
past commit message of this tree, so writing yours there lands as a modification of somebody's
committed file (`git checkout -- COMMIT_MSG.txt` restores it). A message that needs care is several
`-m` flags, which git joins with blank lines.
