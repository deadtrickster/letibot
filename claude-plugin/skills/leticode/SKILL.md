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
  how a child is corrected — not by stopping it.
- `leticode_asks` then `leticode_answer {session, ask, option, note}`. Answer the gate's routine
  asks yourself; take a question that needs a person to the operator.
- `leticode_stop` interrupts, closes and detaches, and a placement settles as it closes — the
  branch comes home as `remote/<host>/<short id>`. `leticode_collect` fetches that branch when a
  stop never reached a close.

## Do not

- `leticode_attach` a session you did not start unless you mean to watch it (`watch_only`): a
  watching head cannot send, answer or stop, and that is the point of it.
- Land the child's branch yourself. A package's job ends at a commit on its branch; the queue and
  the operator take it from there.
- Start a second child for the same worktree: one daemon, one folder, and the queue's own rules
  are about branches you would otherwise collide on.
