# leticode - opencode-shaped agent on letibot primitives

A letibot session that models itself after opencode: the tool union, plus a verbatim port of
opencode's permission model, and the seat's own rules on top. `letibot` runs this seat by
default.

## The seat

`Role::leticode` (`crates/tools/src/runtime.rs`) seats 38 tools, and `max_tools` is 39 - the list's
own count and nothing like the crate's default ceiling, with the one spare the session's own verbs
and the optional web tools are added under. In groups: **files and search** (`read`, `write`,
`edit`, `grep`, `glob`, `notes`); **exec and its follow-ups** (`bash` with
`job_list`/`job_output`/`job_wait`/`job_kill`/`job_extend`, `monitor`, `pkill`, `ps` - a backgrounded
`bash` without `job_wait` is a handle nobody can follow up on, `job_extend` is the way out of a job
which has no deadline of its own, and `pkill` finds by string and kills by (pid, start time) only,
never itself); **delegation** (`task`, `task_result`, `task_message`,
`task_resume`, `task_start`, `hosts`, `fanout`, `scenario`); **the box and its work** (`room`,
`queue`, `merge_gate`, `decisions`, `harness`, `host_changelog`, `host_config`, `transcript`,
`digest`, `plugin`, `plugin_hooks`); and **asking the person** (`ask_user_question`, `todo_write`,
`skill`, `lsp`).

## The instruction files, and the config layer

`crates/leticode-config` holds `leticode.toml` (the model levels) *and* the answer to "which markdown
does a leticode read before it starts": the nearest `leticode.md` at or above the workspace - or the
canonical `CLAUDE.md` in that same directory, when a project wrote that one instead - then the box's,
from `~/.config/letibot/leticode.md` or `~/.claude/CLAUDE.md`. Neither present is an answer, not an
error.

The names are spelled there and nowhere in `letibot-harnessd`, which asks for paths and reads them
(`standing_notes::gather`, folded into the first source - the operator's own instructions - so a rule
still arrives whole, ahead of every note). The operator's rule: *"leticode can support claude.md. but
not letibot"*, *"i want letibot stay a framework, a runtime"*.

### A folder of repositories, and the home directory

A folder that is not inside a git repository takes its single child repository (or a folder of
same-owner repositories) as its project. The home directory never does: a session in `~` has no
repository, no queue door and no queue notices, and the header says "no repository: ~ is a plain
folder". Any other folder can opt out the same way, in its `leticode.toml` or the user file:

```toml
[project]
repository = false   # do not adopt a child repository
```

One rule, `leticode_config::workspace::adopts_children`, is asked by the head and the daemon.

### Config, not a plugin: the numbers and the head's keys

A fixed number or a list of names is a key, not an extension point - a plugin for them would cost a
build and a pin to change a number. Six tables, at the user level and the project's over it, each
with today's value as its documented default (`crates/leticode-config`'s module doc has the whole
list, and `crates/leticode-config`'s tests are what keep the defaults honest):

```toml
[queue]
stuck_after_seconds = 900     # a row stuck this long is the merge queue's census
stuck_repeat_seconds = 7200   # and the census is said again this often

[nag]
after_seconds = 60            # the idle window before the plan-check
floor_seconds = 10            # the shortest re-arm an operator add earns
repeat_cap_seconds = 1800     # the ceiling the doubled interval retreats to

[room]
tie_breaker = "deepseek/deepseek-flash"   # the model that arbitrates the room's items

[warnings]
mute = ["model_slow_first_byte"]  # codes the head files and never draws
coalesce = true                   # notices of one kind inside a minute are one counted row

[keys]
"todos.d" = "f"                   # SURFACE.KEY, as the bar spells them, to one other key

[verbosity.mine]                  # a profile IS the list of things it shows
show = ["edits", "tools=folded", "system"]
```

Three rules. **The project's value wins** over the user's, key by key, as the models do - and
`[keys]`' two ends, `mute` and a profile, are suppressions, so they resolve differently: a mute
UNIONS (a project file must not unsay what the operator muted for every project) and a profile of
the same name is replaced. **A bad value says its file and line** - `[nag] after_seconds = "60"`
is refused where it was written, not silently read as a minute. And **a binding is rebound within
its surface, never the head's global chords**: `esc`, `ctrl-c` and the pane chords are refused as
a target and as a source, because a surface never sees them and the bar would advertise a press
that does nothing (W8 of the first review).

A mute is by KIND and not by copy, which is the point of the table: the 127
`model_slow_first_byte` dismissals a head's `head.toml` can carry are one line here. `head.toml`
still holds what a head decides for itself - the rung it is on, the folds, `retired` - and
`leticode.toml` holds what a project or a box says.

## The permission model

