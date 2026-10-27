<!-- abstract: MEASURED 2026-10-27 building D16/R98: a gate ADMIT had no operator-facing channel at all (only DenialSink exists), and config/permission.json ships `cargo test*`, `pytest*`, `go test*`, `cargo build*` as allow — the exact commands R98 says must ask, so those seed rows go inert on an unconfined host. -->

# An admit had no channel, and the shipped seed allows exactly the runners

MEASURED 2026-10-27, branch `boundary-admits` (R97/D16 and R98(1)), `crates/tools/src/adjudicate.rs`
and `crates/harnessd/src/harness.rs`.

## The gate could refuse loudly and admit silently

`AdjudicatedGate` has had a `DenialSink` since denials were made visible: a refusal is a decision
taken on the operator's behalf and it goes to the hub as `SessionEvent::DenialRaised` *while the
work is happening*. An **admit** had one trace — the audit row in `self.log` — which is read by
`/gate` and by the corpus and by nothing that scrolls. So a mode that stops asking (D16) had no
way to say so, and the operator's only evidence would have been that the cards stopped.

The seam that landed: `AdmissionSink` / `AdmissionNotice` on the gate, `HubAdmissions` in harnessd
collecting one line into an `Arc<Mutex<Vec<String>>>`, and `Harness::flush_gate_admissions`
writing the transcript rows. Three shape facts worth keeping:

* **The gate runs inside the runtime, where `&mut Harness` is not in hand.** A sentence from
  there has to be handed over and spoken later — the same pattern `PluginHost::take_notices`
  uses — not appended on the spot.
* **It is drained at the round boundary, after `append_items(&results)` and never between a tool
  call and its result.** `pair_tool_calls` reads the first non-`tool` row as the end of an
  answering window, so a notice written mid-round makes the model read *no result was recorded*
  beside the real one. `append_notice` is safe only where no call is pending.
* A `Warning` is **not** the channel (the notes pane, and the register is for diagnostics), and
  neither is `DenialRaised` (nothing was refused, and the model has its result). A new wire
  variant was not needed because a transcript row was.

## `config/permission.json` is a durable allow-list of the runners

The shipped seed's `bash` block contains `cargo test*`, `cargo build*`, `cargo check*`,
`cargo clippy*`, `cargo bench*`, `cargo doc*`, `pytest*`, `python3 -m pytest*`, `go test*`,
`npm test*`, `npm run test*`, `make test*`, `make check*`, `ctest*` — i.e. **exactly the commands
R98 says must ask every time on an unconfined host**. With R98(1) skipping
`permission::Action::Allow` for a runner on a host with no boundary, every one of those rows is now
inert there (they still work inside a measured boundary — where D16 admits before 1.5 is reached
anyway). That is the ruling read literally, and it is a big visible consequence for an operator
whose laptop is a mac: every `cargo build` on the host shell asks again.

## Two smaller facts

* `Boundary::complete()` tolerates a seal that is `Unverified`; D16's admit does **not** — the
  claim *nothing here can escape* has no evidence without a tested seal. So `Boundary::encloses`
  is a separate, stricter predicate next to `complete`, and the two are meant to differ.
* `cargo fetch` carried no `network` intent until this slice — `install`/`publish`/`add`/`update`
  were in the table and `fetch` was not, so a gate asking *does this need egress?* said no about
  the one command whose whole job it is. Fixed in `intent.rs` (Network only, not PackageChange).

## The rake, if the ladder is narrowed again

`offer_for` decides the rungs, three settling arms honour them, and two recording sites write
them — and the gate *refuses an option id the request did not offer*, so the recording-site guards
are unreachable while the ladder holds. Do not go looking for a test that proves those two: what
is testable is the refusal. The guards are belts, and the ladder is the mechanism.

## Fixtures that had to move, not the code

Five existing ladder tests in `adjudicate/tests/{card,outcomes}.rs` used `cargo test` / `cargo run`
as their exec example on a host with no boundary, and R98 takes both standing rungs away from
exactly that. They now either sit on `measured_boundary()` (when their subject is the ladder) or
use a non-runner exec (`sed -i`, `cargo fmt`). If a future change breaks them, check first whether
the *fixture* is a runner rather than whether the narrowing is wrong.

## UPDATE 2026-10-27 (same day): R98(1) is behind a switch, so the seed rows are NOT inert by default

The operator, on seeing the first cut: *"asking on every build/test on the Mac would just push
them to allow-all (which they already run, unsandboxed)"*. So the R98 half is now
`[permissions] ask_unconfined_code` in a `leticode.toml` — **off by default**, either file level
may turn it on (`or`, because a repo file must not loosen what the user's own file asked for) —
and with it off an unconfined host behaves exactly as before this package. Everything in the
section above is what the switch does when it is ON; the shipped seed's `cargo test*` rows go on
working while it is OFF.

What the switch would cost, measured over 187,930 corpus decisions (161,097 with a command):
standing-answer settles over project code 4,671, of which **986** at a point that would otherwise
ask (mac: 171 of 834 — 137 of them "a program in the checkout", i.e. `./scripts/…`). D16, for
comparison, would have saved **80** of the 270 cards people answered at `writes-allowed` across
the three hosts. Instrument: `cargo run --offline -p letibot-tools --example d16_corpus --
<host>.json`, over `corpus_replay::unasked`.
