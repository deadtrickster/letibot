<!-- abstract: `gatekeeper_vm` defaults to false in `Config::for_this_box` (the constructor every fixture uses) and the daemon arms it through `Config::for_the_daemon`; `cfg(test)` could never have done this job because it does not reach an integration test. -->

# The reviewer's VM is armed by the daemon, never by a fixture — MEASURED 2026-10-12

`Config::gatekeeper_vm` decides whether the review queue's reviewers may be placed in a firecode VM
(`sessions::gatekeeper::reviewer_placement`). It used to default to `true` in
`Config::for_this_box`, and **that constructor is what every fixture in the tree is built from** —
so any test whose flow reached `serve_reviews` with a real runner placed its reviewer in a VM booted
over the fixture's workspace. Four sites had grown a hand-written `cfg.gatekeeper_vm = false`
(`session_channel` ×2, `gatekeeper`'s prompt test, `review_pass`'s placed-host arm).

## The design that landed, and why the brief's second option was void

* `Config::for_this_box` now sets `gatekeeper_vm: false` — the safe end, in the one constructor a
  test uses without thinking.
* `Config::for_the_daemon` is `for_this_box` + `allow_vm_reviewers()`; `cli::run` is its only
  caller, so production behaviour is unchanged. `letibot-m1` deliberately does not call it (it is a
  measurement script; its reviewers stay on the host, which is the pre-placement behaviour).
* `Config::allow_vm_reviewers()` is the one NAMED way the value becomes true — the daemon calls it
  once, and a test that means to exercise a VM placement calls it by name.
* The four hand-written `= false` sites are **gone**: the constructor covers them.

**`cfg(test)` cannot do this job.** The hazard lives in `crates/harnessd/tests/` (integration
tests), and `cfg(test)` is not set for the library when an integration test is built — so "Host
under `cfg(test)`" would have compiled and changed nothing for exactly the tests that booted VMs.

## The evidence to keep

On this box `firecode` is on PATH (`/home/dead/bin/firecode` → the project's build), so
`firecode_available()` is `Ok` and a VM placement WOULD have booted firecracker:
* `review_pass::a_review_child_is_placed_where_the_box_can_place_one` asserts `Placement::Firecode`
  for a fixture that calls `allow_vm_reviewers()` (the "a test that asks for one still can" half) and
  `Placement::Host` for the ordinary fixture on the same box.
* `session_channel`'s two real-spawn refusal tests now run with no line asking for anything and
  finish in ~1.2 s total, where the default would have measured and copied `/tmp` into a microVM.

`cli::tests::the_daemon_arms_the_reviewer_vm_and_a_fixture_does_not` holds the two constructor
halves, because either alone is a lie.
