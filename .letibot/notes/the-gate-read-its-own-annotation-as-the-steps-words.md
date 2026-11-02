<!-- abstract: A step's stored output ends with lines the GATE wrote - `[gate memory: …]`, `[step: …]` - so a classifier that reads the raw string matches "no such file" inside the gate's own note; `setup::step_words` is the door that drops them. -->

# The gate read its own annotation as the step's last words

`StepMemory::evidence` appends the gate's own lines to a step's stored output: `[gate memory: …]`,
`[step: …]`, `[N process(es) … with its cgroup]`. On a Mac there is no `/proc/self/cgroup`, so every
step's note ends `No such file or directory (os error 2)` - and `setup::names_a_missing_thing`
(`crates/harnessd/src/mergequeue/setup.rs:87`), which scans the last three lines for "no such file",
"not found", "no binary" and their kin, then matched the runner's own sentence. The verdict was *the
gate's own setup failed* for a step whose tests had run.

The fix is `setup::step_words` (`setup.rs:52`): a step is judged by the step's words, and the gate's
annotations are dropped before anything reads them. Any new classifier over a step's output goes through
it.

Two facts to keep beside it. `UNJUDGED` does not mean the code is fine - the step still exited 101, and
what is missing is a cause in a shape `shows_code_failing` knows. And a `cargo test --workspace` step
always contains `test result: ok` from the targets that passed, so `shows_a_pass` is true for any
failure of that step: a step's verdict cannot rest on that line alone.
