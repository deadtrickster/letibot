# The ACP adapter: a letibot session in Zed or Neovim

`letibot-acp` (crates/acp) is an [Agent Client Protocol](https://agentclientprotocol.com) agent on
stdio. An ACP client launches it as a subprocess and speaks JSON-RPC on its standard streams; the
process is a **head**, so the session it holds is the daemon's: closing the client does not end a
turn, and a person can watch the same session in `letibot` at the same time.

```text
Zed / Neovim ──stdio JSON-RPC (ACP v1)── letibot-acp ──unix socket── harnessd
```

It is a mapping and nothing else. `docs/rano-integration.md` §3.1 decided ACP is "an adapter, not
the protocol"; this is that adapter. What the package adds is one more spelling of the wire
`crates/sessionlog/src/protocol.rs` already has: an ACP `session/new` is a letibot session on the
folder's daemon, an ACP `session/prompt` is `ClientFrame::Prompt`, and a `session/update` is an
event that had already arrived.

Read from `zed-industries/agent-client-protocol` at `c392d7e`, schema **v1.31.0** and
`docs/protocol/v1/*.mdx`. The stable protocol version is `1`; `2` is `2.0.0-alpha` and is not
spoken here.

## Running it

The daemon must be up first - the adapter never starts one, because which model, mode and
placement a folder runs on is the launcher's choice (`docs/rano-integration.md` §3.3):

```sh
cd ~/Projects/some/repo && letibot        # the folder's daemon, in a terminal or detached
```

Then point a client at the binary. Zed, `.zed/settings.json`:

```json
{
  "agent_servers": {
    "letibot": { "command": "/home/you/Projects/letibot/letibot/target/release/letibot-acp" }
  }
}
```

`$LETIBOT_SOCKET` is honoured when set, as it is by every other head, so an adapter can be pointed
at a daemon the workspace hash would not name. `letibot-acp --help` prints the two flags it has,
both of which a client never passes.

## The mapping

| ACP | letibot | Where |
|---|---|---|
| `initialize` | - | answers protocol 1, `promptCapabilities.embeddedContext`, `loadSession: false` |
| `session/new` `{cwd}` | `resolve` + `create_session_in` + `Attach` | the folder's daemon; a daemon that is not up is refused, never started |
| `session/prompt` | `ClientFrame::Prompt` | the text blocks are the words; `resource` and `resource_link` blocks become `EditorContext` |
| resource `#L41-L47` | `ContextRef.range` | `docs/rano-integration.md` §4.2 - the daemon renders it, this never does |
| `_meta.letibot.unsaved` | `ContextRef.unsaved` | ACP has no unsaved bit, so a client that cares says so |
| `session/update` `agent_message_chunk` | `Delta{target: Text}` | one chunk per delta, in order |
| `session/update` `agent_thought_chunk` | `Delta{target: Reasoning}` | |
| `session/update` `tool_call` | `ToolCallProposed` | the title is `tool: target`; the kind comes from the tool's name |
| `session/update` `tool_call_update` | `ToolFinished` | the status, the locations, and a `diff` per changed region |
| `session/request_permission` | `DecisionRequested{kind: permission}` | answered with `ClientFrame::Answer`; `OptionKind` is ACP's four kinds |
| `session/cancel` | `ClientFrame::Interrupt` | the answer to the open prompt is `cancelled`, as ACP requires |
| turn end | `TurnFinished{ends_turn}` | `end_turn`, or `max_tokens` on a length stop |
| turn interrupted | `TurnInterrupted` | `cancelled` |
| `Citations` | `resource_link` chunk | `crates/acp/src/marks.rs`, with the lines in the fragment |

### A resource with lines

ACP names a resource by URI and says nothing about lines, so the lines ride a fragment -
`file:///ws/docs/article.md#L41-L47`, the spelling GitHub's permalinks use and that
`docs/rano-integration.md` §4.4 asks for ("a `resource` with a `#L` fragment maps to a
`ContextRef`"). `L41` and `L41-47` are read too.

The range is rano's `SendEvent` shape, which is what the daemon's renderer expects: a column-1 end
is the **start of the line after** the last one named. So `#L41-L47` is
`start {41,1} .. end {48,1}`, and the block the model reads says `article.md:41-47`, numbered as
`read` numbers it. `crates/acp/src/context.rs` is the one place that spelling exists, and
`uri_for` inverts it for the citations going the other way.

### A changed region is a diff

`ToolEditExcerpt` already carries both sides of every region a call changed, which is exactly
ACP's `diff`: `path`, `oldText`, `newText`. The excerpt is the region that differs and not the
whole file, so the hunk a client draws is the change. A file the call created has no `oldText`,
which is the schema's "there was nothing here".

### Marks

`SessionEvent::Citations` is published with `{label, path, start, end, lines}` today, and
`crates/acp/src/marks.rs` turns each into a `resource_link` chunk naming the lines. That module is
where **L7** lands: `Citation.kind` (`cite | note | suggest`) and `Citation.body` add two arms -
a `note` becomes an agent message (ACP has no diagnostic line), and a `suggest` becomes the same
`diff` an edit builds, with `lines` as the old side and `body` as the new one. Nothing else in the
adapter changes for it.

## What is not wired, and why

- **`session/load`** is not advertised. The daemon's snapshot is state, not a conversation replay,
  and `loadSession: true` promises the client a history it would not be given.
- **Question cards.** A `DecisionRequested` of `kind: question` is the model asking a person
  something, and ACP v1 has no stable construct for it (`elicitation` is behind a client
  capability this adapter does not take). The adapter logs the ask on stderr and the session's own
  screen head answers it.
- **`session/notices`** and `usage_update`: a warning is not a conversation row (it reaches heads
  and no engine), and `usage_update` needs a context-window size the head does not read. Both go
  to stderr until a client's capability makes them worth sending.
- **Image and audio blocks.** The capabilities say `false`, and the daemon's attachment door takes
  a path: a client's bytes would have to be written somewhere first, which is a decision for a
  later package and not a mapping.
- **MCP servers.** `session/new` requires the field and this adapter ignores it: a leticode
  session's tools are seated by the daemon's config.
- **One prompt at a time.** ACP clients send one `session/prompt` per session; a second while one
  runs is refused with an error rather than queued, because the daemon would queue it and the
  client would have no way to tell the two replies apart.
