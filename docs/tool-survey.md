# Tool-set survey — five agent harnesses, read as interfaces

Written 2026-09-09 on lab2x1. Six codebases read in parallel by subagents; every claim
below carries a file and line from the source, not from a README or a marketing page.
Where a README is the evidence, it says so.

This is a survey of **interfaces**: tool names, argument schemas, return shapes,
documented behaviour, and the gate in front of each. No implementation code was copied
into this document. Schemas and model-facing description strings are quoted, because
those are the artefact under study.

## What was read, and from where

| harness | path | identity | licence |
|---|---|---|---|
| grok-build | `/data/scratch/harness-survey/grok-build/` | `SOURCE_REV` `eb4a894da8fb7bcd8d8f398a9d909a7868a4fcf1` (`SOURCE_REV:1`); local commit `75810042`, 2026-09-08 | Apache-2.0 (`LICENSE:3`) |
| opencode | `/home/dead/Projects/opencode/` | v1.18.29 (`packages/opencode/package.json:3`), HEAD `1ee74df8`, working tree **clean** | MIT |
| pi | `/data/scratch/harness-survey/pi/` | `@earendil-works/pi-coding-agent` v0.85.1, commit `6160683a`, 2026-09-08 | MIT |
| omp (oh-my-pi) | `/data/scratch/harness-survey/omp/` | v18.1.15 (`Cargo.toml` workspace.package), commit `a33cc268` | MIT (`package.json:5`) |
| deepseek-harness | `/data/scratch/harness-survey/deepseek-harness/` | `dsh-0.1.5-alpha.1` (`package.json`), commit `5dda764e` | MIT |
| **letibot** (for comparison) | `/home/dead/Projects/letibot/` | pinned at commit `e33cc1b6` ("grep: zero files searched is not a claim about the tree") | — |

**letibot's own line numbers are pinned to `e33cc1b6`, not to whatever HEAD is when you read
this.** The tree moved during the survey — HEAD is now `538d358c` on branch `session-resume`,
and `crates/harnessd/src/harness.rs` is under concurrent edit in the working tree (+354/-37
against `e33cc1b6`), so the three `harness.rs` citations below (`:288-289`, `:290-299`,
`:663-671`) resolve **only at `e33cc1b6`** and were verified there. `crates/tools/` is
byte-identical between `e33cc1b6` and `538d358c` and clean in the working tree, so every
other letibot citation resolves at either.

**There is no opencode checkout under `/data/scratch/harness-survey/`.** The only one on
this box is `~/Projects/opencode`, and that is what was read. Its working tree is clean at
`1ee74df8` ("Add a cache-friendly compaction strategy for local models"), so no cited file
is locally modified.

### Deliberately excluded

Three directories in `/data/scratch/harness-survey/` were **not read and are not cited**,
by me or by any subagent:

- `omo/` and `omo-slim/` (oh-my-openagent) — **Sustainable Use License**.
- `crush/` — **Functional Source License 1.1**.

Both licences restrict derivative and competing use in ways that make reading them as
prior art for a harness we are building a legal question rather than an engineering one.
`firecode/`, `kilo/`, `oracle/` and `flowy/` are also present in that tree and were out of
scope for this survey.

### Which agent, in each monorepo

Every one of the five is a monorepo with more than one candidate tool set. What was
surveyed:

- **grok-build** — the `GrokBuild` namespace as assembled for the default agent
  (`crates/codegen/xai-grok-agent/src/config.rs:245-278`, 17 core tools, plus conditional
  additions at `builder.rs:611-662`). Four other namespaces exist in the same registry —
  `GrokBuildConcise`, `GrokBuildHashline`, `Codex`, `OpenCode`
  (`crates/codegen/xai-grok-tools/src/types/tool.rs:29-42`) — and were not surveyed.
- **opencode** — `packages/opencode/src/tool/`, assembled at `registry.ts:229-252`. A
  newer, unwired "v2" tool tree exists at `packages/core/src/tool/` under service id
  `"@opencode/v2/ToolRegistry"` (`packages/core/src/tool/registry.ts:40`); it is cited only
  where it contradicts the live set.
- **pi** — `packages/coding-agent` (the shipped CLI). A second, 4-tool set exists in
  `packages/agent` (`src/harness/tools/`) and is noted where it differs.
- **omp** — `packages/coding-agent`, the only package defining a model-facing tool set,
  with engine detail from the Rust crates `pi-edit`, `pi-ast`, `pi-natives`.
- **deepseek-harness** — the whole `packages/*/tool-*` plugin surface, anchored on two
  shipped compositions: the `dsh-base` bundle (`packages/bundle/base/cordis.patch.yml`) and
  the four agent presets under `packages/preset/agent-presets/presets/`.

---

## The scorecard

These six rows are why the survey happened. Each is a defect measured on this box, or a
layer letibot is adding.

| | opencode | pi | omp | grok-build | dsh | **letibot** |
|---|---|---|---|---|---|---|
| **1.** search reports a files-searched denominator | no | no | **computed, withheld** | no | no | **yes** |
| … zero-files distinguished from zero-matches | no | no | timeout only | no | no | **yes** |
| **2.** read-before-write enforced at runtime | **no** (falsely claimed) | no | **yes** (digest) | **no** (says so) | **yes** (stat CAS) | **yes** (digest) |
| **3.** structural / AST tool | no | no | **yes** | no | no | no |
| … LSP navigation tool | flag-gated | no | yes | flag-gated | not mounted | no |
| **4.** subagent spawn tool | yes | example only | yes | yes | yes | **no** |
| … concurrency ceiling | **none** | 4 (example) | 32 | 32 | **none** | n/a |
| … depth ceiling | 1 | n/a | 2 | 1 | 3 | n/a |
| **5.** output capped by default | yes | yes | yes | yes | yes | **no** (unset) |
| … told it was truncated | yes | yes | yes | mostly | yes | yes |
| … given a way to fetch the rest | spill path | offset + spill | `artifact://` | spill path | spill path | **`read_spill` tool** |
| **6.** edit matching | **fuzzy, written** | **fuzzy, written** | **fuzzy, written** | relaxed (opt-in), written | **exact only** | **exact only** |
| … model told the match was relaxed | no | no | no | no | n/a | n/a (never applied) |

**Nobody has row 1.** omp comes closest and then throws the number away. **Three of five
write a match the model did not ask for and do not mention it.** Only two of the five
enforce the read-before-write rule that four of them talk about.

---

## 1. Per-harness tool sets

Access class abbreviations: **R** read, **W** write, **X** exec, **N** network.

### 1.1 opencode — v1.18.29

Seventeen registry entries, most always-on. Every tool returns the envelope
`{title, metadata, output, attachments?}` (`tool.ts:48-53`); the model sees `output`.

| tool | schema (model-visible) | success | **miss** | class | gate |
|---|---|---|---|---|---|
| `bash` | `command` str **req**; `timeout` int?; `workdir` str? (`shell/prompt.ts:15-23`) | tail of stdout+stderr, or `(no output)` (`shell.ts:575-576`) | **Non-zero exit is not an error.** Exit code goes to `metadata.exit` only (`shell.ts:590`); the model must infer failure from stderr prose | X | `bash` ask per command; patterns derived by tree-sitter parse (`shell.ts:311-336`) |
| `read` | `filePath` str **req**; `offset` int?; `limit` int? (default 2000) (`read.ts:28-36`) | `<path>/<type>/<content>`, `N: ` line prefixes, trailer naming next offset (`read.ts:338-351`) | **Redirect.** `File not found: <p>` + `Did you mean one of these?` with ≤3 fuzzy siblings (`read.ts:76-99`) | R | `read` ask; `.env*` → ask (`agent.ts:130-136`) |
| `glob` | `pattern` str **req**; `path` str? (`glob.ts:10-15`) | absolute paths, one per line | `"No files found"` (`glob.ts:54`) | R | ask, `always:["*"]` |
| `grep` | `pattern` str **req**; `path` str?; `include` str? (`grep.ts:10-18`) | `Found N matches`, per-file headers, `  Line N:` rows (`grep.ts:87-97`) | `"No files found"` (`grep.ts:33`) — see §3.1 | R | ask, `always:["*"]` |
| `edit` | `filePath`, `oldString`, `newString` str **req**; `replaceAll` bool? (`edit.ts:47-56`) | `"Edit applied successfully."` + LSP errors (`edit.ts:196-201`) | Three distinct errors: not-found (`edit.ts:724-726`), multiple matches (`:728`), disproportionate span (`:710-712`) | W | `edit` ask; **default `allow`** |
| `write` | `filePath`, `content` str **req** (`write.ts:20-25`) | `"Wrote file successfully."` (`write.ts:18`) | Essentially cannot miss — creates parents, overwrites (`write.ts:64`) | W | `edit` ask |
| `apply_patch` | `patchText` str **req** (`apply_patch.ts:18-20`) | `Success. Updated the following files:` + `A`/`D`/`M` lines (`:284-293`) | `apply_patch verification failed: <err>` — verify-all before write-any (`:44`) | W | one `edit` ask covering all paths (`:205-215`) |
| `task` | see §3.4 | `<task id state="completed"><task_result>…` (`task.ts:64-79`) | `Unknown agent type: X…` (`:133`); `Subagent failed (task_id: …)` (`:218`) | X | ask on `subagent_type` |
| `webfetch` | `url` str **req**; `format` enum (default `markdown`); `timeout` num? capped 120 (`webfetch.ts:15-21`) | converted body text | `Response too large (exceeds 5MB limit)` (`:98`); `Request timed out` (`:92`) | N | ask on URL |
| `websearch` | `query` **req**; `numResults`, `livecrawl`, `type`, `contextMaxCharacters` (`websearch.ts:10-25`) | provider passthrough | **no explicit empty branch in-tool** | N | provider chosen by session-id hash (`:30-37`) |
| `todowrite` | `todos` array **req** (`todo.ts:6-8`) | list re-emitted as JSON (`:38`) | n/a | W (session) | ask, `always:["*"]` |
| `skill` | `name` str **req** (`skill.ts:8-10`) | `<skill_content>` + ≤10 sampled sibling files (`:45-61`) | dies with `Skill.NotFoundError` (`:23-25`) | R | ask on skill name |
| `question` | `questions` array **req** (`question.ts:6-8`) | `User has answered your questions: …` (`:36`) | unanswered → `"q"="Unanswered"` (`:31`) | interactive | permission `question`, default **deny** (`agent.ts:127`) |
| `lsp` | see §3.3 | `JSON.stringify(result, null, 2)` (`lsp.ts:108`) | `No results found for <operation>` (`:108`); `No LSP server available for this file type.` (`:78`) | R | ask `"*"`; **experimental flag, off** |
| `plan_exit` | `{}` (`plan.ts:13`) | `User approved switching to build agent…` (`:73`) | `Question.RejectedError` (`:46`) | W (session) | default **deny** except plan agent |
| `execute` (code-mode) | `code` str **req** (`code-mode.ts:16-20`) | MCP results projected into script return | not surveyed in depth | X/N | per-MCP-tool permission |
| `invalid` | `tool`, `error` str **req** (`invalid.ts:4-7`) | `The arguments provided to the tool are invalid: …` (`:17`) | n/a | — | none |

Three MCP-resource tools are injected in the session layer only when a connected server
advertises `resources` (`session/tools.ts:27-31,136-139`).

**Gate shape.** `evaluate` takes the *last* matching rule and defaults to `ask`
(`permission/index.ts:28-38`) — but **the default ruleset is `"*": "allow"`**
(`agent/agent.ts:120`). On the stock `build` agent, `bash`, `edit`, `write` and
`apply_patch` run with no prompt. The only non-allow defaults are `doom_loop` (3 identical
consecutive calls), `external_directory`, `question`/`plan_enter`/`plan_exit` (deny), and
`.env` reads. There is **no sandbox**: `assertExternalDirectoryEffect` converts an
out-of-worktree path into a permission ask, it does not block
(`tool/external-directory.ts:15-45`). Because `bash`, `edit`, `read`, `grep`, `glob`,
`webfetch`, `task` and `todowrite` all send `always: ["*"]`, a single "always" approval
allows that whole tool unconditionally (`permission/index.ts:145-151`).

Two model-facing description strings are **false**: `edit.txt:4` and `write.txt:5`. See
§3.2.

### 1.2 pi — coding-agent v0.85.1

Eight tools declared (`src/core/tools/index.ts:95-105`), **four active by default**:

> `const defaultActiveToolNames: ToolName[] = ["read", "bash", "edit", "write"];`
> — `/data/scratch/harness-survey/pi/packages/coding-agent/src/core/sdk.ts:256`

`grep`, `find`, `ls` and `powershell` are opt-in via the `defaultTools` setting
(`sdk.ts:257-263`). There is **no task tool, no todo tool, no web tool, no LSP tool** —
verified by exhaustive grep of `name: "…"` declarations across both packages.

