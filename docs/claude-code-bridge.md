# Claude Code ↔ leticode: the bridge

Written 2026-10-25. The operator: *"can we have a better integration between claude code and
leticode? not just a simple shell run"*.

## Why the shell run is not enough

Claude Code delegates well-scoped work to leticode to save Claude usage (leticode turns go to
DeepSeek at a few cents a package). Until now that was `cd <worktree> && leticode --mode allow-all
"<brief>" > log` in the background, and the first day of it showed three defects of the shape, not
of any one run:

1. **A one-shot run dies with its turn.** A model that backgrounds a long test run and ends its turn
   to wait for it exits the process; the job's completion never reaches it (plugins slice 1, W3).
2. **Permission is all or nothing.** One-shot runs cannot ask anybody, so the only working mode is
   allow-all, which needs the operator's recorded consent per project and enforces nothing the
   brief asks (no push, `--offline`, writes only in the worktree).
3. **Following up means restarting.** A review comment goes back as `leticode --continue "<text>"`,
   which re-runs the daemon start-up, the banner and the resume each time; and the result comes back
   as a 10,000-line log of banner and memwatch lines with the report at the end.

## The shape

**leticode sessions become things Claude Code holds, not processes it runs once.** The daemon
already keeps sessions alive across heads, and its protocol (`crates/sessionlog/src/protocol.rs`)
already has everything a client needs: `NewSession`, `Prompt`, `Answer` (a permission ask's
answer), `Interrupt`, `Detach`, the listings. So the bridge is **a head that is a program**: no
daemon change for its first slice.

```
Claude Code ──stdio MCP── letibot-mcp ──unix socket── harnessd (one per worktree folder)
     ▲                         │
     └── background `letibot-mcp wait …` exits when a session needs attention
```

### `letibot-mcp`: an MCP server over stdio

A new binary (crate `crates/bridge`, package `letibot-bridge`, bin `letibot-mcp`). MCP over stdio is
JSON-RPC 2.0 with `initialize`, `tools/list` and `tools/call`; it is written with `serde_json` and
the sessionlog client, no MCP library. The process lives as long as the Claude Code session, and
holds one head connection per leticode session it started or attached to, so asks reach it.

| tool | input | answers |
|---|---|---|
| `leticode_start` | `worktree` (absolute path), `brief`, `model` (e.g. `deepseek`), `mode` (default `writes-allowed`) | `{session}` — starts the folder's daemon if needed (as the `letibot` launcher does), opens a session, sends the brief as the first prompt, stays attached |
| `leticode_send` | `session`, `text` | `{queued: true}` — an operator line into the running session (queued behind a running turn, as the head's own) |
| `leticode_status` | `session` | `{state: running\|idle\|asking\|failed, turn, todos, last_message, report?, cost_usd, tokens, pending_asks}` |
| `leticode_events` | `session`, `since` (cursor) | the compact progress since the cursor: tool calls (name + one line), job starts and ends, failures, asks — never banner or memwatch lines — and the next cursor |
| `leticode_asks` | `session` | the pending permission asks: id, the call (tool, arguments), the options the gate offers |
| `leticode_answer` | `session`, `ask`, `option` (one the ask offered), `note` | `{answered: true}` — sent as the protocol's `Answer`, so it lands in the decision corpus like the operator's own |
| `leticode_diff` | `session` | the worktree's branch against its merge base: `git log --oneline` and `git diff --stat`, plus the full diff when asked |
| `leticode_stop` | `session` | interrupts a running turn and detaches; the session stays on disk |
| `leticode_list` | — | the sessions this bridge holds, with state |

### `letibot-mcp wait`: the wake-up

Claude Code wakes its model when a background command exits, so the bridge binary also has a CLI
verb: `letibot-mcp wait <worktree> <session> --until attention [--timeout 2h]` connects to the
folder's daemon as a head and **exits when the session needs Claude**: its turn ended (exit 0, the
last assistant message on stdout), an ask is pending (exit 10, the ask on stdout), it failed (exit
20), or the timeout passed (exit 30). Claude runs it as a background command after every
`leticode_start` and `leticode_send`; its exit is the notification.

### Claude Code as the approver

With the bridge attached, the session no longer needs allow-all: `writes-allowed` (approve a class
once) puts the routine through and sends what the gate asks about — a push, network, a write
outside the worktree, the always-ask list — to the attached head, which is the bridge. Claude reads
it with `leticode_asks` and answers with `leticode_answer`. The corpus records each answer with who
gave it. A session whose head is the bridge needs no project consent, because a reviewer is in the
loop; one that is left with no head attached refuses gated calls as any session does.

### The report as data

The brief's closing report (built, tested, could not do, contract changes, commits) is asked for in
a fixed markdown shape, and `leticode_status` parses it into fields when the last assistant message
has it, so the review starts from data.

### `/delegate`: the skill

A Claude Code skill that packages the workflow, so any Claude session delegates the same way:
contracts first and written into the repository; one worktree per package; the brief template with
the hard rules; `leticode_start` + background `wait`; review with `leticode_diff` and the report;
follow up with `leticode_send`; merge only after re-running the checks.

### Later: the other direction

A leticode session escalating a design question it should not decide alone: an `escalate` tool
that posts to the bridge, surfacing in `leticode_status` as `asking`, answered with
`leticode_send`.

## Packages

| id | builds | depends on |
|---|---|---|
| B1 | `crates/bridge`: the MCP server (`initialize`, `tools/list`, `tools/call`) with `leticode_start`, `send`, `status`, `asks`, `answer`, `diff`, `stop`, `list`; the `wait` verb; tests against a daemon started on a temp workspace with a fake provider (as harnessd's own integration tests do), never a real provider | protocol as it is |
| B2 | `leticode_events` with the compact progress and the cursor; the report parsed into fields in `status` | B1 |
| B3 | the `/delegate` skill, and registering the server with Claude Code (`claude mcp add`, the operator's to run) | B1 |
| B4 | `escalate` | B1, a protocol addition |
