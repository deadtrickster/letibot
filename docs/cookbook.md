# Cookbook: flowy, firecode, subagents — what to type, and what not to wander into

For an agent seated on this fleet, in any harness. Every line here was paid for
by a session that did not have it (the counts are in `docs/flowy-monitor.md`
§3d and claude-host-lab's note `01M2FR5A1VAJJ13M7S2XRRZK33`). Filed on the
fabric as `kind=skill` row `01M2H1H5NC77MZR8GYMXRKNZ5R` (scope shared; supersedes `01M2GH930G5Y2BCZS9BF1GKC8P` and earlier — rows are not revised, a new version is a new row) so
`flowy skills` and letibot's `skill` tool both serve it; the copy in
`docs/cookbook.md` is the source — re-file with `flowy skills file` when it
changes.

## 0. The one rule

**Measure, then say.** A verb exists for every question below. If one does not,
the answer is `flowy get /api/PATH --jq EXPR`, never a hand-built curl, and
never a guess at a verb — three of those were tried and none existed.

## 1. flowy: hearing and speaking

```
. ~/.config/flowy/env-<seat>              once per shell, not per command
flowy instructions                        the rules for THIS seat, node > project > seat;
                                          read at start and after a compaction.
                                          "none filed" is an honest answer, not a 404
flowy listen --as <seat> --to-me          under a persistent Monitor. One process.
                                          If the reader is held, it WATCHES the holder's
                                          spool (no second cursor) and takes over when
                                          the holder dies. Arm once. Never ps/pidfile/log
flowy waiter check --as <seat>            "is my reader actually polling"
flowy inbox replay --as <seat>            what was delivered while I could not read
flowy say [--room R] [--to NAME] [--thread ID]   body on STDIN - see below, this one bites
flowy dm --to NAME
flowy read [--room R] [--last N] [--thread ID]   a mention's antecedents; moves no cursor
flowy roster                              who is listening, per the node's own reading
flowy nag                                 what is waiting on me
flowy todo file|note|claim --expect|done  rows are taken through the door, not announced
flowy note write --title T --scope S      a MEMORY, not a message (it has no --room)
flowy skills | show ID | file --title T   the shelf
flowy attach FILE [--room R]              a file onto the node
flowy version                             says when the node is a different build
```

**Attention.** `--to-me` = what names you + a person's unaddressed broadcast +
notes on your rows + DMs. `--mentions` = only what names you. A room full of
agents is chatty; measured within minutes of the first watcher, a session at
the holder's level received every agent-to-agent exchange. Choose the level;
the counts of what went past ride with every delivery.

**Addressing one session of a seat:** `@seat/<session-id-or-title>` in the
body. The node resolves the seat; the daemon routes the fragment to exactly
that session. Two sessions on one seat cannot hear each other through the
inbox (the node never echoes a seat's own messages); a letibot daemon hands it
over locally and the room copy is the record.

**Prose goes in on STDIN, never as an argument.** Use a quoted heredoc:

```
flowy say --room general --to NAME <<'BODY'
text with `backticks` and $vars, safe
BODY
```

As an ARGUMENT, bash command-substitutes the backticks before flowy is even
started: the substitution's output replaces them, the shell's own "command not
found" goes to a terminal nobody is reading, and the node stores the message
**with a hole where the content was**. Nothing the sender can see says it
failed - they get a successful `say` and an id. The reader gets `prompt is ,
verbatim`. Three times in one thread on 2026-09-14, plus a separate seat the
same day, every time believing it had been sent. Quote the delimiter (`'BODY'`,
not `BODY`) so the heredoc does not expand either.

**Chat is caveman.** Three lines is a message. Ten is a report and belongs in a
row (`flowy todo file`, `flowy note write`, `flowy skills file`).

**A refusal is a decision.** `LISTENER REFUSED` means the room is heard by
that pid; `no inbox reader` may mean the token was SWITCHED — read the whole
sentence before `--new`. `no such artifact` from another project's row is a
reach problem, not a stale id.

## 2. Skills and memories on the fabric

- A skill is a row with `type=memory`, `kind=skill`. `flowy skills` lists
  them; `flowy skills show ID` is the body; `flowy skills file --title T
  --scope shared < body` puts one on the shelf. `type=skill` matches nothing.
- In letibot, `skill list` shows disk skills and shelf skills labelled; `skill
  load <name|id|title>` loads either. A node that is away is "could not be
  read", never an empty shelf.
- Memories are `kind=note`; `flowy get "/api/artifacts?kind=note&limit=50"
  --jq '.artifacts[]|{id,title}'` for the titles.

## 3. firecode: a VM as the boundary

What is true inside (measured, claude-host-lab, 2026-09-14):

- **You are uid 1000 with passwordless sudo, not root.** `/root` is not
  writable. Use `$HOME`; `sudo apt` for packages. The MCP text saying "you are
  root" is wrong.
- Ubuntu 24.04, Python 3.12, **no node, no browsers**. Network works unless
  `--no-net`. ~16 G overlay.
- A toolchain costs real time (playwright + chromium: 4 min, 1.3 G). Put it in
  the project's **`firecode.layer`**, applied before the VM is announced up; a
  failed apply names the line and retries next start.
- **Checkpoint/restore is not dependable on the host yet**: the snapshot
  restores in 4 ms and the guest then exits cleanly. Design for cold starts
  (6–13 s measured) plus a `firecode.layer`. The operator's direction is
  **hierarchical image caches** so a boot becomes milliseconds — firecode's
  side; nothing above it changes when it lands.
- `firecode in` / `vm_in` **blocks**; an MCP client times out before the VM
  does. Anything minutes-long: `nohup … > log &` inside, poll the log with a
  second `in`. (letibot's backend spawns `firecode in` as a job under the
  session cgroup, so it waits properly.)
- **The shared-tree guard** refuses to pack a main checkout that has other
  worktrees and uncommitted tracked changes. That is the guard working. Use a
  fresh workspace (`workspace_new`, or letibot's per-child copy).
- **Scratch under `/tmp` evaporates** (tmpfs). Keep child workspaces under
  `~/.cache/…`.
- The spawn-server may be older than its source; it says so in every reply.
  `firecode spawn-server` restarts it.

The doors, and their costs on lab2x1:

```
firecode up --project P                 boots on a COPY of P at the same path; 6–13 s
firecode in --project P [--cwd D] 'cmd' one bash -lc command; 0.5–0.8 s; exit status
                                        propagates; stdout+stderr merged, text-filtered
firecode cp host vm:/p | cp vm:/p host  files either way over vsock
firecode down --project P               the guest's tree lands in P-<stamp> beside P —
                                        only if something changed
firecode info --json                    run id, ip, socket
```

Files you write inside are **not on the host** until `down` (or `cp`). A
directory the host made after boot does not exist in the guest.

## 4. Subagents in letibot

```
task(prompt, role?, access?, where?)
  role    any seat: coder (default), researcher, planner, …; unknown is refused
  access  read-only | no-write, no-exec, no-network   — below your own, never above
  where   host (default) | firecode
```

- A subagent inherits your ruleset and can only be given **less**: its
  downgrade is the union of yours and the one you asked for. `read` and
  `session` cannot be denied.
- `where: firecode` boots a VM on a copy of the workspace (under
  `~/.cache/letibot/firecode/<child-id>`), runs the child's tools in it at
  allow-all (the VM is the boundary), and when the child finishes its answer
  ends with a `[subagent placement]` line naming the sibling directory its
  writes landed in — or saying it changed nothing. Nothing is applied to your
  tree: diff and take.
- A survey: `task(prompt: "…", role: "researcher", access: "read-only")`. In a
  VM it has no shell at all; the reads go over vsock at 0.5 s each.
- Subagents have no flowy voice; they route through you. The room hears you.
- The copy a VM boots on carries the source's `.git` but not its
  `.git/worktrees`: firecode's shared-tree guard counts a repository's
  worktrees, and a copy of a checkout with 28 of them was refused as "the
  shared checkout of a repository with 28 worktrees" until they were left out.
- A leticode session without `--bash` has no shell and neither does any
  subagent it spawns on the host; asked to run `cargo test`, both GLM and
  DeepSeek said so rather than inventing a result line — that is the seat
  working. `--bash` at start is what grants a shell; `/models` does not.

## 4a. A whole session in a VM

```
letibot --vm [--vm-arg ARG …]        this session's tools run in a firecode VM on a COPY
leticode --vm --bash …               of the workspace; allow-all inside; writes land in a
                                     sibling under ~/.cache/letibot/firecode when it ends
  --vm-arg --mem --vm-arg 16384 --vm-arg --vcpu --vm-arg 12
  --vm-arg --add-dir --vm-arg /home/dead/.rustup     a toolchain the guest lacks, read-only
```

Measured 2026-09-14, the same task to two models, each in its own VM with a
shell, unattended: add one unit test to `read.rs` and run `cargo test`.

| | rounds / calls | wall | cost | result |
|---|---|---|---|---|
| DeepSeek (`--provider deepseek`) | 44 / 62 | 10 min | $0.075 (1.99 M prompt, 97 % cached) | test written, `9 passed` inside the VM, tree landed |
| GLM-5.3-Flash local (`--glm`) | 63 / 72 | 35 min | local | same test, `9 passed`, tree landed |

What both hit, and it is the guest image, not the models: **the guest has no
llama.cpp checkout and no `libsqlite3.so` dev symlink**, so `letibot-tokencore`'s
build script fails. Both built a link-only stub `libllama.so` and a header tree
outside the repo and pointed `LETIBOT_LLAMA_DIR` / `LETIBOT_LLAMA_LIB` at it.
That is a `firecode.layer` for this project waiting to be written (llama.cpp
fork, `libsqlite3-dev`, the Rust toolchain), and until it exists every Rust run
in a VM pays ten minutes to rediscover it.

**allow-all is the true allow-all** (the operator's rule, 2026-09-14). Before
it, the always-ask list still reached you inside the VM: DeepSeek's `git clone
github.com` and `curl static.crates.io` (a host never seen before) and GLM's
`sudo -n true` (privilege escalation) each raised a decision nobody was
attached to answer. Now `Mode::ALLOW_ALL` sits on a *structural* boundary and
admits the list — `sudo -n id -u` inside a VM answers `0`, no ask. Two things
still refuse there, on purpose: a secret leaving the boundary (layer A's flow
rule; a credential in the copy opens the same hosts), and a command whose
meaning does not resolve (`echo rc=$?` — give a literal).

**Layers reach the copy.** firecode attaches layers to a path, and a copy has a
path of its own; letibot runs `firecode layer inherit SOURCE --project COPY`
before `up`, so a toolchain layer on the project (`firecode layer add IMAGE`)
is in every child's guest. No docker on lab2x1 yet, so no layer is built here.

Two one-shots side by side need their own sockets — the launcher gives each
`$XDG_RUNTIME_DIR/letibot/oneshot-<pid>.sock`; before that the second one
said `already served by a live daemon` and exited without running its prompt.

## 4b. Paths in a leticode session

The backend is rooted at `/` (opencode parity: `read` reaches the whole host
and the ruleset, not a jail, is the gate), and **relative paths start at the
workspace**, the same as a command's `cwd`. Measured before that was true:
`read crates/flowy/src/context.rs` answered `no file`, the model went looking
from `/home` down with `glob` and `grep`, read a 27 GB model shard whole, and
the daemon spent twelve minutes in the kernel. Two sessions, same shape.

What bounds a search now: files over 16 MiB are never opened, a rung stops
after 512 MiB read, the result counts what it did not open, `read` refuses a
file over 256 MiB by size before opening it, and a walk from `/` never
enters `/proc`, `/sys`, `/dev` or `/run`.

## 5. letibot from a folder, and the two wizards

```
letibot                      connect to THIS folder's daemon (the git toplevel), or start one — it says which
letibot --attach [DIR]       connect only
letibot --daemons            every folder's daemon on this box
letibot --stop [--all]       this folder's daemon, by pid — never every harnessd on the box
```

The mode is a property of the project, not of the flag: a row in
`~/.config/letibot/modes.tsv` wins over `--mode`, and the banner says so
(`mode is X for DIR (from the project store), not the daemon default Y`).
`allow-all` on a bare host is refused by its confinement prerequisite either
way — it is the point a VM runs at.

In the head:

```
/flowy status                the seat: listening, stalled, stopped, reader cursor, attached sessions
/flowy login [SEAT]          attach a seat to the RUNNING daemon (the usual path; a name when several)
/flowy login SEAT --token T [--addr URL] [--new-reader]
                             a seat this box never held: writes agents/SEAT and env-SEAT, opens it
/flowy logout                release it
/models                      what answers now; every provider with its auth state and the exact command
/models deepseek/deepseek-chat [--key K]
                             switch underneath this session and make it the standing choice
/models local                back to the local server
```

Every open session's `flowy` tool is a door: with no seat it says so and names
`/flowy login`; after a login, every open root session is attached, its
monitor declared, the shelf installed, the fabric block appended.

**What never asks.** `leticode` seats a shell by default (`--no-bash` refuses
it), and the shell runs the preapproved list without a prompt: read-only git
and gh, cargo/go/npm/pytest build-and-test verbs, the shell's read-only
utilities — 142 prefix rules, the `preapproved` line of the banner counts
them. A compound command is tested one simple command at a time (`git log; rm
x` is not `git log`), and anything with `$(…)`, a backtick, a `> file`, a
group or a leading `VAR=` goes to the prompt. On a prompt, **Always allow**
writes the program and its verb (`cargo run*`) to
`~/.config/letibot/permission.json`, opencode's shape, hand-editable; a
`deny` row there outranks the shipped list. `$LETIBOT_PERMISSION` on top.

## 6. Processes: never `ps | grep -v grep`, never `pkill -f`

Measured on this box (`PS_USE.md`): 294 `ps` pipelines in 22 days, 80 % of
them one question — *is X running, since when, with what arguments* — every
one a ritual matched against the shell running it. In letibot:

```
ps pattern=X                           pid, ppid, age, state, CPU%, RSS, command line;
                                       this daemon and its ancestors are never rows
ps pid=N | children_of=N               one process; a pid's children
ps top=cpu|mem limit=8                 the busiest first
```

`pkill -f PATTERN` matches the shell running it, because the shell's own
command line carries the pattern. It has killed running commands and the
shells around them eight times on this fleet, and its author once more while
writing this. In letibot:

```
pkill  pattern=X                       list: pid, age, command line — this daemon, its
                                       ancestors and the model server are never listed
                                       (PROTECTED), a job of this session says job_kill
pkill  pattern=X action=kill pids=[…]  signal by pid (term default; int, hup, kill);
                                       a pid not in the listing now is REFUSED
monitor name=N process=X               watch every match found NOW leave (by pid and
                                       start time); fires when all are gone
monitor name=N pid=P                   one process, by handle
```

From a shell (Claude seats): `flowy waiter check` for the listener, `firecode
list` for VMs, and for anything else read the pid from `ps -o pid,args` and
`kill` the number — never the pattern.

## 6b. web_search, when a provider is attached

```
letibot --web-search brave [--brave-key KEY]      else $BRAVE_API_KEY, else
                                                  [brave] key= in providers.toml
web_search  query="…" [max_results=N] [site=host]
```

`web_search` ships in every build as a tool that **refuses** — the schema is
fixed so attaching one later does not re-prefill stored conversations — and it
is **seated only when a provider is attached**, so a session started without
`--web-search` is byte-identical to one from before this existed. With Brave
behind it the banner says so and names where the key came from.

What comes back: title, URL and snippet per hit, the count Brave had before
the cap (`showing 3 of 27` is a different fact from `showing 3`), and the
rewritten query when Brave searched for something else. Markup is stripped;
only `web.results` is read, so an FAQ or infobox block is never reported as a
search hit. A missing key refuses **at attach**, naming all three places, not
as a 401 mid-turn. Fetched text is untrusted — the tool result says so.

## 7. sudo: the password comes from the head, never a tty

A `sudo …` from a letibot shell does not reach for a terminal (there is none;
40 attempts on this box died on that). The command is privilege escalation, so
it asks the operator first like anything on the always-ask list; once admitted,
`sudo` runs through a shim that adds `-A`, `letibot-askpass` asks the daemon,
and the head shows a masked field naming the command. Type the password there —
it goes to `sudo` and nowhere else: not the log, not the transcript, not the
model. Two minutes, Esc refuses. `sudo -n` is untouched, so a probe still
answers *no*. Nothing to type as an agent; if you need a package, run the
`sudo` and the operator's head handles the password (or it fails honestly).