| tool | schema | success | **miss** | class | gate |
|---|---|---|---|---|---|
| `read` | `path` **req**; `offset` num?; `limit` num? (`read.ts:14-18`) | raw file text, **no line numbers** (`read.ts:175,177`) | **Throws** raw Node `ENOENT` (`:103`). Offset past EOF → `Offset N is beyond end of file (M lines total)` (`:137`) | R | none |
| `bash` / `powershell` | `command` **req**; `timeout` num? (**no default timeout**) (`bash.ts:37-40`) | interleaved output, or `(no output)` (`:316`) | **Non-zero exit throws**: `…\n\nCommand exited with code N` (`:364`) | X | none |
| `edit` | `path` **req**; `edits: [{oldText, newText}]` **req** (`edit.ts:21-41`) | `Successfully replaced N block(s) in <path>.` (`:207`) | Six thrown errors (`edit-diff.ts:253-289,347`) | W | none |
| `write` | `path`, `content` **req** (`write.ts:11-14`) | `Successfully wrote to <path>` — echoes the *model-supplied* path (`:86`) | raw fs errors only; creates parents unconditionally (`:78`) | W | none |
| `grep` | `pattern` **req**; `path`? (`"."`); `glob`?; `ignoreCase`?; `literal`?; `context`? (0); `limit`? (100) (`grep.ts:21-33`) | `path:line: text` rows (`:273`) | `"No matches found"` (`:258`) — a plain string, not an error | R + **N** | none |
| `find` | `pattern` **req**; `path`? (`"."`); `limit`? (1000) (`find.ts:26-32`) | newline-joined relative paths (`:276`) | `"No files found matching pattern"` (`:136,261`) | R + **N** | none |
| `ls` | `path`? (`"."`); `limit`? (500) (`ls.ts:11-14`) | alphabetical entries, `/`-suffixed dirs (`:109,124`) | `"(empty directory)"` (`:135`) | R | none |

**No parameter anywhere declares a JSON-Schema `default:`** — the "(default: N)" strings
are prose inside `description`. Two nominally read-only tools carry a **network
side-channel**: `grep` and `find` shell out to `rg`/`fd` via `ensureTool()`, which
**downloads the binary from GitHub over HTTPS on first use**
(`src/utils/tools-manager.ts:111-113,141-142,278`).

**There is no built-in approval system at all.** No allowlist, no sandbox, no path
containment, no interactive confirmation ships enabled. The single gate point is the
`beforeToolCall` extension hook, which **returns `undefined` immediately if no extension
registered a handler** (`src/core/agent-session.ts:487-505`). The block contract is
`{block?, reason?, terminate?}` (`src/core/extensions/types.ts:1125-1134`), and mutated
input is explicitly **not re-validated** (`types.ts:943`). Approval patterns ship only as
unloaded examples (`examples/extensions/permission-gate.ts:11-31`,
`protected-paths.ts:11-27`). `resolveToCwd` expands `~` and honours absolute paths with no
containment check (`src/core/tools/path-utils.ts:48-50`).

### 1.3 omp (oh-my-pi) — v18.1.15

Twenty-six builtin names (`src/tools/builtin-names.ts:1-28`), three hidden — `yield`,
`goal`, `think` (`:32`) — plus five `vibe_*` tools registered only at `taskDepth === 0`
(`src/tools/vibe.ts:281-289`, `sdk.ts:3778-3781`).

**The architectural fact that reframes the catalogue: `xd://` device mounting.**
`tools.xdev` defaults **true** (`src/config/settings-schema.ts:4735-4737`). Under it, tools
declaring `loadMode = "discoverable"` are **removed from the request's tools array** and
driven through `read`/`write` against internal URLs:

> `read xd://` → mounted tool listing; `read xd://<tool>` → docs + JSON parameter schema;
> `write xd://<tool>` → execute, `content` is the JSON args object
> — `/data/scratch/harness-survey/omp/packages/coding-agent/src/tools/xdev.ts:1-31`

That leaves **14 top-level callable schemas** — the ten `essential` tools `read, write,
bash, edit, glob, eval, task, hub, learn, manage_skill` (`src/tools/essential-tools.ts:23-34`)
plus four pinned by `XDEV_KEEP_TOP_LEVEL`: `todo, ask, grep, web_search` (`xdev.ts:58-63`) —
and **12 mounted as devices**: `ast_grep, ast_edit, debug, github, lsp, checkpoint, rewind,
security_scan, memory_edit, retain, recall, reflect`. Device args still go through
`validateToolArguments`, and **the schema is returned on mismatch so a malformed call
self-corrects without a round trip** (`xdev.ts:17-19`).

Selected schemas:

| tool | schema | **miss** | class | gate |
|---|---|---|---|---|
| `read` | `path: string` — **that is the entire schema** (`read.ts:619-623`). Every option rides an inline selector grammar on the path: `:50-200`, `:-60`, `:raw`, `:img`, `?q=<question>`, `db.sqlite:table:key`, `archive.zip:inner` (`prompts/tools/read.md:8-28`) | layered recovery (semicolon fan-out → unique-suffix → plan alias) then `Path '<p>' not found` (`read.ts:1443-1491`). Out-of-range line is **non-throwing** graceful text (`:1810-1820`) | R | fn: `exec` for `ssh://` or bare PDF-image, else `read` (`:709-716`) |
| `grep` | `pattern` **req**; `path`?; `case`?; `gitignore`?; `skip`? ("files to skip … use to paginate when the prior call hit the file limit") (`grep.ts:84-94`) | `"No matches found"` (`:1465-1467`) — see §3.1 | R | fn |
| `glob` | `path`?; `hidden`? (true); `gitignore`? (true); `limit`? (`glob.ts:44-51`) | `"No files found matching pattern"` + `.useless()` (`:347,352`); **on timeout it abstains instead** — see §3.1 | R | `read` |
| `ast_grep` | `pat` **req**; `path`?; `lang`?; `skip`? (`ast-grep.ts:41-48`) | `"No matches found"`, or with parse issues, `"No matches found. Parse issues mean the query may be mis-scoped; narrow \`path\` before concluding absence."` (`:300-309`) | R | `read` |
| `ast_edit` | `ops: [{pat, out}]` ≥1; `paths: string[]` ≥1 (`ast-edit.ts:46-58`) | **always dry-runs first** (`:310`) with `PREVIEW_PENDING_NOTICE` — apply by writing a reason to `xd://resolve` (`tools/resolve.ts:46`) | W | fn |
| `edit` | **mode-dependent** (`edit/index.ts:372-385`). Default `hashline`: `{input}` carrying a `*** Begin Patch` / `[PATH#TAG]` / `PUT`/`CUT`/`REM`/`MV` payload with a Lark grammar attached via `customFormat` (`:393-396`) | see §3.2, §3.6 | W | fn |
| `task` | dynamic (`task/types.ts:154-293`). Default: `{context, tasks: [{name?, agent, task, outputSchema?, schemaMode?, tools?}]}`. Note `"+": "delete"` — unknown keys silently stripped | see §3.4 | X | `exec` |
| `eval` | `language: 'py'|'js'`; `code` **req**; `title`?; `timeout`? (30s); `reset`? (`eval.ts:94-116`) | backend disabled → throws with an exact remedy (`:236-248`) | X | `exec`, unconditional |

`browser` and `computer` are **not tools** — they are Python/JS preludes injected into the
`eval` kernel (`sdk.ts:1925-1939`). The model drives a browser through `eval`.

**Gate shape.** Three capability tiers `read(0) < write(1) < exec(2)` and three approval
modes `always-ask` / `write` / `yolo` (`src/tools/approval.ts:31-41`), resolved as tool's
own `approval(args)` decision → user per-tool setting → mode/tier comparison (`:104-119`).
**An omitted `approval` defaults to `exec`** (`:70-71`). `approval` is frequently a
*function of the arguments*, so tier is path-dependent. A plan-mode guard hard-rejects
filesystem mutation (`plan-mode-guard.ts:143-153`).

One structural note that recurs below: `AgentToolResult.details` is *"Details to be
displayed in a UI or logged"* (`packages/agent/src/types.ts:682-684`) — **anything living
only in `details` never reaches the model.** There is also `useless?: boolean`, *"Marks the
result as contextually useless: safe for compaction to elide once consumed (e.g. zero
matches, wait timeout)"* (`types.ts:690-691`).

### 1.4 grok-build

Seventeen core tools (`xai-grok-agent/src/config.rs:245-278`) plus conditionals
(`builder.rs:611-662`) and a plan-mode trio (`builder.rs:98-121`).

**Model-facing names are not tool ids.** The registry renames five tools and two params
before the model sees them (`config.rs:119-141`):

| tool id | name the model sees | param rename |
|---|---|---|
| `run_terminal_cmd` | `run_terminal_command` | `is_background` → `background` |
| `task` | `spawn_subagent` | `run_in_background` → `background` |
| `get_task_output` | `get_command_or_subagent_output` | — |
| `wait_tasks` | `wait_commands_or_subagents` | — |
| `kill_task` | `kill_command_or_subagent` | — |

Descriptions are MiniJinja templates resolved against the *served* names
(`types/tool_metadata.rs:79-92`), and the architecture explicitly supports full name/param
**randomization** per deployment (`registry/types.rs:4748`, `util/remap.rs:6`).

| tool (model name) | class | AccessKind | approval |
|---|---|---|---|
| `read_file`, `list_dir`, `grep` | R | `Read`/`Grep` (`permission/types.rs:250-252`) | auto-allow (`permission/manager/mod.rs:2073-2075`) |
| `search_replace` | W | `Edit(file_path)` (`:267`) | **prompts** (`manager/mod.rs:2090-2102`) |
| `run_terminal_command`, `monitor` | X | `Bash(cmd)` (`:273-274`) | **full bash pipeline** |
| `spawn_subagent` | X (transitive) | `Edit("task:<type>")` (`:261`) | no prompt; presence + depth gated |
| `use_tool` (MCP) | W + N | `MCPTool{…}` (`:275`) | prompts unless allowlisted (`:107-140`) |
| `web_search` | N | `WebSearch(q)` (`:266`) | auto-allow |
| `web_fetch` | N + W | `WebFetch(url)` (`:283`) | domain-gated (`manager/mod.rs:2137-2160`) |
| `lsp` | R | catch-all | auto-allow; **feature off by default** |
| `workflow`, `scheduler_*`, `image_gen`/`image_edit`/`image_to_video`/`reference_to_video` | X / W / N+W | **catch-all `_ => Read(None)`** (`permission/types.rs:284`) | **no prompt** |
| `todo_write`, `update_goal` | W (session) | `Read(None)` (`:256`) | auto-allow |

**Two classification gaps worth naming.** (1) Media generation, the scheduler and
`workflow` have no explicit `AccessKind` arm and fall through the catch-all at
`permission/types.rs:284` — network-calling, file-writing and subagent-spawning tools are
auto-approved as if read-only. (2) Declared scopes are inconsistent in both directions:
`scheduler_list` declares `Write` for a pure read
(`implementations/grok_build/scheduler/list.rs:70-71`), while `todo_write` and
`update_goal` declare `Read` while mutating session state (`todo/mod.rs:295-296`,
`update_goal/mod.rs:230-231`).

**Shell gating is the most developed of the five.** Commands are parsed with
**tree-sitter-bash** (`permission/bash_command_splitting.rs:1-2`), split per chained
segment, with wrappers (`timeout`, `env`, `nice`) peeled (`manager/mod.rs:678-680`), and
**unparseable input fails closed** (`:551`). There is a prefix safe-list (`:323-355`), a
hard always-prompt list — `rm, chmod, chown, chgrp, chattr, pkill, kill, killall, git push`
(`:499-522`) — and an exec-vehicle list that blocks grant-narrowing (`policy.rs:609-624`).
Network egress is separately controlled by per-child seccomp
(`xai-grok-sandbox/src/child_net.rs:178-222`).

But **the OS sandbox defaults to off** (`xai-grok-shell/src/agent/config.rs:1117`), and a
blanket-approve mode exists as `--always-approve` with aliases including
`--dangerously-skip-permissions` (`xai-grok-pager/src/app/cli.rs:446-450`), overridable
only by an admin pin (`permission/resolution.rs:884`). Deny rules are enforced *before* it
(`manager/mod.rs:1677`), and a PreToolUse hook returning `Ask` overrides it (`:1693`).

`search_tool` **does not search code** despite the name — it is BM25 over the MCP tool
index (`implementations/search_tool/mod.rs:1`).

### 1.5 deepseek-harness — dsh 0.1.5-alpha.1

A Cordis "everything-is-a-plugin" monorepo. There is no single tool app: ~25 tools are
separate plugin packages at `packages/<group>/tool-<name>/`, and **which ones the model
sees is a composition decision**.

Three framework facts govern every row:

1. **The model sees only `name` / `description` / `parameters`.** `ToolRuntime.schemaOf`
   whitelists exactly those three (`packages/core/tools/src/index.ts:1246-1256`).
   `output.schema`, `timeoutMs` and `isConcurrencySafe` are never sent (`:242-251`).
2. **Parameter schemas carry no constraints beyond type/enum/const/default/description**
   (`packages/core/tools/src/schema.ts:335,366,401-406`). Every numeric bound and pairing
   rule is an execute-time throw.
3. **Every throw becomes a soft error result** — one text block `Error: <message>`,
   `isError: true` (`core/tools/src/index.ts:1860-1868`). There is no hard failure path.

Composition across the shipped presets:

