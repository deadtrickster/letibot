<!-- abstract: `mise exec -C <dir>` exits 1 when the directory does not exist and prints `$HOME` as `~` in that message, so a placement that points the wrapper at a checkout before `git worktree add` makes it dies on every host with mise. -->

# `mise exec -C` refuses a directory that is not there, and its error says `~`

`mise exec -C /tmp/definitely-not-here -- true` exits 1 with
`mise ERROR Directory specified with --cd does not exist: /tmp/definitely-not-here`. There is no "run it
where I am" fallback. And mise abbreviates `$HOME` to `~` in that message - with `HOME=/tmp` it printed
`~/definitely-not-here-xyz` for a path given as absolute - so a failure reading `~/letibot-remote/...`
is mise's own rendering, not a tilde passed through quoting.

What it broke: `SshBackend::place` set `-C` to the checkout the moment it could be spelled, before
`git worktree add` had made it, so the next step died on every host with mise and on no host without
one. `dir_arg` (`crates/tools/src/remote.rs:1166`) now points at the host's `root` until the checkout
exists, and expands a leading `~` the way `path` entries are.

The test is `a_placement_through_a_mise_that_insists_its_directory_exists`
(`crates/tools/tests/remote_hosts.rs:1733`), and its point is a stub that validates its `-C` with
`[ -d "$3" ] || exit 1`. A stub doing `shift 4; exec "$@"` checks the argv SHAPE, not whether the
directory is usable.
