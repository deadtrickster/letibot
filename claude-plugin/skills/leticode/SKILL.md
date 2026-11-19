---
name: leticode
description: Hand well-scoped work to a leticode session and follow it from here. Use when a task is clear enough to brief in one message and does not need this session's model, or when something long has to run while this conversation keeps going.
when_to_use: the operator says "delegate this", "run it on leticode", "start a child", "package it out"; a test suite or a build would block this session; the work belongs in its own worktree and branch.
---

# leticode children

The rules of a session, a seat's board and the queue are the server's own: the bridge hands them
at `initialize`, `letibot://leticode` is the contract behind them (`docs/leticode.md`), and each
tool's description says what it does in one line. This skill is the workflow around them.

You are the orchestrator: the child does the work, you decide.

## Start one

`leticode_start {worktree, brief, model?, mode?, host?, vm?}` answers the session id every other
tool takes. The brief is the whole of what the child knows (it sees no part of this conversation),
so it carries: what to build, the files that matter, the hard rules (own worktree, no push, no
branch changes), the proof (`cargo test -p <crate>`, `sh scripts/check-fmt.sh <base>`, shellcheck
and shfmt on any shell it writes), and a closing report of what it built, tested, could not and
committed.

## Wait for it

Do not poll. With the channel loaded the session's `turn_ended`, `ask`, `failed` and `trouble`
arrive as `<channel>` events on their own. Without the channel, run the wait as a background
command, whose exit is the notification:

    letibot-mcp wait <worktree> <session> --until attention --timeout 2h

0 turn ended (the last assistant message on stdout), 10 an ask, 20 failed, 30 timeout, 40 the
provider is in trouble.

## Read it

`leticode_status`, `leticode_events`, `leticode_transcript` and `leticode_diff` are the four
doors, from the cheapest to the most expensive: the state, the progress since your cursor, the
conversation row by row, and the branch against its merge base. Read the status before the
transcript; the transcript is for what the other three cannot say.

## Steer, answer, end

- `leticode_send` corrects a child; stopping it does not. Its line waits behind the running turn.
- `leticode_asks` then `leticode_answer`: answer the gate's routine asks yourself, and take a
  question that needs a person to the operator.
- Talk to a session through the bridge or the room, never by typing into a head over tmux. A head
  cannot tell that from the operator typing.
- `leticode_stop` ends it and settles its placement; `leticode_collect` is the way back for work a
  close never reached.

## Land it

The child's job ends at a commit. Landing is yours, through the queue, never by hand:

1. `git push origin <branch>` - the branch only, never the target.
2. `queue {action: "enqueue", branch, why}` answers the row `id`. The gatekeeper reviews the branch
   against `why`, then the queue rebases it with the other approved branches, gates the stack once
   and lands it. Never run the workspace gate yourself.
3. Wait as a background command, whose exit is the notification:

       letibot-mcp wait-queue <repository> <id> --timeout 2h

   0 landed, 10 rejected, undecided, vetoed or withdrawn, 20 the gate was red or the queue could
   not run it, 30 timeout. stdout is the row's state, the reviewers' `reasons` in it. On 10 send the
   reasons to the child with `leticode_send`, then push its fix and enqueue again.

`leticode_queue_verb` has the operator's `/queue` verbs. They overrule the reviewer or the gate, so
use them only when the operator asked.

## Do not

- Merge or push the target yourself. The queue lands the branch, and the operator rules on what
  it parks.
- Start a second child for the same worktree: one daemon, one folder, and the queue's own rules
  are about branches you would otherwise collide on.