| tool group | `dsh-base` | `standard` | `ptc` | `minimal` |
|---|---|---|---|---|
| `bash`, `pwsh` one-shot | ✅ | ✅ | ✅ | — |
| `bash`/`pwsh` persistent PTY | — | — | — | ✅ |
| `read`, `write`, `edit`, `read_image` | ✅ | ✅ | ✅ | — |
| `glob`, `grep` | ✅ | ✅ | ✅ | — |
| `str_replace_editor` | — | — | — | ✅ |
| `job_list`/`job_output`/`job_kill` | ✅ | ✅ | ✅ | — |
| `skill`, `todo_write`, goal tools, `exit_plan_mode` | ✅ | ✅ | ✅ | — |
| `subagent`, `subagent_fork`, `send_message`, `interrupt_agent`, `list_agents` | ✅ | ✅ | ✅ | — |
| `workflow`, `ralph` | ✅ | ✅ | ✅ | — |
| `web_search`, `web_fetch` | ✅ | ✅ | ✅ | — |
| `ask_user_question` | — | ✅ | ✅ | — |
| `lsp`, `terminal_*`, `session_*`, `cordis_*`, `schedule_*` | — | — | — | — |
| `run_code` | — | — | **only tool** | — |

**The most distinctive architectural choice in the five.** The `ptc` preset sets
`mode: ptc` (`presets/ptc/agent.cordis.yml:269-272`), under which the model is sent
**exactly one tool schema — `run_code`** — plus a generated TypeScript/Python SDK
declaring the rest. A direct call naming any other tool is refused with

> `only \`run_code\` is callable directly — call \`<name>\` from inside a \`run_code\` program instead`
> — `/data/scratch/harness-survey/deepseek-harness/packages/core/tools/src/index.ts:1426-1433`

Mode is `native` by default (`:648-658`).

Schemas for the fs group:

| tool | schema | success | **miss** |
|---|---|---|---|
| `read` | `file_path` **req**; `offset`? (1); `limit`? (2000, also the max, `read.ts:59`) | XML-ish envelope with numbered lines and a footer naming the next offset (`read-render.ts:152-169`) | `cannot read "<p>": not found` (`read-target.ts:26-29`). **A failed read still records a confirmed-absent observation** (`:27`) — which is what later licenses a `write` |
| `write` | `file_path`, `content` **req** — **plus, under a confining backend, `sandbox_permissions` (enum) and `justification`** spread in at `write.ts:74` | `created file` / `updated file` | see §3.2 |
| `edit` | `file_path`, `old_string` (*"Literal text to replace. Must match exactly."*), `new_string` **req**; `replace_all`? (`edit.ts:82-91`) | `The file <p> has been updated successfully.` (`:64-68`) | see §3.6 |
| `glob` / `grep` | `docs/tool-catalog.md:786-834`; source `glob.ts:310-324`, `grep.ts:281-290`. Both spawn the packaged `@vscode/ripgrep` binary through `ctx.subprocess`, never a shell (`search-core.ts:8-12`) | see §3.1 | see §3.1 |

**Gate shape.** There is a generic `ask` gate (`core/tools/src/index.ts:1465-1489`), but
**almost no tool uses it** — the only producers of `kind: 'ask'` are the external hook
bridges (`packages/hooks/hooks-claude-code/src/index.ts:241`). The real gate is the
**sandbox**: modes `read-only | workspace-write | danger-full-access`
(`packages/sandbox/sandbox/src/index.ts:29`), deployment default `read-only`
(`sandbox-policy/src/index.ts:112`) **overridden to `workspace-write` in the base bundle**
(`cordis.patch.yml:211`). Reads always pass; only write/edit are fenced
(`fs-sandbox/src/index.ts:7,124-141`).

**Escalation is the one approval path the model can trigger, and it is a schema feature.**
Under a confining backend, `write`/`edit`/`bash`/`pwsh` gain `sandbox_permissions` +
`justification` (`tool-fs/src/sandbox.ts:59-73`); supplying them calls
`ctx.approval.request` before anything executes (`:87-108`). A denial is rendered as
`[sandbox: file access denied under <mode> mode]` plus `[sandbox: escalation available —
retry this exact operation once with sandbox_permissions … the approval prompt asks the
user]` (`sandbox/src/escalation.ts:71-86`).

**`web_fetch` has no allowlist.** The gate is in the provider: scheme check, no URL
credentials, **DNS must resolve to a public address**, address-pinned connections,
same-origin redirects only (`web-fetch-http/src/policy.ts:32-36`, `network.ts:101-105`).
**MCP tools pass the server's own description and `inputSchema` through verbatim**
(`mcp-client/src/tools.ts:162-172`) with no approval — mounting the server is the whole
decision.

### 1.6 letibot, for comparison — HEAD `e33cc1b6`

Eight tool types defined in one crate, `crates/tools`. The registry ships **six**
(`read_only_tools`, `src/lib.rs:119-131`) or **eight** (`coder_tools`, `:151-158`); the
daemon today seats only the read-only six (`crates/harnessd/src/harness.rs:290-299`), so
`write` and `edit` are **library-present but not seated in the running daemon**.

| tool | schema | success | **miss** | class |
|---|---|---|---|---|
| `read` | `path` **req**; `offset`?; `limit`? (`builtins/read.rs:22-40`) | `%6d| ` numbered lines + next-offset note (`:126-130`) | missing path → `no file at \`{p}\`` **plus the nearest existing ancestor's listing** and closest names (`:143-156`); a directory → **`Ok`** with its listing (`:160-167`); offset past EOF → **`Ok`**, file from line 1, with a note (`:105-113`) | R |
| `grep` | `pattern` **req**; `path`? (`"."`); `glob`?; `case_insensitive`?; `max_matches`? (200) (`builtins/grep.rs:45-65`) | see §3.1 | see §3.1 | R |
| `glob` | `pattern` **req**; `path`? (`"."`); `limit`? (200) (`builtins/glob.rs:26-44`) | `{n} path(s) match …` | **relaxation ladder** (`:113-156`), then `abstained("no path matches \`{src}\`, and no relaxation of it matches either")` (`:121-123`) | R |
| `ask_code` / `ask_corpus` | `question` **req**; `scope`? (`builtins/retrieval.rs:176-190`) | answer + `sources:` block | three distinct misses, never merged: `NotRun` (no backend, `:121-125`), `Abstained` (not covered, `:253-255`), `Abstained` (zero citations, `:264-277`) | R |
| `read_spill` | `hash` **req**; `offset`?; `length`? (`builtins/read_spill.rs:21-38`) | bytes + pagination note (`:70-74`) | `no spilled output is held under \`{h}\`` + a listing of every hash this session *does* hold (`:82`) | R |
| `write` | `path`, `content` **both req** (`builtins/write.rs:34-52`) | create/overwrite report with ±3 lines of context | absent `content` refused rather than treated as empty (`:62-72`); identical content → **not touched**, "so nothing that watches this file was woken" (`:206-213`) | W |
| `edit` | `path`, `old_string`, `new_string` **req**; `replace_all`? (`builtins/edit.rs:53-86`) | diff report | see §3.6 | W |

`Access` has four values but **no tool declares `Exec` or `Network`**
(`src/schema.rs:11-29`), and the `access` field is deliberately not rendered into the
prompt (`:86-96`). The gate is consulted only for non-read access (`src/runtime.rs:684`);
the default gate is `NoBoundary`, which refuses with **`NotRun` — nobody decided — not
`Denied`** (`:278-294`). `AdjudicatedGate` evaluates a `NEVER_WRITE` refuse-list no
adjudicator can override (`src/adjudicate.rs:697-708` — `.ssh .gnupg .aws .kube .config/gh
.password-store .mozilla .config/google-chrome .config/chromium .git`), then a session
grant, then the adjudicator (`:863-876`). `Unavailable`/`Timeout`/`Cancelled` route to
`on_timeout`, which `request_for` always sets to `Deny` (`:814`, `:928-941`) — never a
silent allow.

---

## 2. Cross-harness matrix, grouped by concept

Rows are concepts, not names. ✅ = present and reachable in a shipped default;
◐ = present but flag-gated, opt-in, or not mounted in any shipped composition;
— = absent.

| concept | opencode | pi | omp | grok-build | dsh | **letibot** |
|---|---|---|---|---|---|---|
| **read a file** | `read` | `read` | `read` | `read_file` | `read` | `read` |
| **list a directory** | folded into `read` (`read.ts:264-297`) | `ls` ◐ | folded into `glob` | `list_dir` | folded into `read` | folded into `read`/`glob` |
| **glob for paths** | `glob` | `find` ◐ | `glob` | via `grep` files-mode | `glob` | `glob` |
| **lexical search** | `grep` | `grep` ◐ | `grep` | `grep` | `grep` | `grep` |
| **structural / AST search** | — | — | **`ast_grep`** | — | — | — |
| **structural / AST rewrite** | — | — | **`ast_edit`** | — | — | — |
| **AST-driven read folding** | — | — | **automatic in `read`** | — | — | — |
| **LSP navigation** | `lsp` ◐ (9 ops) | — | `lsp` (14 actions) | `lsp` ◐ (6 ops) | `lsp` ◐ (4 ops, unmounted) | — |
| **exact-string edit** | `edit` | `edit` | `edit` (`replace` mode) | `search_replace` | `edit` | `edit` |
| **structured patch/diff apply** | `apply_patch` (gpt-* only) | — | `edit` (`hashline`/`apply_patch` modes) | — | — | — |
| **whole-file write** | `write` | `write` | `write` | `write` ◐ (opencode ns) | `write` | `write` |
| **shell exec** | `bash` | `bash`, `powershell` ◐ | `bash` | `run_terminal_command` | `bash`, `pwsh` | **—** |
| **persistent shell / PTY** | — | — | `bash` (persistent by default) | — | ✅ (`minimal` preset) | — |
| **code eval kernel** | `execute` ◐ | — | **`eval`** (py/js, state persists across subagents) | — | `run_code` (the whole `ptc` surface) | — |
| **debugger (DAP)** | — | — | **`debug`** (28 actions) | — | — | — |
| **background jobs / async control** | `task background` ◐ | — | `hub` (12 ops) | `get_/wait_/kill_command_or_subagent` | `job_list`/`job_output`/`job_kill` | — |
| **subagent spawn** | `task` | example ext only | `task`, `vibe_spawn` | `spawn_subagent` | `subagent`, `subagent_fork` | **—** |
| **subagent steering (send message)** | — | — | `hub send`, `vibe_send` | `send_subagent_message` | `send_message`, `interrupt_agent` | — |
| **workflow / orchestration DSL** | — | — | — | **`workflow`** (Rhai) | **`workflow`**, `ralph` | — |
| **todo / task list** | `todowrite` | — | `todo` (9 ops) | `todo_write` | `todo_write` | — |
| **goal tracking** | — | — | `goal` (hidden) | `update_goal` | `create_/get_/update_goal` | — |
| **plan mode** | `plan_exit` ◐ | — | — | `enter_/exit_plan_mode` | `exit_plan_mode` | — |
| **ask the user a question** | `question` ◐ | — | `ask` | `ask_user_question` | `ask_user_question` | — |
| **web search** | `websearch` ◐ | — | `web_search` | `web_search` ◐ | `web_search` | — |
| **web fetch** | `webfetch` | — | via `read` (URL path) | `web_fetch` ◐ | `web_fetch` | — |
| **skills** | `skill` | injected as a user message, not a tool | `learn`, `manage_skill` | `skill` | `skill` | — |
| **cross-session memory** | — | — | `retain`/`recall`/`reflect`/`memory_edit` | `memory_search`/`memory_get` ◐ | — | — |
| **retrieval over an index** | — | — | `recall` | `memory_search` | — | **`ask_code`/`ask_corpus`** |
| **spill retrieval** | via `read`/`grep` on a path | via `read` on a temp path | via `read artifact://` | via `read_file` on a path | via `read`/`grep` on a path | **`read_spill` (dedicated tool, content-hash key)** |
| **context checkpoint / rewind** | — | — | `checkpoint`, `rewind` | — | — | — |
| **MCP tool access** | direct registration | — | via MCP config | `search_tool` + `use_tool` (BM25 index) | `mcp__<server>__<tool>` passthrough | — |
| **scheduler / recurring prompts** | — | — | — | `scheduler_create/_delete/_list` | `schedule_*` ◐ | — |
| **media generation** | — | — | via `eval` preludes | `image_gen`, `image_edit`, `image_to_video`, `reference_to_video` | — | — |
| **security scanning** | — | — | `security_scan` | — | — | — |
| **GitHub** | — | — | `github` (11 ops) | — | — | — |
| **structured output / yield** | — | — | `yield` (hidden, subagent-only) | — | `structured_output` (child-only) | — |
| **an explicit "do not use" decoy** | **`invalid`** (`invalid.ts:12`, listed at `registry.ts:232`) | — | — | — | — | — |

---

## 3. The six questions

### 3.1 Does any search tool distinguish "the pattern is not present" from "I searched zero files"?

**Four of the five: no. The fifth computes the number and does not tell the model.
letibot is the only one that reports the denominator.**

This is the defect that prompted the survey: letibot's `grep` reported an empty corpus as
an empty result — *"does not occur in the searched tree"* about a tree holding nine
matches, because a `glob` matched no paths.

**opencode.** The no-match string is a shared constant:

> `output: "No files found",` — `/home/dead/Projects/opencode/packages/opencode/src/tool/grep.ts:33`

returned from two sites (`grep.ts:69`, `:83`); `glob` emits the identical literal at
`glob.ts:54`. The wording is itself misleading for grep — "No files found" is emitted when
files *were* searched and the pattern did not match. An `include` filter matching no files
makes ripgrep exit 1, which the adapter maps to an empty item list, explicitly not an
error (`packages/core/src/ripgrep.ts:141`), and the model gets the same four words. Worse:
**a `path` that does not exist is not an error either** — `grep.ts:61-62` stats the path,
`info` is `undefined` on failure, and the ternary falls through to `path.dirname(search)`,
so **grep silently searches the parent directory** and reports what it finds there as if it
came from the requested path. And ripgrep's `partial` flag, which the adapter does set
(`ripgrep.ts:141`), is discarded at `grep.ts:63-68` — an incomplete search is reported as a
complete one. Files searched is never computed.

