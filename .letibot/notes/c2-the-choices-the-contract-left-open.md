<!-- abstract: C2's contract leaves four things open in crates/plugins; these are the choices taken, and the one that constrains W4's wiring. -->

# crates/plugins (C2): the choices the contract left open — as built

Written 2026-10-25 with the crate itself (commit on `plugins-stack`). Read the crate docs first:
this is only what `docs/plugins-slice1.md` C2 does not say.

**`Stack::discover`'s three roots are not the same shape, and that is deliberate.**
`session_dir` is the session's own *plugin directory* (its children are layers);
`project` is the **project root** (layers in `<project>/.letibot/plugins`);
`user` is the **letibot config dir** (`~/.config/letibot`, layers in `<user>/plugins`).
A `Stack::discover(project = Some(<repo>))` is right; `Some(<repo>/.letibot/plugins)` gives an
empty scope and no error — the worst shape of bug, so it is a table on the method.

**`Stack::call` takes `&self`, so failures had to be split in two facts.**
`Resolved::failures` is one resolution's failures (the event, for whoever tells the model);
`Stack::failures()` is what discovery / `apply` / `poll_changes` could not load (a property of
the stack). A daemon that wants "tell the model a layer is broken" must read **both**: a layer
that failed to load never appears in `Resolved::failures`, because it is never called.

**`Candidate` remembers the module the gate loaded, keyed by the sha of the bytes.** `apply`
swaps in *that* module and refuses when `plugin.wasm` has moved since — otherwise the module
answering is a second compile and the gate's diff describes a module nobody is running.
Consequence for W4: pass the **same `Candidate`** to `gate` and then to `apply`; do not rebuild
it from the directory between the two.

**A look is once per layer per resolution**, a second one is a recorded failure (not a loop),
and a whole resolution is capped at `MAX_CALLS` = 32 module calls.

**`gate` fails on a replay failure the live stack did not also record** — a layer inheriting an
existing trap is not refused for the fix.

**The crate has no engine and its tests never run cargo or wasm.** `Loader` is the seam (rano
becomes one impl); `CargoBuilder` is the production `Builder` and is deliberately untested.
`toml` 0.8.23 and `sha2` 0.10.9 were already in `Cargo.lock` and in the offline cache, so C2's
"use a stand-in if it is not there" never applied.
