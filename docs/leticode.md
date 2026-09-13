# leticode — opencode-shaped agent on letibot primitives

Direction (2026-09-13): a letibot session that models itself after opencode —
the tool union plus a verbatim port of opencode's permission model — and a
serenedash-style dashboard over the pieces that are new.

## Done

- `crates/tools/src/permission.rs` — verbatim opencode permission model
  (`Action`, `Rule`, `evaluate`, wildcard, `config_to_ruleset`), wired into
  `AdjudicatedGate` before the mode, with an `allow_always` reply feeding
  `(permission, pattern)` back into the ruleset.

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
