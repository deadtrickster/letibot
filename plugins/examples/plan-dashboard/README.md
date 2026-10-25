# plan-dashboard

The example letibot pane plugin, and the shape a project's own dashboard takes: it reads the
project's plan as **data** rather than prose, and answers one `View` in serenedash's language —
panels on the overview, a full view behind each panel's key, every bar with its denominator, and
every claim that tripped in one findings list with a fix and a way to check it. The contract is
`docs/plugins-slice1.md` C3 (the pane) and C5 (this example) in the letibot tree.

## What it reads

One file: `<project>/.letibot/status.json`. The plugin's only capability is `read`, and its look is
always `{"read": ".letibot/status.json"}` — nothing else is touched, no process is run.

```jsonc
{"updated":"2026-10-25T21:55:00Z",                        // RFC3339; the only required field
 "release":{"name":"v0.2.0","goal":"every row of tiers 0, 1 and 2 done"},
 "tiers":[{"id":"tier0","title":"Tier 0","rows":[
   {"id":"tier0/a","area":"docs","item":"…","status":"done","note":"","branch":"","agent":""}]}],
 "stages":[{"id":"smoke","title":"smoke checkpoint","status":"waiting","note":"…"}],
 "agents":[{"id":"s-7c1","kind":"leticode","task":"…","branch":"…","status":"running"}],
 "hosts":[{"id":"lab2x1","state":"busy","jobs":3,"note":""}],
 "yours":[{"id":"tag","what":"tag and publish"}]}
```

`status` ∈ `done`, `in progress`, `not started`, `waiting`, `blocked`, `yours`. Every field but
`updated` may be left out, and a field the plugin does not know is ignored — a project with a richer
source of truth writes its own plugin, this one shows the language.

What it draws, and what the keys are (the host owns `q`, `j`, `k`, `f`, `l`, ↑/↓, PgUp/PgDn, Enter
and Esc; the plugin only names panel keys and action keys):

| panel | key | is |
|---|---|---|
| `tiers` | `t` | one line per tier — `done/total`, a bar of that tier's own rows, what is moving in it — and a table per tier behind the key |
| `stages` | `g` | one line per stage, its status as the value |
| `agents` | `a` | kind, task, branch, status |
| `hosts` | `h` | only when the file names hosts: state, and the jobs on it |
| `yours` | `y` | the rows waiting on you, which is also what the badge counts |

The title is `plan · <release name>`; the badge counts what is yours (role `attention`), else the
running agents (role `working`), else nothing. Findings are: a status file older than 24 hours
(`tripped`, with its age as evidence), one per blocked row (`tripped`), and "status file parsed"
(`passed`) when nothing tripped at all — a clean read says so rather than leaving the list empty. A
look that fails, or a file that will not parse, is one panel `unavailable` with the error verbatim
(serde_json's message carries the line and column) and a `missing` finding: never an empty
dashboard that looks like a finished plan.

Actions: `r` (`refresh`) on every stop, and `s` (`start`) on a not-started row, which types
`start work on <item> (status row <id>)` into the session for you.

## Build

```sh
cargo test --offline                                     # the view is a pure function; no wasm host needed
cargo run --offline --example peek                       # the fixture's view as JSON, to read what the host draws
cargo build --offline --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/plan_dashboard.wasm plugin.wasm
```

The build needs the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`)
and the rano SDK at the path in `Cargo.toml` — slice 1's SDK lives in a rano checkout, not on
crates.io. `plugin.wasm` in this directory is that build, committed so a project can copy a working
plugin without a Rust toolchain.

This crate is **not** a member of letibot's workspace (`exclude` in the root `Cargo.toml`, and its
own empty `[workspace]` here): it is a guest module for another host's ABI, so the workspace
neither builds nor gates it.

## How a project uses it

Once the host lands (`docs/plugins-slice1.md` C2/C4), a project-scope layer is a directory under
`.letibot/plugins/` holding the manifest, the wasm and its source:

```sh
mkdir -p <project>/.letibot/plugins/plan-dashboard
cp plugin.toml plugin.wasm <project>/.letibot/plugins/plan-dashboard/
mkdir -p <project>/.letibot/plugins/plan-dashboard/src
cp src/*.rs <project>/.letibot/plugins/plan-dashboard/src/   # so the gate can rebuild it, not just load it
cp status.example.json <project>/.letibot/status.json        # then make it true
```

The layer is discovered at project scope, its `plugin.toml` says `hooks = ["pane_dashboard"]`,
`capabilities = ["read"]`, `contract = 1`, and every change to it goes through the shadow gate.
Keep `.letibot/status.json` written by whoever moves the plan — an agent at the end of a tier, a
scheduler, a person — because a status file nobody rewrites is a dashboard that is quietly wrong,
which is exactly what the 24-hour finding is for.

`status.example.json` here is a realistic plan (four tiers — done, mixed, one blocked row, one
untouched — three stages, two agents, three hosts, one item of yours). Its `updated` is a fixed
date: it was fresh when it was written, so today it also trips the staleness finding, which is a
harmless way to see one.
