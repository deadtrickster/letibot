<!-- abstract: No user namespace can be created inside a confined session's boundary, so every test that spawns a real bwrap or process host skips loudly there; and a session's `/tmp` is a per-session directory, not a fresh tmpfs. -->

# A confined session cannot nest `bwrap`, and `/tmp` is now the session's

Inside a confined session's boundary no user namespace can be created: `unshare -U true` is ENOSPC and
a nested `bwrap` fails the same way, which is `--disable-userns --assert-userns-disabled` doing its job
(`confine.rs`'s `SealKind::NestedNamespaces`).

The consequence is a testing one. From such a session no test that spawns a real `bwrap`, or opens a
real `HostProcesses` (no delegated cgroup v2 either), can pass - they take their
`no_boundary`/`apparatus::present` branch, which is loud and green. So "the boundary tests are green
here" says nothing; those suites must run on the merge gate.

And `/tmp` inside a boundary is the SESSION's. `TmpView::Session(dir)` binds a per-session directory at
`/tmp` instead of `--tmpfs /tmp`, which gave every command a fresh filesystem and lost a file the
previous call wrote. Locally it is `<daemon scratch>/tmp/<session id>`, made by `Bwrap::probe` and
removed by `HostBackend::close`. It must be a SUBDIRECTORY of the daemon scratch: bound at `/tmp`, the
scratch itself would put `job-<id>.out` inside the sandbox where `rm -rf /tmp/*` deletes it.
