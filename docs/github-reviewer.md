# The GitHub reviewer seat

`letibot attach github <scope>` is a long-lived process that watches one GitHub scope and keeps a row
per open pull request: what is new, what a push made owed again, which comment mentioned it. It is a
poll rather than a webhook, because a webhook is an endpoint exposed on this box and a poll is not.

It is a seat of drafts. Nothing is sent to GitHub until the operator has an identity named `letibot`
and says so in config, and this build has no reviewer worker yet, so a row says what it is waiting for
instead of the run looking idle.

```sh
letibot attach github stroppy-io            # every repository of an org
letibot attach github stroppy-io/letibot    # one repository
```

```text
the reviewer seat on stroppy-io/letibot
  drafting: nothing is sent to GitHub (`[github] post` and a token turn it on)
  polling every 60s
```

## What it does with a pull request

One pass every 60 seconds, over everything the scope lists as open:

| what happened | what the row says |
|---|---|
| a new pull request | queued for a review, and the run prints that it is new |
| a push moved the head | owed a re-review of the new commits only, measured against the head last reviewed |
| a comment that mentions the seat's identity | owed an answer, once per comment, and only comments newer than the row and newer than the last one answered |
| the pull request is no longer open | the row is dropped |
| a review could not be made | the row goes back to queued with the reason on it, so the next pass tries again rather than losing the push |

Nothing runs a pull request's code. A review is handed a diff, and a pull request whose head is in
another repository (a fork) is read-only, so a reviewer that would run something in it is the defect the
standing exists to make impossible. Nothing in this seat ever posts as the operator: the seat has its
own identity, `letibot`, and that name is also what a comment must mention to be a question for it.

## What this build does not do yet

There is no reviewer worker. `letibot attach github` wires the seat to a reviewer that refuses every
job by name, so a pull request is never actually reviewed, and the row says so:

```text
stroppy-io/letibot#12 was not reviewed: stroppy-io/letibot#12 is this repository's and the reviewer
worker does not open sessions yet
```

What the seat does give is the state machine around a review: the scope's state, one row per pull
request with its review state and the age of its last action, the drafts, and a rate cap. When a worker
is seated, the rows do not change shape.

## Configuration

```toml
# ~/.config/letibot/leticode.toml
[github]
post = true
token_path = "~/.config/letibot/github.token"
```

| key | default | |
|---|---|---|
| `post` | `false` | `true` sends reviews and answers to GitHub. Without it everything is kept as a draft on the row |
| `token_path` | none | the token file. `~` is the operator's home. Must be mode `0600`, and `posting` needs it |

**Nothing leaves the box until both are set.** `post` is what the operator means and the token is what
makes it possible, so a seat told to post without a token says *token missing* on the scope rather than
failing on every push.

The token file is checked here for being there and for its mode, and its contents are never read: `gh`
is what holds and sends the credential, authenticated the way the operator authenticated it.

**A project file may stop the seat and may not start it.** `post = false` in a project's
`leticode.toml` wins over the operator's `true`, because a project file is one a session with `write`
can edit, and starting the seat would post under the operator's identity on a stranger's repository.

Two numbers are fixed in this build and not read from a file: a repository may get 6 reviews an hour
(0 would mean no cap), and the identity is `letibot`.

## Watching it

`/pane github/github` is the seat's pane: one line per scope with its state, then one row per open pull
request with its review state, the age of its last action and the session working it, and a line saying
whether anything is posted or kept as a draft. Enter on a pull request switches to that session; a row
without one carries no act at all.

The pane draws the daemon's snapshot of the seat, and this build's daemon does not build that feed yet,
so the pane is handed no `github` key at all and says the honest thing instead of drawing an empty
scope:

```text
no scope is attached - nothing is being watched for review
```

While the worker is missing, `letibot attach github` in a terminal is what shows the seat working: one
line per thing it did, printed as the pass does it.

## The scope's state, in the pane's words

| word | what it means |
|---|---|
| `polling` | asking GitHub, and the last answer came back |
| `rate-limited` | the hourly cap for that repository is spent; rows behind it stay queued |
| `token missing` | posting is on and no token file is there, so nothing is attempted |
| `paused` | the operator stopped asking about this scope; the rows stay as they are |
| `failed` | the last poll was refused, and the scope's line carries GitHub's or `gh`'s own words |

## What it says when it fails

- No scope: `attach takes a scope: \`letibot attach github <org|owner/repo>\``, exit code 2.
- Another source: `attach knows one source and it is \`github\`, not \`gitlab\` (\`letibot attach
  github <org|owner/repo>\`)`.
- A scope that is neither an org nor `owner/name`: it is refused by name and by part count, because a
  scope guessed wrong is a seat that reviews somebody else's repositories.
- `post = true` with no `token_path`: refused before anything is polled, because a review posted as
  nobody would be a stranger's comment on the operator's repository.
- A token file that is not there, or is not `0600`: refused with the path and the `chmod` to run.
- A key this build does not know inside `[github]` is refused at load, by name.
- `gh` itself: `LETIBOT_GH` names the binary, and a run that fails reports the command and `gh`'s own
  stderr. Nothing is retried behind the operator's back; the row is owed again on the next pass.

## Turning it off

Stop the process (ctrl-c in its terminal). The seat keeps no store: what it knew about a pull request is
a row in memory, so a restart begins with an empty seat and re-learns the scope from GitHub on its first
pass, which costs a review of every open pull request's current head.
