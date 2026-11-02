<!-- abstract: Compiling one built-in wasm layer costs about 2 s of CPU, so a session installs only the layers it declared a tool from, the compile is cached for the process, and the room pane - the one built-in the daemon answers - is installed at attach. -->

# A built-in wasm layer costs ~2 s of CPU to compile, and the first cut paid it on every session open

`rano::plugin::Plugin::from_bytes_hooks` costs about 2 s of CPU per layer (`claude-transcript`,
232,869 bytes, 1.97 s; `sessions`, 173,119 bytes, 1.39 s). Paying it at every session open made
`no_spin` report 3.006 s of CPU over a 3 s idle window and the idle-exit test miss its 800 ms window -
two reds that look unrelated and are one fact.

So `PluginHost::production` installs only the layers a session declared a tool from
(`install_builtin_tools`), and the compile is kept for the process (`Loader::load_builtin`, a
`OnceLock<Mutex<BTreeMap<String, Arc<dyn Module>>>>`, `Stack::register_builtin_shared`).

The exception is the room's pane (`plugins/builtin/room`, 218,712 bytes): its controls are the HOST's
verbs, so the daemon installs it at `PaneDriver::attach` rather than at open, and only when the host
has a room feed and no layer already answers the hook (`ensure_the_room_pane`).

Beside it: `sh scripts/check-fmt.sh <base>` dies with `Unrecognized option: '>'` on an uncommitted
`git mv`, because `git status --porcelain` prints `R old -> new`. Commit the rename first.
