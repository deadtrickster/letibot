# The room in the TUI - design and packages

Rooms are where sessions, gatekeepers and Claude Code seats talk. The operator reads them in `/rooms`.
Today that view is a plain-text IRC log polled every two seconds. This doc takes stock of flowy's chat,
of what `/rooms` has, and splits the work to close the gap into packages that separate sessions can
build at the same time.

Paths under `flowy/` are `~/Projects/flowy` at `c393d6e`. Other paths are this repository at `3f0b5776`.

## What flowy does

flowy has two chat clients. The Go TUI is the weaker one. The web console is the one worth matching.

### Web console (`flowy/web/src`)

Rendering
- Markdown is `marked` (full GFM) plus DOMPurify, with `breaks: true` for chat (`lib/markdown.ts:16-47`, `:264-281`).
  Unknown tags show as literal text (`:118-127`). Links open in a new tab (`:129-134`).
- Code fences are plain `<pre><code>`. There is no syntax highlighting anywhere.
- A bare ULID becomes a link to its row (`lib/markdown.ts:174-192`, `lib/rowlink.ts:23-62`).
- Mentions are chips in the speaker's colour, with a ring when they name the reader
  (`lib/markdown.ts:214-255`). The server writes `meta.mentions` as `name:id` pairs and the client
  chips only the names the server resolved (`lib/mentions.ts:46-120`).
- Pictures are `![alt](attachment:ID)` or `![alt](ID)` in the body (`lib/markdown.ts:337-375`). Their bytes
  are fetched per id and drawn as loading, missing with its reason, or the image (`lib/attachrefs.ts:22-82`,
  `lib/markdown.ts:405-430`). Attachments the body does not reference are cards under the message
  (`components/AttachmentCards.tsx:21-320`).
- Speaker colour is FNV-1a of the lowercased display name, mod an 8-colour palette
  (`lib/speakercolour.ts:33-64`).
- Grouping: consecutive messages from one actor less than 10 minutes apart, with the same addressee and
  privacy, share one header (`components/MessageList.tsx:220-230`, `:704-750`). Every row keeps its time.
- Times are the clock for today, `Mon D` before today, the year added outside this year (`lib/utils.ts:51-70`).
- Marks on a row: private, attributed or authored, disowned, `to <name>` (`components/MessageList.tsx:799-927`).

Composer (`components/MessageBox.tsx`)
- Multi-line. Enter sends, Shift+Enter is a newline (`:291-294`).
- `@` completion: the fragment back from the caret (`lib/atname.ts:123-135`), candidates from presence split
  into here and elsewhere (`:61-109`), prefix matches before substring matches, top 6 (`:145-156`). Up, Down,
  Tab, Enter, Esc drive it (`MessageBox.tsx:269-290`). A name that cannot hear the room gets a warning (`:377-387`).
- No slash commands, no emoji, no draft persistence, no Up-arrow history.
- Paste or the paperclip uploads files (max 4 MiB) and shows removable chips; the ids go in `attachments[]`
  (`MessageBox.tsx:227-235`, `:450-497`, `lib/attach.ts:44-64`).
- Cite: the browser selection inside a message becomes byte offsets; the composer shows the quote and the
  send carries `parents`, `thread` and `cite` (`lib/cite.ts:69-105`, `routes/ChatRoom.tsx:803-815`).

Threads
- `thread` plus `parents[]`, a DAG (`lib/api.ts:133-140`).
- The room folds every reply after the first unless it names the reader; the head shows `N replies` and the
  latest snippet (`components/MessageList.tsx:280-305`, `:1353-1375`).
- A thread pane as a list or a DAG drawing (`components/ThreadDag.tsx:27-101`).

Unread, presence, live
- Unread is a server-side cursor reader per room (`lib/unread.tsx:310-510`), acked only while the view is
  at the bottom (`MessageList.tsx:500-530`).
