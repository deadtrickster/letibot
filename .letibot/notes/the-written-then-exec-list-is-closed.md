<!-- abstract: `write_stub` proves a stub by exec'ing it with no arguments, so a no-argument run must be harmless and quick - and the one production site, `sudo.rs`'s `write_shim`, ends in a probe of its own. -->

# The written-then-exec'd list is closed — where each site went, and the two guards

`letibot_tools::testing::write_stub` (`crates/tools/src/testing.rs:840`) proves the file by
exec'ing it with no arguments. Two spellings: `[ $# -eq 0 ] && exit 0` as the script's first
line, above any prelude; or `LETIBOT_STUB_PROBE` set on the probe's environment alone, for a
stub whose real run also takes no arguments.

A stub that logs is the one that bites: the probe adds a line to every fixture that asserts a
log's exact contents.

The list is closed. The eight fixtures the ETXTBSY note named plus eight a sweep found
(`grep -rn 'set_permissions\|from_mode(0o7' crates`) all go through the helper, and
`harnessd/src/sudo.rs`'s `write_shim` is the one production site - it ends in
`wait_until_executable`, whose probe is `sudo --version`, the one invocation that answers with
no password and no terminal. Its tmp-and-rename bought nothing: `deny_write_access` is asked of
the INODE, so the temp's inode is the shim's.

The race is provable in a second: write the file by hand, clear `FD_CLOEXEC` with `fcntl`, spawn
a `sleep 1` holder that keeps the write descriptor across its exec, and assert a plain exec is
refused with `ExecutableFileBusy` before `write_stub` is called.
