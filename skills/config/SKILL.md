---
name: config
description: Answer "what setting does X" and propose a change to letibot's own configuration, which the operator applies by answering a card
when_to_use: the operator asks which key to edit to change letibot's behaviour, a model or provider setting is wrong, or a change needs a setting that no flag covers
keywords: config, configuration, leticode.toml, providers.toml, hosts.toml, setting, option, key, toggle, edit config, restart, propose
---

# config

`~/.config/letibot` is masked inside a session on purpose: a model that edits its own boundary or
its own mode grants itself anything. The directory stays masked. The `config` tool is the door.

## What to edit

Never guess a key name, and never ask the operator to look one up. `config explain` answers from
the reference the daemon itself reads:

1. `config explain` — every file, one line each.
2. `config explain {file}` — every key of one file: type, default, what it decides.
3. `config explain {key}` — one key by name, e.g. `{"key": "exec.foreground_seconds"}`; a bare
   word finds it too, and every file that has it.

Files: `leticode.toml` (models, exec, queue, boundary, memory, the box's own settings),
`providers.toml` (API keys, doors, prices, the guard), `hosts.toml` (machines you may place work
on), `prompts.toml` (system-prompt overrides), `permission.json`, `plugin-trust.json`,
`modes.tsv`. Credentials are never written by this tool, and `keys/` is not opened at all.

## Proposing the change

```
config {action: "propose", file: "leticode.toml",
        edit: {"exec.foreground_seconds": 120},
        why: "the cargo test suites need more than 60s in the foreground"}
```

- `edit` is dotted key paths to TOML values; a JSON string is a TOML string, so quote a span:
  `{"provider.wait_if_within": "1h"}`. `null` removes a key.
- `why` is required. It is the line the operator reads on the card.
- The result is applied in memory, the file's own reader judges it, and the diff goes to the
  operator as a card. If the reader refuses, you are told in its words before anybody is asked.

**The operator answers, and nobody else.** No mode, no session or always grant, no bridge seat
and no guard model applies a config change. A refusal is a decision: say what you needed and stop,
do not retry a variant.

## After the yes

The result says what changed, that the file's comments and layout were kept, and how a change
takes effect — the reference's own line for that file, e.g. *a session reads `leticode.toml` when
it opens*, or *a daemon restart may be needed* for `[gc]`, `[docker]`, `[memory]`.

## Reading a file

`config show {file}` prints one file with every credential value replaced. It is also a card: a
read of a masked path is the operator's to allow. Use it when you need the current values, not to
check your own edit — the proposal's diff already tells you what would change.
