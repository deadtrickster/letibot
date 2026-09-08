# Decisions

Answers to the blocking questions in `docs/workstreams.md`. One entry per decision,
with the date it was settled and what it unblocks. An entry here is the operator's
call; the reasoning under it is mine and may be wrong.

Open decisions stay in this file marked **OPEN** rather than living only in a chat
log, because the workstreams document is written against them.

---

## D1 — where the code lives — **SETTLED 2026-09-09**

`~/Projects/letibot`, a public GitHub project, separate from the llama.cpp fork.

Unblocks: the first commit, and every strand's file layout.

Note this is *not* a retreat from "harness and server fused to the 11" (design brief
§3). Fusion is about the seam being a private binary control channel and one release
train — not about one git repository. The server track is off M1's critical path
anyway (see below), so a separate repo costs nothing today and the coupling that
matters is enforced by the contract tests, not by directory adjacency.

## D2 — which flowy reader harnessd may hold — **SETTLED 2026-09-09**

Reuse the `claude-lab2x1` seat for tests, so the operator's interactive session sees
the whole chain end to end rather than a mock.

**The constraint this runs into, which the design must respect.** flowy allows many
*writers* under one name but exactly one *reader*: `waiterlock.go` writes a claim and
checks `kill -0`, and a second waiter on the same name is refused by construction —
`inbox.go:132-137` says "IT IS ONE FLAG BECAUSE IT HAS TO BE". The seat brief states
the consequence in fleet terms: two processes listening under one name means the
roster shows that seat attached while the real one hears nothing, "a lie the whole
fleet then acts on".

So:

| operation | shareable? |
|---|---|
| writing as `claude-lab2x1` (`flowy say`) | yes — token-based, no exclusivity |
| holding the inbox reader | **no** — one waiter, enforced |

During a test run, harnessd takes the reader and the interactive session drops its
`flowy-listen-loop` Monitor. That is the right way round for the stated goal: it is
harnessd's own delivery path being exercised, not a second listener watching it.

The two must not overlap. A test harness that starts harnessd without stopping the
Monitor gets `LISTENER REFUSED`, which is the correct outcome and should be asserted
rather than worked around.

## D4 — flowy's licence — **SETTLED 2026-09-09**

MIT. flowy's delivery tests may be ported (`docs/workstreams.md` W2/W12, and the
list in the flowy delivery survey). Attribution retained.

Contrast `oh-my-openagent`, which is under a Sustainable Use Licence: **no code from
it, ported or adapted.** Its ideas may inform a design; its source may not be copied.

## D3 — firecode's two asks — **OPEN**

Restated in plain terms, because the original phrasing was too compressed. Both are
requests against the operator's own firecode repo, so the question is "will these be
added", and **a written "no" is a complete answer** — it is not a blocker, it decides
which of two tool runtimes gets built.

### Ask 1 — a parent cgroup for a host-side caller

firecode's central invariant is that a run's lifetime is a **cgroup subtree**, not a
process tree: `cgroup.kill` ends a run and every VM started on its behalf,
transitively, and liveness is "is the cgroup populated".

That works when a run is spawned from inside a guest. `harnessd` runs on the **host**,
and a run spawned by a host caller has no parent cgroup — so killing a harnessd
session does *not* reap the runs it started. Orphaned microVMs survive their session.

- **If yes** (`--parent-cgroup <path>` on the spawn/`up` paths, or a cgroup firecode
  accepts as a parent): harnessd inherits the invariant for free. Session teardown is
  one `cgroup.kill`.
- **If no**: harnessd tracks every run it started and reaps them itself — which means
  reimplementing transitive teardown, including runs that started further runs, and
  getting it wrong leaves VMs holding memory after a crash.

### Ask 2 — a persistent shell channel

`firecode in` is one command, its output, its exit status. That is the right
primitive and it is not what an agent's `bash` tool needs. A tool call expects a
**session**: `cd` persists to the next call, `export` persists, `&` background jobs
keep running.

- **If yes** (`firecode shell --attach` with a session id): the tool runtime is thin —
  pass the call through to a real shell.
- **If no**: harnessd synthesises the session itself — tracks cwd and environment,
  prefixes every command, and documents that background jobs do not survive a call.
  That is a real component with its own failure modes, and the worst of them is quiet:
  a `cd` that does not stick produces a tool that works in every test and fails on the
  second command of a real task.

**Why these are two tool runtimes and not two settings:** the yes-branch is a thin
adapter over firecode; the no-branch is a shell-session emulator plus a lifetime
manager. They share an interface and almost no code. Deciding late means building one
and discarding it.

## D5 — Falsifier B scoring rubric — **OPEN**

Depths are specified; the rubric is not, and §9's position rests on it.

## D6 — `max_inline_bytes` — **OPEN**

Deliberately has no default in the plan.

## D7 — the 27B preset — **OPEN**

## D8 — harness licence — **OPEN**

Blocks nothing today, but the repo is public, so it should not stay open long.
