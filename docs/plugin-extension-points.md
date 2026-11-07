# Plugin extension points, and what of the head moves out

The question: the room chat UI looks like a plugin, and the plugin API probably lacks what it needs.
The answer, short: the chat's DECISIONS (what a message looks like, which names complete, what a typed
line means) are a plugin. Its MECHANICS (bytes on the wire, pictures in the terminal, text editing,
scroll, the cursor) stay in the host. Ten host additions close the gap, and the same ten move the
jobs, todos and subagents panes later. `docs/room-tui.md` P1 stays native; P2-P4 are built natively
in the pane language's types; two new packages put the room's decisions into a built-in plugin.

Paths are this repository at `46729b94` unless prefixed `rano/` (`~/Projects/rano/rano` at `548f86b`).

## 1. What the plugin API exposes today

### Hook kinds and who calls them

| kind | dispatched by | in | out |
|---|---|---|---|
| `pane_<name>` | daemon (published as an event) or head (asked on draw) | `Input` below | `View` below |
| `command_<verb>` | head, `/todo` only (`crates/tui/src/app/commands.rs:795-830`) | `{line, feeds}` | `{"act":{"name","args"}}`, one act name performed: `add_todo` (`commands.rs:19`) |
| `tool_<name>` | daemon, at session start or reseat (`crates/harnessd/src/plugins/tool.rs`) | `{name, args, feeds, looks, state}` | a tool result, or `looks` to run (4 rounds) |
| `on_tool_call`, `on_tool_result`, `notice_<kind>`, `condition_<name>` | daemon | `docs/plugins-plan.md` §4.1-4.4 | pass, rewrite, refuse, answer, append, arm |
| `digest_`, `link_` | prefixes admitted (`crates/plugins/src/lib.rs:111-121`), no caller | | |

Per call: `HOOK_FUEL` 50M, 64 MiB memory, 16 MiB each way (`rano/src/plugin.rs:11-17`).
Capabilities: `read`, `read:<path>`, `exec:<prog>`, `http:<host>` (`crates/plugins/src/manifest.rs:62-73`).

### The pane contract

Input (`crates/plugin-sdk/src/pane.rs:325-354`): `v` (3), `state` (what the pane last returned),
`look` (exit, stdout, stderr), `event` (`{item, action}`), `size` (`w`, `h`), `feeds`, `folded`,
`below`. No clock, no locale, no cursor position, no typed text.

View (`crates/sessionlog/src/pane.rs:39-66`): `title`, `badge`, `panels`, `findings`, `legend`,
`look`, `every_s`, `state`. A panel has `summary` and `view` blocks behind a one-letter key.

Blocks (`sessionlog/src/pane.rs:128-146`): `lines` (the grid: label, value, bar, spark, tail),
`table`, `tree`, `markdown`, `kv`. Anything else draws as one muted line naming the kind.
`markdown` is itemized into stops and drawn as wrapped plain text, not through `rano::markdown`
(`crates/tui/src/ui/panes/plugin.rs:862-881`): no fences, no highlighting, no inline styles.

Roles: `muted`, `working`, `attention`, `success`, `failure`, `plain`. A cell is text plus one role
(`plugin-sdk/src/pane.rs:221-260`).

Row acts, on Enter (`plugin-sdk/src/pane.rs:108-127`): `switch` (to a session), `fold`, `event`
(call the pane back), `verb` (a host verb on a named entry, declared in `actions`).
Stop actions, on a letter (`sessionlog/src/pane.rs:397-470`): `say` (a line to the model), `look`,
`event`, `verb`. One lowercase letter, not one of `HOST_KEYS` (`q j k f l`, `pane.rs:18`).

Looks (`sessionlog/src/pane.rs:485-491`): `read` a file, or `exec`, which is refused for panes
(`crates/harnessd/src/plugins/look.rs:16`, *exec looks are not in slice 1*). `every_s` is 0 or >= 5.

Feeds (`plugin-sdk/src/pane.rs:22-45`): eight named. Built: `sessions` and `room` (agents' queues,
not chat) in the daemon (`crates/harnessd/src/plugins.rs:725-765`); `sessions` and `queue` in the
head (`crates/tui/src/app/sessions_pane.rs:571-592`, `crates/tui/src/app/queue_pane.rs:372-400`).
`monitors`, `jobs`, `todos`, `git` are names with no builder; `github` is built only by its pane's
own seat.

### Two hosts, two lifecycles

