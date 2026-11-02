<!-- abstract: Two traps in a fixture that fakes several hosts: `case "$*" in *gone*)` matches the word anywhere in argv, so a work directory named `fanout-gone` reports a healthy host unreachable; and a `BTreeMap` re-orders a scenario's roles while `serde_json` does not. -->

# Two traps in a fake ssh that fakes several hosts

The design fact - a branch is a job of another host's table, and `exec::host::Aggregate` is the seam
that lets this session's `wait_job` see it - is in `docs/remote-hosts.md`, under *Jobs and processes*.

- **`case "$*" in *gone*)` is not a host test.** It matches the word anywhere in the fake's argv, so
  a test whose work directory is named `fanout-gone` reports a healthy host unreachable for a reason
  nobody reads twice. Match the destination, not the line.
- **`serde_json` keeps a call's key order and `BTreeMap` does not**, so a scenario's `roles`
  collected into a `BTreeMap` starts them alphabetically - which is how a test asserts an order it
  never asked for.