`crates/tools/src/permission.rs` is opencode's model, ported: a rule is `{permission, pattern,
action}`, a call is tested against every rule of every ruleset, and the **last match wins**, with
`ask` when nothing matches. The config's key order is preserved because that order is what decides
precedence. `*` is any run and crosses `/`, `?` is one character, a backslash is a slash -
opencode's wildcard, anchored and dotall, not a filesystem glob.

It is wired into `AdjudicatedGate` **before the mode**: `deny` refuses, `allow` admits, `ask` falls
through to the point and the adjudicator. An `allow_always` answer writes `(permission, pattern)`
back into the ruleset, which is why a rule can outlive the process. Two rules keep a durable rule
honest, both in `permission.rs`: `sets_state_for_later` (a segment that is `export`, `unset`, `set`,
`umask`, `alias` … changes what a *later* segment means, so no rule is offered for it), and
`a_durable_rule_can_apply` (a heredoc or a substitution the segmenter will not split is an ask
before any rule is consulted).

## Downgradable subagents

`task` takes `access` - `read-only`, or any of `no-write`, `no-exec`, `no-network` - and `where`
(`host`, or `firecode`). A subagent inherits the parent's ruleset and the downgrade only removes: it
is the union of the parent's own downgrade and the one asked for, so a downgraded session cannot
spawn a wider child by naming a wider role. `role` accepts any seat this build knows and refuses an
unknown one rather than seating it as coder; the disclosure names the downgrade.

Three readers of one fact, and a test that they agree: the tools of a denied class are not seated
(`Registry::without_access`), the backend is opened without them (a read-only view without `Write`,
no process host without `Exec`), and the ruleset carries a `deny` per denied tool - seated or
well-known - that wins by last-rule.

`where: firecode` (`crates/tools/src/firecode.rs`) runs the tools in a VM over vsock, keeps the
model on the host, gives the child a copy of the workspace, and makes the VM the boundary
(allow-all inside). See `docs/subagents.md`.

## Providers, and one daemon per folder

`--provider deepseek|glm|grok`: the transcript as messages over a `MessagesBackend`, with the
token ledger kept as the record, METERED and disclosed (`docs/providers.md`).

`scripts/letibot` connects-or-starts per git toplevel (`--attach`, `--daemons`, `--stop` by
pid). `ClientFrame::Slash` carries `/flowy status|login|logout` and `/models …` to the daemon;
`crates/harnessd/src/slash.rs` answers on the session log, naming the next command when
something is missing. `/models` switches the provider underneath the session and records the
standing choice in `~/.config/letibot/providers.toml`, which a new daemon starts on.

`crates/flowy` is the seat on the fabric, as a monitor: `harnessd --flowy` holds one persistent
seat per daemon, a root session attaches with its own per-room attention table and gets a
continuous `flowy` monitor whose firings are the messages; subagents route through the parent. The
shelf's skills arrive as summaries and its memories as titles in the system prompt, refreshed after
a compaction as a system update. See `docs/flowy-monitor.md`.

## `ask_user_question`

`crates/harnessd/src/answers.rs`'s `HeadQuestioner` poses it as a `DecisionRequested` with `kind:
"question"` and its `choices`, and the answer comes back through the frame the head already had.
Four shapes, all first-class: a choice, a choice **with the person's own note on it** (`<choice> --
<note>` at the composer), words of their own, and an **abstention** (`abstain`, alone) - which is an
answer, not silence and not an empty reply, and which the model receives as `Abstained` rather than
`not_run`.

## Reading an image

`read` reads a picture as a picture. Same tool, same verb, same gate, same permission, same path -
only what comes back differs: an image part with the file's bytes, and a text payload that describes
it. The ruling is the operator's: *"read is read there is nothing to settle"*, *"reading an image is
no different to reading a rust file."*

Measured against the local `llama-server` (`qwen-3.8-27b`, `--mmproj`), because the numbers are what
a future change has to argue with rather than guess about:

| what was measured | result |
|---|---|
| an image in a `user` message | the model named a red square `Red` |
| an image in a **`tool`-role** message | the model named a green square `Green` - media inside a tool result works |
| `data:image/png;base64,…` as the URL | works |
| a bare **path** as the URL | **`HTTP 400 - Failed to load image or audio file`**: the server fetches the URL, so a `data:` URI is required |
| an image above **4.19 MP (2048x2048)** | **resized, never refused**: 2048² and 4096² both cost 4151 prompt tokens, one per 1024 pixels |
| a 16.8 MB request body | accepted; no byte limit was found up to there |

**So there is no size cap on the read path.** A head that imposed one would be discarding what the
server would have taken, and an attachment dropped by the head looks exactly like a picture the
model ignored. And if a message says an image is attached and the model has no image, the attachment
did not arrive - say that, rather than answering as though the picture were merely uninteresting: a
model that answers around a missing picture is indistinguishable from one that saw it and had
nothing to say, and only one of those is worth acting on.

### What an image turn costs the cache

A local-only question: a session on a cloud provider never sends token ids, so none of this applies
to it. It mattered because llama.cpp takes images only when `prompt` carries a `prompt_string` - the
server tokenizes it - while letibot's design is that *the harness* renders, tokenizes and sends ids.

**They do not differ, and the reason is the framing.** Bare text diverges at the first boundary
because BPE merges across it (`cache. And` is not `cache.` + `And`): 110 per-item tokens against 107
as one string, shared prefix **40**. Framed by `<|im_start|>`/`<|im_end|>`, both are 125 and the
shared prefix is **125**, because control tokens are atomic. **The string path is free, provided the
renderer frames its items.**

On the live server with slots pinned, an image prompt at 512x512 cost **258 prompt tokens**, and
**token ids after an image turn came back `cache_n = 84`, identical to its own warm control**: an
image turn does not poison the cache for the ledger's ordinary path. The one unresolved case is the
*first* image turn against a prefix built by token-ids requests, which came back `cache_n = 0` -
suspected to be `process_mtmd_prompt`'s hardcoded `add_special = true` prepending BOS, one token of
difference and a prefix match of zero. The check is cheap: tokenize the ledger's prompt both ways and
compare the first token.

**Open, and named rather than left to be discovered: compaction.** An image in the transcript is
re-sent on every prompt rebuild and stored inline as base64 (4/3 of the file). opencode strips media
on compaction and carries a recovery instruction for a provider that refuses oversize media; R24/R27
own what this tree does, and until then an image lives for the life of the session.
