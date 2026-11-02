<!-- abstract: A fixture's fake ssh inherits the developer's PATH, so the mise default probe finds the real mise and wraps every command - say `mise = false`; and the breaker is a per-process static keyed by host name, so two tests must not share one. -->

# A fake ssh inherits this box's toolchain, and the breaker is per process

`mise` is resolved by default (no `mise` line in `hosts.toml`) with a remote probe,
`[ -x "$HOME/.local/bin/mise" ] || command -v mise`. Every fake ssh here exports its own HOME,
prepends its bin to `PATH` and `exec bash -lc`, so `command -v mise` finds the DEVELOPER's mise and the
command line a test asserts about is `mise exec -C ...` on that box and something else on a box without
it. So a fixture says `mise = false` in its `hosts.toml` or its `Host` literal; the
`~/.local/bin/mise` half means a test that plants one there wins regardless of PATH.

`rpc::before`/`after` and `rpc::record` are per PROCESS statics keyed by host name. A test that opens a
breaker opens it for the whole test binary, so two tests must not share a host name. A retried step
counts twice, so three attempts is two failing steps. And `letibot --migrate` cannot accumulate one at
all, because the migrate tests run the binary as a child - that rule is measured in `rpc::tests` and
`remote_hosts.rs` instead.
