# Tool card sweep, 2026-11-13

The operator reads a tool card in the head and the model reads the same bytes: the card body is
`ToolResult::render()` with the envelope marker lines filtered out (rano's `is_envelope`). So every
cut here is a cut in what the model is sent, and every repeat the operator sees is a repeat the model
pays for.

Two shapes were flagged first and fixed before this sweep: `ef21c2d7` (a command that changed a file
printed two `[note]`s saying the same thing) and `b5977b51` (a backgrounded card's head line repeated
the read hint the body carries). This record covers the rest.

## Method

Samples were produced by two throwaway integration tests in `crates/tools/tests/` (not committed)
that call each tool through `letibot_tools::testing` harnesses and print `render()` with its byte
count. One run before the first commit, one after the last. The second test (`samples2`) was added
after the first two commits, so its "before" already lacks the envelope `tool:` line (about 12
bytes per card); everything else in it is pre-change. Paths and pids differ between runs, so byte
counts carry a few bytes of noise.

The harness host runs the test daemon inside a tmux scope rather than its own cgroup, so every bash
call carries an `UNCAPPED` memory note. That note is environmental here (the `letibot` launcher gives
the real daemon a scope) and is left alone except where one result repeated it per row.

For each shape, the questions were: is a fact said twice, does an envelope line repeat the row
header or the call, is a per-call boilerplate warning already in the tool description or the
system prompt, does a sentence restate the one before it.

## Shapes looked at

bash (foreground ok, foreground failed, timeout, background asked plain/redirected/named, promoted
by the operator, denied by layer A, detected edit), job_output (running, killed, unknown job),
job_wait (deadline), job_list, job_kill, read (ok, missing, capped, bad args), edit (ok, miss,
unshown lines, missing file), write (new, overwrite, denied, no gate, absent tool), grep (hit,
miss), glob (hit, miss), ask_corpus (unattached), todo_write (fresh, update), skill (list, load
missing, no action), room (no verb), notes (list), plugin (bad action), task_result (none),
monitor (nothing to watch), unknown tool, web_fetch (via `tests/external.rs`), task and fanout
background starts (via their unit and integration tests).

No change was needed for: bash foreground ok/failed, read ok/capped, write new, grep/glob hits,
todo_write fresh, skill list, notes list, room/plugin/monitor argument errors, task_result none,
unknown tool.

## Findings, one per commit

### 1. Envelope (`c99e0717`, `crates/tools/src/result.rs`)

Every non-`Ok` envelope opened with `tool: <name>`. The call it answers already names the tool, the
card header says it (`Ran …`), and an `Ok` result never carried the line. Cut from `NO_RESULT`,
`TOOL_ERROR` and `STILL_RUNNING`.

`STILL_RUNNING` also opened with `job: <handle>`. Every backgrounded payload names the handle
(`started `label``) and `next` spells the call that takes it. Cut.

`how:` was printed for every backgrounding. For `Asked` it said "you asked for this to run in the
background" to the model that asked. Kept for `Promoted` and `Operator`, where it is the one fact the
model did not cause (`a_backgrounded_result_is_not_wrapped_as_an_error_or_as_a_success` still
asserts "did not ask").

Before (operator's paste, head `Ran "cd … && cargo fmt --all && …" · backgrounded · 143ms`):

```
    tool: bash
    job: j83808
    how: you asked for this to run in the background
    started `bottom-term-gate · j83808` in the background (pid 1640865).
      name: bottom-term-gate — `job_output`, `job_kill` and `job_wait` take this or `j83808`
      output file: /home/dead/.cache/letibot/gc/…/job-j83808.out (from byte 0) — every byte it prints goes there too, and that file is not bounded by the wi…
      scope: session — reaped when this session ends

    Its completion reaches you automatically — when your current turn ends, or at once if a todo row needs it or wake is "now". So do not build your own clock: no `sleep`, no polling loop.

    STILL RUNNING — not failed and not finished; do not start it again. `job_output` with job="j83808" reads what it has written so far; `job_kill` stops it. If a todo row waits on this: `todo_write` {"wait_on"…
```

After (derived from the code for the same call, commits 1 and 2):

```
    started `bottom-term-gate · j83808` (pid 1640865).
      output file: /home/dead/.cache/letibot/gc/…/job-j83808.out (from byte 0) — all of it, where the window keeps 8 MiB
      scope: session — reaped when this session ends

    Its completion reaches you automatically — when your current turn ends, or at once if a todo row needs it or wake is "now". So do not build your own clock: no `sleep`, no polling loop.

    STILL RUNNING — not failed and not finished; do not start it again. `job_output` with job="j83808" reads what it has written so far; `job_kill` stops it. If a todo row waits on this: `todo_write` {"wait_on": [{"content": "<the row, quoted exactly>", "job": "j83808"}]}.
```

The job id went from six spellings to three: the label, the read call, the todo call. The two calls
are kept because each is the exact argument a model passes. "backgrounded" went from four mentions
(header, `how:`, "in the background", STILL RUNNING) to the header and the tail.

Test: `an_envelope_does_not_restate_the_call_or_the_handle` (no `tool:`, `job:`, `how:` on an asked
backgrounding; one `how:` on a promotion; no `tool:` on a failure or an abstention).
`the_ways_into_the_background_and_the_deadline_are_distinguishable` now asserts an asked
backgrounding carries no `how:`.

### 2. Backgrounded starts (`068e77ec`, bash, fanout, task, task_resume)

bash, asked. "in the background" in the started line repeated the envelope. The `name:` line
("`job_output`, `job_kill` and `job_wait` take this or `jN`") restated what the label `name · jN`
shows and what the `job_*` schemas say ("its id (`j65`) or the name the agent stated"); the call in
`next` passes the id, which is the resolvable half. The output-file clause was cut to "all of it,
where the window keeps 8 MiB". The redirected `next` named the redirect path twice in one sentence.

Before / after, `bash bg plain` (750 -> 616 bytes):

```
<<<STILL_RUNNING 000aa3fb>>>
tool: bash
job: j6
how: you asked for this to run in the background
started `j6` in the background (pid 1812556).
  output file: /tmp/letibot-tools-1812223-1794608703794655679-1-1/job-j6.out (from byte 0) — every byte it prints goes there too, and that file is not bounded by the window's 8 MiB
  scope: session — reaped when this session ends

Its completion reaches you automatically — when your current turn ends, or at once if a todo row needs it or wake is "now". So do not build your own clock: no `sleep`, no polling loop.

STILL RUNNING — not failed and not finished; do not start it again. `job_output` with job="j6" reads what it has written so far; `job_kill` stops it.
<<<END_STILL_RUNNING 000aa3fb>>>
```

```
<<<STILL_RUNNING 000aa3fb>>>
started `j6` (pid 1943078).
  output file: /tmp/letibot-tools-1942466-1794608929385771853-1-1/job-j6.out (from byte 0) — all of it, where the window keeps 8 MiB
  scope: session — reaped when this session ends

Its completion reaches you automatically — when your current turn ends, or at once if a todo row needs it or wake is "now". So do not build your own clock: no `sleep`, no polling loop.

STILL RUNNING — not failed and not finished; do not start it again. `job_output` with job="j6" reads what it has written so far; `job_kill` stops it.
<<<END_STILL_RUNNING 000aa3fb>>>
```

`bash bg redirected` (708 -> 538 bytes), tail before and after:

```
STILL RUNNING — not failed and not finished; do not start it again. Its own window will be EMPTY however long it runs — the command sends stdout to `/tmp/zz.log` — so read `/tmp/zz.log` with `read` when the completion tells you it ended, and do not read the window looking for progress.
```

```
STILL RUNNING — not failed and not finished; do not start it again. Its own window stays EMPTY: stdout goes to `/tmp/zz.log`. `read` that file once the completion says it ended, not the window for progress.
```

bash, promoted. `next` opened with "carry on — `label`'s completion will be delivered to you on its
own when it ends, so there is nothing to wait for", and the payload closed with "Its completion is
delivered when it ends; nothing is held for a break." Two delivery sentences. `next` also carried
the output file as a sentence while the payload carried it as an `output file:` line. The payload
repeated `how.phrasing()` in its bracket, which the envelope's `how:` already says, and a
`command:` line repeating the call. Now: delivery once (`DELIVERED` plus the clock rule, the same
pair the asked branch uses), the file once, the command not at all.

fanout, background. `next` said "carry on — the table reaches you on its own when the last branch
ends, so there is nothing to wait for" and the payload said `DELIVERED`. The `name:` line spelled
both handles. Now the payload is "started `fanout · jN`: N branch(es) on hosts" plus `DELIVERED`,
and `next` is the read and kill calls.

task and task_start. The payload ended "It is working now, and this call did not wait for it.
{DELIVERED} `task_result` with task=… shows where it has got to." and `next` said the same
`task_result` sentence. The first sentence restates STILL RUNNING. Now the payload carries
`DELIVERED` and `next` the `task_result` call.

task_resume. `next` was `DELIVERED` plus the `task_result` call, and the daemon's `said`
(`crates/harnessd/src/harness/children.rs`) ended "Its answer reaches you on its own when it
finishes; `task_result` with task=… shows where it has got to." The daemon's sentence is cut to the
resume fact.

Tests: `a_named_job_is_called_by_its_name_and_its_id_still_resolves` now asserts the label once,
`job="<id>"` once, no `job="<label>"`, `job_output` once, and none of `tool:`, `job:`, `how:`,
`take this or`. The task unit test asserts `task_result` once and `DELIVERED` once in the rendered
start. The fanout background test asserts the label `fanout · jN`, one "reaches you", and no
`take this or`.

### 3. A missing path (`d795f23f`, read, edit)

read and edit on a missing file: outcome "failed — no file at `nope.md`" and a note "`nope.md` does
not exist; the listing of `.` is below." The listing opens with "./ holds N entries", so the note
restated both. The note is now only the step: read "Call read again with one of these paths — do
not guess a second one.", edit "Nothing was written."

```
<<<TOOL_ERROR 5ebfdb90>>>
tool: read
outcome: failed — no file at `nope.md`
[note] `nope.md` does not exist; the listing of `.` is below. Call read again with one of these paths — do not guess a second one.
./ holds 6 entries:
```

```
<<<TOOL_ERROR 5ebfdb90>>>
outcome: failed — no file at `nope.md`
[note] Call read again with one of these paths — do not guess a second one.
./ holds 6 entries:
```

374 -> 308 bytes. Tests assert the missing path is spelled once in each card.

### 4. A search miss (`38d86cde`, grep, glob)

grep: the reason says "`zzzqqq` does not occur in the 8 file(s) searched", the body opened
"searched 8 file(s) under `.`." and closed "the string is not in the searched tree." Three
statements of one miss. The body now says where ("searched under `.`.") and what else to try.

```
reason: `zzzqqq` does not occur in the 8 file(s) searched
searched 8 file(s) under `.`.
relaxations tried, all of them empty:
  - `zzzqqq` was retried without case sensitivity

the string is not in the searched tree. `glob` searches file *names*, and `ask_code` answers questions the text does not spell out.
```

```
reason: `zzzqqq` does not occur in the 8 file(s) searched
searched under `.`.
relaxations tried, all of them empty:
  - `zzzqqq` was retried without case sensitivity

`glob` searches file *names*, and `ask_code` answers questions the text does not spell out.
```

glob: for `**/*.zz` the ladder produced `**/*.zz` twice more (the "drop the directory" rung and
the "extension" rung both respell the pattern), so the body said "Relaxations tried: `**/*.zz`,
`**/*.zz`." and the tree was walked three times for one pattern. The ladder now drops a rung whose
pattern is already on it, and the body leaves the clause out when no rung differs. Same result, same
outcome. 487 -> 426 (grep), 445 -> 393 (glob).

Tests: grep asserts `file(s)` once and no "not in the searched tree"; new
`a_relaxation_is_listed_once`.

### 5. Unattached retrieval (`f775aeca`)

`NotAttached` renders `why`, `nothing_happened`, `attach` and `instead`. Retrieval's `attach` field
was "no retrieval backend is attached to this session, so nothing was queried; `grep` and `read`
over this tree answer what it would have." That is `why` and `instead` again. The daemon wires
`Unavailable` into every session (`harness/open.rs`), so the attach line now says there is no
switch.

```
no retrieval backend is attached to this session, so `ask_corpus` did not run.
no corpus was queried and no index was opened, so this is the absence of a result and not a result: nothing here says anything about what is or is not out there.
to attach it: no retrieval backend is attached to this session, so nothing was queried; `grep` and `read` over this tree answer what it would have.
what still works here: `grep` and `read` over this tree
```

```
no retrieval backend is attached to this session, so `ask_corpus` did not run.
no corpus was queried and no index was opened, so this is the absence of a result and not a result: nothing here says anything about what is or is not out there.
to attach it: the daemon wires none into any session, so there is no switch to turn on
what still works here: `grep` and `read` over this tree
```

548 -> 470 bytes. Test asserts "no retrieval backend" once and `` `grep` `` once.

### 6. Job results (`ed5d43f7`, job_list, job_kill, job_output)

job_list printed, on every running row, "a host pid, which your shell cannot see (it has its own
PID namespace): `ps` and `kill` prove nothing about this job. Use `job_list` and `job_kill`." The
`job_list` description carries the same sentence, and the row told the reader of `job_list` to use
`job_list`. Each row also carried the memory clause in full, which for an uncapped daemon is a
300-byte explanation identical on every row. Now each row says "a host pid", the caveat is said once
after the rows, and a memory clause an earlier row spelled is "as the row above says".

Rows before and after (3 running jobs, 3320 -> 2421 bytes for the whole listing):

```
j5  [running]
  command: sleep 30 > /tmp/zz.log
  scope:   session.s0 — reaped when this session ends
  pid:     1812513 — a host pid, which your shell cannot see (it has its own PID namespace): `ps` and `kill` prove nothing about this job. Use `job_list` and `job_kill`.
  elapsed: 0.7s
  output:  0 bytes, nothing written yet
  file:    /tmp/letibot-tools-1812223-1794608703794655679-1-1/job-j5.out (from byte 0)
  cgroup:  5 processes — 3 sh, 1 cat, 1 sleep
  cpu:     idle — 0.0s of the last 0.2s
  memory:  UNCAPPED (64 MiB was asked for: this daemon shares its cgroup `/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/tmux-spawn-a8a3bd2d-b89b-4366-96ec-c55da8b09944.scope` with 115 other process(es) — it was not started in a scope of its own (the `letibot` launcher does that with `systemd-run --user --scope`), so tool calls run UNCAPPED)
```

```
j5  [running]
  command: sleep 30 > /tmp/zz.log
  scope:   session.s0 — reaped when this session ends
  pid:     3176114 — a host pid
  elapsed: 0.7s
  output:  0 bytes, nothing written yet
  file:    /tmp/letibot-tools-3175538-1794609938189898799-1-1/job-j5.out (from byte 0)
  cgroup:  5 processes — 3 sh, 1 cat, 1 sleep
  cpu:     idle — 0.0s of the last 0.2s
  memory:  as the row above says
...
A pid above is a host pid, which your shell cannot see (it has its own PID namespace): `ps` and `kill` prove nothing about these jobs. Use `job_kill`.
```

job_kill opened with the reaping summary ("session.job-j6: observed 5 process(es), killed by
cgroup.kill, 0 survivor(s) [pids]") above the lines that spell the observed list, the mechanism and
the survivors. Now it opens `scope: session.job-j6`. 838 -> 702 bytes.

```
session.job-j6: observed 5 process(es), killed by cgroup.kill, 0 survivor(s) [1812556 /bin/sh, 1812569 /bin/sh, 1812570 cat, 1812571 /bin/sh, 1812572 sleep]

the job was: j6
```

```
scope: session.job-j6
the job was: j6
```

job_output on a killed job printed `PARTIAL` whole: "its output is partial — it was ended, not
finished. `job_output` reads what it wrote; `job_extend` gives a running job a longer deadline, and
`job_kill` ends one you no longer want." In a `job_output` result, about a job that is no longer
running, the second sentence names the call being read and two calls that do not apply. job_output
now uses the first sentence; settlement notices and the bridge push keep the whole constant. The
file bracket said "the file keeps growing with the window" on every read, including a killed job's;
now "all of it is also in `…`, from byte N of this job's output; the window keeps the last 8 MiB".
844 -> 640 (killed), 809 -> 735 (running).

Tests: new `a_listing_says_what_every_row_shares_once`;
`the_description_and_the_listing_both_say_your_shell_cannot_see_these_pids` (renamed) checks the
description and `PID_CAVEAT`; `exec.rs` asserts `HOST_KILL` once in a kill and, reading the killed
job back, "its output is partial" without "`job_output` reads".

### 7. Refusals (`69932048`, `crates/tools/src/authorise.rs`)

A refusal's prose opened "`write` was not run." and every remedy (`DenialNotice::grant`, built in
`adjudicate.rs`) ends or opens "Nothing was executed and nothing changed." The opening is cut; the
call names the tool. The grant strings stay, because the operator's notice shows them alone
(`every_denial_reaches_the_operator_with_the_grant_path_attached`).

A denial also printed `outcome: denied (somebody decided)` in the body under the envelope's
`outcome: denied (decision adj-…)`. Cut for a denial. Kept for a not-run, whose line says what it is
not ("nobody decided — this is not a denial, and it is not permission either", test-guarded).

`Reach::Nobody` said "the same call asked again reaches the same verdict" after the opening
paragraph said "a different spelling of the same action reaches the same decision". Cut.

```
outcome: denied (decision adj-session-0001)
REFUSED — not failed.

`write` was not run. This is a DECISION about what is permitted, not an error about what works, …

who decided: human:test
basis: the test harness refuses every gated call
outcome: denied (somebody decided)
reading: ask — intents [write_file]
```

```
outcome: denied (decision adj-session-0001)
REFUSED — not failed.

This is a DECISION about what is permitted, not an error about what works, …

who decided: human:test
basis: the test harness refuses every gated call
reading: ask — intents [write_file]
```

1071 -> 1015 (write denied), 1881 -> 1805 (bash denied by layer A). Tests: the denied refusal has
no `outcome:` in its prose; new `a_refusal_says_a_retry_reaches_the_same_decision_once`; the gated
edit asserts no "was not run".

### 8. Edit and write confirmations, the unshown-lines refusal (`686177d9`)

`ChangedSpan::describe` always appended "the file went from X to Y lines". When the span is the
whole file, the replaced counts are the lengths ("lines 1–2 differ (2 line(s) replaced by 1); the
file went from 2 to 1 lines"); when the length did not change it read "went from 2 to 2". Now the
length is left out for a whole-file change and said as "the file is N lines" when unchanged.

```
README.md: 1 replacement(s). lines 2–2 differ (1 line(s) replaced by 1); the file went from 2 to 2 lines
`README.md` rewritten. lines 1–2 differ (2 line(s) replaced by 1); the file went from 2 to 1 lines
```

```
README.md: 1 replacement(s). lines 2–2 differ (1 line(s) replaced by 1); the file is 2 lines
`README.md` rewritten. lines 1–2 differ (2 line(s) replaced by 1)
```

The unshown-lines refusal's reason ended ": `read` them first" and its body said "call `read` with
offset=4 and limit=1 and edit again". With nothing shown, the body also said "This session has been
shown none of it of `src/lib.rs`." after the reason said so. The reason keeps the fact, the body the
call, and the shown sentence appears only when something was shown.

```
outcome: failed — this session has not been shown lines 4-4 of `src/lib.rs`: `read` them first
… Output of `bash` (`cat`, `grep -n`, `sed -n`) does not count. This session has been shown none of it of `src/lib.rs`. call `read` with offset=4 and limit=1 and edit again. Nothing was written.
```

```
outcome: failed — this session has not been shown lines 4-4 of `src/lib.rs`
… Output of `bash` (`cat`, `grep -n`, `sed -n`) does not count. call `read` with offset=4 and limit=1 and edit again. Nothing was written.
```

517 -> 442 bytes. Tests: new `a_change_says_the_files_length_once`; the unshown-region test asserts
`` `read` `` twice (the rule and the call) and no "none of it".

### 9. todo_write (`25f2ed1b`)

An answer that hid only done rows said "not listed: 1 done, 0 waiting on something (each is asked
about when its wait ends, or when the row moves)." Zero parts and the wait explanation are now left
out when they count nothing. 211 -> 123 bytes for the sample.

```
not listed: 1 done, 0 waiting on something (each is asked about when its wait ends, or when the row moves). They are still on the board.
```

```
not listed: 1 done. They are still on the board.
```

### 10. Promoted run (`80de46de`)

After commit 2 a promoted card still said the move three times: "ran in the foreground for: 109ms
before it went to the background", "how: `deadtrickster` moved this to the background from the
head", and the payload's "[`j1` was moved to the background]". The envelope now says who and when on
one line, the payload bracket names the handle, and the payload's trailing newline no longer leaves
two blank lines above the tail. 749 -> 699 bytes.

```
<<<STILL_RUNNING 5ebfdef6>>>
ran in the foreground for: 109ms before it went to the background
how: `deadtrickster` moved this to the background from the head
before

[`j1` was moved to the background]
  output file: … (from byte 0) — all of it, where the window keeps 8 MiB
  owned by: session.s1 — reaped when this session ends

Its completion reaches you automatically — … So do not build your own clock: no `sleep`, no polling loop.


STILL RUNNING — not failed and not finished; do not start it again. `job_output` with job="j1" reads what it has written so far.
```

```
<<<STILL_RUNNING 5ebfdef6>>>
how: `deadtrickster` moved this to the background from the head, after 113ms in the foreground
before

[running on as `j1`]
  output file: … (from byte 0) — all of it, where the window keeps 8 MiB
  owned by: session.s1 — reaped when this session ends

Its completion reaches you automatically — … So do not build your own clock: no `sleep`, no polling loop.

STILL RUNNING — not failed and not finished; do not start it again. `job_output` with job="j1" reads what it has written so far.
```

Tests: `an_envelope_does_not_restate_the_call_or_the_handle` asserts "background" once on a
promotion; `a_promote_request_from_the_head_moves_the_running_command` asserts "moved this to the
background" once.

### 11. web_fetch (`fdd086dc`)

The body opened "fetched https://… — 200 text/html, …" and the quarantine header under it opened
"source: https://…". The header stays (it travels with the scratch file, read in a later turn); the
body line drops the address. A redirect note still names both addresses. Test asserts the address
once in the payload.

## Kept, and why

- "STILL RUNNING — not failed and not finished; do not start it again." One copy per backgrounded
  result. It exists because models retried a running command.
- `DELIVERED` plus "So do not build your own clock: no `sleep`, no polling loop." in every
  backgrounded start, although the bash description carries both. The redirected-background test
  asserts both in the result, and the polling it prevents happens at exactly this moment. One copy
  per result.
- The todo `wait_on` hint in `next`. It is the one place the exact call is spelled with this handle.
- `job="jN"` in `next` beside the label. The label is not a resolvable handle; the call spelling is.
- `NO_RESULT`'s closing "This call produced no result. Nothing above is an answer…". The tag says
  it structurally, the sentence says it to a reader of prose, and it is test-guarded
  (`may be cited`). One copy per card.
- The untrusted-text DATA warning per fetched page and search listing. It is the quarantine, not
  boilerplate.
- The refusal's opening paragraph and "Do not retry a variant." The paragraph is the reason, the
  sentence the instruction; both are test-guarded.
- `DenialNotice::grant` strings still say "Nothing was executed and nothing changed." The operator's
  notice shows the grant on its own and a test guards it there. For a layer-A refusal the basis also
  says "Nothing ran."; that is the normaliser's sentence and was left to its owner.
- Short outcome reasons repeated in the body (`skill load missing`: "no skill named `nope`" in the
  outcome and the body). `outcome_word_beside` deliberately repeats reasons under 40 characters;
  changing that rule is out of scope.
- The per-call `memory: ran UNCAPPED` note on bash. It only appears when the daemon has no scope of
  its own, which is the test harness here and not the launcher's daemon.
- job_list's per-row `scope: … — reaped when …` beside the scopes section. Settled rows listed with
  `all: true` can name a scope that is no longer open, so the row is the only place their reaping
  rule is said.

## Byte counts per family (before -> after, one sample each)

| family | sample | before | after |
|---|---|---|---|
| envelope + background start | bash bg plain | 750 | 616 |
| envelope + background start | bash bg redirected | 708 | 538 |
| envelope + promotion | bash promoted by operator | 749 | 699 |
| envelope | bash timeout | 649 | 638 |
| envelope | job_output unknown | 414 | 397 |
| read/edit missing path | read missing | 374 | 308 |
| search miss | grep none | 487 | 426 |
| search miss | glob none | 445 | 393 |
| retrieval | ask_corpus unavailable | 548 | 470 |
| jobs | job_list (3 running) | 3320 | 2421 |
| jobs | job_kill | 838 | 702 |
| jobs | job_output killed | 844 | 640 |
| jobs | job_output running | 809 | 735 |
| refusal | write denied | 1071 | 1015 |
| refusal | bash denied (layer A) | 1881 | 1805 |
| edit/write | edit unshown lines | 517 | 442 |
| edit/write | edit ok | 144 | 132 |
| edit/write | write overwrite | 112 | 79 |
| todo | todo_write update | 211 | 123 |

The refusal, edit-unshown, todo and promotion "before" numbers were taken after commit 1, so they
already exclude the `tool:` line.