- Messages arriving while scrolled back raise a `N new messages - jump to latest` pill (`MessageList.tsx:1390-1396`).
  There is no unread divider in the list.
- Roster: members here and away, listener state, `polling Xago`, `acted Xago` (`components/RoomRoster.tsx:68-446`).
- Live: a long poll on `/api/chat/{room}/wait`, about 25 s (`lib/api.ts:2103`, `routes/ChatRoom.tsx:662-690`).
- Older pages when within 240 px of the top, keeping the reader's place (`MessageList.tsx:425-488`).
- Search over chat activity, top 8, opens the room not the message (`components/RoomSearch.tsx:25-77`).
- Pins (`components/PinnedStrip.tsx:32-89`), bookmarks, four fixed reactions (`MessageList.tsx:1463-1468`).

### Go TUI (`flowy/internal/tui`)

bubbletea and lipgloss, no glamour, no chroma. Four columns: rooms, stream, todos, thread
(`rooms.go:166-250`). One line per message, cut and never wrapped (`rooms.go:341-398`), `HH:MM` and a
17-column name (`:123`, `:381`). Colours for addressed-to-me, agent, human (`theme.go:107-152`).
Acks as `👀3` (`rooms.go:510-677`). One-line `textinput` composer, no completion, no history (`model.go:221-282`).
No pictures: the inline picture of `c393d6e` is web only. Live updates by long poll (`cmds.go:190`).

## What letibot has today

Server (`crates/room`)
- `Message` is `seq, ts, room, author, kind, to, thread, refs, text` (`crates/room/src/chat.rs:140-156`).
  `text` is at most 4000 bytes (`chat.rs:24`). `refs` point at todo and queue rows (`chat.rs:127-136`).
- `mentions` parses `@name`, `cc@folder:name` and `cc@folder:*` (`chat.rs:159-192`). The server rewrites
  each mention to the resolved handle before storing (`crates/room/src/server.rs:1074-1095`), so stored text
  carries `@s-<id>` for a session.
- 500 messages and 256 KiB of text per room ring (`chat.rs:19-22`), one log, one `seq`.
- Requests `rooms, say, dm, read{since,last,thread,before}, roster, inbox, ack, invite, leave`
  (`crates/room/src/wire.rs:64-160`). Pushes `Chat{message, names_you}` to connections that may see a
  message (`wire.rs:390-393`, `server.rs:1222`, `:1254`, `:1848`).
- `ROOM_PROTOCOL_VERSION` is 8 and the hello demands equality (`wire.rs:15-34`). serde ignores unknown
  fields, there is no `deny_unknown_fields`.
- One frame is at most 1 MiB (`server.rs:40`).

Daemon
- The operator seat speaks to the coordinator as `operator` and takes chat requests only
  (`crates/harnessd/src/room/operator.rs:57-72`). It connects with a no-op wake and installs no `on_chat`
  hook (`operator.rs:91-99`), so pushed chat stops at the daemon.
- A head asks with `ClientFrame::RoomChat` and is answered with `ServerFrame::RoomChat`
  (`crates/sessionlog/src/protocol.rs:714-716`, `:1029-1034`, `crates/sessionlog/src/server.rs:505-520`).
  There is no unsolicited chat frame.

TUI (`crates/tui`)
- State: `RoomsView` holds rows, messages per room, read and seen cursors, the selection by identity,
  roster, paging, and pending sends (`crates/tui/src/app/rooms.rs:105-160`). Polls every 2 s open and every
  10 s closed (`rooms.rs:14-16`, `:411-427`).
- Sending: a line is a `say` or `dm`; `>14 text` replies to 14 (`rooms.rs:376-389`, `:592-637`). Failed sends
  keep their words and reason, Enter resends, `d` dismisses (`rooms.rs:642-668`).
- `@operator` detection (`rooms.rs:355-373`). A click on a member inserts `@handle ` (`rooms.rs:718-731`).
- Drawing: channel list with unread counts (`crates/tui/src/ui/panes/rooms.rs:96-109`, `:406-426`), member
  column or strip (`:266-362`), the chat body (`:452-492`), stick-to-tail and page-back keeping place
  (`:555-580`).
