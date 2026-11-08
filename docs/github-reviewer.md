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

## The seat lives in the daemon

`letibot attach github <scope>` does not poll anything itself: it asks the folder's daemon to hold
the scope and prints what the daemon holds. The seat - the scopes, the rows, the drafts, the
snapshot - is the daemon's (`crates/harnessd/src/github.rs`), for the reason the pane exists: a pane
is drawn from the daemon's own snapshot, so a seat in a terminal would be one the pane could not
see. A daemon that is not listening is refused by name; start `letibot` in that folder first.

The daemon polls a held scope every 60 seconds on a clock of its own, and an attach asks for a pass
straight away. One pass at a time: a pass still running means the next tick is skipped, not queued.

## A mirror per repository, and one tree per pull request

- **Every repository the scope names gets its own bare mirror**, under
  `~/.local/share/letibot/github/<folder key>/<owner>-<name>.git`, cloned the first time and fetched
  after. A pull request of an org's second repository is therefore reviewed from THAT repository's
  tree, which is what makes an org scope useful: ten repositories are one seat, not ten checkouts.
  A repository this folder is itself a checkout of is cloned from its own `origin` - the remote the
  operator's credential is set up for; any other is cloned from `https://github.com/<owner>/<name>`.
- **Each pull request's tree is a detached worktree of its repository's mirror**, at GitHub's
  `refs/pull/<n>/head` (the same ref for a fork's pull request), under
  `<owner>-<name>-<number>`. One tree per pull request, so a re-review lands where the last one was
  made rather than in a second copy of it.

## The review is the queue's review panel

A row owed a review is handed to a panel: every member judges the pull request and answers the merge
queue's own verdict - `accept`, `reject` or `needs_human`, with its reasons and what it looked at -
and the gatekeeper's rule turns those verdicts into one decision. One refusal is a refusal, one
undecided member asks a person, and only a panel that every member accepted accepts. A member that
cannot judge refuses the whole panel: half a panel is a review nobody configured. A panel with no
member is a refusal, never a pass.

The verdict becomes **one GitHub review**, and its event is always `COMMENT`: an unattended reviewer
neither approves nor blocks, so the decision reaches the author as words. The body opens with
`verdict: reject` (or `accept`, or `needs_human`), carries each member's reasons attributed to it,
and names what the panel read and ran; a reason spelled `path:LINE: what is wrong` is also an inline
comment on that line.

**This build's member kind is R116's headless `claude -p` run**, configured the way the merge queue
configures it - `[queue.claude] binary` in the project's `leticode.toml`, which is the operator's
opt-in and needs no second word. Its `structured_output` is the queue's verdict, its prompt is the
queue's own review prompt told which door the verdict goes through, and its conversation is kept so
a re-review resumes it. A project with no `[queue.claude]` has a panel with no member, so its rows
say exactly that and stay owed; a letibot-session member is not in this build's panel yet.

**A review that could not be made stays owed.** A member that fails, a run that ends without a
verdict, or a rejection of the scope itself leaves the reason on the row and the review owed again
on the next pass.

## What it offers

R114's scorer, over the tree's lexical arithmetic (`letibot_tools::similarity`) and nothing else:

- **a pull request whose words are like an open todo row or a running session's brief** is offered
  as one to watch - `watch this: the notes pane loses its scroll position`;
- **a review this seat made before** on a change like this one is offered as context.

The candidates are read at every pass, so a todo closed and a session ended stop being offered; the
row's own drafts are never offered back to it. The score sits in the row's detail, not on the line a
person scans. Offers are additive: a row nobody has anything like carries none at all.

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

The pane draws the daemon's snapshot of the seat, built by the daemon from the same document
`letibot attach github` prints. A daemon nobody has attached a scope on is handed no `github` key at
all, so the pane says the honest thing instead of drawing an empty scope:

```text
no scope is attached - nothing is being watched for review
```

`PluginHost::with_github` is where the key comes from: the daemon installs one seat on its registry,
every session's host is handed it, and the pane declares `feeds = ["github"]` as it always did. One
snapshot, so the terminal and the pane cannot disagree about what is held.

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
- No daemon listening for the folder: refused before anything is asked, naming the socket and the
  `letibot` to start there, because the seat is the daemon's and a verb that started its own daemon
  would be choosing the flags of the one the operator runs.
- A daemon that was given no seat (an older build) refuses the frame by name: `this daemon holds no
  GitHub reviewer seat`.
- A daemon with no way to ask GitHub - no `gh` and no panel - says so on every scope it holds (the
  `failed` state) rather than on every row.
- A pull request of a repository no mirror can be made for: the clone's own words are the row's
  reason, and the row stays owed.
- A panel member that fails, a `claude -p` run that ends without `structured_output`, or a panel with
  no member at all: the reason is written on the row, and the review is owed again on the next pass.

## Turning it off

Nothing to stop: the seat goes when the daemon does. It keeps no store of its own, so a restarted
daemon begins with an empty seat and re-learns the scope from GitHub on its first pass, which costs a
review of every open pull request's current head. A scope is attached again with `letibot attach
github <scope>`.
