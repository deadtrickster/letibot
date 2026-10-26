<!-- abstract: MEASURED 2026-10-26: on macOS `Harness::open` refuses `Seat::Coder`/`Runner` ("needs a confined execution backend"), so a harnessd test whose fixture seats `coder` measures the absent boundary instead of its subject; seat `leticode` where the seat is incidental and `cfg.unconfined` where the coder seat IS the subject. -->

# A test fixture that seats `coder` cannot open on macOS — seat `leticode`, or set `cfg.unconfined`

MEASURED 2026-10-26 on the Mac, with a loadable vocabulary (`build-glm` + the box's
vocab-only GGUF), `LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace`:

```
the `coder` role needs a confined execution backend and one could not be built over "/tmp":
io: this session asks for a project-scoped boundary and does not have one …
```

`Seat::Coder` and `Seat::Runner` are the two seats whose `needs_exec_backend()` is true, and
`Harness::open` builds `HostBackend::confined_granting` for them — which cannot be built
anywhere on Darwin. **Five harnessd test files had fixtures seated `coder` and so measured the
absent boundary instead of their own subject**: `message_between_turns` (7 tests),
`session_channel` (6), `session_lifecycle` (7), `operator_shell_turn` (1), and
`prefix_fingerprint::a_tool_that_arrived_is_named_in_the_sentence` (1) — plus `wired.rs`'s three
seat-wiring tests. The reason each was written that way is real: `coder` carries the delegation
trio (`task`, `task_result`, `task_message`) and `write`.

## Two shapes, and which to use

* **The seat is incidental** (messaging, lifecycle, subagent threads, a tool arriving in the
  prompt) → seat **`Seat::Leticode`**. It has the same trio, `write`/`edit`/`grep`/`glob`, and
  `bash` behind `allow_bash` exactly like `m2_coder` — and it is UNCONFINED by design
  (`harness.rs` sets `cfg.unconfined` from the seat), because opencode's model has no workspace
  boundary. It is also the seat a Mac's own `leticode` daemon runs, and the seat every
  already-working test in this tree uses for "a session that can DO things"
  (`operator_shell.rs`, `sudo_ask.rs`, `clear_context.rs`, `run_off_worker.rs`).
* **The seat IS the subject** (its tools, its trail, its adjudicator, its denials) →
  `cfg.unconfined = cfg!(target_os = "macos")` on a `Seat::Coder` config, as `wired.rs`'s
  `seat_config()` does. That is not an invention: `cfg.unconfined` is the field a subagent of a
  `leticode` session inherits (`harness.rs`'s `sub_cfg`). What it cannot measure is confinement,
  and nothing in those tests asserts one.
* **The boundary is the subject** → `apparatus::applies(what, cfg!(target_os = "linux"))`, or a
  skip a Linux run can still refuse. Do not invent a session to get around it.

## What the seat change did NOT fix, and what I ruled out

`session_lifecycle::a_message_to_a_parked_child_reopens_it_from_the_store` is still red on the
Mac (20 s of PATIENCE, the model stub's turn count never moves). **It is not confinement**:
`Sessions::open` builds the reopened session's cfg as `Config { ..self.base.clone() }` and only
overrides `seat` from a caller's `Owned`, so with a `leticode` daemon base the child reopens
leticode/unconfined and would have opened. Cause not established — the park window in that
fixture is 300 ms, which is the first thing to look at, not the last.