- A message is `seq! HH:MM <author> text`, text through `clean_line` and `wrap`, no markdown
  (`panes/rooms.rs:157-226`). Session ids are shown as titles via `rooms_names` (`panes/rooms.rs:430-450`),
  but only for the author column, not for `@s-...` inside the text.
- Keys: Up and Down switch channel, `r` roster, `d` dismiss (`crates/tui/src/app/keys/bindings.rs:405-430`).
- 61 tests in `crates/tui/src/app/tests/rooms.rs`.

What the TUI already has elsewhere and the room does not use
- Markdown with syntax-highlighted fences: `rano::markdown::IncrementalMarkdown` and `BlockCache`
  (`crates/tui/src/ui/transcript/assistant.rs:41-48`, `crates/tui/src/ui/render.rs:286-300`).
- Inline pictures through kitty Unicode placeholders, gated on `Proto::Kitty | KittyPassthrough`
  (`crates/tui/src/ui/screen.rs:127-137`), with uploads and re-placement on resize
  (`crates/tui/src/app/attention.rs:190-230`), `markdown_images` (`render.rs:93-112`), and the picture
  overlay (`crates/tui/src/app/pictures.rs`). rano also knows iTerm2 (`rano::term::Proto::Iterm`).
- Clipboard image paste and file chips (`crates/tui/src/app/attachments.rs:1-60`).
- Soft newline on Alt+Enter and kitty Shift+Enter (`crates/tui/src/app/keys/input.rs:55`).
- Slash completion that cycles on Tab (`crates/tui/src/app/composer.rs:295-331`) and the completion row
  (`crates/tui/src/ui/composer.rs:13-60`).
- OSC 8 links (`cfg.links`, `screen.rs:121-125`).

## The gap

| | flowy web | `/rooms` today |
|---|---|---|
| markdown, code | GFM, no highlighting | plain text |
| pictures | inline by body reference, cards | none |
| speaker colour, grouping | hash colour, 10 min runs | bold name every row |
| mentions | chips, reader ring | `!` on the seq, `@s-id` raw in text |
| `@` completion | presence, ranked, here and elsewhere | click on a member only |
| `/` commands in chat | none | global `/` verbs only |
| multi-line | yes | Alt+Enter works, nothing says so |
| reply | cite with quote preview | `>14 text` typed by hand |
| threads | fold, count, pane | `↳14` marker |
| unread | cursor, jump pill | channel counts, no divider, no jump |
| live | long poll, 25 s | 2 s poll |
| search | activity search | none |
| reactions, pins | yes | none |
| history recall, drafts | none | none |

letibot already beats flowy on code: rano highlights fences. The rest is a port into a terminal.

## Target design

### Message block

One message draws as a block, not a line.

```
── today ──────────────────────────────────────────────
14:02  gatekeeper@letibot                         #212
       merged `room: blob store`, gate green on lab2x1
       ```text
       test result: ok. 412 passed
       ```
14:03  cc@letibot                                 #213
       ↳ 212 gatekeeper: merged `room: blob store`...
       @operator can you look at the push frame?
       [picture 1280x720 shot.png - enter opens]
── unread ─────────────────────────────────────────────
14:09  fix-room-push                              #214
       done, see `crates/sessionlog/src/server.rs:505`
```

- Header: time, display name in the speaker colour, then marks (`to X`, `dm`), the `#seq` at the right edge,
  faint. Continuation messages of a run (same author, same `to`, same room, under 10 minutes, no day
  boundary) drop the header and keep only a faint time in the gutter when it differs.
- Body: `rano::markdown` with the same options as assistant prose, at the body column. Fences highlighted.
  Inline code, emphasis, lists, quotes, tables as rano draws them.
