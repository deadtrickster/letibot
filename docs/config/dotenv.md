# `.env` files

A project's `.env` files are untracked, so nothing carries them: not a package worktree, not a
remote host, not a guest. letibot carries the ones the operator names and loads them into the
commands it runs. Configured by the `[dotenv]` table of
[`leticode.toml`](leticode.toml.md#dotenv):

```toml
[dotenv]
load = true              # a command starts with the tree's .env and .env.local
carry_to = ["xmg"]       # remote placements that carry the files; empty by default
carry_to_vm = false      # and a firecode VM on one of those hosts needs this too
```

## Which files

`.env`, `.env.local`, `.env.<name>`, `.env.<name>.local` — in the checkout's root only, and only
the ones git ignores or does not track. A tracked one already travels with the tree. `.envrc` is
never one of them: direnv executes it, and a file another program runs is not a variable list.

`carry_to` is empty by default and `carry_to_vm` is false, because these files hold credentials. A
host is carried to because the operator wrote its name; a VM on that host needs the second switch
as well. A file that is carried is written at **0600**, its **name** is what any line about it says,
and one git does not already ignore is added to that worktree's `.git/info/exclude` — a carried
credential is never a commit, and nothing is ever collected back.

A firecode VM's copy (`--vm` without a declared host) keeps the files only when `carry_to_vm` is
true; otherwise they are **removed** from the copy. A folder mounted read-write (`--mount-rw`) is
the operator's own tree and is never touched.

## What a command sees

`bash` and the background jobs it starts load `.env`, then `.env.local` (later file wins) before
they run, and re-read them when either file's mtime changes — no restart. Precedence is docker
compose's:

1. a pair the call itself named,
2. a pair letibot set (its own environment, a `[boundary] env`, a perimeter's),
3. the file.

Nothing in a file is executed: `$(…)` and backticks are data. The grammar is dotenv's basics —
`KEY=value`, an optional `export `, `#` at the start of a line, blank lines, `'single'` quoted
values literally, `"double"` quoted with `\n`, `\t`, `\r`, `\"`, `\\` and `\$`, and `${VAR}`
expanded from earlier keys or the daemon's environment (an unset name is empty). A line that is not
a `KEY=value` is skipped with one note naming its **line number** and never its content.

Every loaded value of eight bytes or more joins the session's secret list, so it is masked in a
tool's output and reaches no transcript row, store row or log. That is a redaction, not a
boundary: the file still holds the bytes.

On a remote host, the pairs ride the descriptor a secret rides (never an argv), and only for a host
in `carry_to`. A firecode VM there gets the file but not the pairs — `firecode in` carries one
command string and no descriptor, so a value could only reach it on an argv the host's `ps` reads.

Reading or editing a `.env` is not restricted by this feature: the gate treats it as any other file
in the project.
