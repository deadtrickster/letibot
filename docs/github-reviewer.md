# The GitHub reviewer seat

`letibot attach github <scope>` is a long-lived process that watches one GitHub scope and keeps a row
per open pull request: what is new, what a push made owed again, which comment mentioned it. It is a
poll rather than a webhook, because a webhook is an endpoint exposed on this box and a poll is not.

It is a seat of drafts. Nothing is sent to GitHub until the operator has an identity named `letibot`
and says so in config, so a review is written into that pull request's session, kept on the row as a
draft, and read by a person before anything leaves the box.

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

## The worker: one session per pull request

A row owed a review is a brief run in that pull request's own letibot session, and what the session
answers is the review the row keeps as a draft.

- **The session is opened once and reused**, so the second review of a change is made by the session
  that made the first: a push's re-review knows what the last one said. Enter on a row in the pane
  goes to it, and the operator attaches to it like any other session.
- **Its tree is the pull request's head**, fetched from `origin` as GitHub's `refs/pull/<n>/head` (the
  same ref for a fork's pull request) and checked out detached under
  `~/.local/share/letibot/github/<folder key>/<owner>-<name>-<number>`. One tree per pull request, so
  a re-review lands where the last one was made.
- **The sessions are the folder's daemon's**, which must be up: a review runs in a session, and a seat
  that started its own daemon would be choosing the flags of the one the operator runs. Started where
  no daemon is listening, the seat refuses by name and says to start one there first.
- **The brief carries what a reviewer needs**: the title and the description its author wrote, the
  diff (the whole pull request, or only the commits after the head last reviewed), and what has
  already been said on it. A fork's brief says its tree is somebody else's and that nothing of it runs.
- **The answer is the review**: the prose is the summary, and each line spelled
  `path:LINE: what is wrong` is an inline comment on that line. The verdict is always `COMMENT` - an
  unattended reviewer neither approves nor blocks - and an answer with no words in it is not a review,
  so the row goes back to owed rather than keeping an empty draft.
- **A review that could not be made stays owed.** A session that fails, that asks a permission card
  (a seat answers no cards), or that is still working after twenty minutes leaves the reason on the
  row and the review owed again on the next pass.

The seat reviews a pull request of **the repository it was started in**. A pull request of another
repository in the scope is refused by name rather than reviewed from the wrong tree; ten repositories
of an org are ten seats, one per checkout, until a mirror per repository exists.

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

**A draft is drawn, not counted.** Under a row that is owed nothing there is one faint line per draft -
`draft review of c1d2e3f: This loads the widget once now.` or `draft reply to comment 7: Because …` - and
the whole text is in the row's detail, because the decision a draft asks for is about those words. A
row with no draft draws no such line.

The pane draws the daemon's snapshot of the seat, and this build's daemon does not build that feed yet,
so the pane is handed no `github` key at all and says the honest thing instead of drawing an empty
scope:

```text
no scope is attached - nothing is being watched for review
```

Until the seat lives in the daemon, `letibot attach github` in a terminal is what shows the seat
working: one line per thing it did, printed as the pass does it.

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
- No daemon listening for the folder the seat was started in: refused before anything is polled, naming
  the folder, because a review runs in a session and the seat will not start a daemon of its own.
- A pull request of a repository this checkout is not: the row says so and stays unread, rather than
  being reviewed from the wrong tree.
- A session that answers nothing, fails, or asks a card: the reason is written on the row, and the
  review is owed again on the next pass.

## Turning it off

Stop the process (ctrl-c in its terminal). The seat keeps no store: what it knew about a pull request is
a row in memory, so a restart begins with an empty seat and re-learns the scope from GitHub on its first
pass, which costs a review of every open pull request's current head.
