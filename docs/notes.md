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

## Moving in-tree notes

`scripts/migrate-notes.sh PROJECT [--repo DIR] [--remote URL] [--push]` copies
`PROJECT/.letibot/notes/*.md`, on disk and tracked at HEAD, into the checkout under the project's
key and commits them. A note of the same name with different text is reported and not copied.
The project tree is not changed; remove the old directory from it afterwards.

## Readers

The prompt section, the `/standing` pane, the notes offer and the notes keeper all read the
project's directory in the checkout. A file left in a tree's `.letibot/notes/` is read by nobody,
and `write` and `edit` refuse a path into it.
