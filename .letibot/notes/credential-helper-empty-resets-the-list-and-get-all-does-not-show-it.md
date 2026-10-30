<!-- abstract: MEASURED 2026-10-30 on the Mac (git 2.54.0, Apple Git-157): `-c credential.helper=` really does clear the inherited helpers — but `git config --get-all credential.helper` still lists them, and a control run without the clearing word was answered by osxkeychain's token while the named helper was never called. -->

# `credential.helper=` clears the effective list, and `--get-all` cannot show that

MEASURED 2026-10-30 on the operator's Mac, git 2.54.0 (Apple Git-157), while making every GitHub
push name `!gh auth git-credential` (branch `agent/gh-pushes-github-when-authenticated`,
`crates/harnessd/src/mergequeue/git.rs`).

## The three runs, one repo, one url (`protocol=https`, `host=github.com`)

A temp repo with `credential.helper = <a stub script>` that answers `username=inherited` and
appends to a marker file:

| argv | who answered |
|---|---|
| `-c credential.helper= -c credential.helper='!f() { echo username=named; … }'` | **`named`**; the stub's marker file was never created |
| `-c credential.helper='!f() { echo username=named; … }'` (no clearing word) | **`osxkeychain`** — the operator's own account and token; the stub never ran |
| `-c credential.helper= -c credential.helper='!gh auth git-credential'` | `gh`'s helper: `username=deadtrickster` |

So the empty value is honoured, and without it an inherited helper answers FIRST and the named one
is never asked. That is the whole reason the argv carries both words.

## The rake: `git config --get-all` says the opposite

```
$ git -c credential.helper= -c credential.helper='!gh auth git-credential' config --get-all credential.helper
osxkeychain
<the stub's path>

!gh auth git-credential
```

The empty value appears as an EMPTY LINE in the raw list and the earlier entries are still there —
`--get-all` prints the file's values, not the list git's credential machinery ends up with. A
reader who checks the clearing this way concludes it did nothing. `git credential fill` with a
redacted `password` line is the measurement that answers the question.

## And the trap under it

`credential fill` prints the real token on stdout. The control run above put the operator's GitHub
token in this session's transcript once; from then on the run was
`| sed -E 's/^password=.*/password=<redacted>/'`. Never call `credential fill` without it.

## The push argv it justifies

`git -c credential.helper= -c credential.helper='!gh auth git-credential' push --dry-run
https://github.com/deadtrickster/letibot.git <sha>:refs/heads/<probe>` — exit 0,
`* [new branch]`, and `ls-remote` for that ref answers empty afterwards, so a dry run really wrote
nothing.
