# commands/*.md

A Markdown file becomes a slash command: `review.md` is `/review ARGS`. The daemon expands the
body and sends it as the operator's prompt. The commands are listed in `/help` and offered by Tab
with their description.

Where they are read, nearest first. The first scope that has a name wins.

1. `<project>/.letibot/commands/NAME.md`
2. `<project>/.claude/commands/NAME.md` (Claude Code's directory)
3. `$XDG_CONFIG_HOME/letibot/commands/NAME.md`, else `~/.config/letibot/commands/NAME.md`

A name a head or daemon verb already answers (`/help`, `/models`, ...) never reaches the file and
is left out of the listing. A `command_<verb>` plugin layer is asked before the file.

A subdirectory is a namespace: `commands/git/commit.md` is `/git/commit`, up to four levels.
Dot-directories and dotfiles are skipped.

## Placeholders

opencode's, in the body:

| placeholder | becomes |
|---|---|
| `$ARGUMENTS` | everything after the name, as typed |
| `$1` .. `$9` | one word each; `"quoted words"` are one word. The highest one used takes the rest of the line. A missing one is empty |

With no placeholder in the body, any arguments are appended after a blank line.

## Frontmatter

Optional, between `---` lines at the top.

| key | meaning |
|---|---|
| `description` | the line in `/help` and Tab. Default: the body's first line |
| `argument-hint` | shown before the description, e.g. `PATH` |
| `allowed-tools` | Claude Code's list (`Bash(git add:*), Read`, or a YAML list). The turn the command starts may call only these tools |
| `model` | the model that turn runs on, spelled as `/models` takes it (`deepseek/deepseek-chat`, `local`) |

Both hold for the one turn the command starts. The next turn is back on the session's model and
tools, and the session row is not changed.

`allowed-tools` names are Claude Code's or this tree's (`Read` and `read`, `WebFetch` and
`web_fetch`). `Bash(git add:*)` lets `bash` run `git add` with any arguments, `Bash(git status)`
that command only, and a held command may not chain (`&&`, `;`, `|`, redirects, `$(...)`). A
pattern on any other tool is ignored, so `Read(src/**)` is `read`. The tool schemas stay in the
prompt, so a call outside the list is refused and the prompt tells the model the list. The
session's own verbs (`reseat_self`, `compact_self`, ...) are never narrowed.

A `model` that does not resolve, or a fleet model (`[model."..."]`, whose switch re-renders the
conversation), is not applied, and the `command_file` row says why. The turn then runs on the
session's model.

A file whose frontmatter opens and never closes is refused with that reason. `/NAME` with no file
anywhere is refused naming the paths that were looked at.

```markdown
---
description: review a change
argument-hint: PATH
---
Review $ARGUMENTS. List the bugs first, then the style points.
```
