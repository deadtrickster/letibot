<!-- abstract: A head's half of a `--continue` is under 50 ms for a 2000-row conversation - 0.14 ms to replay, 43 ms for the first draw, 0.18 ms a draw after - and the one cost that is not milliseconds is the first `plugin_host()` call at 1.48 s. -->

# The head's half of a start is milliseconds, and the first picker draw is 1.5 s

Measured in release, which is the profile the operator runs because `scripts/letibot` picks
`target/release`: a 2000-row session costs 0.14 ms to replay (`App::load`, `crates/tui/src/app/events/mod.rs:78`),
43 ms for the first `App::screen(120,40)` and 0.18 ms for every draw after it, and the daemon's `Hello`
snapshot is 408 µs for 545 KB of JSON. So load plus first draw is under 50 ms.

The one cost that is not milliseconds is `App::plugin_host()`
(`crates/tui/src/app/sessions_pane.rs:447`), which builds the plugin stack - discovery plus a wasmtime
compile of every built-in layer: 1.48 s in release and 12.5 s in debug on the first call, 0.2-0.4 ms
after, paid once per head per folder by whichever of `picker_rows`, `/todo` or the queue pane asks first.

A "why is it slow" question about this head has to name which of the four it is - the replay, the first
draw, the daemon's snapshot, the plugin host - because three are microseconds and one is a second and a
half. Time it in release with a throwaway `Instant` test; debug is not a substitute, since the wasm
compile is 8x slower and the first draw 3x.
