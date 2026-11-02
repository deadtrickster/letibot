<!-- abstract: Seating a `Write` tool on the orchestrator seat makes `Mode::requires_given` demand a writable backend, so `letibot --read-only` and the room's coordinator refuse to open; register the real tool before `with_session_tools`. -->

# A `Write` tool on the orchestrator seat closes every read-only session — and the registration order is a second trap

`Mode::requires_given` (`crates/tools/src/mode.rs`) maps `Prereq::WritableBackend` to `seats.write`,
which is `has_write_tools` read off the resolved schemas at open, and both `ALWAYS_ASK` and
`WRITES_ALLOWED` require it. The orchestrator seat is the read-only one by construction:
`letibot --read-only` sets `ROLE=orchestrator` with a read-only backend, and the room's coordinator
session is `Seat::Orchestrator` too.

So a `Write` tool seated there fails every open with *"the `always-ask` mode needs 2 prerequisites for
what this session seats, and 1 of them are missing. missing: writable backend"* - twelve harnessd lib
tests, and in production both `--read-only` and the coordinator. A `Write` tool belongs on a seat that
always opens with a writable backend (`leticode` does); a seat whose point is read-only does not get
one.

The second trap is order. `with_session_tools` seats a `nowhere()` stand-in for a role naming a tool,
but `Registry::register` errors on a duplicate NAME, so `Harness::open` must register the real, scoped
tool BEFORE calling it, or every session on the box opens with
`Setup("registering the host changelog: ... already registered")`.
