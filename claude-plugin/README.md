# The leticode plugin for Claude Code

One installable plugin around the leticode bridge (`crates/bridge`, `docs/claude-code-bridge.md`).
It replaces two things a seat used to need: `claude mcp add` for every project, and a shell alias
around `claude` that registered the server by hand.

It carries six pieces:

| piece | what it is |
|---|---|
| the MCP server `leticode` | `letibot-mcp` from `PATH` — the session tools, the room tools and the seat's todo board |
| the channel | `capabilities.experimental["claude/channel"]`, which the server declares; each turn end, ask, failure and provider trouble of a held session is pushed as `notifications/claude/channel` |
| the room waiter | a plugin monitor: `letibot-mcp room-wait --attention to_me` in a loop, so a message for this seat reaches the session without the channel flag |
| the `leticode` skill | brief a child, wait for it, read it, answer its asks, stop or collect it |
| the `room-waiter` skill | what arrives, what to answer, and what an ask is that chat is not |
| the panes | `hooks/register.tsx`, beside the chat: the packages this bridge holds (`/packages`; click a name to follow its transcript), remote runs from `rrun` (`/runs`), and this seat's todo board (`/leticode-todos`) |

## Install

In a Claude Code session:

```text
/plugin install leticode --marketplace deadtrickster/letibot
```

That is one line, and it adds the marketplace too (Claude Code 2.1.275 or later). The long form,
if you prefer to add the marketplace first, is `/plugin marketplace add deadtrickster/letibot` and
then `/plugin install leticode@leticode`. From a checkout, `/plugin marketplace add ./` inside it
does the same against the working tree.

`install.sh` puts `letibot-mcp` in `~/.local/bin` with the daemon and the head, so the server the
plugin names is on `PATH` already; Claude Code must be started with that directory on its `PATH`
for both the server and the waiter.

## The flag, until the channel is allowlisted

**The plugin does not make the channel work on its own.** Custom channels are not on Claude Code's
approved allowlist — `claude-plugins-official`, or an organisation's own
`allowedChannelPlugins` — so a channel that publishes to this repository still has to be started
with the development flag, in an interactive session:

```text
claude --dangerously-load-development-channels plugin:leticode@leticode
```

`--channels plugin:leticode@leticode` is the form the flag becomes once it is allowlisted. Without
either, Claude Code drops the notifications silently and the tools still work: the room waiter
still wakes the session, and `letibot-mcp wait <worktree> <session> --until attention` is the
per-session wake as a background command.

A bare `claude mcp add` registration of the same server is bypassed with
`--dangerously-load-development-channels server:leticode` — the server name, for the case where
the plugin is not what registered it.

## Permission relay, off by default

The bridge can relay this session's own permission prompts — the tool approvals you would answer at
the Claude Code terminal — to the operator as one room DM, and take `yes <id>` / `no <id>` back
(`docs/claude-code-bridge.md` § *Permission relay*). It is off until you ask for it, because
declaring the capability is a promise that a verdict will be answered:

```toml
# ~/.config/letibot/leticode.toml
[bridge]
permission_relay = true
```

Two things then have to hold for it to work at all: **the session must be started with a channel
flag** — `claude --dangerously-load-development-channels plugin:leticode@leticode` above, or
`--dangerously-load-development-channels server:leticode` where a bare `claude mcp add` registered
the server — and the room coordinator must be up. Without the flag Claude Code never sends a
prompt, and the terminal dialog is the only door, as it was. That dialog stays open either way:
whichever answer comes first wins, and an id older than 30 minutes is forgotten rather than
answered.

## What it does not do

- **Trust and MCP consent dialogs.** The relay covers tool approvals (`Bash`, `Write`, `Edit`);
  Claude Code does not relay the others.
- **Non-interactive sessions.** Plugin monitors do not start with `-p`, and the development flag
  is ignored there, so a headless run gets the tools and neither push.
- **The queue.** `leticode_queue`, `leticode_submit` and `wait-queue` (B5) are not built; land a
  child's branch the way the operator does.

## Checking a change to this directory

```sh
claude plugin validate ./claude-plugin     # the manifest, its paths, and the MCP entry
sh tests/claude-plugin.sh                  # what can be checked without Claude Code
```

`tests/claude-plugin.sh` walks the two JSON files, the skills, `install.sh --help` and the
README, and runs in `cargo test` beside the other shell suites
(`crates/tools/tests/shell_suites.rs`).
