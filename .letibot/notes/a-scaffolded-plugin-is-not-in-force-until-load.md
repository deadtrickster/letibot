<!-- abstract: MEASURED 2026-10-26 (P2): `plugin scaffold` writes a layer directory the LIVE stack never picks up (discovery is at open, and the hot-reload poll skips a dir that is not a layer yet) — `plugin load` is what makes it a layer; and `PluginHost::refresh_panes` must ask an OPEN pane again WITH its look, or a look-asking dashboard freezes on its loading frame with every_s 0. -->

# A scaffolded plugin is not a layer until `load`, and a re-render without the look freezes a pane

MEASURED 2026-10-26, package P2 — two ordering facts a test of the `plugin` tool
gets wrong first, both silent.

## `scaffold` writes where discovery READS, not where the stack already looked

`PluginHost::for_session` calls `Stack::discover` once, at open, over
`<data>/sessions/<id>/plugins` (`crates/harnessd/src/plugins.rs`). After that:

* `Stack::poll_changes` walks **the layers it already has** — a new directory is
  not one — and `PluginHost::gate_now` then drops any candidate with an empty
  layer id. So the two-second hot-reload tick NEVER picks up a new session layer.
* `PluginHost::load` is what adds it: `Stack::candidate(&dir, Scope::Session)` (by
  directory, `pub`) → the gate → `Stack::apply`, which assigns `next_id(scope)`.

So `plugin stack` right after `plugin scaffold` says **`no layers at all`**, and
that is truthful rather than a bug: a source with no module is not in force. It
becomes a layer at the next session open (a `Failure` naming the missing module if
it was never built) or immediately on `plugin load`. A test asserting the layer
appears after `scaffold` fails for the right reason — write it as *scaffold → build
→ load → listed*.

## A re-render must pass the look, or an open dashboard freezes

`PluginHost::refresh_panes` (which `plugin load`, `disable`/`enable` and the
hot-reload gate all call) used to re-render every open pane with
`Req::announced()` — `may_look: false`. A pane whose first frame ASKS for a file
(the plan-dashboard's `plan` frame, `every_s: 0`) then answers that asking frame
again: the head draws "reading …" and the pane's refresh clock is 0, so nothing
asks again until somebody closes and reopens it.

Three cases, three render requests:

| pane | request |
|---|---|
| open before, still there | `Req::fresh()` — asked again WITH its look |
| open before, gone | `SessionEvent::PluginPaneGone`, entry dropped |
| never opened | `Req::announced()` — `may_look: false`, because a dashboard nobody opened must not read files |

## The rake

A pane's `every_s: 0` is not "no refresh" only — it is *nothing will ask me again
on my own*. Any code path that hands a pane a frame it does not want must either
keep the look or leave the pane's clock alone.

## UPDATE 2026-10-29 — the second half of that sentence is no longer true

`docs/plugins-plan.md` §4.8's documented install of a built-in's tool is a `plugin.toml` and no
`plugin.wasm`, and the daemon published a `plugin_layer` warning on every session for it — a false
alarm for a correct install (note `a-documented-install-was-a-broken-layer`). So `Stack::discover`
now treats **a manifest with no module as a DECLARATION**: listed, `sha256` empty, `module: None`, no
`Failure`. A scaffolded-but-never-built layer is the same state, so the sentence above reads *"it is
listed with `sha —`, nothing answers it, and nothing is recorded as a failure"* — the missing build
is what `plugin stack` shows, not a warning. Everything else here stands: `plugin stack` still says
`no layers at all` until `load` in the session that scaffolded it, and the hot-reload poll still
never picks up a new directory.
