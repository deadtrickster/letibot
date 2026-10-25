<!-- abstract: MEASURED 2026-10-26 wiring panes (W4): the example's committed plugin.wasm had NEVER been executed and every call trapped on SystemTime::now; C2's `look` (ask below) and C3's `look` (run this for me) are the same key; and View::validate demanded a key from a panel that has no view. -->

# The pane contracts' three collisions, found the first time the wasm ran

MEASURED 2026-10-26, branch `plugins-wire`, wiring W4 (harnessd) onto W1/W2/W3/W5.

## The example plugin's committed `.wasm` had never been executed, and trapped on every call

`plugins/examples/plan-dashboard/plugin.wasm` (256,008 bytes) calls
`std::time::SystemTime::now()` in `view()`; on `wasm32-unknown-unknown` std answers that with a
PANIC, and a panic in the guest is a trap:

```
{"why":"p1 (project): `pane_dashboard`: trapped: error while executing at wasm backtrace: …"}
```

The string `time not implemented on this platform` was IN the committed binary —
`strings plugin.wasm | grep 'not implemented'` — and W5's own note says its tests were native,
because no wasm host existed here yet. So a plugin can be "built for wasm32" and still be a module
that has never run once.

Fixed in `src/lib.rs`: `fn now() -> Option<i64>` is `None` under `cfg(target_arch = "wasm32")`,
`findings(status, now: Option<i64>)` skips the staleness finding without a clock (an age is a
MEASUREMENT; "could not ask" is not "it is fresh"), and `view_at` keeps its clock for the native
tests. Rebuilt: **plugin.wasm = 255,297 bytes** (the panic machinery is gone).

## C2 and C3 both use the top-level key `look`, and they mean different things

* C2's answer protocol: `{"look":{"below": <input>}}` — *ask the layer below*.
* C3's pane document: `"look": {"read": …}` / `{"exec": […]}` — *run this for me*.

`Stack::call` reads `look` first and requires `below`; a pane answer has none, so the layer's
answer is recorded as `look without below — there is nothing to ask the layer below` and the pane
resolves to **nothing**. A dashboard that asks for a file therefore draws nothing at all.

W4's bridge is in `crates/harnessd/src/plugins.rs` (`PANE_LOOK = "pane_look"`): every module the
plugin host loads is wrapped (`Spelled`/`SpelledModule`) so a **pane** hook's `look` that carries
no `below` is renamed on the way out of the module and renamed back on the way into the view
(`unspell`). A real look-below is untouched — it has `below`, which no `LookSpec` can. Nothing in
`crates/plugins` or `crates/sessionlog` changed for this; it is the daemon's seam because the
daemon owns the pane contract on the way in and the stack's protocol on the way out.

## `View::validate` demanded a key from a panel that has no view

The example's first frame is ONE panel (`plan`, "reading .letibot/status.json") with no `view` —
C3's keys open views, and a panel whose `view` is absent draws its `summary`, so it needs no key.
W3's `validate` refused it: *"panel `plan` takes key "": a panel's key is one lowercase letter"*,
which turned the loading frame into the host's unavailable panel.

Relaxed in `crates/sessionlog/src/pane.rs`: a panel is key-checked **unless** it has neither a key
nor a view. A panel that HAS a view still needs one lowercase letter to open it (that is the rule
the head depends on), so every fixture in `pane.rs`'s own tests still holds.

## What the whole slice then did, once

`cargo test -p letibot-harnessd --test plugins_pane` — real daemon, real socket, `PaneOpen` from a
real `HeadClient`, rano compiling the committed module, two `PluginPane` frames: `plan` (the frame
that asks) then `plan · v0.2.0` with a `tiers` panel, in 4.1 s.