- Mentions: `@handle` drawn as `@display-name` in the named member's colour. A mention of the operator is
  inverse attention and marks the header. `@s-<id>` resolves through `rooms_names`.
- Links: URLs and `path:line` become OSC 8 links where `cfg.links` is on.
- Refs: the existing todo and queue refs as a faint trailer.
- Reply: one faint quote line `↳ N author: first line...` above the body. If N is not loaded, `↳ N`.
- Pictures: a `![alt](blob:ID)` in the body draws the picture at that line; attachments the body does not
  reference draw after it. Kitty: placeholder rows, sized by `image_cells`, capped at 12 rows in the chat.
  iTerm2 and no protocol: one row `[picture WxH name - enter opens]`. Enter on a selected message opens the
  picture overlay that `pictures.rs` already has.
- System messages stay one quiet row.
- Day separator when the local date changes. Unread divider before the first message newer than the
  channel's seen mark at the moment the channel was opened; it stays until the channel is left.

### Speaker colour

FNV-1a 32 of the lowercased handle (not the display name, so a rename keeps the colour) mod a palette of
8 `Role`s that read on both dark and light backgrounds. The operator is always `Role::Strong`. System is faint.

### Composer in a room

The prompt composer, routed by `composer.rs:99`, gains room behaviour while `/rooms` is focused.

- Enter sends, Alt+Enter and Shift+Enter add a line. The footer says so.
- `@` completion: the fragment back from the cursor. Candidates are the selected room's roster first
  (connected before offline), then members of other rooms flagged `elsewhere`, then the bridge aliases
  `cc` and `cc@<folder>:*`. Match on handle and display name; prefix before substring; top 6 in the
  completion row. Tab cycles, Enter or Tab accepts, Esc closes. Accepting inserts the handle, never the
  display name, because the server resolves handles.
- `/` completion of room verbs: `/dm`, `/invite`, `/leave`, `/reply N`, `/me`, `/search`, `/jump`, plus the
  head's own verbs. Room verbs listed first.
- History: Up on an empty composer recalls this channel's sent lines, newest first; Down walks back. 50 per
  channel, in memory.
- Drafts: switching channel keeps the unsent text per channel and restores it on return.
- Reply by selection: `R` on a selected message sets the reply target; the composer shows
  `replying to #N author: ...` above it, Esc clears it. `>N text` keeps working.
- Paste: an image on the clipboard or a pasted image file becomes a chip, uploaded to the room on send and
  referenced as `![name](blob:ID)` at the chip's place. Reuses `attachments.rs` `Clipboard` and chip logic.

### Navigation

- Message cursor: `Tab` from an empty composer, or a click, selects a message; Up and Down then move by
  message, Esc returns to the composer. On a selected message: `R` reply, `y` copy text (OSC 52), Enter
  opens its picture or first link, `t` shows its thread (root plus replies, through `read{thread}`).
- `End` or `G` jumps to the tail. `u` jumps to the unread divider. While scrolled back, new messages raise a
  footer `N new - End jumps`.
- `Ctrl+K` room switcher: fuzzy over channel labels and DM peers, unread first.
- `/search TEXT`: substring over the loaded messages of the channel, `n` and `N` step through hits, hits
  highlighted. Server-side search is a follow-up.

### Live delivery

The operator seat installs an `on_chat` hook. The daemon fans each pushed message out to every head that
has chatted, as `ServerFrame::RoomChatPushed {message, names_you}`. The head merges it like a `read` row.
The 2 s poll drops to a 15 s safety poll while pushes arrive, and returns to 2 s when a push is missed
(a `seq` gap) or the daemon is older.

### Wire changes

All additive. Old store files replay unchanged.

crates/room
- `chat::Attachment {id, mime, name, bytes, w: Option<u32>, h: Option<u32>}`. `id` is the sha256 hex of the bytes.
- `Message.attachments: Vec<Attachment>`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- `Request::Say` and `Request::Dm` gain `#[serde(default)] attachments: Vec<String>`, blob ids. The server
  fills `Message.attachments` from the blob store and refuses an unknown id (`Refused::UnknownBlob`). At
  most 4 per message.
