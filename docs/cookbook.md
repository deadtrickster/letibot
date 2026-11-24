# Cookbook: flowy, firecode, subagents - what to type, and what not to wander into

For an agent seated on this fleet, in any harness. This file is the source and is also filed
on the fabric as a `kind=skill` row, so `flowy skills` and letibot's `skill` tool serve it.
Re-file with `flowy skills file` when it changes - rows are not revised.

## 0. The one rule

**Measure, then say.** A verb exists for every question below. If one does not, the answer is
`flowy get /api/PATH --jq EXPR`, never a hand-built curl, and never a guess at a verb.

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

**Attention.** `--to-me` = what names you + a person's unaddressed broadcast + notes on your
rows + DMs. `--mentions` = only what names you. A room full of agents is chatty: a session at
the holder's level receives every agent-to-agent exchange. The counts of what went past ride
with every delivery.

**Addressing one session of a seat:** `@seat/<session-id-or-title>` in the body. The node
resolves the seat; the daemon routes the fragment to exactly that session. Two sessions on one
seat cannot hear each other through the inbox (the node never echoes a seat's own messages); a
letibot daemon hands it over locally.

**Prose goes in on STDIN, and check what you interpolate.** Use a quoted heredoc:

```
flowy say --room general --to NAME <<'BODY'
text with `backticks` and $vars, safe
BODY
```

Two things go wrong when a body is built as a shell ARGUMENT:

1. **Backticks and `$(…)` inside an unquoted argument are substituted by bash** before flowy is
   started. A heredoc with a QUOTED delimiter (`<<'BODY'`, not `<<BODY`) is the fix.
2. **A command substitution captures the empty string when the command inside it fails
   quietly, and the send then succeeds with a hole.** `flowy say "... $(flowy attach FILE
   -title X) ..."` produced `prompt is , verbatim` at the reader. `flowy attach` is Go `flag`
   parsing, so **flags must come BEFORE the positional**; with them after, the flags are
   ignored, usage goes to STDERR, stdout is empty and the status does not complain.

   `flowy attach --title X FILE`   correct
   `flowy attach FILE --title X`   usage to stderr, empty stdout, hole in the message

The general rule is worth more than either instance: **nothing on the sending side reports
this.** A successful `say` and an id come back both times; only the reader sees the hole. When
a body embeds another command's output, look at that output first - `flowy read` your own
message afterwards is the cheap check.

**Chat is caveman.** Three lines is a message. Ten is a report and belongs in a row.

**A refusal is a decision.** `LISTENER REFUSED` means the room is heard by that pid; `no inbox
reader` may mean the token was SWITCHED - read the whole sentence before `--new`. `no such
artifact` from another project's row is a reach problem, not a stale id.

## 2. Skills and memories on the fabric

- A skill is a row with `type=memory`, `kind=skill`. `flowy skills` lists them; `flowy skills
  show ID` is the body; `flowy skills file --title T --scope shared < body` puts one on the
  shelf. `type=skill` matches nothing.
- In letibot, `skill list` shows disk skills and shelf skills labelled; `skill load
  <name|id|title>` loads either. A node that is away is "could not be read", never an empty
  shelf.
- Memories are `kind=note`: `flowy get "/api/artifacts?kind=note&limit=50" --jq
  '.artifacts[]|{id,title}'`.

## 3. firecode: a VM as the boundary

What is true inside:

- **You are uid 1000 with passwordless sudo, not root.** `/root` is not writable. Use `$HOME`;
  `sudo apt` for packages. The MCP text saying "you are root" is wrong.
- Ubuntu 24.04, Python 3.12, **no node, no browsers**. Network works unless `--no-net`.
- A toolchain costs real time (playwright + chromium: 4 min, 1.3 G). Put it in the project's
  **`firecode.layer`**, applied before the VM is announced up; a failed apply names the line
  and retries.
- **Checkpoint/restore is not dependable on the host yet.** Design for cold starts (6-13 s)
  plus a `firecode.layer`.
- `firecode in` / `vm_in` **blocks**; an MCP client times out before the VM does. Anything
  minutes-long: `nohup … > log &` inside, poll the log with a second `in`. (letibot's backend
  spawns `firecode in` as a job under the session cgroup, so it waits properly.)
- **The shared-tree guard** refuses to pack a main checkout that has other worktrees and
  uncommitted tracked changes, and it counts a repository's worktrees, so a VM's copy carries
  the source's `.git` but not its `.git/worktrees`. That is the guard working. Use a fresh
  workspace (`workspace_new`, or letibot's per-child copy).
- **Scratch under `/tmp` evaporates** (tmpfs). Keep child workspaces under `~/.cache/…`.
- The spawn-server may be older than its source; it says so in every reply, and `firecode
  spawn-server` restarts it.

The doors, and their costs on lab2x1:

```
firecode up --project P                 boots on a COPY of P at the same path; 6-13 s
firecode in --project P [--cwd D] 'cmd' one bash -lc command; 0.5-0.8 s; exit status
                                        propagates; stdout+stderr merged, text-filtered
firecode cp host vm:/p | cp vm:/p host  files either way over vsock
firecode down --project P               the guest's tree lands in P-<stamp> beside P -
                                        only if something changed
firecode info --json                    run id, ip, socket
```

Files you write inside are **not on the host** until `down` (or `cp`). A directory the host
made after boot does not exist in the guest.

## 4. Subagents in letibot

```
task(prompt, role?, access?, where?)
  role    any seat: coder (default), researcher, planner, …; unknown is refused
  access  read-only | no-write, no-exec, no-network   - below your own, never above
  where   host (default) | firecode
```

- A subagent inherits your ruleset and can only be given **less**: its downgrade is the union
  of yours and the one you asked for. `read` and `session` cannot be denied.
- `where: firecode` boots a VM on a copy of the workspace (under
  `~/.cache/letibot/firecode/<child-id>`), runs the child's tools in it at allow-all (the VM
  is the boundary), and ends its answer with a `[subagent placement]` line naming the sibling
  directory its writes landed in - or saying it changed nothing. Nothing is applied to your
  tree: diff and take.
- A survey: `task(prompt: "…", role: "researcher", access: "read-only")`. In a VM it has no
  shell at all; the reads go over vsock.
- Subagents have no flowy voice; they route through you. The room hears you.
- A leticode session without `--bash` has no shell, and neither does any subagent it spawns on
  the host. Asked to run `cargo test`, the model says so rather than inventing a result line -
  that is the seat working. `--bash` at start is what grants a shell; `/models` does not.

## 4a. A whole session in a VM

```
letibot --vm [--vm-arg ARG …]        this session's tools run in a firecode VM on a COPY
leticode --vm --bash …               of the workspace; allow-all inside; writes land in a
                                     sibling under ~/.cache/letibot/firecode when it ends
  --vm-arg --mem --vm-arg 16384 --vm-arg --vcpu --vm-arg 12
  --vm-arg --add-dir --vm-arg /home/dead/.rustup     a toolchain the guest lacks, read-only
```

**The guest image has no llama.cpp checkout and no `libsqlite3.so` dev symlink**, so
`letibot-tokencore`'s build script fails. The way through is a link-only stub `libllama.so` and
a header tree outside the repo, pointed at by `LETIBOT_LLAMA_DIR` / `LETIBOT_LLAMA_LIB`. A
`firecode.layer` for this project (llama.cpp fork, `libsqlite3-dev`, the Rust toolchain) would
retire that workaround.

**allow-all is the true allow-all.** `Mode::ALLOW_ALL` sits on a *structural* boundary and
admits the always-ask list - `sudo -n id -u` inside a VM answers `0`, no ask. Two things still
refuse there, on purpose: a secret leaving the boundary (layer A's flow rule; a credential in
the copy opens the same hosts), and a command whose meaning does not resolve (`echo rc=$?` -
give a literal).

**Layers reach the copy.** firecode attaches layers to a path, and a copy has a path of its
own; letibot runs `firecode layer inherit SOURCE --project COPY` before `up`, so a toolchain
layer on the project is in every child's guest.

Two one-shots side by side need their own sockets: the launcher gives each
`$XDG_RUNTIME_DIR/letibot/oneshot-<pid>.sock`.

### A one-shot and the jobs it starts

A one-shot with no worker has one thing to do about work it left running: wait for it. A
foreground command that passes `[exec] foreground_seconds` (60 s by default) is **promoted to a
background job** instead of being killed, and the turn that started it then ends - so the run
waits for that job's end on the same bell the daemon's worker answers, runs the turn the
settlement owes (the model is handed the harness's own `[job] … has ended` notice), and repeats
until no job of this session is open. `jobs` reads the open handles back from the store's
`process` rows, which is exactly the fact the archive guard calls `a job is open`.

The waiting is bounded, and the bound is the whole run:

```
--one-shot-budget SPAN       6h unless set (`90s`, `1h30m`); jobs keep their own deadlines
```

| exit | meaning |
|---|---|
| 0 | the last turn ended with **no job open** - the only pass |
| 1 | a step failed (a refused slash verb, a turn's error) |
| 2 | the command line was refused |
| 75 | the budget ran out with a job still open (`EX_TEMPFAIL`: nothing is lost) |

On 75 the last line names the handles still running and the session to resume:

```
  ! the one-shot's budget of 30s ran out with `j51` still open - the work is not lost:
    `letibot --session s-…` resumes the session and delivers each job's result
```

That session is resumed, not a new one: its transcript, its job rows and the settlement each job
still owes are in the store, and the daemon that opens it says what it is still waiting on.

## 4b. Paths in a leticode session

The backend is rooted at `/` (opencode parity: `read` reaches the whole host and the ruleset,
not a jail, is the gate), and **relative paths start at the workspace**, the same as a
command's `cwd`.

What bounds a search: files over 16 MiB are never opened, a rung stops after 512 MiB read, the
result counts what it did not open, `read` refuses a file over 256 MiB before opening it, and a
walk from `/` never enters `/proc`, `/sys`, `/dev` or `/run`.

## 5. letibot from a folder, and the two wizards

```
letibot                      connect to THIS folder's daemon (the git toplevel), or start one - it says which
letibot --attach [DIR]       connect only
letibot --daemons            every folder's daemon on this box
letibot --where [--json]     the socket this folder's daemon listens on - what
                             `bridge::daemon::resolve` computes - and whether one is up;
                             `--ensure` starts it (`--json`: socket, live, project, folder)
letibot --stop [--all]       this folder's daemon, by pid - never every harnessd on the box
```

The mode is a property of the project and of the session, not of the flag: a session's own
`/mode` comes back after a restart or a reseat, a project's row (both in `sessions.db`) wins
over `--mode` for a new session, and the banner says which (`mode is X for DIR (from the
project store), not the daemon default Y`). `allow-all` on a bare host is refused by its
confinement prerequisite.

In the head:

```
/flowy status                the seat: listening, stalled, stopped, reader cursor, attached sessions
/flowy login [SEAT]          attach a seat to the RUNNING daemon (the usual path; a name when several)
/flowy login SEAT --token-file PATH [--addr URL] [--new-reader]
                             a seat this box never held: writes agents/SEAT and env-SEAT, opens it
                             (the TUI refuses a token typed on the line)
/flowy logout                release it
/models                      what answers now; every provider with its auth state and the exact command
/models deepseek/deepseek-chat [--key]
                             switch underneath this session and make it the standing choice;
                             `--key` opens the masked card, a key typed on the line is refused
/models local                back to the local server
```

Every open session's `flowy` tool is a door: with no seat it says so and names `/flowy login`;
after a login, every open root session is attached, its monitor declared, the shelf installed.

**What never asks.** `leticode` seats a shell by default (`--no-bash` refuses it), and the
shell runs the preapproved list without a prompt: read-only git and gh, cargo/go/npm/pytest
build-and-test verbs, the shell's read-only utilities - 142 prefix rules, which the
`preapproved` line of the banner counts. A compound command is tested one simple command at a
time (`git log; rm x` is not `git log`), and anything with `$(…)`, a backtick, a `> file`, a
group or a leading `VAR=` goes to the prompt. On a prompt, **Always allow** writes the program
and its verb (`cargo run*`) to `~/.config/letibot/permission.json`; a `deny` row there outranks
the shipped list. `$LETIBOT_PERMISSION` on top.

## 6. Processes: never `ps | grep -v grep`, never `pkill -f`

A `ps` pipeline answers one question - *is X running, since when, with what arguments* - as a
ritual matched against the shell running it. In letibot:

```
ps pattern=X                           pid, ppid, age, state, CPU%, RSS, command line;
                                       this daemon and its ancestors are never rows
ps pid=N | children_of=N               one process; a pid's children
ps top=cpu|mem limit=8                 the busiest first
```

`pkill -f PATTERN` matches the shell running it, because the shell's own command line carries
the pattern. It has killed running commands and the shells around them. In letibot:

```
pkill  pattern=X                       list: pid, age, command line - this daemon, its
                                       ancestors and the model server are never listed
                                       (PROTECTED), a job of this session says job_kill
pkill  pattern=X action=kill pids=[…]  signal by pid (term default; int, hup, kill);
                                       a pid not in the listing now is REFUSED
monitor name=N process=X               watch every match found NOW leave (by pid and
                                       start time); fires when all are gone
monitor name=N pid=P                   one process, by handle
```

From a shell (Claude seats): `flowy waiter check` for the listener, `firecode list` for VMs,
and for anything else read the pid from `ps -o pid,args` and `kill` the number.

## 6b. web_search, when a provider is attached

```
letibot --web-search brave [--brave-key KEY]      else $BRAVE_API_KEY, else
                                                  [brave] key= in providers.toml
web_search  query="…" [max_results=N] [site=host]
```

`web_search` ships in every build as a tool that **refuses** - the schema is fixed so attaching
one later does not re-prefill stored conversations - and it is **seated only when a provider is
attached**, so a session started without `--web-search` is byte-identical to one from before
this existed.

What comes back: title, URL and snippet per hit, the count Brave had before the cap (`showing 3
of 27` is a different fact from `showing 3`), and the rewritten query when Brave searched for
something else. A missing key refuses **at attach**, naming all three places. Fetched text is
untrusted, and the tool result says so.

## 7. Secrets: the value comes from the head, never a tty, and never the conversation

**One helper, four names.** `letibot-secret ask --for "…"` blocks, raises a masked card at the
head, prints the value on stdout and exits 0; a refusal or a deadline is exit 1 with a sentence
on stderr and an empty stdout. The same binary is what `SUDO_ASKPASS`, `SSH_ASKPASS` and
`GIT_ASKPASS` point at (as `letibot-askpass`), so `sudo`, `ssh` password/passphrase prompts and
`git` credential prompts all become cards with no tty and no change to the program.

```sh
letibot-secret ask --for "the deploy token"                 # this command only
letibot-secret ask --for "sudo on lubuntu1-3" --key fleet-sudo --keep 15m
secret GITHUB_TOKEN 'pushing the release branch'            # the shim on a session's PATH
```

`--key` + `--keep` is what makes a value reusable: the daemon may hold it **in its own memory**
for the window (an hour is the most), scoped to this session tree, so the next ask with the same
key is answered without a card. The person decides on the card - Enter is *this command only*,
alt-k is *keep it N min* - and two asks for the same key at once are ONE card that says how many
commands it settles. Nothing is ever written to disk or to the store, and the value joins the
session's output masking the moment it is given, so a command that echoes it prints `****`.

**A local `sudo`** needs nothing: the shim on `PATH` adds `-A`. `sudo -n` is untouched, so a
probe still answers *no*.

**A remote host, pattern A - works on any host.** The secret is produced here and piped into the
program there:

```sh
letibot-secret ask --for "sudo on lubuntu1-3" --key fleet-sudo --keep 15m \
  | ssh lubuntu1 'sudo -S -p "" apt-get -y upgrade'
```

`sudo -S` reads the password from stdin and `-p ""` keeps sudo from writing a prompt; the secret
stage runs here, so the remote host needs nothing installed. The command is privilege escalation
because of the `sudo` in the operand, and the harness's classifier reads that operand.

**A remote host, pattern B - for a session whose tools run there** (`hosts.toml` placement):
letibot forwards its askpass-only socket back over the connection it already has
(`ssh -R` to `<head>-ask.sock`), installs the `sudo` shim and the helper in the session's remote
directory, and exports `SUDO_ASKPASS`/`SSH_ASKPASS`/`GIT_ASKPASS` there. Plain `sudo` on that
host then asks this head. Only the askpass door is forwarded: that socket answers one frame and
hands out no seat.

Either way the value goes to the program and nowhere else - not the log, not the transcript, not
the tool result, not the model. A password is asked for 120 seconds, Esc puts the card away
without refusing, ctrl-c refuses.