**pi.** `matchCount` is incremented per rg match event (`grep.ts:228`); there is no
files-searched counter in the file. Zero matches returns the bare string
`"No matches found"` (`grep.ts:256-260`). The `glob` argument is passed straight through to
ripgrep (`:165`), rg exits 1 in both cases, and exit code 1 is explicitly treated as
success (`:251`) — so glob-matched-nothing and pattern-absent land in the same branch. A
non-existent *path* does throw `Path not found: <p>` (`:128-133`) before rg is spawned; a
bad *glob* gets no such treatment. Note also that hitting the 100-match cap **kills the rg
child** (`:234-237`), so the search is abandoned rather than completed, and the model is
not told how much went unsearched.

**deepseek-harness.** `grep` zero-results returns `No matches found`
(`packages/fs/tool-fs-search/src/grep.ts:229`); `glob` returns `No files found`
(`glob.ts:232`). The denominator is **available and deliberately dropped**: `parseRecord`
skips ripgrep's `summary` record explicitly (`grep.ts:143-145`) and `buildGrepCommand`
never passes `--stats` (`:111-116`). An `include` glob matching zero files also exits 1, so
it is byte-identical to a genuinely absent pattern. What *is* distinguished is a
non-existent path argument — rg exits 2, which becomes
`grep search failed (exit 2): <stderr>`, code `SEARCH_FAILED` (`search-core.ts:124-129`).
So a bad path is loud; a bad glob is silent.

**grok-build.** The conflation is explicit in the source. At
`crates/codegen/xai-grok-tools/src/implementations/grok_build/grep/mod.rs:973-980`, the
branch that produces `"No matches found"` fires on **either** rg exit 1 with empty stdout
**or** rg exit 2 whose stderr contains the literal `"No files were searched"`. ripgrep's
zero-denominator signal is folded into the same string as a genuine zero-match search.
There is no test for the glob-matched-nothing case. Two further denominator shrinkers are
silent: `.gitignore` is respected (told to the model only in prose, `grep/mod.rs:250`), and
policy `DenyReadGlobs` are injected as `--glob !…` excludes with no disclosure (`:745-751`).
What grok-build *does* do best is the path case: a pre-check before rg runs returns
`Error: {path} does not exist.` plus a "Did you mean {p}?" correction and up to three
similar sibling names (`grep/mod.rs:693-719`, `util/path_suggestions.rs:32-51,105-117`).
Timeout is also honest: *"Ripgrep search timed out after {secs} seconds. The search may
have matched files but did not complete in time…"* (`grep/mod.rs:946-953`).

**omp — computes it, withholds it.** `filesSearched` is threaded through the entire search
pipeline (`grep.ts:646,726,740,1210,1230,1315`), but **`GrepToolDetails` has no
`filesSearched` field** (interface at `grep.ts:874-902`), and the zero-match branch emits
only `"No matches found"` (`:1465-1467`). Since `details` is UI-only
(`packages/agent/src/types.ts:683-684`), the number never reaches the model. `ast_grep` is
worse-tempting: `AstGrepToolDetails.filesSearched` **is** populated (`ast-grep.ts:290`) and
**is** rendered — but only into the TUI status line for the human (`:459,466,479`); the
model-facing text assembly (`:397-412`) never includes it.

omp is nonetheless the only one of the five that thinks about the problem at all, in three
narrower places:

- A **glob whose base directory exists but whose pattern matched zero files** returns
  `"No matches found"` — identical to pattern-absent (`path-utils.ts:1640-1644` sets the
  filter and never validates it).
- **Missing paths are surfaced.** All paths missing → throws
  `Path not found: {missing}; list each target in the semicolon-delimited \`path\``
  (`grep.ts:1128-1139`); *some* paths missing → results **plus**
  `Skipped missing paths: {…}` appended to the model text (`:1447-1448,1583-1585`).
- **A glob timeout with zero hits is an explicit abstention, and the code says why.** The
  `"No files found matching pattern"` claim is *suppressed* and replaced with: *"Glob timed
  out after Ns before finding any matches — the scan is incomplete, NOT proof of absence.
  The walk is bounded by directory size, not pattern width; scope the search to a deeper
  directory…"* (`glob.ts:559-562`), guarded by a comment at `glob.ts:344-347` reading
  *"never emit the definitive 'No files found' claim next to a timeout notice (the two
  statements contradict each other)"*. Files over 4 MiB and 30-second grep timeouts are
  likewise reported rather than silently shrinking the corpus (`grep.ts:1408-1431`,
  `:1261-1265`).
- `ast_grep` with zero matches **and** parse errors says so: *"Parse issues mean the query
  may be mis-scoped; narrow `path` before concluding absence."* (`ast-grep.ts:301-306`).

**letibot — the denominator is reported, and a zero denominator is a scope failure.**
Three outcomes, three different classes (`crates/tools/src/builtins/grep.rs`):

- **Corpus empty, zero files opened** → `Invocation::failed`, explicitly not an abstention
  (`:207-222`). Reason: `` 0 files matched the scope, so `{source}` was never searched for ``
  (`:219`). Body: `` nothing under `{scope}` was opened, so this call says NOTHING about whether `{source}` occurs. Fix the scope and ask again. `` (`:216`). When a `glob` was
  supplied it names the specific defect: `` `glob` is matched against each file's PATH from the session root, not its name, so `{g}` selects nothing under a subdirectory. Try `**/{…}` or drop `glob` and narrow with `path`. `` (`:210-213`).
- **Pattern genuinely absent** → `Invocation::abstained` (`:227-246`). Reason:
  `` `{source}` does not occur in the {files_scanned} file(s) searched `` (`:243`).
- **Scope path does not exist** → `Failed("no directory at \`{scope}\`")` with the nearest
  listing (`:101-111`).

The denominator is counted at `:284-297` — `scanned` increments only after a successful
read, and binary files are skipped **without incrementing**, so it means *files actually
opened and examined as text*. Guarded by tests at `:409` and `:430`.

### 3.2 Does any harness ENFORCE read-before-write, rather than promising it?

**Three of five do not enforce it. Two of those three claim in a description string that
they do. Two enforce it — by different mechanisms, and both have a hole.**

| harness | enforced? | mechanism | where |
|---|---|---|---|
| opencode | **no** — and falsely claimed | none | `edit.txt:4`, `write.txt:5` |
| pi | **no** — and not even claimed | none | `edit.ts:159-212` |
| grok-build | **no** — and says so in a code comment | config-time toolset check only | `search_replace/mod.rs:803-806` |
| omp | **yes**, in the default mode | **content digest** + session snapshot store | `pi-edit/src/modes/hashline/patcher.rs:186` |
| dsh | **yes** | **stat-tuple CAS** + session observation map | `fs-local/src/index.ts:181-196` |
| **letibot** | **yes** | **content digest** on a runtime-level session ledger | `crates/tools/src/files.rs` |

**opencode — the claim, verbatim:**

> `- You must use your \`Read\` tool at least once in the conversation before editing. This tool will error if you attempt an edit without reading the file.`
> — `/home/dead/Projects/opencode/packages/opencode/src/tool/edit.txt:4`

> `- If this is an existing file, you MUST use the Read tool first to read the file's contents. This tool will fail if you did not read the file first.`
> — `/home/dead/Projects/opencode/packages/opencode/src/tool/write.txt:5`

Both are false. `edit.ts:69-212` checks non-empty path, oldString≠newString,
external-directory, empty-oldString, existence, not-a-directory — then reads the file
itself (`:126`) and writes (`:155`). **No consultation of conversation history, no read
registry, no mtime, no content hash.** `write.ts:38-101` checks external-directory and
nothing else. Repo-wide searches for `FileTime`, `lastRead`, `hasRead`, `readBeforeWrite`
return zero hits; `mtime` appears twice in the whole tool tree and both are spill-file GC
(`truncate.ts:62-63`). `ctx.messages` is available to tools (`tool.ts:43`) but only
`read.ts:300` touches it, for AGENTS.md resolution.

Adjacent nuance: the **unwired v2 tree** does implement a stale-content guard —
`packages/core/src/tool/edit.ts:115` maps a `StaleContentError` to *"File changed after
permission approval. Read it again before editing."* — but that is a TOCTOU guard between
approval and write, not a read-before-write requirement, and it is not in the tool set the
CLI exposes.

**pi — not enforced, and not claimed.** The complete `edit` path
(`src/core/tools/edit.ts:159-212`) is: validate → `resolveToCwd` →
`withFileMutationQueue` → `ops.access(path)` (an `R_OK|W_OK` permission-bit check whose
result is **discarded**, `:95,176`) → `readFile` **now** (`:186`) → match and apply →
`writeFile` (`:198`). No mtime, no digest, no session map, no read-state parameter.
`write.ts:58-89` never reads the target at all. Repo-wide grep for
`has not been read|read the file first|modified since|readFileState|mustReadFirst` across
both packages returns nothing. Tellingly, `getFileRevision()` — which composes exactly the
fingerprint one would want, `dev:ino:size:mtimeNs:ctimeNs` — exists at
`src/utils/paths.ts:36-43` and **every call site is the agent's own config stores**
(`core/models-store.ts:77,87`, `core/auth-storage.ts:341,383,…`). Zero tool call sites. The
only protection is `withFileMutationQueue` (`file-mutation-queue.ts:32-60`), which
serializes the agent's own writes on a realpath-canonicalized key and does nothing about an
external change landing between read and edit. Neither `edit.ts:151-152` nor
`write.ts:52-53` mentions reading first — there is no promise to break.

**grok-build — says so out loud.** The requested sites, verbatim:

> ```
> // Unless `skip_read_before_edit` is set, require a Read tool in the toolset
> // (read-before-edit is encouraged via description and RL grading, not runtime-enforced).
> ```
> — `crates/codegen/xai-grok-tools/src/implementations/grok_build/search_replace/mod.rs:803-806`

> ```
> /// Consecutive edits to the same file succeed without any prior read.
> #[tokio::test]
> async fn consecutive_edits_succeed_without_prior_read() {
> ```
> — same file, `:1023-1025`

The test writes a file, runs two edits with no read between, and asserts both return
`EditsApplied` (`:1035-1057`). Three corroborations: `requires_expr` is evaluated **once at
toolset finalize time** over the proposed toolset (`registry/types.rs:854-861`), checking
only that *a Read-kind tool exists in the toolset*, never that a read happened; the config
knob is documented as `/// Deprecated runtime no-op…` (`search_replace/mod.rs:99-102`); and
a repo-wide grep for read-tracking state returns only `skip_read_before_edit`. The `write`
tool is the same — it reads old content only to populate a notification, then writes
unconditionally (`implementations/opencode/write/mod.rs:116-142`) — despite its description
saying *"read it first with the ${{ tools.by_kind.read }} tool"* (`:22`). The compensating
mechanism is a hint on failure: *" The user may have changed the file since you last read
it."* appended to every `NoMatchesFound` (`search_replace/mod.rs:636-640`).

**dsh — enforced, by a stat tuple, in three layers.**

1. **Session-state map.** `packages/fs/fs-observation-policy/src/index.ts:28` holds
   `WeakMap<owner, Map<targetKey, FsObservation>>`, keyed on `actor.agent.session`
   (`:36-41`). An entry is `{kind:'present', version}` or `{kind:'absent'}`, and **absence
   of an entry is a third, distinct state** (`:22-28`).
2. **Intent decision per operation.** `writeIntent` (`:65-71`): unseen or confirmed-absent
   → `createIfAbsent`; confirmed-present → `replaceIfVersion`. `editIntent` (`:78-88`):
   unseen → **throws `FsError('edit requires reading "<path>" first', 'FS_NOT_OBSERVED')`**
   at `:82`.
3. **Provider-side compare-and-swap inside a per-target lock**
   (`packages/fs/fs-local/src/index.ts:181-196` write, `:236-248` edit). The version is
   **neither pure mtime nor a content digest** — `versionOf` builds
   `dev:ino:size:mtimeNs:ctimeNs` from `BigIntStats` (`fs-local/src/fsio.ts:73-76`). It
   catches truncate-and-rewrite, inode swap and same-size edits; it does not catch a write
   that restores identical stat metadata. Mismatch → `FS_STALE_VERSION`
   *"file changed since it was read"* (`:190-192`). `createIfAbsent` onto an existing file →
   `FS_NOT_OBSERVED` *"cannot overwrite existing "<p>" without reading it first"* (`:193-196`)
   — **so an unread `write` to an existing file is refused, not merely discouraged.**

Model-facing remedies at `packages/fs/tool-fs/src/error.ts:21-34`. **The holes:**
enforcement lives in a *separate plugin* whose own comment says removing it "leaves the
bare provider's unconditional mutation behavior"
(`fs-observation-policy/src/index.ts:4-5`); and the `minimal` preset uses `fs-local` in an
isolated realm **without** the policy (`presets/minimal/agent.cordis.yml:74-83`).

