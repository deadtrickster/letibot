<!-- abstract: `look` means two things in the two pane contracts - C2's ask the layer below and C3's run this for me - so a dashboard asking for a file resolves to nothing unless the daemon tells them apart; and a panel with no view needs no key. -->

# The pane contracts' three collisions, found the first time the wasm ran

`look` is one key with two meanings. C2's answer protocol says `{"look": {"below": <input>}}` -
ask the layer below (`crates/plugins/src/stack.rs`). C3's pane document says
`"look": {"read": …}` or `{"exec": […]}` - run this for me (`LookSpec`,
`crates/sessionlog/src/pane.rs:63`). `Stack::call` reads `look` first and requires `below`, so a
pane's answer is recorded as *look without below* and the pane resolves to nothing. The daemon
is the seam that tells them apart (`crates/harnessd/src/plugins/look.rs`): a pane hook's `look`
carrying no `below` is renamed on the way out of the module and back on the way into the view.

A committed `plugin.wasm` can be built for wasm32 and never have run: the example called
`SystemTime::now()` in `view()`, which panics on that target, so every call trapped.
`strings <plugin.wasm> | grep 'not implemented'` is how to tell.

`View::validate` (`pane.rs:533`) refused a panel with no key. It must not: a panel whose `view`
is absent draws its `summary` and needs no key. A panel that HAS a view still needs one
lowercase letter, which is the rule the head depends on.
