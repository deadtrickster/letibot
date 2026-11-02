<!-- abstract: `gatekeeper_vm` defaults to false in `Config::for_this_box`, the constructor every fixture uses, and only the daemon's `Config::for_the_daemon` arms it; `cfg(test)` could never have done this job because it does not reach an integration test. -->

# The reviewer's VM is armed by the daemon, never by a fixture

`Config::gatekeeper_vm` decides whether the review queue's reviewers may be placed in a firecode VM
(`sessions::gatekeeper::reviewer_placement`). It defaulted to true in `Config::for_this_box`, and that
constructor is what every fixture in the tree is built from, so any test whose flow reached
`serve_reviews` with a real runner booted a VM over the fixture's workspace.

`Config::for_this_box` now sets it false. `Config::for_the_daemon` is `for_this_box` plus
`allow_vm_reviewers()`, and `cli::run` is its only caller, so production behaviour is unchanged;
`allow_vm_reviewers()` is the one named way the value becomes true, and a test that means to exercise a
VM placement calls it by name. The four hand-written `cfg.gatekeeper_vm = false` sites are gone.

`cfg(test)` cannot do this job: the hazard lives in `crates/harnessd/tests/`, and `cfg(test)` is not set
for the library when an integration test is built, so a "Host under `cfg(test)`" change would have
compiled and changed nothing for exactly the tests that booted VMs.

`cli::tests::the_daemon_arms_the_reviewer_vm_and_a_fixture_does_not` holds both halves, because either
alone is a lie.