**omp — enforced, by content digest, in the default mode.** `DEFAULT_EDIT_MODE =
"hashline"` (`src/utils/edit-mode.ts:6`). A hashline payload must carry a `[path#TAG]`
header per file — a 4-hex xxhash of the LF-normalized content, minted into a
**session-scoped** store by `read`, `grep` and `write` (`crates/pi-edit/src/store.rs:26-35,
156-202`). The check itself is at
`/data/scratch/harness-survey/omp/crates/pi-edit/src/modes/hashline/patcher.rs:186` — a
hash of live content compared against the tag the model supplied, not an mtime.

The rejection **distinguishes two cases using the session store**, which mtime could not
(`modes/hashline/mismatch.rs:70-104`): a tag minted this session → *"file changed between
read and edit… If a prior edit in this session modified this file, copy the [path#newhash]
header from that edit's response; otherwise re-read the file"*; a tag unknown to the store
→ *"hash #{x} is not from this session… never invent the tag and never reuse one from a
prior session."*

**Three holes, all real.** (a) A head/tail-only edit **applies anyway** on mismatch, with a
drift warning (`patcher.rs:224-233`). (b) The genuinely stronger guard —
`assert_seen_lines`, which rejects an edit anchored on lines a prior read never *displayed*
(`patcher.rs:111-174`; a summarized read mints a tag but does not mark elided lines seen) —
is wired to `edit.enforceSeenLines`, **`default: false`** (`settings-schema.ts:3655-3663`).
(c) The `replace` mode has **no read-before-write check at all**
(`crates/pi-edit/src/modes/replace.rs:31-69`), and `resolveEditMode` silently downgrades
hashline → replace for the `kimi`, `mimo`, `deepseek` and `stepfun` model classes unless
`PI_STRICT_EDIT_MODE` is set (`utils/edit-mode.ts:43-53`). **On a DeepSeek-class model, omp
has no read-before-write enforcement.**

**letibot — content digest on a runtime-level ledger.** `crates/tools/src/files.rs` is the
whole mechanism, and its module doc (`:1-50`) names this exact gap in opencode and
grok-build. State is a `FileLedger` on the *runtime*, not on a tool (`:77-80`;
`runtime.rs:604-606` — "read-before-write is a fact about the *session*, not about `edit`"),
handed to every tool through `InvokeCtx.files` (`runtime.rs:112-116`) so two tools cannot
hold two ledgers. The key is `Seen { digest, bytes, whole_file }` (`files.rs:56-70`), digest
= SHA-256 truncated to 16 hex, taken over the **whole file** at read time
(`:58-59`; recorded at `builtins/read.rs:80-81`). The rationale for a digest over mtime is
written down at `files.rs:28-35`: a `touch`, a checkout restoring identical content, and a
formatter that rewrote a file to what it already was all move mtime without changing a
byte. Paths are normalized so `./src//lib.rs` and `src/util/../lib.rs` collapse to one key
(`:135-147`).

Enforcement is at `builtins/edit.rs:425-490` and `builtins/write.rs:168-204`, both checking
*both* conditions (never-read, and read-but-digest-moved). **The design choice that makes it
cheap:** both refusing paths **record the content they are refusing over** before returning
(`edit.rs:435,470`, `write.rs:198`) and return the file in full, numbered — so the guard
costs one call and the identical retry proceeds. Neither omp nor dsh does this; both make
you re-read.

### 3.3 Structural / AST tools

**One of the five ships a structural tool. Four ship an LSP tool, three of those off by
default or unmounted. Nobody but omp has ast-grep or tree-sitter reaching the model.**

**omp — the only real prior art, and it is three layers.**

- **Engine:** `ast-grep-core = { version = "0.39", … }` (`/data/scratch/harness-survey/omp/Cargo.toml:402`)
  plus `tree-sitter` and ~58 grammar crates (`crates/pi-ast/Cargo.toml:14-75`). The
  `SupportLang` enum enumerates **58 languages** (`crates/pi-ast/src/language/mod.rs:270-328`),
  with an extension map at `:548-611` and a `lang`-override alias table at `:670-829`.
- **Layer 1 — structural search and rewrite.** `ast_grep` (`src/tools/ast-grep.ts:161`),
  described to the model as *"Search code with AST patterns (structural grep)"* (`:164`),
  schema `{pat, path?, lang?, skip?}` (`:41-48`), emitting an optional `meta: KEY=value`
  line for metavariables. `ast_edit` (`:180`), *"Perform AST-aware code edits (structural
  refactoring)"* (`ast-edit.ts:205`), schema `{ops: [{pat, out}], paths: []}` (`:46-58`) —
  and it **always dry-runs first** (`:310`), staging a proposal the model must confirm by
  writing a reason to `xd://resolve` (`tools/resolve.ts:46`).
- **Layer 2 — AST-driven `read` folding, not a queryable outline.**
  `crates/pi-ast/src/summary.rs:14-63` defines a tree-sitter walk folding large bodies and
  comments into `elided` segments with BFS progressive unfolding. Wired into `read` at
  `src/tools/read.ts:1665-1697`, `read.summarize.enabled` **default true**
  (`settings-schema.ts:3727-3729`). The model is told: *"Parseable code, no selector →
  structural summary (declarations only, body elided). Footer names recovery selector —
  re-issue ONLY those ranges."* (`prompts/tools/read.md:19`). **This is automatic
  compression, not a symbol query** — there is no parameter to request it and no way to ask
  for "the symbols in this file" as a list.
- **Layer 3 — symbol search is LSP-backed.** `lsp` with `action: "symbols"` gives
  `textDocument/documentSymbol` with a `file`, or `workspace/symbol` across all live servers
  with a `query` (`lsp/tool.ts:983-1071,1433-1462`).

**The gap in omp's own AST layer:** `ast_grep` cannot distinguish "language unsupported"
from "file unparseable" from "pattern absent" — all three collapse into the same zero-match
plus `parse_errors` string list (`crates/pi-natives/src/ast.rs:685-689,725-730`), and
`ast-edit.ts` always passes `failOnParseError: false` (`:312,456`) so the distinction never
becomes a branchable error. It is the same class of defect as §3.1, one layer up.

**LSP, across the four that have it:**

| harness | tool | operations | reachable by default? |
|---|---|---|---|
| opencode | `lsp` | 9: `goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, prepareCallHierarchy, incomingCalls, outgoingCalls` (`lsp.ts:11-21`) | **no** — `OPENCODE_EXPERIMENTAL_LSP_TOOL` (`registry.ts:247`, `runtime-flags.ts:45`) |
| grok-build | `lsp` | 6: `goToDefinition, findReferences, hover, goToImplementation, documentSymbol, workspaceSymbol` (`implementations/lsp/types.rs:106-115`) | **no** — `Feature::LspTools` `default_enabled: false` (`xai-grok-config-types/src/registry.rs:97-104`), *and* needs a hand-written `lsp.json`, *and* the project-local file is folder-trust gated |
| dsh | `lsp` | 4: `goToDefinition, findReferences, goToImplementation, hover` (`packages/lsp/tool-lsp/src/index.ts:109-123`) | **no** — not mounted in `dsh-base` or any of the four presets; appears only in a snapshot fixture |
| omp | `lsp` | 14 actions incl. `symbols`, `rename`, `request` (`lsp/types.ts:8-22`) | yes, as an `xd://` device |

Two schema notes worth carrying forward. opencode's schema **forces `line` and `character`
to be supplied even for `documentSymbol` and `workspaceSymbol`**, where they are then
discarded (`lsp.ts:23-35` vs `:50-55,69-71`). grok-build takes 0-indexed input and emits
1-based output (`implementations/lsp/types.rs:137` vs `format.rs:45-46`) — a real foot-gun.
dsh's seam is explicit about what it left out: *"Symbols and call hierarchy are not
operations here; they need different schemas"* (`packages/lsp/lsp/src/types.ts:16`).

**opencode, pi, grok-build, dsh: no ast-grep, no tree-sitter tool.** Verified by repo-wide
grep in each. Two of them use tree-sitter for something else entirely, and the inversion is
worth noting: **both use AST parsing to police the model rather than to help it.** opencode
loads bash and PowerShell grammars inside the bash tool to parse the *command line* for
permission-pattern extraction (`shell.ts:9,311-336`); grok-build parses shell commands with
tree-sitter-bash in the permission gate (`permission/bash_command_splitting.rs:1-2`).
grok-build's tree-sitter-backed `xai-codebase-graph` exists but reaches only the **editor**
over ACP extension methods (`xai-grok-shell/src/extensions/code_nav.rs:1-13`) — the tools
crate has no dependency on it, and a crate that cannot link the library cannot expose it.

**letibot: nothing shipped and nothing planned.** A repo-wide search for
`tree-sitter|tree_sitter|ast-grep|ast_grep` across `*.rs`, `*.toml`, `*.md` returns exactly
one hit, and it is a negative — `crates/ui/src/highlight.rs:82` says the highlighter should
be *replaced by* syntect or a tree-sitter grammar. `crates/tools/Cargo.toml:9-24` lists four
dependencies (`letibot-transcript`, `serde`, `serde_json`, `sha2`) and the comment at
`:18-20` states the crate deliberately has no `regex` either. Nothing in `TODO.md`,
`DECISIONS.md` or `docs/*.md` proposes an outline, symbol-search or AST tool. The nearest
thing in spirit is `crates/tools/src/builtins/pattern.rs`, whose module doc (`:4-21`) argues
its hand-written regex subset is chosen *because* the relaxation ladder is a property of the
pattern's syntax tree, and that an unsupported construct is reported back rather than
silently approximated (`:17-21`, surfaced at `grep.rs:167-172`).

### 3.4 Subagent / task tools

**Four of five ship one. pi ships an example. letibot has none.**

| | opencode | pi (example ext) | omp | grok-build | dsh |
|---|---|---|---|---|---|
| tool name | `task` | `subagent` | `task`, `vibe_spawn` | `spawn_subagent` | `subagent`, `subagent_fork` |
| agent selected by | `subagent_type` param | `agent` param | `agent` field per task | `subagent_type` param | **the tool name** — backend bound at load |
| child tool set | derived, denies added | per-agent frontmatter allowlist | per-agent frontmatter allowlist | three stacked layers | **parent's composition, unfiltered by default** |
| result | last text part, XML-wrapped | flattened text | `<task-result>` envelope | text notice + poll tool | last assistant message |
| **concurrency ceiling** | **none** | **4** | **32** | **32** | **none** |
| **depth ceiling** | **1** | n/a | **2** | **1** | **3** |

**opencode.** Schema `{description, prompt, subagent_type, task_id?, command?, background?}`
(`task.ts:43-62`) — `background` is **hidden unless** `OPENCODE_EXPERIMENTAL_BACKGROUND_SUBAGENTS`,
because `task.ts:366` overrides the advertised JSON schema with a base set that omits it. The
description is augmented at advertise-time with a generated roster of invokable agents
(`registry.ts:265-278`). Child restrictions are layered as unconditional denies
(`task.ts:143-155`): `todowrite` denied unless explicitly granted, `task` denied unless
explicitly granted — **so subagents cannot spawn subagents by default.** The ceiling,
quoted:

> `if (depth >= (cfg.subagent_depth ?? 1)) {` — `/home/dead/Projects/opencode/packages/opencode/src/tool/task.ts:111`
>
> `` `Subagent depth limit reached (${cfg.subagent_depth ?? 1}). Increase "subagent_depth" to allow nested subagents.` `` — `:114`

**There is no concurrency ceiling** — no semaphore, no queue, no max-in-flight anywhere in
`task.ts`; the only limiter is depth, and `task.txt:13` actively encourages parallel calls.

**pi.** No built-in tool. The example at `examples/extensions/subagent/index.ts` registers
one, and it is the only place in pi with admission control. Quoted:

> `const MAX_PARALLEL_TASKS = 8;` — `/data/scratch/harness-survey/pi/packages/coding-agent/examples/extensions/subagent/index.ts:33`
>
> `const MAX_CONCURRENCY = 4;` — `:34`

Over-limit is a refusal returned as *content*, not an error (`:605-613`); fan-out is
throttled by `mapWithConcurrencyLimit` (`:645`); per-task output cap 50 KiB (`:36`). The
child is a **separate `pi` process** spawned with `["--mode","json","-p","--no-session"]`
(`:300`) and the tool allowlist forwarded as `--tools` (`:307`), so isolation is
process-level. Note that pi's **built-in** tool concurrency has no ceiling at all —
`executeToolCallsParallel` fires everything through an unbounded `Promise.all`
(`packages/agent/src/agent-loop.ts:547-550`).

**omp.** Schema is dynamic (`task/types.ts:154-293`); default shape
`{context, tasks: [{name?, agent, task, outputSchema?, schemaMode?, tools?}]}`. Bundled
agents and their **allowlists**, from frontmatter: `scout` = `read, grep, glob, web_search`
(`prompts/agents/scout.md:4`); `reviewer` = `read, grep, glob, bash, lsp, web_search,
ast_grep`, may spawn `scout` only (`reviewer.md:4-5`); `security-reviewer` = `read, grep,
glob, lsp, ast_grep`; `task` and `sonic` declare none and therefore get the **full** set
(`task/agents.ts:47-71`). The mechanism is an **allowlist, not a denylist** — an absent key
means full access (`task/executor.ts:3103-3111`). Ceilings, quoted:

> `"task.maxConcurrency": { type: "number", default: 32, … }` — `/data/scratch/harness-survey/omp/packages/coding-agent/src/config/settings-schema.ts:5071-5073`
>
> `"task.maxRecursionDepth": { type: "number", default: 2, … }` — `:5104-5106`