- Daemon panes. The head sends `PaneOpen{pane,w,h}`, `PaneAction{pane,item,action}`, `PaneClose`
  (`crates/sessionlog/src/protocol.rs:889-914`); the daemon answers `SessionEvent::PluginPane{pane,
  view}` (`crates/sessionlog/src/event.rs:1499-1512`). Re-asked on open, on an action, on a reload,
  and when `every_s` comes round (`plugins.rs:656-700`). Never because a feed changed.
- Head panes. `pane_sessions` and `pane_queue` are asked synchronously, on draw, against the head's
  own stack (`sessions_pane.rs:469-506`). Live by construction, uncached.

Built-ins: the head installs every one (`sessions_pane.rs:397`) and asks two. The daemon installs a
built-in only when the session declared a tool it also declares (`plugins.rs:276-301`), or the room
pane when a room link exists (`plugins.rs:303-346`). So `irc` (`plugins/builtin/irc`) and `github`
are compiled in and reachable from nowhere.

### Keys

`App::plugin_key` (`crates/tui/src/app/plugin.rs:190-257`): only a `Key::Char` reaches a pane, and
only as a panel key, `f`, `l`, or a letter of the selected stop. Any other letter falls through to
the composer. Arrows, Enter, Esc, PgUp/PgDn, the wheel are the host's. Enter performs the row's act
or opens its detail (`plugin.rs:263-307`). A switch closes the pane: a plugin pane is a screen, not
a column (`plugins-plan.md` §4.9).

### What a pane cannot do, in one list

Read a key that is not a stop's lowercase letter. Own a text input. Complete anything. Perform a
host act that is not `switch`, `fold`, or a declared verb on a row. Draw markdown as markdown, a
picture, or a colour outside six roles. Know the time. Run a program. Be told that its data
changed. Be placed beside the conversation. Be found, when it is a built-in.

## 2. Evidence: where plugins hit the walls

Sessions read from `sessions.db` (read-only) and the session plugin directories. Four session
layers exist on this box, all fuzzing dashboards, all `pane_` with `read`:
`fuzzpane` (`s-1791919539675077359`, serenedb), `fuzz` and `fuzzlegs` (`s-1792687430252160872`,
etcd/db-fuzz), `fuzz-campaign` (`s-1793479579380980357`, arangodb). No session has written an
interactive pane: none uses `event`, `verb`, `say` or `state` for anything but a refresh.

W1. Every pane needed a generator script beside it, because a pane cannot run anything.
`fuzz/status.py` in serenedb, `status.sh` in arangodb, a status JSON in etcd. The docstring the
agent wrote: *"The pane cannot run anything, so everything it shows has to be measured here
first."* (`s-1791919539675077359`, seq 2887). The generator then had its own bugs (stripped env,
stdout vs stderr, seq 2900-2908), outside the shadow gate and outside any test of the pane.

W2. `read` resolves against the session's workspace root, which is not the checkout. fuzzlegs drew
`look exit 1` for the rest of the session: *"its live read is refused ... the path it names doesn't
resolve against this session's workspace root"*, then *"noted rather than guessed at further"*
(`s-1792687430252160872`, seq 3019, 3562). fuzzpane worked around it by copying the status file
into a second `.letibot/` at the workspace root (`s-1791919539675077359`, seq 2897).

W3. A pane cannot be found. The operator: *"how to ipen that pane?"* (`s-1791919539675077359`,
seq 2990). In the main session the operator asked for the irc-style side list; `/pane irc` said it
does not exist, `/pane irc/nicks` said *no plugin panes*, and the agent guessed wrong twice before
reading `plugins.rs:290` (`s-1789462738453908838#t78`, seq 732-756). It is still unreachable.

W4. The trust book costs a round trip per rebuild. Every `load` changes the module hash, unpins the
layer, and asks the operator for `/plugin trust` (`s-1791919539675077359`, seq 3072, 3098).

W5. Agents do not know what a plugin can be. Eleven room messages (#574-#589, same session,
seq 2140-2190) concluded a plugin cannot add a tool. `tool_` hooks exist; the `plugin-hooks` skill
teaches four kinds (`skills/plugin-hooks/SKILL.md:3`) and not `tool_`, `command_` or `pane_`.

