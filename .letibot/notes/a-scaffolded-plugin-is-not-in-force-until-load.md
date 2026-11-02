<!-- abstract: `plugin scaffold` writes a directory the live stack never picks up, because discovery is at open and the hot-reload poll only walks layers it already has; and a re-render that withholds the look freezes a pane. -->

# A scaffolded plugin is not a layer until `load`, and a re-render without the look freezes a pane

`PluginHost::for_session` calls `Stack::discover` once, at open, over `<data>/sessions/<id>/plugins`. After that `Stack::poll_changes` walks the layers it already has - a new directory is not one - so the two-second hot-reload tick never picks up a new session layer.

`PluginHost::load` is what adds it: `Stack::candidate(&dir, Scope::Session)`, the gate, `Stack::apply`. So `plugin stack` right after `plugin scaffold` says `no layers at all`, which is truthful - a manifest with no module is a DECLARATION, listed with an empty `sha256` and no failure.

`PluginHost::refresh_panes` used to re-render every open pane with `Req::announced()` (`may_look: false`). A pane whose first frame ASKS for a file - the plan dashboard's, `every_s: 0` - then answers that asking frame again and nothing asks again until somebody reopens it. Three cases, three requests: open before and still there gets `Req::fresh()`, open before and gone gets `PluginPaneGone`, never opened gets `Req::announced()`.

Rake: a pane's `every_s: 0` means nothing will ask it again on its own, so a path that hands it a frame it does not want must either keep the look or leave its clock alone.