- `Request::PutBlob {upload, name, mime, part, last, data}`: `data` is base64 of at most 512 KiB, so a
  frame stays under the 1 MiB limit. Parts append in order; `last` finishes the upload, hashes it and
  answers `Response::Blob {attachment}`. Max 8 MiB per blob. Mime allowlist: png, jpeg, gif, webp, text/plain.
- `Request::GetBlob {id, offset}` answers `Response::BlobPart {attachment, offset, data, last}`, 512 KiB per part.
- Blobs live in `room-<host key>.blobs/<id>` beside the chat store. A blob no kept message references is
  deleted when the chat store is rewritten, and an unreferenced upload after one hour.
- A text reference is `![alt](blob:<id>)`. It needs no field: an old reader sees the markdown.
- `ROOM_PROTOCOL_VERSION` 8 to 9. Without the bump an old coordinator drops `attachments` from a `say`
  silently; with it the mismatch is the existing refusal sentence and `ROOM_UPGRADE`.

crates/sessionlog
- `ServerFrame::RoomChatPushed {message: serde_json::Value, names_you: bool}`. `PROTOCOL_VERSION` 64 to 65.

crates/harnessd
- The operator seat allowlist adds `PutBlob` and `GetBlob` (`operator.rs:63-72`).

Not changed: reactions, edits, deletes, typing, pins. Each is a new request and a new record kind; see
open questions.

## Packages

P0 goes first and lands alone. P1 to P4 then run in parallel; their paths do not overlap. Each package
adds its own test file, registered by P0, so no two packages edit one file.

### P0 - contracts and seams (size M)

Owns
- `crates/room/src/chat.rs`, `crates/room/src/wire.rs`, `crates/room/src/server.rs` (say, dm, put and
  get blob handlers), `crates/room/src/blob.rs` (new), `crates/room/src/lib.rs`, `crates/room/src/room.rs`
  (`Refused::UnknownBlob`, `BlobTooLarge`, `BadMime`).
- `crates/harnessd/src/room/operator.rs` (allowlist only).
- `crates/sessionlog/src/protocol.rs` (the frame type and version only).
- `crates/tui/src/app/rooms.rs`: `Msg.attachments: Vec<Attach>`, `RoomsView.opened_seen: BTreeMap<String, u64>`
  (seen mark at channel open, for the divider), `RoomsView::merge` made `pub(crate)`, and the
  `on_room_pushed(&mut self, message: Value, names_you: bool)` entry, empty.
- `crates/tui/src/app/keys/bindings.rs`: every room binding below, each dispatching to a stub in the owning
  package's file. `crates/tui/src/app/keys/panes.rs` for the dispatch.
- New stub files, each with the signatures below and `todo!`-free defaults that keep today's behaviour:
  `crates/tui/src/ui/chat/mod.rs`, `crates/tui/src/app/chat_input/mod.rs`, `crates/tui/src/app/chat_nav/mod.rs`,
  `crates/tui/src/app/chat_media.rs`, `crates/tui/src/app/rooms_push.rs`.
- `crates/tui/src/app/mod.rs`, `crates/tui/src/ui/mod.rs`, `crates/tui/src/app/tests/mod.rs` (registers
  `rooms_push`, `rooms_render`, `rooms_compose`, `rooms_nav` test modules, each with one passing smoke test).
- `docs/room.md` (Messages, Requests, Versions sections).

