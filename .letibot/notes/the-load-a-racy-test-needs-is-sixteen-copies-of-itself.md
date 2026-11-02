<!-- abstract: A test that races the scheduler passes alone and fails every time when sixteen copies of its own binary run at once, so "it passes when I run it" proves nothing. -->

# A racy test passes alone; sixteen copies of its own binary make it fail every time

The recipe, when a test is red in the gate's whole-workspace run and green alone:

```sh
BIN=~/…/target/debug/deps/<test binary>
for r in 1 2 3; do seq 1 16 | xargs -P 16 -I{} sh -c "$BIN > /tmp/m-$r-{}.log 2>&1 || echo FAIL-$r-{}"; done
```

Sixteen PROCESSES of one binary, three rounds. Each gets its own daemon, socket and temp dir, so they do
not interfere - they compete for the scheduler, which is the point. `mode_consent`'s
`a_mode_that_needs_nobody_applies_and_allow_all_waits_for_a_person` passed 25/25 alone and failed 48/48
this way: it spawned the person thread and called `set_mode` on the next line, so whether the person's
`Attach` landed first was the scheduler's to decide, and the daemon's refusal read like a policy answer.
The fix waits for the fact - a channel send after `HeadClient::attach` returns.

A hand-run binary is not the suite's environment either: `cargo test` exports `~/.cargo/bin` on `PATH` and
a plain ssh shell does not, so `intent::env_tests::a_path_that_finds_the_same_file_changes_nothing`
(`crates/tools/src/intent/env_tests.rs:293`) fails with *"the program `cargo` is not in it"*.