W6. A pane that validates can still be unreadable. cc asked serenedb to redo its pane because the
operator could not read it (#730), and rano's pane window had no constant height or counted
overflow (#731). The padded window landed on rano branch `agent/pane-budget` (#750).

W7. Liveness, on the native panes, said by the operator: *"subagents pane didnt pick them up live -
I had to reopen it"*, *"pane refresh is still a thing"*. A daemon plugin pane has the same defect
by design: nothing re-asks it when its feed changes.

W8. The keyboard model the operator expects: *"there are global shortcuts and pane-specific
shortcuts ... panes own everything which is not global"*, and *"the pane own keyboard in a way, so
enter is a pane thing"*. Today a plugin pane owns lowercase letters on stops and nothing else.

W9. Layout. *"a side panel irc client style where i see the session list and can switch"*, *"we
need a concept of tabs or windows"*. The `irc` layer draws a column inside a full-screen rectangle
and keeps its own width in `state` (`plugins/builtin/irc/src/lib.rs:1-60`), because the host has no
placement.

W10. Appetite. *"lets migrate as much as possible to jitted wasm, find the edge where perf
suffers"*, *"do it as a plugin"*, *"look at letim, i wonder if we can build stl viewer plugin"*,
*"job pane can tail the file ... extremely cheap"*. rano already answers pictures from a plugin
(`provider_picture`, RGB with a size cap, `rano/src/plugin.rs:241-290`).

## 3. The room chat as a plugin

### The split

The host keeps what touches bytes, the terminal or the network: the room wire and its push frame,
the blob store, the head's `RoomsView` (messages, cursors, sends, paging), text editing, pictures,
scroll, the message cursor, acks. The plugin keeps what is policy: how a message is laid out, runs
and headers, speaker colours, mention names, the dividers, the reply quote, which completions rank
first, what `/dm` or `/reply 12` means, what the switcher lists first.

One rule from the session list carries over (`plugins-plan.md` §7, step 10): the way out cannot be
a plugin. The chat is how agents reach the operator, so the native drawing stays as the fallback a
broken layer falls to, as `App::session_list` does for `pane_sessions`.

### Capability by capability

| capability | today | missing | addition |
|---|---|---|---|
| live messages | head polls 2 s (`crates/tui/src/app/rooms.rs:14`); P0/P1 add the push frame | a feed of chat, and a re-ask when it changes | X1 |
| message block: header, body, refs | `lines`/`table` rows | an entry list anchored at the bottom | X2 |
| code fences, inline markdown | `markdown` is plain wrapped text | `rano::markdown` in pane bodies | X2 |
| colour per speaker | 6 roles | a hue palette, by index | X4 |
| mentions as names in a colour | none | a token map on the block | X4 |
| pictures | none | a picture by blob id, drawn by the host | X3 |
| unread divider, day separator | none | divider entries in the block | X2 |
| "N new", stick to tail, page back keeping place | native rooms view | host-owned in the block | X2 |
| scroll, message cursor | stops are rows | entries are stops, cursor is the host's | X2 |
| composer, multi-line, history, drafts | host composer routes to `rooms_say` (`crates/tui/src/app/composer.rs:99`) | a pane-owned input surface | X5 |
| `@` completion | none in panes | a completion hook | X6 |
| send, dm, reply, invite, leave, read a thread, older | none | acts the host performs | X7 |
| `R` reply, `y` copy, `t` thread, `u` unread, `G` tail, `ctrl-k` | stop letters, lowercase | view-level keys, cursor mode | X8 |
| times: `HH:MM` today, `Mon D` before | no clock | `now_ms` and the local offset | X9 |
| switcher | none | a host picker the pane fills | X7 (`pick`) |
| threads | `↳N` marker | `room_read{thread}` act, plugin state | X7 |
| unread counts in the status row | `badge` | none | exists |
| reply target, scope, folds | `state` | none | exists |

### The smallest set of host additions

X1. `chat` feed and change-driven asks. Built by the head from `RoomsView`: channels with unread
and labels, roster per room, the selected channel, `seen` and `opened_seen`, failed sends, and a
WINDOW of messages for the selected channel (the viewport plus one page each way, at most 200
messages or 256 KiB). A head pane is asked when the feed's generation moves (a push, a poll, a
page, a send), not on every draw, and its last view is cached. Daemon panes get the same rule: a
feed change re-asks an open pane; `every_s` stays for looks. Messages carry `seq, ts, author, kind,
to, thread, refs, text, attachments`, as the wire has them.

X2. `stream` block. `{"stream":{"entries":[…],"highlight":"…","mentions":{…}}}`. An entry is
`{id, head:[Cell], time, quote, body, pictures:[blob id], facts:[Cell], role, actions}` or a divider
`{divider:"today"|"unread", text}`. The host draws a body through `rano::markdown` with the same
options as assistant prose, anchors the block at the bottom, sticks to the tail, keeps the reader's
place when older entries arrive above, says `N new - End jumps` while scrolled back, and asks the
host's own `RoomsView` for older pages at the top. An entry is a stop, so the cursor moves by
message. The `markdown` block gets the same renderer, which fixes it for every pane.

X3. Pictures by reference. A body's `![alt](blob:ID)` and an entry's `pictures` name blobs the host
already holds or fetches (`GetBlob`, P0). Kitty draws placeholder rows capped at 12; iTerm2 and no
protocol draw one row naming it. Enter on it opens the overlay (`crates/tui/src/app/pictures.rs`).
The plugin never sees bytes. Manifest: `capabilities = ["blobs:room"]`.

X4. Hues and mentions. `Role` gains `hue:0` to `hue:7`, mapped by the theme to colours that read on
dark and light. The host never takes an RGB from a plugin. The stream block's `mentions` maps a
token in a body to a display text and a hue (`{"@s-1793…":{"text":"@fix-room-push","hue":3}}`,
`"@operator":{"me":true}`); the host rewrites and colours tokens while rendering markdown, so the
plugin never edits the markdown it is handed.

X5. Input surface. `View.input = {key, placeholder, multiline, footer}`. While the pane is open the
host composer is the pane's: the host edits text, keeps history and drafts per `key` (the channel),
handles paste and picture chips, Alt+Enter and Shift+Enter. Enter calls `command_<pane>` with
`{line, key, reply_to, chips, feeds}`. The plugin answers acts (X7) or a refusal sentence the host
shows. Text never reaches the plugin per keystroke.

X6. `complete_<name>` hook. In `{v, text, cursor, key, feeds}`; out
`{from, to, candidates:[{insert, label, note, hue}]}`, at most 8. Called on Tab, and on `@` or `/`
at a word start, debounced. The host draws the row with `rano::agent::composer::completions_line`
(`crates/tui/src/ui/composer.rs:6`), cycles on Tab, accepts on Enter, closes on Esc. A new prefix
in `HOOK_PREFIXES` and the SDK.

X7. Acts as answers. `command_` (and `event`) answers may carry `{"acts":[…]}`, each declared in
`actions`. New host verbs: `room_say{room, text, thread, attachments}`, `room_dm{to, text, thread,
attachments}`, `room_read{room, thread|before}`, `room_invite`, `room_leave`, `copy{text}` (OSC 52),
`open{url|blob|path}`, `pick{title, items:[{id,label,note}], event}` (the host's fuzzy picker; the
choice comes back as an event). The host performs each with the operator's identity, the same
request the native view sends today (`rooms.rs:592-637`), so the coordinator's own checks apply.

X8. View-level keys and cursor mode. `View.keys = [{key, label, event|verb}]`, where `key` is a
letter of either case or one of a named set (`ctrl-k`, `end`, `home`). With an input surface, keys
are live only in cursor mode: Tab on an empty input enters it, Esc leaves it, letters type
otherwise. Host keys stay the host's and grow by the global chords. The event carries the entry
under the cursor as `item`.

X9. Time. Every pane, command and completion input carries `now_ms` and `utc_offset_s`. A guest
cannot read a clock (`skills/plugin-pane/SKILL.md:48`), and a chat needs today versus yesterday.

X10 is not needed by the chat and is in §4: `dock` placement and `exec` looks.

### Safety

- The plugin paints no cell. Every byte on the terminal comes from the host's renderer. Text from
  the plugin and from chat messages goes through `clean_line` before it is drawn.
- The plugin does no I/O. Messages arrive as a feed, pictures as ids, older pages and threads as
  acts the host performs. No `exec`, `read` or `http` is needed or granted.
- Every act is declared in `plugin.toml` and checked by `View::validate_for` and its `command_`
  twin. A `room_say` is sent as the operator, through the operator seat's allowlist
  (`crates/harnessd/src/room/operator.rs:57-72`), so a layer cannot speak as anybody else and
  cannot reach a request the seat refuses.
- The `chat` feed is every message in every room the operator sees, DMs included. It is handed to a
  built-in or a pinned layer only. A session-scope layer that asks for it is refused at admit,
  because an unpinned layer reading the operator's DMs is a widening, not a view.
- A trap, a fuel exhaustion or a refused view falls to the native renderer for that frame and is
  said once, as `sessions_list` does today.
- Cost is bounded by the window (X1): 200 messages of at most 4000 bytes is under 1 MiB in, well
  under `MAX_HOOK_BYTES`. Fuel for a full window is a measurement P5 owes (§5), not a guess here.

## 4. Other parts of the head that move to plugins

Ranked by value over cost. "Needs" names the additions above.

| rank | part | today | needs | value | cost |
|---|---|---|---|---|---|
| 1 | `irc` nick list, the side session list | written, unreachable (W3, W9) | X10 dock, a head caller for `pane_nicks` | the operator asked twice | S |
| 2 | jobs pane (`crates/tui/src/ui/panes/jobs.rs`, 112 lines) | native | `jobs` feed, X2 stream for the tail, acts `job_kill`, `open` | *"job pane can tail the file"* | S-M |
| 3 | todos pane (`ui/panes/todos.rs`, 729 lines) | native; `command_todo` already a layer | `todos` feed (the command feed exists, `plugins-plan.md` §4.9), acts for `OWN_VERBS` (`commands.rs:9`) | per-project policy: TODO.md schema, `@agent` tags, leticl edits the same file | M |
| 4 | monitors pane (`ui/panes/monitors.rs`) | native | `monitors` feed, act `monitor_stop` | low traffic | S |
| 5 | notes pane (`ui/panes/notes.rs`, 450 lines) | native | a `notes` feed, act `open{path}` into rano | *"damn notes pane is a mess"* | S |
| 6 | subagents pane (`ui/panes/subagents.rs`) | native | X1 live, X2 tail of a child's rows, X5 input to message a child, act `task_message` | most operator complaints of any pane | L; its liveness bugs are host bugs and go first |
| 7 | native queue pane (`ui/panes/queue.rs`, 657 lines) | `pane_queue` is already a layer | nothing; delete the duplicate drawing once the layer is the only one | one renderer | S |
| - | `github` built-in | written, daemon never installs it | the install rule below | | S |

X10, for ranks 1 and the dashboards:
- `dock`: `View.dock = "right" | "screen"`, and `width` in cells. The host lays the column out
  beside the conversation, owns the divider and the width keys, and the conversation reflows. The
  `irc` layer drops its own width arithmetic.
- Install rule: a built-in pane is listed by `/pane` whether or not a tool matched, and a head-side
  built-in pane is listed beside the daemon's. A built-in with no `[[tools]]` costs no prompt bytes.
- `exec` looks for a pinned pane, through `Gate::admit_unasked` as conditions already do
  (`plugins-plan.md` §4.4). This deletes the generator scripts of W1. An unpinned layer keeps
  `read` only.
- `read` names the checkout. A pane look resolves `read` against the workspace's git common
  directory's checkout as well as the workspace, which is the `repo` the sessions feed already
  computes (`plugins-plan.md` §4.9, *The feeds a DAEMON-drawn pane is handed*). This closes W2.

Stays native: the conversation and transcript, the composer's editing core, the session picker
(the way out), permission cards and decisions (the gate), help, the term pane.

Not a host API but owed by W5: the `plugin-hooks` skill lists every kind the host dispatches
(`tool_`, `command_`, `pane_`), and says which a seat without `bash` can still use.

## 5. `docs/room-tui.md` P1-P4: split

P0 and P1 stay native, as written. Wire, push frame, blob store, operator seat, bridge uploads are
transport, and no plugin can own them. P2-P4 are also built natively, but against the pane
language's types instead of `Msg`, so the room's decisions sit in one adapter that a plugin later
replaces. Building the plugin first would hold the operator's chat behind ten host additions; the
native adapter would be needed anyway as the fallback.

Order: P0, then H0, then P1-P4 in parallel, then P5, then P6. Every package is gated by
`cargo test -p letibot-room -p letibot-harnessd -p letibot-sessionlog -p letibot-tui -p letibot-tools
-p letibot-bridge -p letibot-plugin -p letibot-plugins` and clippy, as room-tui.md says.

### H0 - pane contracts for streams, inputs and acts (S, after P0, before P2-P4)

Owns `crates/plugin-sdk/src/pane.rs` (`Feed::Chat`, `Role::Hue`, `Input.now_ms`,
`Input.utc_offset_s`), `crates/plugin-sdk/src/complete.rs` (new), `crates/plugin-sdk/src/lib.rs`,
`crates/sessionlog/src/pane.rs` (`Block::Stream`, `StreamEntry`, `Divider`, `View.input`,
`View.keys`, `View.dock`, validation), `crates/plugins/src/lib.rs` (`complete_` prefix),
`crates/plugins/src/manifest.rs` (`blobs:room`, acts list), `docs/plugins-plan.md` §4.9.
Additive: unknown fields are ignored, an unknown block draws as its name, so no `pane::V` bump.

Done when:
- `sessionlog/src/pane.rs`: `a_stream_block_round_trips`, `an_old_head_draws_a_stream_as_its_kind`,
  `a_key_bound_twice_is_refused`, `a_view_key_on_a_host_chord_is_refused`,
  `an_undeclared_act_in_an_answer_is_refused`.
- `plugin-sdk/src/pane.rs`: `a_hue_outside_the_palette_does_not_parse`.
- `plugins/src/manifest.rs`: `an_unpinned_layer_asking_for_the_chat_feed_is_refused_at_admit`.
- `plugin-sdk/src/lib.rs`: the prefix test admits `complete_`.

### P1 - live delivery and agent-side attachments (unchanged)

As in room-tui.md, same paths, same done-tests.

### P2 - the stream renderer (L, changed)

Owns `crates/tui/src/ui/chat/**` (the P0 stub: `chat_blocks(&Stream, &ChatCtx)` in place of
`chat_blocks(&[&Msg], …)`), `crates/tui/src/app/chat_adapter.rs` (new: `RoomsView` to `Stream`,
which holds grouping, hues, mentions, dividers and the quote line; the half a plugin replaces),
`crates/tui/src/ui/panes/rooms.rs` (body, header, footer), `crates/tui/src/app/tests/rooms_render.rs`.

Relies on H0's types, P0's `Msg.attachments` and `opened_seen`.

Done when room-tui.md's P2 list passes, reworded where it names `Msg`, plus:
- `the_adapter_is_the_only_place_that_reads_msg`
- `a_stream_entry_body_draws_through_rano_markdown`
- `a_hue_maps_to_a_theme_colour_on_dark_and_light`
- `a_mention_token_draws_as_its_name_in_its_hue`

### P3 - composer in a room (M, changed)

Owns `crates/tui/src/app/chat_input/**`, `crates/tui/src/ui/chat_compose.rs`,
`crates/tui/src/app/tests/rooms_compose.rs`. Same behaviour as room-tui.md, with two seams: history
and drafts are keyed by an input `key`, not by `Chan`; completion goes through
`trait Complete { fn complete(&self, text: &str, cursor: usize) -> Completion }`, whose first
implementation is the room's (roster first, prefix before substring, top 6).

Done when room-tui.md's P3 list passes, plus:
- `history_and_drafts_are_keyed_by_the_input_key`
- `the_completion_row_draws_what_a_complete_impl_answers`

### P4 - navigation and pictures (M, unchanged in scope)

Same paths. The cursor, jumps, `N new`, switcher, search and blob cache act on stream entries, not
on room messages, so a plugin's stream gets them for free.

Done when room-tui.md's P4 list passes, plus:
- `the_cursor_moves_by_stream_entry_in_a_plugin_pane`

### P5 - the head hosts a chat plugin (M, after P2-P4)

Owns `crates/tui/src/app/chat_plugin.rs` (new: the `chat` feed builder, the generation memo, the
`command_` and `complete_` dispatch for an input surface, the room acts and `copy`, `open`, `pick`),
`crates/tui/src/ui/panes/plugin.rs` (stream, picture and markdown blocks drawn by P2's renderer),
`crates/tui/src/app/plugin.rs` (view keys, cursor mode, input routing),
`crates/tui/src/app/tests/chat_plugin.rs`.

Done when:
- `a_head_pane_is_asked_once_per_feed_generation_not_per_draw`
- `a_pushed_message_re_asks_the_chat_pane`
- `enter_in_a_plugin_input_calls_its_command_hook_with_the_line`
- `a_room_say_act_sends_as_the_operator`
- `an_act_the_manifest_did_not_declare_is_said_and_not_performed`
- `tab_asks_the_complete_hook_and_draws_its_candidates`
- `a_trapping_chat_layer_falls_back_to_the_native_renderer`
- `a_window_of_200_messages_answers_under_the_fuel_cap` (records the fuel; the number goes in this
  doc)
- `the_chat_feed_carries_now_ms_and_the_local_offset`

Built. Two facts worth keeping from the build:

- **The fuel a 200-message window costs, measured** (lubuntu1, 2026-11-11): a feed of 200 messages
  is **329,871 bytes** of JSON, and one call carrying it through a real engine - this binary's own
  `sessions` layer, whose input parse is that JSON - burned **3,456,254 fuel** against the engine's
  50M per call. The test asserts a ceiling of 8,000,000, a little over twice the measurement, so a
  doubling of the engine's own work fails and the cap is never near. P6's chat layer will add its
  own policy on top of a number that is therefore the BOUNDARY's price.
- **`pick` is the one `X7` act that is not performed.** `room_say`, `room_dm`, `room_read`,
  `room_invite`, `room_leave`, `copy` and `open` are host verbs the head already had; `pick` needs
  the host's fuzzy picker to take layer-filled items and hand the choice back as an event, and that
  is P6's switcher. A layer that asks for it is answered with a sentence rather than silently.

Three seams outside P5's own list were needed, and each is one line plus its reason: `Host::call`
(an UNCACHED hook call, with the fuel the answering module burned - the existing `ask` caches on the
input's JSON, which for a pane asked once per generation is the wrong question), `App::chat_pane` (a
field for the memo, as P3 needed one for the composer), and the four call sites the head already had
for a chat: `ui/panes/rooms.rs`'s stream, `composer.rs`'s `Surface::Rooms` arm, `keys/mod.rs`'s Tab,
and `chat_input`'s `write_candidate` becoming crate-visible so a layer's completion writes into the
same editor.

Hooks unchanged: no daemon change was needed. The `chat` feed is built by the head from its own
`RoomsView`, so the `feed change re-asks the pane` rule of X1 is the head's memo alone; the daemon
half of that paragraph (an open daemon pane re-asked when its feed moves) is still nobody's.

### P6 - `plugins/builtin/chat` (M, after P5)

Owns `plugins/builtin/chat/**` only. `crates/builtins/build.rs` picks it up with no edit. Ports
`chat_adapter.rs`'s decisions and P3's room completion and verbs into the layer. The native adapter
stays as the fallback and is what a disabled layer draws.

Done when (in `plugins/builtin/chat/src/tests.rs`):
- `two_messages_from_one_author_within_ten_minutes_share_a_header`
- `the_same_handle_always_gets_the_same_hue`
- `a_session_mention_is_mapped_to_its_title`
- `the_unread_divider_precedes_the_first_unseen_message`
- `slash_reply_12_answers_a_room_say_with_thread_12`
- `slash_dm_answers_a_room_dm`
- `at_completion_ranks_the_rooms_roster_first`
- `an_unknown_verb_is_refused_with_a_sentence`
- and in `crates/tui/src/app/tests/chat_plugin.rs`: `the_builtin_chat_layer_draws_what_the_native_adapter_draws`
  on the 61 fixtures of `crates/tui/src/app/tests/rooms.rs`.

Built. `plugins/builtin/chat` answers the three hooks a chat owns: `pane_chat` (the `stream` block
and the input surface), `command_chat` (a line becoming `room_say`/`room_dm`, with `thread`), and
`complete_chat` (the roster first, a prefix before a substring, six in the row). It declares
`capabilities = []`, `feeds = ["chat"]` and `actions = ["room_say", "room_dm"]`, so a reviewer
reads the whole of what a typed line can cause. The nine done-tests above pass, and P5's own nine
still do with the layer installed.

What the facts came to:

- **The parity is whole, not sampled.** `the_builtin_chat_layer_draws_what_the_native_adapter_draws`
  builds seven scenes of the room from `rooms.rs`'s own fixtures - a run with a mention and a quote, a
  year/yesterday/today of dividers, the unread mark before the second message and before the first,
  a system row with a ref, a picture, a DM channel and a project room - and asserts
  `Stream == Stream` for each: grouping, headers, hues, mentions, dividers, the quote and the empty
  sentence. `the_builtin_chat_layer_completes_what_the_native_completion_completes` asserts
  `Completion == Completion` for eight lines, so the layer's Tab is the room's Tab.
- **The layer is drawn by the same code as the adapter.** Nothing in `ui/chat/**` knows which
  answered; `App::chat_stream` asks the layer and falls to the adapter in the same frame.
- **The window's price, through the layer that draws it:** a 200-message window costs
  **7,270,168 fuel** (lubuntu3, 2026-11-12) against P5's 8,000,000 ceiling, of which 326,221 is the
  call itself - the view, and a window with nothing in it. Every message of the window is an entry,
  so the whole window is drawn. Most of the drop from the first measurement is one decision: the
  layer's input envelope is its own struct over the SDK's `Chat`, so the window is parsed ONCE
  rather than into a `Value` tree and then again into `Chat` - that single pass was 5.6M fuel.
- **The module is 220,617 bytes**, inside the band the other five occupy (sessions 187,584 ·
  github 199,381 · irc 215,676 · **chat 220,617** · room 243,250 · queue 275,714, each in its own
  profile; a second build of the same source is 24 bytes smaller in every one of them - chat at
  220,593 - so the band is what is claimed, not the byte). It is one of the larger surfaces of the
  six - a stream, two hooks and a clock - so its profile takes `opt-level = "z"` and one codegen
  unit. `the_builtin_chat_module_is_no_bigger_than_the_largest_builtin` pins it inside the band.

Three seams outside P6's own directory were needed, each additive on the wire, and one departure
inside it:

- **The `chat` feed says what a handle is CALLED** (`Chat.names`, and `ChatMember.connected`). Only
  the head holds the session list, and a layer that resolved a name itself would rename every
  session on the screen. `App::rooms_names` is now one rule for every handle - it used to disagree
  with `rooms_display_name` about which of a roster label and a session title wins, and a name that
  changes between the stream and the completion row is a bug either way.
- **The feed says what the composer completes from** (`Chat.verbs`, `Chat.aliases`): the head's own
  command list and the bridge's `@cc@…` addresses are the head's to know, and a layer that spelled
  one from a channel's label would offer an address nobody resolves. `App::rooms_verbs` and
  `App::rooms_aliases` are now the one list each, used by the native completion and the feed both.
- **`Chat.opened_seen` is `Option<u64>`**: a channel opened with nothing read yet is opened (the
  mark goes above the first message) and is not the same fact as a channel this head never opened
  (no mark at all) - the wire said `0` for both, and P2's own test puts `0` there deliberately.
  `Chat.heard` is the other half of an empty channel: *nothing said yet* and *nobody has answered*
  are two sentences, and only the head knows which.
- **The layer reads its own input envelope** ([`Ask`](../../plugins/builtin/chat/src/lib.rs)) rather
  than the SDK's `pane::Input`: the host's is the whole envelope - state, looks, events, size,
  folds - with `feeds` as a map of `Value`, and a chat window through it is parsed twice. The
  PAYLOAD is still the SDK's `Chat` and `complete::Completion`, which is where a renamed field would
  actually break a layer; the three names the envelope adds (`feeds`, `now_ms`, `utc_offset_s`) are
  held by the fuel test, which fails if the host stops sending them.

Two things the layer cannot get right, and does not pretend to:

- **A window that spans a DST change.** A guest is handed ONE `utc_offset_s`; the adapter reads the
  zone per message. Two messages either side of a transition can print an hour apart. The parity
  test keeps its fixtures inside one offset period for that reason.
- **A quote cut inside an emoji sequence.** The layer measures columns with a hand-written table
  (`is_wide`/`is_zero_width`, the blocks a room here is written in) because a guest links no `rano`;
  a ZWJ emoji is two columns to `rano` and four to the layer. Plain text, CJK and combining marks
  agree.

## 6. Open questions

1. Does the native adapter stay forever as the fallback, or go once P6 is proven? Default: stays,
   for the reason the session list's does.
2. Who may read the `chat` feed: built-ins and pinned layers only? Default: yes; a project that wants
   its own chat UI pins it.
3. Cursor mode (Tab on an empty input) or pane-wide uppercase keys while typing? Default: cursor
   mode; letters belong to the text.
4. `exec` looks for pinned panes (X10): allow through `admit_unasked`? Default: yes, which deletes
   the generator scripts every session pane needed.
5. `dock` beside the conversation: is that the "tabs or windows" the operator asked for, or the
   first step of it? Default: first step; one right-hand column, host-owned width.
6. The trust round trip on every rebuild (W4): pin a session layer automatically when the session
   that built it loads it? Default: no; a session pin stays the operator's, but `load` says the
   one command to type.

## 7. Decisions

2026-11-07, the operator:

- Question 1: the native renderer stays only as a safety net. It is kept until the built-in chat
  plugin (P6) has run without traps for a while, then deleted. It is not a second product: P2-P4
  build it as the adapter, and P5-P6 put the plugin on it.
- This review was scoped too tightly on rooms. A second review covers the whole harness and head
  (`docs/plugin-extension-points-wide.md`); questions 2-6 are answered after it, against both.
- Started now, independent of both reviews: the four walls of section 2 that are bugs, not design
  (TODO R151): built-in panes unreachable by `/pane`, `read` resolved against the workspace not the
  checkout, the pin lost on every rebuild, and the `plugin-hooks` skill missing `tool_`, `command_`
  and `pane_`.