Contract it defines (TUI side)
```rust
// ui/chat/mod.rs - P2 fills
pub(crate) struct ChatCtx<'a> {
    pub names: &'a BTreeMap<String, String>,  // handle -> display name
    pub me: &'a str,                           // "operator"
    pub width: usize,
    pub cfg: &'a RenderConfig,
    pub picture: &'a dyn Fn(&str) -> Option<(u32, u32, u32)>, // blob id -> (image id, w, h), from P4
    pub divider_after: Option<u64>,            // seq; unread divider follows it
    pub selected: Option<u64>,                 // seq under the message cursor
    pub highlight: Option<&'a str>,            // search needle
}
pub(crate) struct Block { pub seq: u64, pub lines: Vec<Line> }
pub(crate) fn chat_blocks(msgs: &[&Msg], ctx: &ChatCtx<'_>) -> Vec<Block>;
pub(crate) fn speaker_role(handle: &str) -> Role;

// app/chat_input/mod.rs - P3 fills
impl App {
    pub(crate) fn rooms_complete(&mut self) -> bool;          // Tab in a room; false = not ours
    pub(crate) fn rooms_history(&mut self, up: bool) -> bool;  // Up/Down on empty composer
    pub(crate) fn rooms_switched(&mut self, from: Option<&Chan>, to: &Chan); // drafts
    pub(crate) fn rooms_reply_target(&self) -> Option<u64>;
    pub(crate) fn rooms_set_reply(&mut self, seq: Option<u64>);
    pub(crate) fn rooms_outgoing(&mut self, text: &str) -> (String, Vec<String>); // text with blob refs, blob ids
}

// app/chat_nav/mod.rs - P4 fills
impl App {
    pub(crate) fn rooms_cursor(&self) -> Option<u64>;
    pub(crate) fn rooms_cursor_move(&mut self, up: bool) -> ControlFlow<Option<Action>>;
    pub(crate) fn rooms_jump(&mut self, to: Jump) -> ControlFlow<Option<Action>>; // Tail | Unread
    pub(crate) fn rooms_switcher(&mut self) -> ControlFlow<Option<Action>>;
    pub(crate) fn rooms_search(&mut self, needle: &str);
    pub(crate) fn rooms_search_needle(&self) -> Option<&str>;
}

// app/chat_media.rs - P4 fills
impl App {
    pub(crate) fn room_picture(&self, blob: &str) -> Option<(u32, u32, u32)>;
    pub(crate) fn room_pictures_wanted(&mut self, blobs: &[&Attach]); // asks GetBlob for unseen ids
    pub(crate) fn room_upload(&mut self, png: Vec<u8>, name: &str) -> String; // chip text; PutBlob parts
}

// app/rooms_push.rs - P1 fills
impl App { pub(crate) fn on_room_pushed(&mut self, message: Value, names_you: bool); }
```

Done when these pass, plus every existing test in `crates/room`, `crates/harnessd`, `crates/sessionlog`,
`crates/tui`:
- `crates/room/src/chat.rs`: `an_old_store_line_replays_without_attachments`,
  `a_message_without_attachments_serialises_as_before`.
- `crates/room/src/blob.rs`: `a_blob_put_in_parts_hashes_to_its_id`, `a_blob_over_the_cap_is_refused`,
  `a_blob_no_message_keeps_is_deleted_at_rewrite`.
- `crates/room/src/server.rs`: `a_say_with_a_blob_carries_its_attachment`, `a_say_naming_an_unknown_blob_is_refused`,
  `get_blob_returns_what_put_blob_stored`, `a_wrong_mime_is_refused`.
- `crates/harnessd/src/room/operator.rs`: `the_seat_passes_blob_requests`.
- `crates/room/src/wire.rs`: `the_protocol_is_nine`.

### P1 - live delivery and agent-side attachments (size M)

Owns `crates/harnessd/src/room/operator.rs` (the hook, after P0), `crates/sessionlog/src/registry.rs`,
`crates/sessionlog/src/server.rs` (room-chat worker and fan-out), `crates/sessionlog/src/client.rs`,
`crates/tui/src/driver.rs`, `crates/tui/src/app/rooms_push.rs`, `crates/tui/src/app/tests/rooms_push.rs`,
`crates/tools/src/builtins/room.rs`, `crates/harnessd/src/room/link.rs` (say path), `crates/bridge/src/rooms.rs`,
`crates/bridge/src/mcp.rs`, `crates/bridge/tests/room_tools.rs`.

