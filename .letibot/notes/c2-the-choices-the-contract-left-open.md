<!-- abstract: The choices `crates/plugins` took where its contract left them open: the three discovery roots' shapes, two kinds of failure that must both be read, and a `Candidate` keyed by the bytes the gate loaded. -->

# crates/plugins (C2): the choices the contract left open — as built

`Stack::discover`'s three roots are not the same shape, deliberately: `session_dir` is the session's
own plugin directory (its children are layers), `project` is the PROJECT ROOT (layers in
`<project>/.letibot/plugins`), and `user` is the letibot config dir (layers in `<user>/plugins`). So
`discover(project = Some(<repo>))` is right, and `Some(<repo>/.letibot/plugins)` gives an empty scope
with no error.

`Stack::call` takes `&self`, so failures had to be split in two facts. `Resolved::failures` is one
resolution's failures (the event, for whoever tells the model); `Stack::failures()` is what
discovery, `apply` and `poll_changes` could not load. A daemon that wants to say a layer is broken
must read BOTH, because a layer that failed to load is never called and so never appears in
`Resolved::failures`.

`Candidate` remembers the module the gate loaded, keyed by the sha of the bytes, and `apply` refuses
when `plugin.wasm` has moved since. Pass the SAME `Candidate` to `gate` and then to `apply`; do not
rebuild it from the directory between the two, or the module answering is a second compile and the
gate's diff describes a module nobody is running.

A look is once per layer per resolution, and a whole resolution is capped at `MAX_CALLS` = 32 module
calls. The crate has no engine and its tests never run cargo or wasm: `Loader` is the seam and
`CargoBuilder` is the production `Builder`, deliberately untested.
