<!-- abstract: The daemon's pane input carried no `feeds` at all, so a layer declaring `feeds = ["sessions"]` drew an empty pane, and a plugin pane's row `enter` act was never performed - both now live in `crates/harnessd/src/plugins/`. -->

# Two halves of C3 that only one host implemented — and where they now live

The head built the `sessions` feed because the session list is drawn in process. The daemon's
`input_value` wrote `v`, `state`, `look`, `event` and `size` and no `feeds` key at all, so a layer
declaring `feeds = ["sessions"]` was handed nothing and drew an empty pane - a manifest the host does
not honour, which on a screen reads as a broken plugin. Now `crates/harnessd/src/plugins/feed.rs`,
`PluginHost::with_sessions`, and `render` building `feeds_for(hook)` BEFORE it takes `self.lock()`: the
mutex is not reentrant, and asking inside `render`'s own guard is a deadlock that reads as a daemon
which stopped answering panes. `PANE_V` was 1 while the SDK's was 2, so a layer reading `v` was told no
by a host about to hand it a feed.

C3's row `enter` act was performed by the session picker and by no plugin pane - `App::plugin_enter`
opened the stop's detail for every row. It reads `Target::enter()` first now: `switch` switches and the
pane closes (a plugin pane is a screen, not a column), `event` becomes a `PaneAction`, `fold` falls
through to the detail because this surface has no fold state.
