<!-- abstract: MEASURED 2026-10-31: seating a `Write` tool on `m1_orchestrator` makes `Mode::requires_given` demand a writable backend, so `letibot --read-only` and the room coordinator refuse to open — and a daemon must register a tool BEFORE `with_session_tools` or every open is a Duplicate. -->

# A `Write` tool on the orchestrator seat closes every read-only session — and the registration order is a second trap

MEASURED 2026-10-31, branch `agent/the-host-changelog`, on lab2x1, adding `host_changelog`
(`Access::Write`, a file under the config dir).

## 1. `m1_orchestrator` + a `Write` tool = twelve red harnessd tests

Seating it on the default seat made 12 `-p letibot-harnessd --lib` tests fail, all with the same
sentence:

```
the `always-ask` mode needs 2 prerequisites for what this session seats, and 1 of them are missing.
  missing: writable backend
```

The mechanism: `Mode::requires_given` (`crates/tools/src/mode.rs`) maps `Prereq::WritableBackend`
to `seats.write`, and `seats.write` is `has_write_tools` read off the RESOLVED SCHEMAS at open
(`harness.rs`, ~4935). Both `ALWAYS_ASK` and `WRITES_ALLOWED` require it. The orchestrator seat is
the read-only one by construction — `letibot --read-only` sets `ROLE=orchestrator; ADJ=""` (no
downgrade, read-only backend) at the launcher's default `MODE=writes-allowed`, and the room's
coordinator session (`s-room-<host key>`) is `Seat::Orchestrator` too. So the refusal would have
reached both commands, not just the fixtures.

**The rule**: a `Write` tool belongs on a seat that always opens with a writable backend —
`leticode` does, because its role already seats `write`/`edit` and `may_write` follows the role. A
seat whose whole point is *read-only* does not get one, however good its `list` verb would be
there.

## 2. Register it BEFORE `with_session_tools`, or every open is a Duplicate

`with_session_tools` now seats a `nowhere()` `host_changelog` where none is registered (the rule
`queue`, `plugin` and `room` already follow: a role naming a tool must resolve on any registry that
function builds). `Registry::register` errors on a duplicate NAME — so `Harness::open` must register
the real, scoped tool BEFORE calling `with_session_tools` (it does, at the same place `room` is
registered). Registering after is `Setup("registering the host changelog: … already registered")`
for every session on the box.

## The rakes under both

* **A test suite that opens only the default seat will not notice a leticode-seat problem**, and a
  suite that opens only leticode will not notice this one. The 12 reds came from `harness::tests`,
  which opens the default seat.
* The failing assertion names the mode and the missing prerequisite, not the tool that made it
  needed — the tool is nowhere in the message. If a `missing: writable backend` appears after a
  seat change, read `Mode::requires_given` before anything else.