Relies on: `ServerFrame::RoomChatPushed`, `RoomsView::merge`, `Request::PutBlob`, `Say.attachments`.

Does
- Operator seat `on_chat` hook into a registry broadcast to heads that have chatted.
- Head merges pushed messages, keeps `seen` and unread counts right, drops the poll to 15 s while pushes
  arrive, back to 2 s on a `seq` gap.
- Session `room_say` tool and the bridge's `room_say` take `attachments: [path]`; the daemon or bridge reads
  the file, uploads in parts, and says with the ids. Paths outside the session workspace are refused.

Done when these pass:
- `operator.rs`: `a_pushed_message_reaches_every_chatting_head`, `the_authors_own_message_is_not_pushed_back`.
- `sessionlog/src/server.rs`: `a_head_that_never_chatted_gets_no_push`.
- `tui/src/app/tests/rooms_push.rs`: `a_pushed_message_draws_without_a_poll`,
  `a_pushed_message_in_another_channel_counts_as_unread`, `a_seq_gap_returns_to_the_fast_poll`,
  `a_push_and_a_poll_of_one_message_draw_it_once`.
- `crates/bridge/tests/room_tools.rs`: `room_say_uploads_a_picture_and_references_it`.
- `crates/tools/src/builtins/room.rs`: `an_attachment_outside_the_workspace_is_refused`.

### P2 - message rendering (size L)

Owns `crates/tui/src/ui/chat/**` (`mod.rs`, `block.rs`, `group.rs`, `colour.rs`, `mention.rs`, `divider.rs`),
`crates/tui/src/ui/panes/rooms.rs` (body, header, footer; the channel and member columns stay as they are),
`crates/tui/src/app/tests/rooms_render.rs`.

Relies on: `ChatCtx`, `Msg.attachments`, `opened_seen`, `room_picture`, `rooms_cursor`, `rooms_search_needle`
(stubs return `None` until P4 lands, so pictures draw as the one-line fallback and nothing is selected).

Does: the message block above. Markdown through `rano::markdown`, grouping, speaker colour, mention
rewriting `@s-id` to names, day separator, unread divider, reply quote line, picture rows or fallback,
OSC 8 links, selection and search highlight, `#seq` at the right edge. The scroll math in
`panes/rooms.rs:555-580` keeps working on block lines.

Done when these pass, plus the 61 in `rooms.rs` (reworded where the line shape changed, none deleted):
- `a_code_fence_draws_highlighted_and_unwrapped`
- `inline_markdown_is_styled_not_shown_as_asterisks`
- `two_messages_from_one_author_within_ten_minutes_share_a_header`
- `a_day_boundary_breaks_a_run_and_draws_a_separator`
- `the_same_handle_always_gets_the_same_colour`
- `a_mention_of_a_session_id_draws_its_name`
- `a_mention_of_the_operator_marks_the_header`
- `the_unread_divider_sits_before_the_first_unseen_message`
- `a_reply_draws_a_quote_of_the_message_it_answers`
- `a_picture_without_a_protocol_is_one_row_naming_it`
- `a_picture_with_kitty_draws_placeholder_rows`
- `a_link_is_an_osc8_link_when_links_are_on`
- `every_block_fits_eighty_and_forty_columns`
- `a_control_byte_in_a_message_never_reaches_the_terminal`

### P3 - composer in a room (size M)

Owns `crates/tui/src/app/chat_input/**` (`mod.rs`, `complete.rs`, `history.rs`, `drafts.rs`, `reply.rs`),
`crates/tui/src/ui/chat_compose.rs` (new: the completion row and the reply strip for rooms),
`crates/tui/src/app/tests/rooms_compose.rs`.

Relies on: `RoomsView.roster`, `rooms_names`, the stub dispatch from `bindings.rs`, `room_upload` (P4) for
pasted pictures. Until P4 lands the chip uploads nothing and `rooms_outgoing` returns the text unchanged.

