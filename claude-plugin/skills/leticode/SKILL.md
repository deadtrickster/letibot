---
name: leticode
description: Hand well-scoped work to a leticode session and follow it from here. Use when a task is clear enough to brief in one message and does not need this session's model, or when something long has to run while this conversation keeps going.
when_to_use: the operator says "delegate this", "run it on leticode", "start a child", "package it out"; a test suite or a build would block this session; the work belongs in its own worktree and branch.
---

# leticode children

A leticode session is a real session in a real worktree, on the model you name. The bridge holds
it as a head, so it survives this turn ending and its asks come to you rather than to a person.
You are the orchestrator: it does the work, you decide.

## Start one

`leticode_start {worktree, brief, model?, mode?, host?, vm?}` answers the session id every other
tool takes. The brief is the whole of what the child knows — it sees no part of this conversation —
so it carries: what to build, the files that matter, the hard rules (own worktree, no push, no
branch changes), the proof (`cargo test -p <crate>`, `sh scripts/check-fmt.sh <base>`, shellcheck
and shfmt on any shell it writes), and a closing report of what it built, tested, could not and
committed. `mode` defaults to `writes-allowed`: writes go through, commands and the network ask
you. `host` places its tools on another machine; `vm` runs them in a firecode guest.

## Wait for it

Do not poll. With the channel loaded the session's `turn_ended`, `ask`, `failed` and `trouble`
arrive as `<channel>` events on their own, and `idle_with_work` says it is between turns with jobs
or rows still open. Without the channel, run the wait as a background command — its exit is the
notification:

    letibot-mcp wait <worktree> <session> --until attention --timeout 2h

0 turn ended (the last assistant message on stdout), 10 an ask, 20 failed, 30 timeout, 40 the
provider is in trouble.

## Read it

- `leticode_status` — state, the last message, todos, `pending_asks`, `last_trouble`, and `report`
  parsed into `built` / `tested` / `could_not` / `commits` when the last message is the closing
  report.
- `leticode_events` — the progress since your cursor: calls, jobs, failures, asks, turns, subagents.
- `leticode_transcript` — the conversation, row by row, cursor `i`.
- `leticode_diff` — the branch against its merge base.

## Steer, answer, end

- `leticode_send` queues a line behind the running turn; the daemon starts a turn for it. That is
  how a child is corrected — not by stopping it. The line is stored under this seat's handle, never
  as the operator's words.
- `leticode_todo {session, rows}` writes rows on the board of a session you drive, under this
  seat's handle, in the order given. Its model is told at its next turn. Refused on a watch.
- Talk to a session through the bridge or the room, never by typing into a head over tmux. A head
  cannot tell that from the operator typing.
- `leticode_asks` then `leticode_answer {session, ask, option, note}`. Answer the gate's routine
  asks yourself; take a question that needs a person to the operator.
- `leticode_stop` interrupts, closes and detaches, and a placement settles as it closes — the
  branch comes home as `remote/<host>/<short id>`. `leticode_collect` fetches that branch when a
  stop never reached a close.

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

`queue {action: "status", id}` reads the row at any time. `leticode_queue_verb` has the
operator's `/queue` verbs (`approve`, `restart`, `veto`, `rm`, `reset`, `clean`, `regate`),
recorded under this bridge's handle. They overrule the reviewer or the gate, so use them only when
the operator asked.

## Do not

- `leticode_attach` a session you did not start unless you mean to watch it (`watch_only`): a
  watching head cannot send, answer or stop, and that is the point of it.
- Merge or push the target yourself. The queue lands the branch, and the operator rules on what
  it parks.
- Start a second child for the same worktree: one daemon, one folder, and the queue's own rules
  are about branches you would otherwise collide on.
