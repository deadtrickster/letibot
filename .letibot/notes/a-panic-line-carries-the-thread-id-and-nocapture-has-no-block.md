<!-- abstract: A `panicked at` line carries the thread's id between the name and the words, and under `-- --nocapture` a failing test has no `---- stdout ----` block - so a parse expecting either shape finds nothing. -->

# A panic line carries the thread's id, and `--nocapture` leaves no `---- stdout ----` block

The real shape, printed by this box's cargo:

```text
thread 'a_turn_that_blew_up_is_reported' (472254183) panicked at a/tests/httphead.rs:5:40:
```

So a parse that strips `thread '` and then expects the closing quote last answers `None` on every real panic. The name ends at the closing quote: `rest.split_once('\'')`, which is `gate::panic_thread` (`crates/harnessd/src/mergequeue/gate.rs`).

The gate's steps run `cargo test … -- --nocapture`. With nothing captured, libtest prints the panic in the run and writes no `---- <name> stdout ----` block for it, while a `#[should_panic]` that did not panic keeps its block - so a reader that looks for a panic only inside a block loses every test that actually panicked. The thread name is not always the test's either: a test that spawns a thread prints `thread '<unnamed>'`.

Rake: a shape you have not printed is a shape you are guessing at. The fixtures had `thread 'worker::a_test' panicked at` and a stdout block for the failing test, and both are true of a captured run and false of the one the gate makes.
