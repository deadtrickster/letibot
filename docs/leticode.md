# leticode — opencode-shaped agent on letibot primitives

Direction (2026-09-13): a letibot session that models itself after opencode —
the tool union plus a verbatim port of opencode's permission model — and a
serenedash-style dashboard over the pieces that are new.

## Done

- `crates/tools/src/permission.rs` — verbatim opencode permission model
  (`Action`, `Rule`, `evaluate`, wildcard, `config_to_ruleset`), wired into
  `AdjudicatedGate` before the mode, with an `allow_always` reply feeding
  `(permission, pattern)` back into the ruleset.
- `crates/flowy` — the seat on the fabric, as a monitor: `harnessd --flowy`
  holds one persistent seat per daemon, a root session attaches with its own
  per-room attention table and gets a continuous `flowy` monitor whose firings
  are the messages; `flowy` is the sixteenth tool (`status`, `attention`,
  `subscribe`, `say`, …). Subagents route through the parent. See
  `docs/flowy-monitor.md`.
- **Downgradable subagents.** `task` takes `access` — `read-only`, or any of
  `no-write`, `no-exec`, `no-network` — and `where` (`host`, or `firecode`).
  A subagent inherits the parent's ruleset and the downgrade only removes: it
  is the union of the parent's own downgrade and the one asked for, so a
  downgraded session cannot spawn a wider child by naming a wider role. Three
  readers of one fact, and a test that they agree: the tools of a denied class
  are not seated (`Registry::without_access`), the backend is opened without
  them (read-only view without `Write`, no process host without `Exec`), and
  the ruleset carries a `deny` per denied tool — seated or well-known — that
  wins by last-rule. `role` now accepts any seat this build knows and refuses
  an unknown one rather than seating it as coder. `where: firecode` is a
  declared seam: refused by name until the backend exists, never run on the
  host in its place. The disclosure names the downgrade.
- **`where: firecode` filled** — `crates/tools/src/firecode.rs`: tools in a
  VM over vsock, model on the host, the child on a copy of the workspace, the
  VM the boundary (allow-all inside). `docs/subagents.md`; `docs/cookbook.md`
  (filed on the fabric as skill `01M2FRW9D3A8K8V9660BJQX5PZ`).
- **Cloud providers** — `--provider deepseek|glm|grok`: the transcript as
  messages over a `MessagesBackend`, the token ledger kept as the record,
  METERED and disclosed. `docs/providers.md`.
- **`pkill` and a process watcher that cannot self-match** —
  `crates/tools/src/exec/procs.rs` is an in-process `/proc` finder that removes
  this daemon, its ancestors and its protected pids before matching; `pkill`
  lists by pattern and kills by (pid, start time) only; `monitor process=` /
  `pid=` resolve to handles once and watch those. The daemon declares its model
  server protected. leticode is 17 tools with it.
- **The fabric block** — the shelf's skills as summaries and the memories as
  titles in the system prompt, refreshed after a compaction as a system update,
  cached for when the node is away. `docs/flowy-monitor.md` §5c.

## The three tools, and what each is

| tool | opencode's | what it is | hard part |
|---|---|---|---|
| `task` | subagent | spawn a child turn with a role + tool subset, run to completion, return its result | delegation + a second turn loop + budget/lifetime |
| `lsp` | LSP | run a language server, read diagnostics/hover/definition for the file at hand | spawning + talking LSP, one server per language |
| `skill` | skill | a named, loadable prompt/capability; list them, load one into context | almost none — a registry + a list/load tool |

## The dashboard

A serenedash-style terminal TUI (the same palette / row grid / boxed panels /
side-by-side reflow as `llama-dash`), with one panel per piece:

- **tasks** — the subagent tree: name, role, state (queued/running/done/failed),
  tokens so far, elapsed, and a sparkline of progress. The detailed one.
- **lsp** — servers up, per-language diagnostics counts (error/warn), last
  hover/definition latency.
- **skills** — the registry: name, description, loaded/not, last-loaded.

Data comes from the daemon over the existing session socket (events for task
spawn/finish, a query for the lsp/skill registries), so the dashboard is a view
and the daemon is the record.

## Order

1. `skill` (registry + tool) — no moving parts.
2. `lsp` (one server, diagnostics only, behind a boundary) — medium.
3. `task` (subagent turn, role + subset, budget) — the real work.
4. the dashboard, tasks first.
