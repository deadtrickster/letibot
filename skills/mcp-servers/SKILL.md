---
name: mcp-servers
description: Configure, pin, check and reload the MCP servers this session reaches through `mcp_search` and `mcp_call` - where `[mcp.NAME]` lives, how a checkout's `.mcp.json` is pinned, how to prove a server works, and `/mcp reload` in place of a daemon restart.
when_to_use: the operator wants a new, changed or broken MCP server configured, a project's `.mcp.json` started, or `mcp_search` finds no server or a server's calls fail
keywords: mcp, mcp server, .mcp.json, mcp_search, mcp_call, model context protocol, mcp reload
---

# MCP servers

Servers are the daemon's. Their tools reach you as `NAME__tool` through `mcp_search` (find a tool,
see its schema) and `mcp_call` (run it). `/mcp` lists each server with its state, tool count, pid or
url, and where it was read.

## Where a server is configured

`[mcp.NAME]` in the user file, `~/.config/letibot/leticode.toml`. A project leticode.toml's
`[mcp]` is never started. NAME is letters, digits, `-` and `_`, never `__`.

| key | meaning |
|---|---|
| `command` | program for a stdio server; set this or `url` |
| `args` | its arguments |
| `env` | variables for it; its environment is otherwise cleared |
| `url` | `http://` or `https://` streamable HTTP endpoint |
| `headers` | sent with every HTTP request |
| `timeout_seconds` | start, listing and each call; default 60 |
| `enabled` | `false` keeps the table and starts nothing |

```toml
[mcp.github]
command = "github-mcp-server"
args = ["stdio"]
env = { GITHUB_TOKEN = "..." }
```

## A checkout's `.mcp.json`

It starts only when the user file pins that exact file at the sha256 of its bytes. Read the file
first, then hash it:

```sh
sha256sum /abs/path/to/checkout/.mcp.json
```

```toml
[mcp_projects.pinned]
"/abs/path/to/checkout/.mcp.json" = "<sha256>"
```

An edit to the file unpins it, and its servers stop at the next reload. The `mcp_server` warning
names the line to pin. A name the user file also has is the user's.

## Editing the user file

leticode.toml is letibot's own rules, so a write to it is guarded. Propose the exact diff and let
the operator approve the card. Do not try another path around the guard.

A token goes in the server's `env` or `headers` in that file, never in the transcript. Never echo a
header or env value back, not even to confirm it. Write `"..."` in a proposal and let the
operator fill it in, or name the variable it comes from.

## Proving a server works

1. `/mcp` - it should say `running` with a non-zero tool count. `failed: ...` carries the start
   error, often the stderr tail.
2. `mcp_search` for one of its tools.
3. One cheap `mcp_call`, a read or a list. Never a write as a test.

## Applying a change

`/mcp reload` re-reads the user file and the pinned `.mcp.json`. It starts added servers, stops
removed ones, restarts changed ones (command, args, env, url, headers) and leaves the rest running.
It answers one line per change. `/mcp restart NAME` restarts one, for example after its binary was
rebuilt. A changed file or a stdio binary with a new mtime is also reloaded at the next turn start,
said in one `mcp:` row. Never ask for a daemon restart for an MCP change.