Does: `@` completion, room `/` verbs, per-channel history and drafts, reply target and strip, image chips,
the footer naming Enter and Alt+Enter.

Done when these pass:
- `at_completes_from_the_room_roster_connected_first`
- `a_prefix_match_ranks_before_a_substring_match`
- `completion_matches_a_display_name_and_inserts_the_handle`
- `a_member_of_another_room_is_marked_elsewhere`
- `tab_cycles_and_esc_closes_the_completion`
- `slash_in_a_room_offers_room_verbs_first`
- `up_on_an_empty_composer_recalls_this_channels_last_line`
- `history_is_per_channel`
- `a_draft_survives_a_channel_switch`
- `r_on_a_selected_message_replies_and_esc_clears_it`
- `alt_enter_adds_a_line_and_enter_sends_both`
- `a_pasted_picture_becomes_a_chip_and_a_blob_reference`

### P4 - navigation and pictures (size M)

Owns `crates/tui/src/app/chat_nav/**` (`mod.rs`, `cursor.rs`, `jump.rs`, `switcher.rs`, `search.rs`),
`crates/tui/src/app/chat_media.rs`, `crates/tui/src/ui/chat_switcher.rs` (new),
`crates/tui/src/app/tests/rooms_nav.rs`.

Relies on: `Request::GetBlob`, `Request::PutBlob`, `Attachment`, the picture upload queue in
`attention.rs` (called, not edited), `PictureRef` overlay in `pictures.rs` (called, not edited).

Does: message cursor, Enter opens picture or link, `y` copies, `t` thread view, jump to tail and to
unread, the `N new` footer, `Ctrl+K` switcher, `/search` with `n` and `N`, the blob fetch and cache
(bounded: 32 pictures or 64 MiB, least recently drawn out first), uploads in 512 KiB parts.

Done when these pass:
- `tab_on_an_empty_composer_selects_the_newest_message`
- `up_and_down_move_by_message_not_by_line`
- `u_jumps_to_the_unread_divider`
- `new_messages_while_scrolled_back_say_how_many`
- `ctrl_k_filters_channels_and_enter_switches`
- `search_highlights_hits_and_n_steps_through_them`
- `a_picture_is_fetched_once_and_drawn_from_the_cache`
- `the_picture_cache_drops_the_least_recently_drawn`
- `an_upload_goes_in_parts_under_the_frame_limit`
- `enter_on_a_picture_message_opens_the_overlay`
- `t_shows_the_thread_of_the_selected_message`

## Order and gates

P0, then P1 to P4 in parallel, each gated by `cargo test -p letibot-room -p letibot-harnessd
-p letibot-sessionlog -p letibot-tui -p letibot-tools -p letibot-bridge` and clippy. P2 lands before P3 and P4 only if they collide on
rebase; by construction they should not. The room protocol bump means the coordinator restarts on the
P0 build (`ROOM_UPGRADE`); daemons and heads follow with the next release.

## Open questions

1. Protocol bump 8 to 9 forces a coordinator restart and every daemon to match. Default: bump, because the
   alternative is silent attachment loss on an old coordinator.
2. Blob storage in the coordinator's state directory, 8 MiB per blob, GC with the ring. Default: yes.
   Alternative is a daemon-local store, which loses pictures for bridges.
3. Reactions (flowy's four acks) need a new request and a record kind. Default: a follow-up package after
   these, shape `react{seq, emoji, on}` and `Message.reactions` summarised on read.
4. Thread folding as flowy does it (hide replies after the first) changes what the operator sees by
   default. Default: no folding; `t` shows a thread on demand.
5. Server-side search over the full log, beyond the 500-message ring. Default: out of scope; the ring is
   the history.
6. Should agents see pictures? A session reading a message with an attachment gets the name and size in
   `Message::line`. Feeding the bytes to a model is a separate decision.
