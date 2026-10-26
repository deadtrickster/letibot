<!-- abstract: MEASURED 2026-10-26 (RH6): a fixture's fake ssh inherits the developer's PATH, so the mise default probe (which runs ON the fake "remote") finds the real /opt/homebrew/bin/mise and wraps every command — a test that means "fixture" must say `mise = false`; and the breaker/records are per PROCESS, so a migrate test that spawns `letibot` per call can never accumulate one. -->

# A fake ssh inherits this box's toolchain, and the breaker is per process

MEASURED 2026-10-26, package RH6 (`crates/tools/src/rpc.rs`, `crates/tools/src/remote.rs`), while
adding the mise wrapper and the process-wide circuit breaker.

## The default mise probe asks the *machine running the test*

`mise` is resolved by default (no `mise` line in `hosts.toml`) with a remote probe:
`[ -x "$HOME/.local/bin/mise" ] || command -v mise`. Every fake ssh in this tree is a shell script
that does `export HOME=<its own home>; export PATH=<bin>:$PATH; exec bash -lc "$last"` — so
`command -v mise` finds **the developer's own** `/opt/homebrew/bin/mise`, and every assertion the
test makes about the command line is then made about `mise exec -C … -- bash -lc …` on that box and
something else on a box without mise. `~/.local/bin/mise` first means a test that *plants* one there
wins regardless of PATH, which is how the default-probe test is deterministic.

The rule for fixtures: `mise = false` in their `hosts.toml` / their `Host { … }` literal, with a
comment saying so. That also exercises the key. Written into every fixture that could otherwise
inherit the box: `remote_hosts.rs`'s `Rig::new`/`host_named`, the migrate fixture's hosts.toml, and
`builtins/hosts.rs`'s `host()`.

## The breaker and the ring are per PROCESS, and `--migrate` is one process per call

`rpc::before`/`after` and `rpc::record` are `static`s keyed by the host's name — the contract wants
one breaker shared by the backend, `hosts` and `migrate`, and three private ones would cost three
timeouts each. Consequences a test writer trips over:

* **A test that opens a breaker opens it for the whole test binary.** Two tests using one host name
  share the streak: `remote_hosts.rs`'s unreachable-host test uses `u1`/`u2` rather than `h1`/`h2`
  for exactly this reason (three failures would turn a later test's *reachable* into *not
  reached*). Unique names, or one host per test.
* **A retried step counts twice.** `Ssh::call` reports the breaker once per *attempt*, so a
  `Retried::Once` step that fails twice is two connection failures — three attempts open it, i.e.
  two failing steps.
* **`letibot --migrate` cannot accumulate a breaker at all**: the migrate tests run the binary as a
  child, so each invocation starts with an empty one. A migrate test of rule 5 would have to reach
  three failures inside a single run — which no path does, because the first failing step ends the
  verb. Rule 5 is therefore measured in `rpc::tests` (the state machine) and `remote_hosts.rs` (the
  same state through a real backend); migrate's own test measures rule 1's bound instead.

## A job's poll is a heartbeat and would fill the ring on its own

`RemoteProcesses::refresh` polls every 120 ms, and `RECORD_RING` is 256 — so recording successful
polls means one `job_wait` of thirty seconds erases every call a reader could act on. `Ssh::attempt`
now takes `Reported::Always | Reported::Failures` and a poll is only written down when it fails.
The test asserts zero `asking about a job` records *and* that the host's count stays under a
hundred — not *which* records survived, because the ring is shared by every test in the binary and
another host's traffic can legitimately evict a placement's early lines.
