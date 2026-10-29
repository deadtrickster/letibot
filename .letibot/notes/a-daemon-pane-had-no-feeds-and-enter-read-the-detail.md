<!-- abstract: MEASURED 2026-10-30: the daemon's pane input carried no `feeds` at all (only the head built them), so a layer on disk declaring `feeds = ["sessions"]` drew an empty pane; and `App::plugin_enter` opened every row's DETAIL, so C3's row `enter` act (switch/fold/event) was performed by the session picker and by no plugin pane. -->

# Two halves of C3 that only one host implemented — and where they now live

MEASURED 2026-10-30, branch `agent/an-irc-session-list-pane` (the `irc` nick list,
`plugins/builtin/irc`).

## 1. `feeds` existed for the head only

`crates/tui/src/app/sessions_pane.rs` has built the `sessions` feed since feeds landed, because the
session list is drawn IN PROCESS by the head. `crates/harnessd/src/plugins.rs`'s `input_value` wrote
`v`, `state`, `look`, `event` and `size` — **no `feeds` key at all** — so a layer on disk that
declared `feeds = ["sessions"]` was handed nothing and drew an empty pane. A manifest declaration
the host does not honour is worse than none: on a screen it reads as a broken plugin.

Now: `crates/harnessd/src/plugins/feed.rs` (`SessionFeed`, `RegistryFeed`), `PluginHost::with_sessions`,
and `render` building `feeds_for(hook)` — **before** taking `self.lock()`, because the mutex is not
reentrant and asking inside `render`'s own guard is a deadlock that reads as a daemon that stopped
answering panes. `Sessions::open_plugins` is the one place that hands a real host the registry.

**`PANE_V` was `1`** while `letibot_plugin::pane::V` was 2 — a layer reading `v` to decide whether
it would be handed a feed was told *no* by a host about to hand it one. It is the SDK's constant
now, and `plugins::tests::a_pane_hands_its_state_back_and_the_input_is_recorded` pins it.

## 2. A row's `enter` was performed by the picker and by no plugin pane

C3: *"a row may carry an act the person performs with Enter … performed by the host"*. The session
picker does it (`sessions_pane.rs::perform`); `Surface::PluginPane` did not — `App::plugin_enter`
opened the stop's **detail** for every row. So a pane whose rows are a list to switch between (the
nick list) had a switch gesture that read prose.

`plugin_enter` now reads `Target::enter()` first: `switch` → `switch_to` **and the pane closes**
(a plugin pane is a screen, not a column beside the conversation), `event` → `PaneAction`, `fold` →
falls through to the detail because this surface has no fold state. Test:
`plugin_pane::a_rows_enter_act_switches_and_the_pane_leaves_with_it`.

## 3. There is no host gesture for a pane's own column

No hide-a-pane key, no divider, no per-pane width: `Surface::PluginPane` is a screen, and the panes
that ARE narrow (the queue's standings) are drawn on a row the composer owns. So a pane that wants
to be a column beside a conversation owns that itself — a row's `actions` are letters the host sends
back as `PaneAction`, and the width / hidden flag / scope ride in the pane's `state`. **The
conversation does not reflow**, because the host is not laying the column out; that is the honest
limit, written up in `docs/plugins-plan.md` §4.9.

## 4. The rake that bit twice

`crates/tui`'s `sessions_override` failed once and passed twice with no change: the built-ins table
is one `OUT_DIR/table.rs` per build dir and the wasm guests are rebuilt by a build script, so the
first run of a test binary can read a table from before the guest existed. `.letibot/notes/
the-builtins-table-is-one-file-for-every-worktree.md` is the same rake.