Concurrency is a real per-tool `Semaphore` (`task/index.ts:632-640`); depth is enforced **by
omission** — the `task` tool simply disappears from the child's schema at the limit
(`tools/index.ts:663-665`). There is also a soft per-child request budget
(`"task.softRequestBudget"`, default 200) whose documented behaviour is *"Crossing it
injects a wrap-up steering notice; at 1.5x the budget the run is force-stopped and the agent
must yield its partial findings"* (`settings-schema.ts:5153-5161`). Results arrive
asynchronously (`async.enabled` default true) as an injected system-notice turn
(`session/async-job-delivery.ts:13,110`); the parent's context receives **only** the
`<task-result>` envelope, never the child's tool calls (`task/types.ts:59-79`). Over 5000
chars the envelope switches from `<output>` to `<preview full-output="agent://<id>">`
(`task/result-summary.ts:54-55`).

One asymmetry: vibe workers get `taskDepth: session.taskDepth ?? 0` **unincremented**
(`vibe/runtime.ts:1335`), unlike `task`'s `parentDepth + 1`, and no concurrency ceiling for
`vibe_spawn` was found.

**grok-build.** Schema at `xai-tool-types/src/task.rs:13-110`: `prompt` **req**,
`description` **req**, `subagent_type` (default `"general-purpose"`), `run_in_background`
(**default `true`**, `:143-145`), `isolation` (`"none"|"worktree"`), `resume_from`, `cwd`,
`model`. Two fields are `#[schemars(skip)]` and hidden: `capability_mode` and `task_id`
(`:49-51,107-109`). Child tool sets are restricted in **three stacked layers**: definition
(`explore` = **only** `read_file`, `list_dir`, `grep`, `xai-grok-agent/src/config.rs:349-358`;
`plan` = those plus `todo_write`, with the comment *"search_replace and run_terminal_command
intentionally omitted (read-only)"*, `:362-374`), policy
(`xai-grok-subagent-resolution/src/definition.rs:221-239`, which strips `Task` at max depth
and **strips `workflow` from every child unconditionally**), and an extra guard so
`send_subagent_message` is never in a child toolset (`builder.rs:671-680`). Ceilings,
quoted:

> `pub const DEFAULT_MAX_CONCURRENT: usize = 32;` — `crates/codegen/xai-grok-tools/src/implementations/grok_build/task/admission.rs:6`
>
> `pub const MAX_SUBAGENT_DEPTH: u32 = 1;` — `…/grok_build/task/mod.rs:40`

At the concurrency limit the default is **Queue, not fail** (`admission.rs:10-14`);
`GROK_SUBAGENT_LIMIT_BEHAVIOR=fail` yields *"Concurrent subagent limit reached: {limit}
subagents are already running for this session. Do not retry…"* (`:83-87`). **Workflow-owned
spawns bypass admission entirely** (`:108-111`), and the spawn queue itself is an unbounded
`VecDeque` (`task/coordinator/queue.rs:19-35`). `workflow` carries its own budgets, quoted
in its own description: *"`agent_budget`, an absolute cap on cumulative child-agent calls…
default 128. The host also caps live children per run (32 by default, host-configured)"*
(`workflow/mod.rs:277`), hard max 1024 (`:158`).

**dsh.** Schema `{description, prompt, run_in_background?}` plus `provider`/`model`/
`reasoning_effort` when `modelSelectionSettings` is on (`tool-subagent/src/index.ts:389-428`).
**There is no agent-type argument** — the backend is bound at *load* time via
`config.provider` + `config.toolName` (`:48-55`), which is why the composition registers
four separate tools. **The child gets no restricted tool set by default**: it joins the
parent's composition (`subagent/src/child-agent.ts:204`) and a filter applies only if
configured (`:217`), and **no shipped composition sets `toolFilter`**. The real boundary is
a *policy pin* — the child's approval policy is forced to `'never'` (`:242-247`), making any
`ask` a deterministic deny, and the child is told so in a fixed prompt line (`:171-175`).
**There is no concurrency ceiling for `subagent`.** The nearest cap is on background *jobs*
(10 per owner, `packages/jobs/jobs-local/src/index.ts:28,92-97`), and a continuable subagent
does not use a job slot. `maxDepth` defaults to 3 (`tool-subagent/src/index.ts:129`,
enforced `child-agent.ts:49-56`, durable in the session header so a resume cannot reset it).
The workflow engine's ceilings, quoted:

> ```
> maxConcurrentAgents: z.natural().default(0),
> maxTotalAgents: z.natural().min(1).default(1000),
> maxItemsPerCall: z.natural().min(1).default(4096),
> ```
> — `/data/scratch/harness-survey/deepseek-harness/packages/workflow/workflow-worker-thread/src/index.ts:115-122`

`0` auto-resolves to `Math.min(16, Math.max(1, availableParallelism() - 2))` (`:151-152`).
**But the workflow engine passes no `maxDepth` at all** (`host.ts:355-368`) and workflow
children inherit the full tool set including `workflow` — an unbounded nesting path with no
counter-measure found.

**letibot: no subagent tool exists.** The name appears only as a string in role definitions
— `roles::orchestrator()` names `task` (`crates/tools/src/runtime.rs:326`) and
`roles::coder()` names `bash` (`:339`) — and **neither role can be seated**, because
`Registry::resolve_role` returns `RoleError::Unknown` for a role naming a tool the build
lacks (`:561-574`, doc at `:318-319`). What actually seats is `m1_orchestrator()` (`:365-377`)
and `m2_coder()` (`:386-391`). Subagents are milestone M6
(`docs/implementation-plan.md:2694-2700`), and the plan is explicit that the ceiling should
be *"a **configured** number the operator can move, reported in `EXPLAIN`, not an unbounded
fan-out"* (`:1422-1423`) — which, against opencode and dsh both shipping **no** ceiling, is
the right instinct. The propagation rule a subagent would need already exists and is tested:
`propagate()` at `crates/tools/src/result.rs:229-262` — a caller all of whose calls abstained
is `Must(Abstained)`; a caller with no `Ok` and a mix of failures is `Must(Failed)` rather
than being dressed as an abstention.

### 3.5 Output capping

**All five cap. Four of five spill to a file or URL and tell the model where. The
interesting differences are in what is capped *silently* and what has no recovery path.**

| | opencode | pi | omp | grok-build | dsh | letibot |
|---|---|---|---|---|---|---|
| universal wrapper | **yes** (`tool.ts:131-143`) | no — per-tool | **yes** (`output-meta.ts:724-953`) | no — per-tool | **yes** (`spill-policy`) | **yes**, but **`NoBudget` by default** |
| line cap | 2000 (`truncate.ts:14`) | 2000 (`truncate.ts:11`) | 3000 (`streaming-output.ts:9`) | 1000 read (`context.rs:3`) | 2000 read | none by default |
| byte cap | 50 KiB (`truncate.ts:15`) | 50 KiB (`truncate.ts:12`) | 50 KiB (`streaming-output.ts:10`) | 40 000 (`lib.rs:7`) | 50 000 (`cordis.patch.yml:383-386`) | none by default |
| spill target | `<data>/tool-output/<toolID>`, 7-day GC (`truncate.ts:12,68-73`) | `/tmp` file (`output-accumulator.ts:19-22`) | `artifact://<id>` | a file path | a 0700 per-session path | content-hash in a store |
| how to fetch the rest | `read`/`grep` on the path, *"delegate to the explore agent"* | `read` with the next `offset`; or the temp path | `read artifact://<id>` | `read_file`/`grep` on the path | `read` with offset/limit, or `grep` the path | **`read_spill(hash, offset, length)`** |

**opencode.** `Truncate.output` wraps every tool unless the tool sets `metadata.truncated`
itself. The full text is **always** written to disk before truncation (`truncate.ts:68-73`),
and the model is told, with the path and with different advice depending on whether it has
a `task` tool (`truncate.ts:129-131`):

> *"The tool call succeeded but the output was truncated. Full output saved to: <file>\nUse the Task tool to have explore agent process this file with Grep and Read (with offset/limit). Do NOT read the full file yourself - delegate to save context."*

The spill directory is auto-added to each agent's `external_directory` allow-list so the
model can actually reach it (`agent.ts:296-311`) — a detail three others get wrong by
omission. `read` offers explicit pagination (`Use offset=<next> to continue.`,
`read.ts:345`). **`grep` and `glob` have a hard 100-row cap with no pagination and no
offset parameter** — the only advice is "use a more specific path or pattern"
(`grep.ts:101`, `glob.ts:60`) — and truncation is inferred from `rows.length === limit`, so
exactly 100 real matches are reported as truncated.

**pi.** Two shared constants (`truncate.ts:11-13`), two directions: `truncateHead` for
files, `truncateTail` for shell output — read keeps the beginning, bash keeps the end.
Markers are good and every one names the way forward: `[Showing lines X-Y of N. Use
offset=Z to continue.]` (`read.ts:163`), `[Showing lines X-Y of N. Full output: <path>]`
(`bash.ts:328`), `100 matches limit reached. Use limit=200 for more, or refine pattern`
(`grep.ts:288-290`). A single over-long line is **dropped entirely** and replaced with a
redirect to another tool: `[Line N is <size>, exceeds 50.0KB limit. Use bash: sed -n 'Np'
<path> | head -c 51200]` (`read.ts:155`). Two caveats: per-line grep truncation tells the
model twice (inline `... [truncated]` plus a summary notice), and `find`/`ls` set
`resultLimitReached` on `>=`, so a directory holding exactly 500 entries reports its limit
reached though nothing was dropped (`find.ts:275`, `ls.ts:115`) — a false positive, biased
safe.

**omp.** The universal backstop spills anything over `tools.artifactSpillThreshold`
(default **50 KB**, `settings-schema.ts:875-877`) to an artifact, keeping a head window of
`tools.artifactHeadBytes` (default 20 KB, `:919-921`) alongside the tail so the *middle* is
what gets elided. The notice is the most informative of the five —
`formatTruncationMetaNotice` (`output-meta.ts:485-542`) emits e.g. *"Showing lines 1-200 and
4000-4210 of 4210; 3,800 middle lines (142 KB) elided"* and appends **both** `. Use
:<nextOffset> to continue` and `. Read artifact://<id> for full output` (`:431-433`).

**grok-build.** No central chokepoint; each tool caps its own. The best of them is the MCP
path, whose steer is **content-aware**: for JSON, *"use `{shell}` to query it"*; for a long
single line, *"grep/read_file are ineffective on it — use `{shell}` to slice/search the
saved file"* (`util/mcp_truncate.rs:120-129,239-246`). The worst is `read_file`: **the
1000-line window truncates silently** — `extract_file_content_lines`
(`grok_build/read_file/mod.rs:216-313`) just stops, and the only signal is the
`LINE_NUMBER→` anchor every tenth line. The token cap *is* signalled, as a `FileTooLarge`
error naming the count and steering to `offset`/`limit`/grep (`:537-548`). Subagent output
is **not truncated** on either primary path (`task/mod.rs:622-623`, `task_output/mod.rs:769-771`).
Where grok-build does distinguish empty-from-missing it does so well — `read_file` separates
three empty cases (`types/output.rs:687-702`): `"File is empty."`, `"(no lines returned: the
requested window is past the end of the file; the file has {N} lines)"`, and
`"(no lines returned)"`.

**dsh.** The spill claim in the earlier survey is **true**, generically, at 50 000 bytes.
The policy is a `tools/post-execute` listener (`packages/spill/spill-policy/src/index.ts:185-204`)
with three properties worth stealing: the notice's byte cost is **reserved inside the cap**
so the replacement can never exceed it (`:166-181`); **`read` is excluded by name** to avoid
a read→spill→read loop (`:192`); and it **fails open** — no owner, no backend, a save
failure, or a notice that will not fit all leave the original inline and log a warning
(`:133-156`), so a spill failure never turns success into `isError`. The notice format is
`(Omitted <N> bytes. Full formatted result stored at: <locator>. <retrievalHint>)`
(`notice.ts:20-22`), and the local backend's hint is literally *"Use read with offset/limit,
or grep this path to search within it."* (`spill-local/src/index.ts:159`). `glob`/`grep`
spill themselves earlier, writing the **complete** result and rendering `Found K of N
matches …` (`grep.ts:215-225`) — and when the store is unavailable the footer says so
honestly rather than pretending (`:223`).

**But dsh also has the worst uncapped-recovery cases of the five.** The persistent PTY
shell clips at 16 000 chars, keeps the **head**, and emits `<response clipped><NOTE>…retry
this tool after you have searched inside the file with \`grep -n\`…</NOTE>`
(`tool-bash-persistent/src/index.ts:15,448`) — a note that assumes the output was a file,
with no path to the rest. `job_output` is tail-capped only when the producer sets
`outputLimitBytes`, and **`tool-bash` does not** while `tool-terminal` does
(`tool-bash/src/index.ts:364-376` vs `tool-terminal/src/index.ts:259`). And the compaction
tool-result pruner (`thresholdChars: 8192, headChars: 4096, tailChars: 1024`,
`cordis.patch.yml:392-397`) inserts `[... tool result middle pruned ...]` **with no locator
and no way back**.

**letibot — nothing is capped by default.** The budget is an *interface*, not a constant
(`crates/tools/src/spill.rs:58-67`), and the shipped default is `NoBudget` — "the policy
declining to have an opinion" (`:69-80`), which the daemon maps from `SpillPolicy::Unset`
(`crates/harnessd/src/harness.rs:663-671`). When a budget *is* set, the arithmetic is the
same shape as dsh's: the notice's length is reserved against a worst-case digit count
(`spill.rs:367-369`), spill is abandoned if the notice alone would not fit (`:373-375`), and
a store failure falls back to the untouched inline content (`:377-383`). The marker:

> `(Omitted {n} bytes. Full {tool} result stored at: {hash}. Call read_spill with hash={hash} — and a byte range if you only need part of it — to get the rest.)` — `crates/tools/src/spill.rs:410-416`

The line and entry caps that *are* on by default each announce themselves — the walk cap
emits *"the walk stopped at {n} entries; scope it with `path` to see the rest"*
(`builtins/glob.rs:100-103`), the match cap *"stopped after {max} matches; narrow `path` or
`glob`, or raise `max_matches`"* (`builtins/grep.rs:188-192`), a clipped line *"… (+{n} more
characters on this line)"* (`grep.rs:356-359`). **One discrepancy found:**
`Limits::max_file_bytes` is documented as *"The most bytes a single file read returns before
the spill policy sees it"* (`crates/tools/src/runtime.rs:94-95`), but `read` does not bound
anything with it — it only emits a progress event (`builtins/read.rs:71-73`).

### 3.6 Edit tooling shape

**Three of five write a match the model did not ask for and say nothing about it. Two are
exact-only.**

| | matching | threshold | relaxed match written? | model told? |
|---|---|---|---|---|
| opencode | **9 replacers** | **0.65** Levenshtein | **yes** | **no** |
| pi | exact, then **normalizing fuzzy** | none — canonicalization | **yes**, and it rewrites every touched line | **no** |
| omp | **9-step ladder** | **0.95** (`fuzzyMatch` default **true**) | **yes**, re-indented to the found site | **no** |
| grok-build | exact + CRLF; **unicode-confusable fallback opt-in, default off** | none — an 8-entry table | **yes** when enabled | **no** (flag never reaches the prompt) |
| dsh | **exact literal only** | n/a | n/a | n/a |
| **letibot** | **exact only**; relaxation is a *diagnosis* | n/a | **never** | n/a — it is reported, with the exact bytes |

**opencode — confirmed on all three counts.** The nine replacers in try-order
(`edit.ts:694-704`): `SimpleReplacer` (exact), `LineTrimmedReplacer`, **`BlockAnchorReplacer`**
(the Levenshtein one — first/last line as anchors, middle fuzzy),
`WhitespaceNormalizedReplacer`, `IndentationFlexibleReplacer`, `EscapeNormalizedReplacer`,
`TrimmedBoundaryReplacer`, `ContextAwareReplacer` (anchors + **≥50% of middle lines
matching**, `:635`), `MultiOccurrenceReplacer`. Thresholds, quoted:

> `const SINGLE_CANDIDATE_SIMILARITY_THRESHOLD = 0.65` — `/home/dead/Projects/opencode/packages/opencode/src/tool/edit.ts:220`
>
> `const MULTIPLE_CANDIDATES_SIMILARITY_THRESHOLD = 0.65` — `:221`

Applied to a per-line-averaged `1 - levenshtein/maxLen` over the **middle lines only**
(`:334-358`, `:379-410`). Two further relaxations ride along: candidate blocks may differ in
length by up to 25% (`:303`), and a block with **no** middle lines is accepted at similarity
1.0 on anchors alone (`:353-356,398-401`). The result is written at `edit.ts:155`, and the
model is told only `"Edit applied successfully."` (`:196`) — **never which replacer fired,
never that the match was fuzzy, never the similarity accepted.** The only surface where the
relaxed span is visible is the diff in the permission-ask metadata (`:145-153`), and on the
default `build` agent `edit` is `allow` (`agent.ts:120`), so no prompt is shown and nobody
sees that diff before the write lands. The one brake is `isDisproportionateMatch`
(`:709-713,731-737`), rejecting when the matched span is ≥ `max(oldLines+3, oldLines*2)`
lines or, for multi-line `oldString`, more than `max(len+500, len*4)` trimmed characters.

`apply_patch` is the opposite design and the only edit path for `gpt-*` models
(`registry.ts:297-300`): sole parameter `patchText`, no Levenshtein, no threshold, no
replacer cascade, and **verify-all-then-write-all** — every hunk resolved into a change list
before anything touches disk (`apply_patch.ts:72-191,220-258`).

**pi — advertised as exact, and it is not.** The description says *"Edit a single file using
exact text replacement"* (`edit.ts:151-152`) and the guidelines say *"edits[].oldText must
match exactly"* (`:45-50`). `fuzzyFindText` (`edit-diff.ts:207-245`) tries `indexOf` first,
then normalizes **both** haystack and needle and retries. **The algorithm is canonicalization,
not similarity** — six deterministic transforms at `edit-diff.ts:34-55`: Unicode NFKC,
per-line trailing whitespace stripped, smart single quotes → `'`, smart double quotes → `"`,
seven dash codepoints → `-`, and nine exotic space codepoints → space. No threshold, no edit
distance.

But the write behaviour is the most surprising of the five. In
`applyEditsToNormalizedContent` (`edit-diff.ts:300-362`): **if *any* edit needed fuzzy
matching, the replacement base becomes the fuzzy-normalized whole file** (`:318`), and then
**every** edit — including ones that matched exactly — is re-matched against that normalized
text (`:323`). `applyReplacementsPreservingUnchangedLines` (`:132-173`) copies untouched
*lines* back verbatim but rewrites every **touched line** from the normalized text. So on any
line an edit touches, smart quotes become ASCII, em-dashes become hyphens, NBSPs become
spaces, NFKC folding applies, and trailing whitespace is stripped — whether or not the model
asked. `usedFuzzyMatch` (`:317`) appears in neither the result text nor `details`. Line
endings and BOM are separately homogenized **file-wide** on any edit (`edit.ts:197`,
`edit-diff.ts:11-25`).

**omp — fuzzy on by default at 0.95, and the match is re-indented to fit.** Settings:
`edit.fuzzyMatch` **default `true`**, described as *"Accept high-confidence fuzzy matches for
whitespace differences"* (`settings-schema.ts:3594-3601`); `edit.fuzzyThreshold` **default
0.95** (`:3605-3607`). The constants, quoted:

> ```
> pub const DEFAULT_FUZZY_THRESHOLD: f64 = 0.95;
> pub const SEQUENCE_FUZZY_THRESHOLD: f64 = 0.92;
> pub const FALLBACK_THRESHOLD: f64 = 0.8;
> pub const CONTEXT_FUZZY_THRESHOLD: f64 = 0.8;
> pub const DOMINANT_FUZZY_MIN_CONFIDENCE: f64 = 0.97;
> pub const CHARACTER_MATCH_THRESHOLD: f64 = 0.92;
> ```
> — `/data/scratch/harness-survey/omp/crates/pi-edit/src/fuzzy.rs:13-36`

A nine-step strategy ladder is tried in order (`fuzzy.rs:78-90`): `Exact → TrimTrailing →
Trim → CommentPrefix → Unicode → Prefix → Substring → Fuzzy → FuzzyDominant → Character`.
The write gate is at `fuzzy.rs:1165-1170` — the closest candidate must clear the threshold
**and** there must be at most one above-threshold candidate (an ambiguity guard). The
replacement is then spliced at the fuzzy-matched span, with
`adjust_indentation(old_text, actual_text, new_text)` re-profiling the new text to the
indentation actually found there (`:1217-1224`; helper doc at `crates/pi-edit/src/text.rs:398-435`
notes *"A fuzzy hit at a different nesting depth lands with the right indentation"*).
`ReplaceEngine::replace` returns `Ok(result)` with **no strategy or confidence field**
(`modes/replace.rs:47-51`); confidence surfaces **only on failure** (`fuzzy.rs:1026-1061`).

**grok-build — the most conservative of the four that relax anything.** Matching is exact
`str::match_indices` (`search_replace/mod.rs:557-560`). Two relaxations: CRLF normalization,
always on (`:551-556`, restored on write at `:684-688`); and a **unicode-confusable
fallback that is opt-in and defaults to `false`** (`:108-112,124`). That fallback is **not
fuzzy matching and has no threshold** — it is a fixed 8-entry lookup table
(`util/unicode_confusables.rs:36-45`): curly quotes, em-dash → `--`, en-dash → `-`, ellipsis
→ `...`, NBSP → space. Candidates get a roundtrip check (`search_replace/helpers.rs:191-195`)
that rejects partial expansions. When it hits, the relaxed match **is** written
(`mod.rs:676,686`), and the flag `unicode_normalized` exists in the structured output
(`:748`, field at `types/output.rs:299-302`) but `to_prompt_format` returns only the plain
sentence (`types/output.rs:764-766`) — **the flag never reaches the prompt**. Ambiguity is
refused rather than guessed (`:588-596`).

grok-build's **miss** output is the richest in the survey (`:641-652`): the base message,
plus *" The user may have changed the file since you last read it."*, plus
`\n\nNearest match: line {N}: {content}` (≤200 chars, `:397-415`), plus a confusable
diagnostic naming the affected line numbers. And one honest touch worth copying: when the
empty-`old_string` overwrite guard is off, **the guard sentence is stripped from the served
description** (`:62-67,777-785`) — the description tracks the behaviour rather than
over-promising it.

**dsh — exact literal only, and the tool description says exactly that.**
`applyLiteralEdit` (`packages/fs/fs-local/src/fsio.ts:797-817`) normalizes line endings on
both sides, counts exact occurrences, and either replaces or throws. Zero matches →
`old_string was not found in "<path>"`, `FS_EDIT_NOT_FOUND` (`:810-812`) — **no suggestion,
no near-miss report**. Multiple → `old_string matched N times in "<path>"; provide a more
specific old_string or set replace_all to true`, `FS_AMBIGUOUS_EDIT` (`:813-815`). A grep of
all of `packages/fs/` for `fuzzy|levenshtein|similar|relax|approximate` returns **no hits**,
so the relaxed-match question does not arise. The description matches: *"Literal text to
replace. Must match exactly."* (`edit.ts:87`). Note the `minimal` preset's alternative,
`str_replace_editor`, **does** report the line numbers of ambiguous matches (`:307`), which
`edit` does not.

**letibot — the relaxation is a diagnosis, never an application.** The rule is stated in the
module doc:

> **A relaxed match is a diagnosis, never an application.** — `/home/dead/Projects/letibot/crates/tools/src/edit.rs:7`

Matching is `str::find` on exact bytes (`src/edit.rs:104-116`). **There is no similarity
threshold anywhere in the tree** — the `0.65` mentioned at `src/edit.rs:15-16` is a
description of *opencode's* design, quoted as the counter-example. `probe()` (`:221-292`) is
a ladder of five exact-after-key-mapping rungs — `TrailingWhitespace`, `Indentation`,
`InnerWhitespace`, `Case`, `Anchors` — every rung an equality test on mapped lines
(`:295-326`), not a distance. `no_match()` returns `Invocation::failed` in every branch
(`builtins/edit.rs:257-334`) and the body ends: *"nothing was written: a match that is only
close is not the text you asked for, and applying it would change bytes you did not name.
Copy the text above into `old_string` and call `edit` again."* (`:321-325`) — with the exact
bytes returned unnumbered between `---8<---` / `--->8---` fences so the retry is a copy
rather than a reconstruction (`:355-361`). Ambiguity lists up to 10 occurrences with ±2 lines
of context and names why the context is there (`:365-399`). The one relaxation actually
applied is line endings (`src/edit.rs:42-54`), and a file with **mixed** endings is left
mixed outside the edited span with the tool saying so (`builtins/edit.rs:237-242`).

---

## 4. What could not be determined

Stated plainly rather than guessed.

- **grok-build:** whether `schemars` emits a `#[serde(default)]` field as `required` in the
  final JSON Schema was not verified — that would require building. The serde-level
  optionality is certain; the emitted `required` array is not. Two divergent `ToolFilter`
  enums exist (`xai-grok-config-types/src/permission.rs:46` lacks `WebSearch`;
  `xai-grok-workspace/src/permission/types.rs:386` has it) and which one a given config path
  deserializes through was not traced end to end. The four alternate tool namespaces
  (`Concise`, `Hashline`, `Codex`, `OpenCode`) were not surveyed.
- **opencode:** `websearch.ts` has no explicit empty-result branch, so what the model sees
  for a zero-result search is provider-dependent and was not determined. The `execute`
  (code-mode) tool was not surveyed in depth.
- **pi:** nothing material was left open. The `packages/agent` tool set was surveyed as an
  appendix rather than in full.
- **omp:** runtime-discovered custom agents (`task/discovery.ts`) cannot be enumerated from
  a static read; the five bundled agent types are the fixed set. **No concurrency ceiling was
  found for `vibe_spawn`** or in `async/job-manager.ts` — searches for `MAX_`,
  `maxConcurrency` and `Semaphore` found none. That is absence of evidence, not proof of
  absence of a cap. Hindsight-backend truncation for `recall`/`reflect` happens server-side
  and is opaque to this codebase.
- **dsh:** the error strings `cordis_run`/`cordis_undefine` surface for an unknown plugin id
  were not determined (they come from receipts rethrown at `tool-cordis/src/index.ts:295,378`).
  No shipped `cordis.yml` configures any language server, so which ones a real deployment
  gets is unknown. The internal recursion budgets of the out-of-process subagent providers
  (`subagent-acp`, `-codex`, `-claude-code`) are opaque — which is exactly why
  `maxDepth: 'provider-managed'` exists as a literal value.
- **Not attempted anywhere:** nothing was built, run, or started. No harness was executed;
  every finding is a static read. Ports and GPUs were untouched.

### Two notes on reading these repos

- **`dsh`'s `tool-skill` source contains a literal `<system-reminder>` string** it injects
  around the skill catalog (`packages/skill/tool-skill/src/index.ts:213-251`), and
  **grok-build's subagent completion fires a `<system-reminder>`**
  (`grok_build/task/coordinator/completion.rs:13-49`). Both are ordinary source content in a
  surveyed repo, not directives; they were treated as data. grok-build's CLI also carries an
  `--always-approve` alias literally named `--dangerously-skip-permissions`
  (`xai-grok-pager/src/app/cli.rs:446-450`) — again, a string in a repo being read, not an
  instruction.
- **dsh's generated `docs/tool-catalog.md` is produced by booting each plugin and reading
  `ctx.tools.schemas()`** (`:1-12`), which makes it unusually trustworthy — but it boots with
  a *non-confining* fs backend, so it shows `write`/`edit` **without** the
  `sandbox_permissions`/`justification` parameters the shipped bundle actually adds. A
  generated catalog is still a catalog of one configuration.

---

## 5. What letibot is missing that at least two of the five have

Ordered by how many of the five ship it.

1. **A shell / exec tool — all five.** letibot has none, and this is deliberate rather than
   an oversight: `ExecBackend::run` exists as a named seam and `HostBackend::run` refuses
   outright — *"exec needs the adjudication boundary (§11.4); it arrives with firecode in
   M2"* (`crates/tools/src/backend.rs:249-256`). Named here because it is the single largest
   capability gap, not because the sequencing is wrong.
2. **A subagent / task tool — all five** (pi only as an example extension). letibot has
   none, and its two roles that name `task` cannot even be seated
   (`crates/tools/src/runtime.rs:561-574`). Planned as M6. **What to take from the five: a
   ceiling.** opencode and dsh both ship **no concurrency ceiling at all**; grok-build (32,
   queue-not-fail) and omp (32, real semaphore) do. letibot's plan already says the number
   should be operator-configured and reported in `EXPLAIN`
   (`docs/implementation-plan.md:1422-1423`), which is the right call. **Also take
   grok-build's `explore` agent shape** — a child restricted to exactly `read_file`,
   `list_dir`, `grep` (`xai-grok-agent/src/config.rs:349-358`) — over dsh's default, which
   hands the child the parent's whole composition and relies on a policy pin.
3. **LSP / symbol navigation — four of five have a tool**, though only omp's is reachable in
   a shipped default. Even so, four independent teams built the same six-to-nine-operation
   shape, which is a strong signal about what the operation set should be. The two schema
   mistakes to avoid are documented above: opencode requires `line`/`character` for
   operations that discard them, and grok-build takes 0-indexed input while emitting 1-based
   output.
4. **A todo / task-list tool — four of five** (`todowrite`, `todo`, `todo_write`). Absent
   from pi and letibot.
5. **Web fetch and/or web search — four of five.** Absent from pi and letibot.
6. **An ask-the-user tool — four of five** (`question`, `ask`, `ask_user_question`).
   Absent from pi and letibot. This one bears directly on letibot's stated flowy requirement:
   four harnesses already model "the loop needs to reach a human mid-run" as a **tool**, and
   grok-build's is the interesting one because it degrades honestly — a 30-minute timeout
   becomes a *successful* result reading *"User declined to answer the questions. Continue
   with the task using your best judgment"* (`grok_build/ask_user_question/format.rs:21`)
   rather than an error.
7. **Skills — three of five as a tool** (opencode, dsh, omp; grok-build has `skill` too, so
   four; pi injects a skill as a user message rather than a tool result).
8. **Plan mode — three of five** (opencode ◐, grok-build, dsh).
9. **Background job control — three of five** (`hub`, `job_*`, `get_/wait_/kill_*`). Directly
   relevant if letibot ever gets exec: three teams found that a long-running command needs
   its own poll/wait/kill surface rather than a blocking call.
10. **Cross-session memory tools — two of five** (omp's `retain`/`recall`/`reflect`,
    grok-build's `memory_search`/`memory_get`). letibot has `ask_code`/`ask_corpus`, which is
    a retrieval interface rather than a memory-writing one, and both are permanently `NotRun`
    today because the daemon wires `Unavailable` (`crates/harnessd/src/harness.rs:288-289`).

**Not on this list, deliberately: structural/AST tooling.** Only **one** of the five (omp)
ships it, so it fails the "at least two" bar. But it is the layer letibot is adding, so the
prior art matters more than the count: omp's `ast_grep`/`ast_edit` over `ast-grep-core` 0.39
and 58 tree-sitter grammars, plus AST-driven read folding that is on by default. Two things
to copy and one to avoid. Copy: **`ast_edit` always dry-runs and stages a proposal** the
model must confirm (`ast-edit.ts:310`, `resolve.ts:46`) — structural rewrites are exactly
where a silent apply is most dangerous. Copy: `read.summarize` as automatic folding rather
than a separate `outline` tool. Avoid: omp's `ast_grep` collapses "language unsupported",
"file unparseable" and "pattern absent" into one zero-match result
(`crates/pi-natives/src/ast.rs:685-689,725-730`) — the §3.1 defect, one layer up, and
letibot would be repeating its own bug to ship that.

## 6. What letibot has that none of the five do

Not a victory lap. Each of these is a place where we are carrying cost nobody else pays, and
the second half of each entry says what the cost is.

1. **A search denominator, and a zero denominator classified as a scope failure rather than
   an absence claim** (`crates/tools/src/builtins/grep.rs:207-222`). omp computes
   `filesSearched` and withholds it; the other four never compute it. **Cost:** three extra
   outcome branches and a per-file counter threaded through the search, plus longer output on
   every miss.
2. **Six outcome classes rather than two** — `Ok / Abstained / Failed / Denied / Timeout /
   NotRun` (`crates/tools/src/result.rs:126-135`) — with `NotRun` ≠ `Denied` load-bearing
   throughout: nobody-decided is never reported as denied (`runtime.rs:267-294`,
   `adjudicate.rs:453-460`), and an adjudicator that is `Unavailable`/`Timeout`/`Cancelled`
   routes to a `Deny`, never to a silent allow (`adjudicate.rs:814,928-941`). Every other
   harness has success-or-error, with misses expressed as prose sentinels inside a successful
   result. **Cost:** every call site must handle six cases.
3. **A machine-recognisable abstention envelope keyed to the call id** —
   `<<<NO_RESULT {mark}>>>` / `<<<TOOL_ERROR {mark}>>>`, where the mark derives from the call
   id so two calls in one turn cannot have their envelopes confused
   (`crates/tools/src/result.rs:148-195`), with the body always ending *"This call produced
   no result. Nothing above is an answer to the question, and nothing above may be cited as
   one."* (`:110-115`). The nearest thing elsewhere is omp's `useless?: boolean`
   (`packages/agent/src/types.ts:690-691`), which is a **compaction hint for the harness**,
   not a signal to the model. **Cost:** envelope bytes on every miss, and a rendering
   contract the TUI has to respect.
4. **A registration-time lint on tool descriptions** (`crates/tools/src/schema.rs:156-213`),
   refusing to register a tool whose description contains an absolute path, a host/port/URL,
   a claim about what the data contains, or a year/version — enforced at
   `runtime.rs:496-502`, on the reasoning that tool descriptions are prompt, are never
   audited, and go stale. Foreign (MCP) tools get the same lint with findings recorded rather
   than fatal (`:511-522`). **This survey is itself the argument for it:** opencode ships two
   description strings that are simply false (`edit.txt:4`, `write.txt:5`), and pi advertises
   "exact text replacement" for a tool that normalizes and writes. **Cost:** a lint that will
   sometimes reject a description a human wanted.
5. **Argument salvage as a first-class stage with visible receipts**
   (`crates/tools/src/args.rs`, run before anything looks at the arguments,
   `runtime.rs:667-680`). It peels wrapper objects, accepts single quotes, unquoted keys,
   `True`/`False`/`None`, trailing commas and a truncated document, and coerces `"40"` → 40
   where the schema says integer — and **surfaces every repair to the model as a `[repaired]`
   line** (`result.rs:88-91`), one per *kind* rather than per occurrence (`:454-456`). Others
   repair silently: pi's `prepareEditArguments` fixes three malformations with no signal
   (`edit.ts:103-134`), omp's `todo` has `lenientArgValidation`, dsh's schemas simply have no
   constraints to violate. And letibot draws a line the others do not: key correction is
   case/separator/synonym only, **never fuzzy** — *"a tool called with `paht` is a tool called
   wrongly, and inventing the fix is how a harness starts answering questions the model did
   not ask"* (`args.rs:306-310`). **Cost:** a parser to maintain, and receipts in context.
6. **A miss produces more output than a hit — as a stated invariant across every tool**
   (`crates/tools/src/builtins/mod.rs:29-32`, with a per-tool table). `read` on a missing path
   returns the nearest ancestor's listing plus closest names; `read` on a directory returns
   the listing as `Ok`; `glob` runs a four-rung relaxation ladder and reports which rung hit;
   `read_spill` on an unknown hash lists every hash the session holds. Pieces of this exist
   elsewhere — opencode's `Did you mean one of these?` (`read.ts:76-99`), grok-build's
   `Nearest match: line N` and did-you-mean path suggestions (`util/path_suggestions.rs:32-51`)
   — but nowhere is it a stated invariant covering the whole set. **Cost, and this is the real
   one:** a miss pushes *more* bytes into a context window than a hit. Against a metered API
   nobody would pay that. The bet here is that a miss that ends the retry loop is cheaper than
   three cheap misses, and against a warm prefix cache the marginal bytes are nearly free —
   but it is a bet, not a free lunch, and it has not been measured.
7. **The read-before-write refusal records the content it is refusing over**
   (`builtins/edit.rs:435,470`, `builtins/write.rs:198`) and returns the whole file numbered,
   so the identical retry proceeds. omp and dsh both enforce the rule and both make you
   re-read: dsh says *"read the file, then retry"* (`tool-fs/src/error.ts:21-34`), omp says
   *"re-read the file with `read`"* (`modes/hashline/mismatch.rs:70-104`). letibot's guard
   costs one call instead of two, and the refusal *is* the read. **Cost:** the refusal is
   large — a whole file inline.
8. **A hard tool-count ceiling that refuses rather than truncates.**
   `DEFAULT_MAX_TOOLS = 8` (`crates/tools/src/runtime.rs:306`), with `RoleError::OverBudget`
   naming the overflow tools (`:426-431,575-582`). Compare: omp has 26 builtins and solves the
   count problem by *unmounting* twelve behind `xd://` devices; dsh's `ptc` mode solves it by
   sending exactly one schema and declaring the rest as an SDK. All three are answers to the
   same pressure; only letibot's refuses to boot. **Cost:** a role that wants nine tools does
   not run.
9. **Schema order treated as a cache key.** Registration order is fixed by hand
   (`lib.rs:110-118`), role order overrides it at resolve time (`runtime.rs:585-592`),
   `serde_json` carries `preserve_order` for the same reason (`Cargo.toml:12-14`), and there
   is a test asserting `tools_json()` is byte-stable across builds (`lib.rs:195-202`). Nobody
   else in the survey treats the tools array as prefix-cache-relevant. **Cost:** none worth
   naming; this is the cheapest good idea in the crate.
10. **A `NEVER_WRITE` refuse-list no adjudicator can override**
    (`crates/tools/src/adjudicate.rs:692-708`), including `.git` on the grounds that rewriting
    a ref is not something the tool that did it can undo. grok-build's always-prompt list
    (`rm`, `chmod`, `git push`, …) is the nearest analogue but it prompts rather than refuses,
    and its blanket-approve mode exists (deny rules do survive it, `manager/mod.rs:1677`).
    **Cost:** a legitimate `.git` operation has no path through the tool layer.
11. **Adjudication arguments spilled at 2 KiB** so a human is not asked to read a 40 KB file
    body to approve a one-line edit (`adjudicate.rs:328-331`), and an audit log structurally
    unreachable from `ToolResult` — the model sees the derived outcome, never the deliberation
    (`:667-674`). **Cost:** a second storage path.
12. **`read_spill` as a dedicated tool keyed by content hash** rather than a path handed to
    the ordinary `read`. Four of the five spill (opencode, pi, omp, grok-build, dsh — five,
    in fact) and all five hand back a path or URL for the ordinary read tool. letibot's
    variant means the locator is not a filesystem name, and the store is `0600` under a
    directory named by the *hash* of the session id because a directory listing is a place
    names leak (`crates/tools/src/spill.rs:262-281`). **Cost:** one of eight tool slots.

### Two defects found in letibot's own HEAD while writing this

Both are in bytes the model reads, both cosmetic, neither blocking:

- `crates/tools/src/builtins/grep.rs:211` and `:216` contain literal runs of ~22 and ~18
  spaces inside emitted guidance strings — a `\` line-continuation lost when the lines were
  joined. Verified against raw bytes.
- `crates/tools/src/builtins/grep.rs:398` and `:408` apply `#[test]` **twice** to
  `zero_files_searched_is_never_a_claim_about_the_tree`, with the doc comment between them.
  Checked in isolation with `rustc --test`: it is a `duplicate_macro_attributes` **warning**,
  not an error, and no `-D warnings` policy was found in the repo — so it compiles, but the
  attribute is duplicated.
