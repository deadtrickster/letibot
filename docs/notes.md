# Notes

Standing notes are markdown files the harness reads into the system prompt and re-reads after
every compaction. Sessions write them with the `notes` tool. They are not kept in the project tree.

## Where they live

One git checkout holds the notes of every project, one subdirectory per project:

```
~/.local/share/letibot/notes/          the checkout ($XDG_DATA_HOME/letibot/notes)
  letibot-2c6a5ebf04bb/                one project
    a-note.md
```

The subdirectory is `<name>-<12 hex>`, keyed by the project's normalized `origin` URL, so every
clone and worktree of a project on every box shares one directory. A repository with no `origin`
is keyed by its main checkout's path. `letibot notes path` prints both for the current folder.

Every write the `notes` tool makes is a commit in the checkout. The checkout is made a repository
on the first write. Nothing pushes implicitly.

## Configuration

`~/.config/letibot/leticode.toml`:

```toml
[notes]
repo = "~/.local/share/letibot/notes"
remote = "https://github.com/letiflow/letibot-notes.git"
```

The user file wins over a project's `leticode.toml`, since a project file is writable by its own
sessions. `$LETIBOT_NOTES_REPO` overrides both; tests that spawn a daemon set it.

`letibot notes push` fetches `origin`, rebases onto `origin/main` and pushes `main`. A conflicting
rebase is aborted and reported.

## Shadow proposals

The shadow's `turn_end` job may propose a note: a row in the watched session quoting the operator
or the model, with its author and row. `/note accept N` writes it through the `notes` tool, under
a `shadow-<first words>` name, with a `Source:` line naming the author, the quoted row's time, the
session and row, and the shadow session. `/note decline N` writes nothing. Without N both act on
the newest proposal. The model's seat need not hold `notes`: the accept is the operator's.

A proposal that only says again what the turn's last answer said is not offered at all: 17 of the
proposals this job made were the answer restated, which is a card the operator has just read. The
shadow's own record says when one was dropped.

## The turn monitor

The same `turn_end` call also reads the turn for what it did beyond the ask - a test emptied or
skipped, a file written outside the worktree, a force push, a branch deleted, a different task
than the one asked for, or an answer that claims work the turn did not do. A finding is ONE row in
the watched session, `[monitor] beyond the ask: …`, which the operator sees in the head and the
model meets in its context. It never blocks, pauses or asks: the session is told, not stopped.
Nothing is said when nothing was found, which is the normal case. The rule half needs no model, so
a finding stands even when the reply fails; `[shadow] turn_monitor` and `turn_monitor_max_chars`
switch it and cap the digest the check is shown (`docs/config/providers.toml.md`).

## Moving in-tree notes

`scripts/migrate-notes.sh PROJECT [--repo DIR] [--remote URL] [--push]` copies
`PROJECT/.letibot/notes/*.md`, on disk and tracked at HEAD, into the checkout under the project's
key and commits them. A note of the same name with different text is reported and not copied.
The project tree is not changed; remove the old directory from it afterwards.

## Readers

The prompt section, the `/standing` pane, the notes offer and the notes keeper all read the
project's directory in the checkout. A file left in a tree's `.letibot/notes/` is read by nobody,
and `write` and `edit` refuse a path into it.
