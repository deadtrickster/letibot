# TODO

Work queue. **The first section is everything that can be finished without the
operator**; nothing in it waits on a decision, and each entry says how to check it
is still open before starting.

Settled work is in `TODO-settled.md`, with its verdict, the tests that prove it, and the entry as
it was filed. Nothing closed belongs in this file.

## If you are an agent picking work from this file

1. **Take one item from READY. Nothing else.** Sections 2 and 3 are blocked on a
   human; starting one produces work that cannot land.
2. **Re-run the item's `still open?` check first.** Statuses go stale. On 2026-09-10
   three items in this file — T12, T20.4, T21.1/2 — were found already implemented
   while still filed as open, and would have been re-done from scratch.
3. **Verify with the item's `done when`, not by reading your own diff.** If a check
   needs a live model, `~/bin/model status` says what is serving; several test files
   require it and will otherwise pass vacuously.
4. **If you find it already done**, move it to `TODO-settled.md` with the evidence
   and stop. That is a complete, valuable outcome.
5. **If it turns out to need a decision**, move it to section 3 with the question
   stated. Do not guess the answer.

## Repo facts you will need

    cargo check --workspace
    cargo test -p <crate>            # crates/: transcript turn dialect{,-glm,-qwen}
                                     #   tools harnessd sessionlog tui ui code
                                     #   backend tokencore metrics room plugins
    python3 tests/fidelity/run_gate.py    # template fidelity gate, must print GATE PASS

Tests needing a live model server on 127.0.0.1:8080: `sessionlog/{live_e2e,late_head,
sessions,resume_frames}`, `harnessd/{loop_closes,wired}`, `turn/{live_qwen,
engine_decisions}`, `tools/{exec,background,confine}`.

---

# 1. READY — no operator needed

*Each entry says how to re-check that it is still open. Re-run that check before starting; statuses
go stale.*

---

## R120 - One seat and a working automode, instead of roles - **PARKED 2026-11-03: no oracle passes yet**

The operator, deciding N4: "the more i use things the more i understand the roles were overcomplication - we need working automode". N4 (a coder can write a test and cannot run it) is superseded: the answer is not a shaped verb for one role, it is fewer roles.

- One working seat that can read, write and run commands. What it may do without asking is the MODE's business, not the role's. The coder/runner/m2 split and role switching go, except where a confined seat is the point (a VM or a placement).
- `automode` works out of the box: today it requires an adjudicator and a reachable oracle and refuses without them. Its default decider is the session's own provider or a cheap fast model, with no setup. It admits in-checkout reads, writes, builds and tests; asks the operator for the network, paths outside the project, the always-ask list and destructive or irreversible actions; tells the model why when it denies; and records each decision with its reason, so the decisions can be reviewed and measured (the etalon oracle corpus exists for that).
- It must be predictable: the same call in the same context gets the same answer, and the operator can read why.

N4 moves to TODO-settled.md as superseded by this entry.

Parked by the operator the same day. There is no default oracle because every open, locally hosted model tried so far FAILED the oracle tests (the etalon corpus, `crates/harnessd/src/etalon_oracle.rs`), and automode without a passing oracle is a model approving its own actions. The work resumes when a model passes the corpus; until then the seats keep today's modes.

## R118 - A GitHub reviewer seat: `letibot attach github <org|repo>` reviews every PR and answers as @letibot - **OPEN, filed 2026-11-03, the operator's ask**

The operator wants letibot to work like CodeRabbit on their own repositories, starting with stroppy-io: "it just attaches to whatever pr is on the repo given to it and does reviews ... responding as @letibot".

- `letibot attach github <org or owner/repo>` starts a long-lived reviewer seat for that scope. It polls with `gh api` for new PRs and new pushes to open PRs. Polling is used rather than webhooks, so no endpoint is exposed.
- Each PR is a letibot session with its own worktree of the PR head. A new PR gets a review panel (reviewers plus the gatekeeper, the same machinery as the merge queue's review) and one GitHub review: inline comments on lines and a summary. A push gets a re-review of the new commits only.
- A comment that mentions `@letibot` in the PR's thread is answered by that PR's session. The operator can attach to the session like any other.
- Identity: the seat posts as its own GitHub account or App named `letibot`, with a token scoped to the configured repositories. It never posts as the operator. The operator creates the account or App; the token lives in letibot's config, mode 0600.
- Limits: one review per push, a per-repo rate cap, and no review of a PR from a fork without the operator's opt-in (an untrusted diff runs nothing; review is read-only).
- R114 callers: a new PR or issue similar to an open todo or a running session's brief is offered as "watch this"; a session whose brief matches an open PR attaches to its thread; similar past reviews are offered as context.

Depends on R116 (a review verdict schema shared by every reviewer) and R114 for the similarity offers.

A pane, `/pane github`, lists the attachers: one line per scope (org or repo) with its state (polling, rate-limited, token missing), and under it one row per open PR with its review state (queued, reviewing, posted, re-review pending, answering a mention), the last action and its age. Enter on a PR row attaches to that PR's session; a key opens the PR in the browser; another pauses or resumes a scope. It is a plugin pane like the queue's, so the seat's state lives in the daemon and the pane only draws it.

## R116 - Claude Code as a merge-queue reviewer, through headless `claude -p` - **OPEN, filed 2026-11-02, after the operator's queue trial**

The queue hands a branch to a reviewer and needs a verdict back. Headless Claude Code fits without waking any live session:
`claude -p --output-format json --json-schema <verdict schema> --permission-mode <mode> --allowedTools <read-only list>`, started with its cwd in a worktree of the branch.

- The verdict arrives in `structured_output`. A run without that field is a failed review, not a pass.
- Store the result's `session_id`, so a re-review after new commits uses `--resume <id>` and keeps the reviewer's context.
- Time limit enforced by the queue. End a run with SIGINT, which ends the turn cleanly; SIGTERM exits 143.
- The verdict schema is the queue's existing reviewer verdict, so a Claude reviewer and a letibot reviewer are interchangeable in a panel.

Docs: https://code.claude.com/docs/en/headless.md

## R117 - The leticode MCP bridge pushes package and queue events into the orchestrating Claude Code session as a channel - **OPEN, filed 2026-11-02, research preview**

Today an orchestrating Claude Code session learns that a package finished through a tracked background `leticode-wait` per session. A channel replaces that: the bridge is already an MCP server, so it can declare `capabilities.experimental["claude/channel"]` and send `notifications/claude/channel` with `{content, meta}` when a session ends its turn, asks, fails, or a queue row changes state. An idle Claude Code session starts a turn on it.

- Claude Code sends no acknowledgement. Expose a reply tool through which the orchestrator acknowledges each event, and re-send an unacknowledged event with backoff.
- Permission asks can be relayed through `experimental["claude/channel/permission"]`.
- Claude Code must be started with `--dangerously-load-development-channels server:leticode`. It works in interactive sessions only, and custom channels are not on the allowlist.
- Keep `letibot-mcp wait` as the fallback for sessions started without the flag.

Docs: https://code.claude.com/docs/en/channels-reference.md

## R115 - A host placement's commits do not come back, its shells outlive the stop, and its notes land on the Mac - **filed 2026-11-02; left: item 1's collect verb for an already-stopped session and the branch fetched home by the daemon's last act, plus the notes block read over the backend**

Three placement bugs seen running packages on lubuntu1, lubuntu2 and lab2x1.

1. `leticode_start` says commits come back as `remote/<host>/<short id>` when the session ends. After `leticode_stop` and `letibot --stop` no such branch appears, for letibot and rano worktrees alike. The only way back is a manual fetch with letibot's own key from `~/<root>/<wt>/<last 8 of session id>`. A peer session whose permission classifier blocks that fetch cannot collect its work at all. Fix: fetch the placement's HEAD into `remote/<host>/<short id>` on stop, and add an explicit collect verb to `letibot-mcp` for a session already stopped.
2. `letibot --stop` ends the local daemon but not the shells it started on the host. Shells from stopped packages were still running on lab2x1 about two hours later, in placement directories that had been removed.
3. The `notes` tool writes the Mac worktree's `.letibot/notes` even when the session's tools run on a host. Subagents of a host session then leave uncommitted notes in the operator's local tree, and their work never reaches the placement's branch. The notes tool should write where bash, read and write do.

**Landed (this branch): item 2.** A host has no cgroup, so what a session started there is found by a mark: every command carries `LETIBOT_SESSION=<session>-<daemon pid>-<n>` in its environment (`Ssh::with_mark`), which means everything it forked carries it too, at any depth. `Ssh::end_session_processes` sweeps for it in one bounded call - `/proc/<pid>/environ` where the host has one, `ps eww` where it has not - TERMs, waits, KILLs, and answers with what is LEFT rather than with what it signalled. Two doors reach it, because neither reaches the other: the session's own close (`SshBackend::settle`, before the fetch home) and the daemon's last act (`remote::placements_down`, from `cli`'s guard and from the forced exit), since `--stop` closes no backend at all - which is why the shells outlived it. A process that clears its own environment is out of reach and the sentence says so. `crates/tools/tests/remote_sweep.rs` drives both doors against the fake ssh: the session's background job and the child a foreground call forked are gone, and a neighbour in the same checkout with its own mark is untouched. Measured RED first - the job survived the close.
**Still open?** the last act ends processes and does not bring the branch home: the `git fetch` lives in `close_backend`, and the stop path never reaches it. That is the next thing the last act should do.

**Landed (this branch): item 3.** The tool's file verbs now go through the session's `ExecBackend`, the one `write`, `read`, `grep` and `bash` use: a `NoteFs` takes the backend for any path under the backend's `workspace_path()` and `std::fs` for anything else, so the box-wide notes directory - which the harness reads on this box and the tool never writes - stays where it is. `DirEntry` gained `modified: Option<u64>`, because ordering notes newest-first and dating the caveat needed an mtime and the listing was already being read: `HostBackend` fills it from the metadata it has, `SshBackend` from `stat -c %Y` falling back to `stat -f %m` inside the shell script it already runs, and a firecode VM answers `None` because its walk carries no times. Two tests, red first: `a_note_lands_in_the_backends_tree_and_not_the_scopes_workspace`, and `remote_hosts::a_note_written_in_a_placed_session_lands_in_the_remote_checkout`, where a placed session's note lands in its own checkout and the local workspace keeps none.
**Still open?** the harness's injected notes block is still assembled from the daemon's copy (`standing_notes::gather` reads this box), so a note written on a host is real, comes home on the branch, and does not reach this session's prompt until a base rebuild after it lands here. Reading that block over the backend is a change of its own.

## R114 - Embeddings as the second similarity tier, in `letibot_tools::similarity` for every caller - **filed 2026-11-02, operator's ask; items 1 and 2 landed (`crates/embeddings`, blended into `similarity::blended` and reaching the notes and skills offers, the duplicate gate, the context claim, the waits offer and the already-done offer); left: item 3's measurement from the take-up logs (are the misses synonyms?), the paraphrase scored against the worst measured negative, and the per-round cost on lab2x1**

`crates/tools/src/similarity.rs` is lexical, and its own measurement shows where that ends: a paraphrase of the review-queue note scored 0.126, under a docker prompt's 0.133 against an unrelated note, so no floor separates them. The skill offer (R113) will meet the same gap on synonyms (presentation and beamer).

1. Engine. Decided 2026-11-02 (operator): candle is the runtime, in pure Rust, with no llama. The built-in model is a fast small one (all-MiniLM-L6-v2, or multilingual-e5-small if English-only proves too narrow), downloaded once into the data dir and never compiled into the binary. A user who wants quality gives a model file: a local directory with `config.json`, `tokenizer.json` and `model.safetensors` of an architecture candle runs (BERT family, nomic, Qwen3-Embedding, Gemma3 to verify), e.g. Qwen3-Embedding-0.6B or nomic-embed-text v1.5. An architecture the build does not know is refused at open by name, not at the first round.
   Configuration, one key with three values: `[embeddings] mode = "off"` (lexical only, the default until point 3 says otherwise), `"internal"` (candle with the built-in model, or `model = "<dir>"` for the user's own), or a URL to an OpenAI-compatible `/v1/embeddings` endpoint with its model name. A URL sends note and skill text to that endpoint, so it is only ever the operator's explicit setting, and its failures fall back to lexical and are said once per session, not per round.
   Scope: the blended score lives in `letibot_tools::similarity`, so every caller gets it at once: the notes offer, the duplicate gate on a note write and on an edit made durable (a paraphrased duplicate is the case lexical misses), the context-claim check, and the skill offer (R113). `mode = "off"` turns it off for all of them. Each caller keeps its own floor, tuned from its own log: offering a note and refusing a duplicate want different thresholds.
2. Shape. Each note and skill is embedded once when it changes, cached on disk beside the corpus; per round only the round's text is embedded. Cosine over the whole corpus, no vector store, since the corpus is hundreds of documents. Storage is a cache, not a record: `<data dir>/embeddings.db` (SQLite, its own file so `sessions.db` gets no schema bump), one table keyed by `(model, sha256 of the embedded text)` holding the dimension, the vector as an f32 blob and a last-used time. A content key needs no invalidation: an edited note is a new key, a changed `mode` or model is a new key, and rows unused for 30 days are dropped at open. Deleting the file loses nothing but time. At open the corpus's vectors are loaded into memory; a few thousand 256-1024 dim vectors are a few MB. The score is combined with the lexical one, not a replacement for it, and one floor is tuned from the take-up log.
3. Data first. R113 logs every skill offer taken and missed, as the notes offer already does. The test set is the missed cases from both logs plus the paraphrase above. If the misses are not mostly synonyms, this entry closes as not needed.

**Done when**: the paraphrase case scores above the worst negative in the measured set, the offer's per-round cost on lab2x1 CPU is stated, and the harness builds with the embedder and without llama.
**Still open?** whether R113's log shows synonym misses.

---


More callers, from the operator on 2026-11-03:

- Waits. When a brief, task or subagent prompt is written, score it against the open todo rows of the session and its relatives. A close match is offered as "this looks like todo X, owned by Y - consider `needs`/wait_on it instead of starting it".
- Already done. Score a new brief against recent commit subjects and TODO-settled.md. A close match is offered as "looks like <sha> already did this - check before starting". On 2026-11-02 four of fifteen packages found their whole scope already fixed on main.
- Compaction relevance. Rank old turns by their score against the current todos and plan, keep the relevant ones in full and summarise the rest, instead of cutting by age. This feeds R22.

## R113 - Skills are offered like notes, the index shows only pins, and a live check proves the whole path - **filed 2026-11-02, operator's ask; items 1-3 landed (the offer in the request tail over the whole frontmatter, the index listing only pins, `[skills] pin` read from both levels); left: item 4's live check on lab2x1 - `/skills` hands a test skill over, `task {skills}` preloads it in a child, an unknown name is refused, each already pinned by an in-tree test**

The prompt's skill index (`crates/tools/src/builtins/skill.rs`, `SkillRegistry::index`) sorts by source priority then name and stops at 60 rows, 4000 bytes or 800 tokens. Nothing ranks by relevance, so a box with many installed skills fills the slots alphabetically, and nothing offers a skill the way `crates/harnessd/src/notes_offer.rs` offers a note. The operator (2026-11-02): the offer is wanted, listing skills in full is not, pins are wanted.

1. Offer. Per round, score the skill corpus (`SkillRegistry::ranked`) against the latest prompt, the plan, the todo board and the model's latest text and reasoning (the notes offer reads only the prompt and tool results), and name at most two above `QUERY_FLOOR` in the request tail (`TurnRequest::tail`), the same delivery as the notes offer: never a row, never a load, dropped when the model does not take it.
   The score is lexical (`letibot_tools::similarity`), so a skill's document is its whole frontmatter (`description`, `when_to_use`, keywords), not only the name and one line: "latex presentation" must reach a skill described as beamer slides. Embeddings are a later step, not this entry.
2. Index. The prompt lists only pinned skills plus a count of the rest and the `skill` verbs. No source gets listed in full.
3. Pins. `[skills] pin = ["name", ...]` in the project file and the operator's config. A pin that names no skill is said at open, by name.
4. Live check, before 1-3 land and again after: a session on lab2x1 with a test skill in the worktree's `.letibot/skills`, `/skills` lists and hands it over, `task {skills: [...]}` preloads it in a child, and an unknown name is refused. The R112 change moved the skill roots onto `Config`, so the check covers that too.

**Done when**: a session whose prompt is about a skill's subject gets that skill offered in the tail without the model searching, an unpinned skill is not in the index, and the live check passes on main.
**Still open?** this entry's list.

---

## R105 - The work order after the 2026-11-01 review, folded with the open list - **five of the nine steps are closed (1, 2 except R99's live Done-when, 3, 5, 7); left: step 4's four live facts on lab2x1, step 6's tree-wide comment rule (478 blocks over three lines today), step 8's two decisions about script layers, and step 9, the backlog entries further down**

Record: `docs/review/2026-11-01-ab5f7841-90dd9c84.md` (three reviewers, two rounds, synthesis). In order; each step relies on the one before it.

1. Guards. Remove the 15 `#![allow(clippy::all)]` crate by crate, each replaced by targeted allows with a reason. The gate's clippy step must not pass on a warm target. A check that every `#[test]` sits on its own function (or `--list` counts per binary). A test that cannot run on the gate host fails there instead of printing SKIPPED (`crates/bridge/tests/daemon_record.rs:84-89`).
2. Orchestration bugs that cost runs: R106, R107, R108; re-check R99's own Done-when now that R101 is closed.
3. Contracts from the review: version the sessions plugin answer so a layer written for groups is refused out loud; record reviewers-on-host in DECISIONS.md and restore a test of the VM arm (`mergequeue/review.rs:700`); the single-attempt retry test (`crates/http/src/retry.rs`); the room protocol bumps only at release points.
4. R100, firecode for remote sessions.
5. De-duplication: one queue renderer (`crates/tui/src/ui/panes/queue.rs` or `plugins/builtin/queue`), one `now_ms`, one truncation helper, `crates/harnessd/tests/common`.
6. Deslop per `hands-off/templates/deslop.md`: comments to the 2-3 line rule, `.letibot/notes` merged into rewritten topic docs.
7. Structure: split `open_with_registry`, `rounds`, `run_to_completion`; break `harness.rs` into modules.
8. Plugins: script layers (`docs/plugins-plan.md` step 11).
9. Backlog, unchanged: R95, R98 half 1, R104, R89-R93, R64-R67 and the older open entries below.

**Status 2026-11-03, each step re-checked against main.** 1, 2, 3, 5 and 7 are closed. 4 is built and owes a live run on lab2x1. 6 is half: the docs and the reviewed range's code are deslopped, the tree-wide comment rule is not. 8 is not built and stops on a decision. 9 is the backlog further down.

1. **Closed.** No crate root carries `#![allow(clippy::all)]` any more; they went crate by crate, with the items annotated that the allow had hidden (d1a22002 provider, 5f799705 turn, 76b1d3b9 sessionlog, 2c604e95 tools, a91283f7 tui, 77634a08 flowy, 90c249a9 opencode, e6f1239e dialect-glm). The gate's clippy step is `scripts/clippy-gate.sh`, which touches every member's root so a warm target cannot answer for the lints (3feb2606, and agents.md says why). `scripts/check-test-attrs.sh` is the gate's second step, and rejects a `#[test]` that is not on a function of its own (d0e344cb). The daemon-record test refuses on a box without python3 instead of printing SKIPPED (6daadf89).
2. **Closed, except R99's Done-when, which is a live run.** R106 landed as `ecae1af7` (`a_session_that_joins_a_daemon_runs_its_own_model`), R107 as `f1e7f9ec` (with `crates/bridge/tests/wait_activity.rs` holding a wait on a running job and answering through it when the daemon says quiet), and R108 is closed in TODO-settled.md (`8d1b0f2a`). R99's code is `ccf7b895`, `559048a7` and `36752792`; its Done-when, six packages placed on lab2x1 at once and a forced mid-write drop, needs a real host and a real sshd, so nothing in a sandbox can say it.
3. **Closed.** The sessions answer is versioned: `MOVED` (`crates/plugins/src/lib.rs:98`) and `[contracts] pane_sessions = 3` refuse a layer written for the groups at load, naming the layer and both numbers (`b5a2925a`, `a_layer_written_for_the_old_shape_is_refused_and_the_layer_below_answers`). D18 records reviewers-on-host, and the VM arm's test is back (`fcc559aa`, `a_project_that_asks_for_vms_gets_its_reviewer_placed_in_one`). The single-attempt retry test is `one_attempt_means_no_retry_and_the_default_still_retries` (`59c9c90d`). The room's number moves at release points, said above the constant (`609521f9`).
4. **Built; the shape has tests now, the live run still owes four facts.** `Placement::RemoteVm` is built, and the two arms the review found asserted nowhere have tests: the checkout a VM placement makes is a clone and a host placement's is a linked worktree (`7a964d46`), and the mode a placement puts a session in is `mode_in_vm`, with `RemoteVm` asserted at allow-all (`16e91852`). Still owed are the lab2x1 run's four facts: item 2 with a real `firecode` copy carrying a commit out, item 4 with a resumed session, the guest's `cargo test` with no card, and the copy-out's cost off the daemon's own line.
5. **Closed.** One queue renderer (`e147d979`: the pane is the `queue` layer's answer and the head hosts it), one clock (`682e5a69`, fifteen copies into `letibot-metrics`; the `i64` sites keep a named two-line adapter, and the copy that arrived after it went in `397b3ef2`), one truncation helper per meaning in `letibot_transcript::text` (`2b356d3a`; two aliases went in `397b3ef2`), and the tests' shared support is `crates/harnessd/tests/support` (`a43d2748`), not `tests/common`.
6. **Half.** The docs were deslopped one by one, and the notes were merged into them and cut: `.letibot/notes` holds 160 files, with the ones whose content moved deleted in the same commit. The comment rule is not tree-wide: `python3 scripts/comment-blocks.py count $(git ls-files 'crates/**/*.rs')` still finds 456 blocks over three lines.
7. **Closed.** `harness.rs` is 1,894 lines and its parts are `crates/harnessd/src/harness/` (`open.rs` e4e00ff0, `turns.rs` 967c5718, `children.rs` 0d2aca9c, `models.rs` 42931d7c, `wall.rs` c2a9c60d and the rest), and the three long functions are named steps (`rounds` 7aa68541, 60a3af8a, 8767a6f6, 8e3b3a9a; `run_to_completion` c86e6f44; `open_with_registry` 43f26515, 2b0701a2, 3ce469cf, 592241c4).
8. **Not built, and it stops on two decisions this entry cannot take** (`docs/plugins-plan.md` step 11): where a JavaScript engine comes from, with none in `Cargo.lock` and none in the offline registry cache, and the JS-side contract, including how the shadow gate's replay stays exact with a clock and `Math.random` inside it.
9. **Unchanged**: the backlog entries further down this file are their own work.

**Done when**: steps 1-8 are closed or moved to their own entries. **Still open?** step 4's live run, step 6's comment rule, step 8's two decisions, and step 9's backlog.

---

## R95 — A variable an earlier command assigned is unread in the next one — **filed 2026-10-27; left: the environment the gate reads is only the spawn seed, because no production code opens a `ShellSession` — `SessionEnv::observe` and `shell_path` are reached by tests alone, so an `export` in one `bash` call still does not reach the next (the corpus counterfactual: 108 + 12 commands)**

About 500 recorded commands (`$p`, `$d`, `$pr`, `$CARGO_TARGET_DIR`, …) use a variable the
session set in an EARLIER `bash` call, and G3 resolves only a variable assigned earlier in the
SAME command, so these stay unread (`docs/boundary-and-adjudication.md` §4i.4).

**The operator's decision (D14):** the session's environment is tracked, and a value the session
set may make a command readable for adjudication. Observed, not inferred: the persistent shell's
trailer (`ShellSession`, which already reports cwd) reports the environment after every call.
`PATH` is recorded but not used for resolution until R96 is ruled.

**Done when**: a design note in `docs/boundary-and-adjudication.md` (where it is observed, its
lifetime across a daemon restart, a migrated session, a confined and a remote shell, and how the
card shows it); the resolver reads the tracked environment; the corpus replay shows the
cross-command bucket gone without any command becoming unread; the card names a variable the
session set when it decided the reading.

**And R96, `$PATH`, decided 2026-10-27 (D15): resolved too, built with this.** 527 commands
on lab2x1 and 1,347 on the mac. When `PATH` is resolved, each stage's program is resolved to the
file it will run, and the stage, shape and card name that file when it differs from what the
bare name finds today, so a grant given for one binary cannot settle a stage that runs another.

**Still open?** grep the corpus replay's unread list for variables with no assignment in the
same command.

---

## R98 — macOS has no boundary for a command, so tests there must ask (D16, amended) — **half 1 BUILT 2026-10-27 (the policy, `runs_project_code_unconfined`), and half 2's code is in the tree (`crates/tools/src/exec/seatbelt.rs`, `crates/tools/tests/seatbelt.rs`); left: the one run on a Mac — `cargo test -p letibot-tools --test seatbelt`, then a session at `writes-allowed` — which no Linux box can stand in for**

D16 admits a command without a card only inside a measured boundary, and its amendment says
that on a raw host a command that executes code the agent wrote (a test runner) must keep
asking, and no standing grant may admit it, because the oracle does not read a runner's files.
The macOS host shell has no boundary at all (firecode VMs are the only macOS boundary letibot
has, `docs/firecode-on-macos.md`), so every `cargo test` on the Mac is either asked about or
theater.

Two halves, the first smaller and first:

1. **Make the rule true on a raw host now.** A `bash` call whose stages run a test runner or a
   build that executes project code (`cargo test|run|build` with build scripts, `pytest`,
   `go test`, `npm test`, `make`, a script the session wrote) is not settled by a session or
   durable grant when the backend is unconfined; it asks every time, and the card says why
   (*"runs code the agent wrote, on a host with no boundary"*). Measure what this costs on the
   corpus replay (how many past grants would no longer settle) before landing. — **BUILT.** — The
   rule is the policy now, and it has no setting: `runs_project_code_unconfined` reads one fact,
   `has_measured_boundary`, which `Harness::open` hands it off the backend — so a Linux test
   constructs the Mac's case by handing the gate `None`, and nothing in the gate probes a platform.
   `[permissions] ask_unconfined_code`, `with_unconfined_code_asks` and the startup note that
   disclosed it are gone with the switch — in `crates/leticode-config`, and in
   `crates/harnessd/src/harness/open.rs`. Tests:
   `a_host_with_no_measured_boundary_asks_with_nothing_switched_on`,
   `the_same_runner_is_admitted_inside_the_boundary` (unchanged) and
   `the_card_narrows_the_ladder_and_says_why`, whose note must say *no boundary*. The cost was
   measured on the corpus before it was turned on — `docs/boundary-and-adjudication.md` §R98 (1),
   **171** of the mac's 834 standing settles would ask, 137 of them the session's own programs and
   24 `cargo test`.
2. **Give the Mac a boundary** — **BUILT 2026-10-27**, `crates/tools/src/exec/seatbelt.rs`, and
   written up in `docs/boundary-and-adjudication.md` §4k. Seatbelt (`sandbox-exec -f <profile>`)
   renders the same `ViewSpec` the Linux side renders as a bubblewrap argv, and the probe measures
   it from inside: the deny list beating an allow (a canary inside a writable `$HOME`), a write
   outside the checkout refused, a write inside it succeeding, fork/exec, a nested `sh -c` denied
   the same path, `kill -0` on the launcher `EPERM`, and both egress subjects refused when egress
   is denied. `confine::probe` chooses the mechanism by platform, so `HostBackend::confined`, the
   session's `/tmp` and every grant re-probe through one dispatch, and a Mac session's `bash`,
   jobs and persistent shell all run inside it. A Mac that cannot measure one is **unconfined and
   says so** (`boundary_absent` on the session's log, with the refusal's own words).

   The measured cost: **+5.1 ms per exec** (100 `true` calls: 1.28 → 6.41 ms/call), +4.5 ms on a
   real `cargo --version`, and it does not grow with the command's work. What macOS cannot give —
   a PID namespace, so processes are unreachable but not absent, and no `PR_SET_NO_NEW_PRIVS` —
   is printed on the boundary's own description as `ABSENT` / `NOT APPLICABLE`, never omitted.
   Reaping stays the process-group tree (`exec::scope::ProcessGroups`), as it already was. — ✓ fixed in 5442b7c8, test the_probe_measures_a_boundary_and_says_what_macos_cannot_give

   **What half 2 still needs: a Mac.** `crates/tools/src/exec/seatbelt.rs` and
   `crates/tools/tests/seatbelt.rs` are in the tree now, and nothing on a Linux box can run
   `sandbox-exec` or a `#![cfg(target_os = "macos")]` test, so the profile has never been read by
   the kernel that enforces it and D16's admit *inside* a Mac boundary has never run.

**Done when**: (1) on an unconfined backend no grant settles a test run, with a test; (2) a Mac
session's commands run inside a measured boundary that `~/.ssh` and the profiles are absent
from, and D16 admits them there. — **(1) is done**: the rule is the policy and
`a_host_with_no_measured_boundary_asks_with_nothing_switched_on` proves it from the injected fact,
with the ladder narrowed and no grant recorded. **(2) is done on its Linux side**: macOS has no
mount namespace to make `.ssh` and the profiles *absent*, so they are **denied** rather than absent,
the probe measures the denial, and the description says which of the two it is holding. The
`bwrap` half is measured on this box; the seatbelt half is not.

**Still open?** One run, on a Mac: `cargo test -p letibot-tools --test seatbelt` (seven real
measurements, no skip), then a session at `writes-allowed` — inside the seatbelt boundary `cargo
test` must be admitted with no card (D16), and on a Mac whose boundary cannot be built it must ask
every time with the card saying *no boundary* (R98 (1)). Half 1's half of that sentence is proved on
Linux; half 2's is a reading of a profile until that run happens.

---

## R100 — A remote session cannot run in a firecode VM: `--host` and `--vm` are two placements, not one — **BUILT and on main (`ad447d53`, `46949113`, `e4c69ac2`); left: the live proof - the four facts the 2026-11-02 lab2x1 run did not reach (a real `firecode` copy of a clone carrying a commit out, a resumed session keeping its clone, the guest's `cargo test` with no card, and the copy-out's cost off the daemon's own line); filed 2026-10-28, operator's ask**

The operator: *"how to run those remote letibots in firecode"*, and *"lab2x1 has firecode, it even
has source code for it"*. It does: `~/bin/firecode` (login `PATH`), the source in
`~/Projects/firecode`, and `/dev/kvm` with an ACL for the operator's user (checked 2026-10-28;
`firecode ls` answers). What is missing is on letibot's side: `Placement` is either `Firecode`
(local, this Mac) or a remote host, and `cli.rs` says a remote session is not in a VM.

**Build**: a combined placement, `leticode --host lab2x1 --vm` — `firecode up`/`in` driven over the
session's existing ssh link (login shell, so `~/bin` is on `PATH`), the session's tools inside that
guest, the VM measured as the session's boundary so D16 admits builds and tests in it, and the
commits coming back the way remote placements' do. Then `leticode_start {host, vm: true}` in the
bridge. This is also where Docker for sessions belongs (the operator, 2026-10-27: *"firecode suites
better here"*): lab2x1's `dockerd` is rootful, so the socket is root on the host, and a container
inside a guest stays inside a boundary.

**Done when**: a package started with `{host: "lab2x1", vm: true}` builds and tests letibot inside a
guest on lab2x1 at `writes-allowed` without a card, and its commits come back.

**Landed (2026-10-31): the SILENT DROP is fixed; the combination is refused by name.**
`--where` and `--host` wrote ONE field (`cfg.placement`), so the flag that came LAST won and the
other ask was dropped without a word: `--host lab2x1 --where firecode` booted a VM **here** on a
copy of this box's workspace, and `--where firecode --host lab2x1` ran on lab2x1 with no VM at all.
Neither refused anything, so both looked healthy. The two asks are now recorded separately and
resolved once after the whole line is read, by `cli::placement_from_flags`, which refuses the pair
with the reason and says what each flag alone does; `--help` says the same. **What the pair SHOULD
mean is a VM on `lab2x1`, whose copy source is the remote checkout** — and that is still not built,
because `FirecodeBackend` runs a `firecode` binary on THIS box and holds THIS box's paths
(`FirecodeSpec::bin`, `~/.cache/letibot/firecode`, a promoted job's scratch file, the `.git` it
reads to decide a worktree is mounted). It needs a remote runner for the firecode backend, not a
flag. `docs/subagents.md` §2 carries the decision. Tests:
`crates/harnessd/tests/cli_placement.rs` (both flag orders, through `cli::run`) and the pair test in
`cli::tests`, which now asserts the pair IS the placement (the build below). — ✓ fixed in ad447d53, test a_vm_and_a_host_are_one_placement_whichever_flag_came_first

**Built (2026-11-01, branch `firecode-remote`): the pair is the placement.** `leticode --host lab2x1
--vm`, `leticode_start { host: "lab2x1", vm: true }` and `where: "host:lab2x1:vm"` are one
`Placement::RemoteVm(name)`; either flag alone still means what it did. `SshSpec::vm` carries the
ask into `SshBackend::place`, and five things follow from it:
— ✓ fixed in 46949113, test a_vm_and_a_host_are_two_asks_and_the_pair_is_a_vm_there

1. **The host is asked once per host per process** (`Ssh::probe_firecode`): `command -v firecode` in
a login shell, and `$LETIBOT_KVM_DEVICE`/`/dev/kvm` readable and writable by this connection's user.
A host that cannot boot a guest is refused **by name before anything is arranged there** — no
bundle, no checkout — and a probe that did not answer is not cached. — ✓ fixed in 46949113, test a_host_without_firecode_is_refused_by_name
2. **The checkout is a CLONE when the ask is a VM**, not the linked worktree every other placement
gets. The guest holds a *copy* of the directory, and a worktree's `.git` is a file naming a
directory in the mirror that the copy does not carry — so no commit inside the guest would have
worked. `bring_back` fetches the branch from the checkout in that case, from the mirror otherwise. — fixed in 46949113 in code, untested: nothing asserts the VM checkout is a clone rather than a linked worktree, so a revert to `git worktree add` would leave a guest copy with no repository and only a comment to notice
3. `firecode up --project <checkout>`, and then **every call is `firecode in --project <checkout>
--cwd <checkout> '<script>'`** — `Ssh::remote_command` wraps every door at once, so a command, a
file read, a file write, a job's launcher, a poll and the group kill all run in the guest, and the
cancel path builds its own command for the same reason. — ✓ fixed in 46949113, test a_vm_placement_boots_the_guest_and_runs_every_call_in_it
4. **The guest is the boundary**, so `cfg.placement.is_vm()` puts the session at `allow-all`
(`Harness::open`'s `in_vm`) and no `bwrap` is probed for. — fixed in 46949113 in code, untested: no test opens a session at `Placement::RemoteVm`, so the `allow-all` arm (`crates/harnessd/src/harness/open.rs:594`) is asserted nowhere
5. **On close: `firecode down`, then the guest's work is taken onto the host checkout** — `down`
leaves a sibling `<checkout>-<stamp>` when something changed, that copy's branch is fetched into
the checkout and fast-forwarded, and the branch then comes home as `remote/<host>/<short id>` as
every host placement's does. — ✓ fixed in 46949113, test a_commit_made_in_the_guest_comes_back_as_the_branch

**Tests** (`crates/tools/tests/remote_vm.rs`, a fake `firecode` on the fake remote's `PATH` through
the same fake ssh `remote_hosts.rs` uses): `up`, then `in` for the session's calls, then `down`, in
that order; a commit made in the guest arrives as the branch that came back; a host with no
`firecode` is refused by name with nothing arranged there; and a host placement never asks for a
guest. Plus `cli::tests::a_vm_and_a_host_are_two_asks_and_the_pair_is_a_vm_there`,
`bridge::tools::tests::the_vm_flag_turns_a_host_into_a_vm_on_that_host` and
`bridge::daemon::tests::a_vm_on_a_host_is_both_flags`.

**The live run on `lab2x1` (2026-11-02), and the two defects it found.** `leticode --host lab2x1
--vm` **booted a guest** and a **foreground `bash` ran inside it**, so half of item 1 is answered for
a command that travels in the command text; a manual `firecode down` there took **15 s** and
delivered the copy to `<project>-<run id>`. It did not reach item 2, and item 1's `cargo test` (no
card) and item 4 were not tried, because:

1. **A background job never started.** Its launcher is staged by `Ssh::write_abs`, whose bytes travel
   on the ssh's **stdin** (`base64 -d > "$t"`). `firecode in` sends the command over the vsock and
   never the caller's stdin, so the guest waited for an end that cannot come: ten calls sat for 52
   minutes and the jobs directory was never made. Fixed: a step fed on stdin now takes the guest's
   own road, the payload in the command when it is small (`VM_INLINE_WRITE`) and staged on the host,
   where stdin works, then carried in by `firecode cp` above that. The audit of every stdin-fed call
   in `remote.rs` and `firecode.rs` found this one and no other: `Ssh::write_abs` is the only one,
   `firecode.rs` feeds no stdin at all (its writes are inline base64 or `firecode cp`), and the two
   other `run_bounded_fed` sites (`mergequeue/gate_host.rs`, `room/supervise.rs`) feed a declared
   host's ssh and never a guest. — ✓ fixed in e4c69ac2, test a_background_job_starts_in_a_guest_whose_client_forwards_no_stdin
2. **A one-shot stopped with SIGINT left the VM up**, with the calls still stuck in it. The forced
   exits (`stop_on_signal`'s second signal and its 5 s give-up) end the process without closing a
   backend, and the orderly stop closes no backend at all. Fixed: a guest this process booted is on
   one list (`remote::guests_down`), bounded at 30 s for `down` and 10 s for the copy-out, and every
   exit path runs it once and prints one line saying whether the VM came down and where the copy
   landed. A session's own close takes its guest off that list, so no guest is brought down twice. — ✓ fixed in e4c69ac2, test a_guest_nothing_closed_is_brought_down_once_by_the_last_act

**Still owed, for the next live run**: item 2 (whether a real `firecode` copy of a clone carries a
commit out), item 4 (a resumed session keeps its clone, and a file one command writes is there for
the next command), the `cargo test` inside the guest with no card, and the copy-out's real cost read
off the daemon's own line rather than a manual `down`. — OPEN: no live proof — the 2026-11-02 run saw a guest boot and one foreground `bash` inside it and nothing more, so the guest `cargo test` with no card, item 2, item 4 and the copy-out's cost are still untried on lab2x1.

**Done when**: a package started with `{host: "lab2x1", vm: true}` builds and tests letibot inside a
guest on lab2x1 at `writes-allowed` without a card, and its commits come back.

---

## R104 — A ⌘-chord reaches the head as its plain letter, because rano drops the super bit — **OPEN, filed 2026-10-29, rano's**

MEASURED 2026-10-29 on this box, rano pinned at `bf3fa48`: the operator selects with the mouse
and types ⌘C. The text is copied (the terminal's own copy, and this head's drag release), **and a
`c` lands in the composer** — because the chord never arrives as a chord. Ghostty sends it as
`CSI 99;9u` (modifier 9 = 1 + 8, and bit 8 is super — this head does ask for the kitty keyboard),
and:

* rano's `Mods::from_bits` is `Mods(b & 7)` (`src/term/event.rs:27-29`, and there is no super
  constant at all), so the bit is discarded before any letibot code sees it; — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter
* `kitty_key` then re-encodes the character through `kitty_legacy`, which carries alt and ctrl
  only (`src/term/decode.rs:402-421`, `482-512`), so `CSI 99;9u` decodes to `Key::Char('c')` with
  empty mods; — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter
* and the one door to the raw bytes is lossy the same way — `Terminal::raw_input` answers
  `legacy_bytes` under the kitty keyboard (`src/term/terminal.rs:504-514`). — OPEN: rano's `legacy_bytes` still turns a `CSI … u` into the legacy spelling (`crates/tui/src/app/keys/input.rs:571` pins it) and that is the door a `!term` pane's program reads (`crates/tui/src/head.rs:1041`), so ⌘C still types a `c` into a pane — a decision, not a line. Rechecked at rano v0.10.1 (4e16345f): the super bit is still carried and the head's chord test passes on it, and `Terminal::raw_input` still answers `legacy_bytes`, so this bullet stays open.

**The fix is two lines in rano**, not in this tree: keep the bit in `Mods::from_bits`, and in
`kitty_key` emit `Char(ch)` **with its mods** when a bit the legacy spelling cannot carry is set.
Verified against a scratch copy of the pinned rev: both `CSI 99;9u` and `CSI 99;13u` then decode
to nothing, every existing decode test still passes, and **letibot needs no change** — `key()`'s
last character arm already answers `None` for a modifier it does not bind. — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter; the head's own bindings landed in 8eaa4985, so "letibot needs no change" was not quite so.

**And ⌘⌃C is worse than the report**: with the super bit masked it decodes to `CtrlC`, which
interrupts the turn instead of copying. — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter (both bits are kept and the head answers nothing)

`crates/tui/src/app/tests/keys.rs` carries a characterization test pinning today's decode
(`a_cmd_chord_arrives_as_its_plain_character_because_rano_drops_the_super_bit`), written to fail
loudly with instructions the day rano keeps the bit, and
`a_selection_does_not_swallow_what_is_typed` refuses the tempting wrong fix (a head that goes deaf
under a selection). — ✓ fixed in 8eaa4985, test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter (the characterization test and its file are gone; a_selection_does_not_swallow_what_is_typed is at `crates/tui/src/app/tests/editor.rs:1136`)
**Still open?** `printf '\033[99;9u'` into the key decoder, or press ⌘C over a selection after a
rano bump — a `c` in the composer means it is still rano's. — ✓ fixed in 46f8409b (rano f4c624e), test a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter — the decoder is fed exactly those bytes and no `c` comes out

---

## The review of 2026-10-12 — five read-only reviews of main at `50b3e55`, filed as R89–R93

The operator's ask: *"start review subagents - we need to find edge cases, ux inconsistencies,
notifications that posted in full instead of line + popup pattern etc etc"*. Five subagents read
the code (and `sessions.db` / `metrics.db` read-only); nothing was built or changed. Every finding
is below as its own entry; each report is kept verbatim in section 4 under *"The review of
2026-10-12, in full"*, and the numbers in brackets point into it. Line numbers are on `50b3e55`.
CONFIRMED means the reviewer read the whole path or saw it on disk; SUSPECTED means it inferred
the last step. **Still open?** for each: re-read the cited lines on the current main.
**R89** is settled 2026-11-03 and its entry is in `TODO-settled.md`; the other four are below.

---

## R90 — Keys, cards and popups disagree, and several send an answer the operator did not pick — **filed 2026-10-12; left: item 27 (a todo row that waits with no card open notifies nobody) and item 32 (the `allow_session` id still drawn on every permission row)**

Paths under `crates/tui/src/`. Ranked by how likely the operator hits it. All from reading code.
**1–4 and 6 can send an answer or `/reseat` the operator did not choose** — do those first, each
pinned by a key-sequence test.

1. **The reseat card takes a pane's keys.** `key_reseat_card` (`app/reseat.rs:183-230`) runs before
   `key_surface` (`keys/mod.rs:235` vs `:239`); `slot_taken` (`reseat.rs:110`) ignores open
   surfaces; the hint bar shows the pane's hints. Enter on a todo row, or `1` in ctrl-s, sends
   `/reseat`; Tab and Esc in todos act on the card. [keys 1] — ✓ fixed in 12d33550, test an_open_pane_takes_the_slot_and_its_keys_never_reach_the_card
2. **Digits answer question cards whose rows are not numbered** (`keys/cards.rs:415`); typing `16`
   answers choice 1. The permission bar also names row numbers it does not draw; the reseat and
   quit cards do number theirs. [keys 2] — ✓ fixed in fee16209, test a_digit_on_a_question_marks_its_numbered_row_and_enter_answers
3. **A question arriving — or brought back by alt-a — over composer text takes that text as its
   answer** (`composer.rs:193-213`; `todos.rs:316-352, :136-165`). The permission path holds the
   words; the question path sends them. [keys 3] — ✓ fixed in 07509a3c, test a_question_arriving_over_typed_words_holds_them_instead_of_taking_them
4. **A second decision resets the highlight on the card on screen** (`events/asks.rs:59` `sel = 0`
   while `open[0]` is unchanged): ↓↓ to deny, a second permission arrives, Enter allows. And `sel`
   is not reset when the front card changes. [keys 4] — ✓ fixed in 07509a3c, test a_decision_arriving_behind_the_card_does_not_move_its_highlight
5. **Esc means something different on every card; on a permission card it falls through and arms
   the Esc-Esc interrupt** (`cards.rs:397`). The prompt card has no way back (no alt-a, no resident
   line). [keys 5] — ✓ fixed in 4d95678c, test esc_puts_a_permission_card_away_and_alt_a_brings_it_back
6. **The quit chord over a decision**: key order `key_decision` (`keys/mod.rs:224`) before
   `key_quit_card` (`:227`), draw order the same, but the bar checks `quit_card` first
   (`hint_bar.rs:83`) — `1` meant as "detach" answers the permission. [keys 6] — ✓ fixed in d70d70f5, test the_quit_card_never_stands_over_a_decision
7. **Editor focused: a decision is drawn but every key edits the file** (`editor.rs:411-440`; the
   bar checks the editor before `open`, `hint_bar.rs:116-119`). [keys 7] — ✓ fixed in fe3c131d, test a_decision_arriving_takes_the_keyboard_back_from_the_editor
8. **Todos pane: Enter on a row waiting on you opens its question, the bar says "reads it whole"**
   (`keys/bindings.rs:412`). [keys 8] — ✓ fixed in 7a493c3f, test the_todos_bar_says_enter_answers_a_waiting_rows_question
9. **No "+N more waiting"**; a head-local todo card at `open[0]` can hide a model question with a
   deadline. [keys 9] — ✓ fixed in 9d327dce, test the_front_card_counts_what_waits_behind_it
10. **↑↓ do not scroll /help or /status and recall history instead** (`bindings.rs:157` READING has
    no UPDOWN); the following Enter is swallowed. [keys 10] — ✓ fixed in 7a493c3f, test the_arrows_scroll_a_reading_surface
11. **Enter silently dropped on reading overlays when a line is typed** (`owns_enter`); TodoRow
    swallows all keys while Help lets letters through. [keys 11] — ✓ fixed in 7a493c3f, test a_swallowed_enter_over_a_reading_pane_is_said
12. **A todo-ask card is not closed when its row stops waiting** (`events/todos.rs:16-30`,
    `frames.rs:115-127`); answering sends `AnswerTodo` for a stale question. [keys 12] — ✓ fixed in 4786af0f, test a_todo_card_closes_when_its_row_stops_waiting
13. **The tab says Waiting with nothing waiting** — `attention.rs:25, :48` count `away` across all
    sessions and stale todo cards. [keys 13] — ✓ fixed in 4786af0f, test the_tab_waits_only_on_what_waits_in_this_session
14. **A resync drops an open todo-ask card and its draft becomes a prompt** (`events/mod.rs:343`).
    [keys 14] — ✓ fixed in 4786af0f, test a_resync_puts_the_todo_card_away_with_its_draft
15. **In a subagent, Esc puts a question away and the next Esc leaves with it unanswered and
    unseen** (`cards.rs:397`). [keys 15] — ✓ fixed in 4d95678c, test esc_keeps_every_other_meaning_it_has_while_inside_a_subagent
16. **Todo draft card vs an arriving decision: drawn one, keyed the other** (`keys/mod.rs:189` vs
    `screen.rs:173`); same for `key_ask`. [keys 16] — ✓ fixed in fe3c131d, test a_draft_card_in_front_of_a_decision_is_drawn_keyed_and_barred_as_one
17. **`/peek ID` says "esc back to the subagents" whatever is below** (`bindings.rs:298, :478`;
    `frames.rs:304`); a late `Peeked` reopens after the pane closed. [keys 17] — ✓ fixed in 81463c0b, test a_late_peek_reply_after_its_pane_closed_is_not_opened; the bar's half in 7a493c3f, test a_peek_over_no_pane_names_the_conversation_as_where_esc_goes
18. **Mode/model pickers' bar shown while a decision is open** (`hint_bar.rs:120`). [keys 18] — ✓ fixed in fe3c131d, test the_bar_names_the_decision_over_a_picker
19. **Cards that own the keyboard (secret, prompt, key_ask, todo_draft, term_ask, mode_confirm) show
    conversation chords in the bar**; pressing the advertised ctrl-t cancels allow-all. [keys 19] — ✓ fixed in fe3c131d, test a_card_that_owns_the_keyboard_has_its_own_bar
20. **/help omits alt-a, alt-r, alt-p, Esc-puts-away, digit answers and /reseat**; alt-p has two
    meanings, one documented. [keys 20] — ✓ fixed in 9d327dce, test the_help_names_the_card_keys
21. **alt-p does nothing visible while another card holds the slot**; the card pops later, maybe in
    front of the next Enter. [keys 21] — ✓ fixed in 12d33550, test an_open_pane_takes_the_slot_and_its_keys_never_reach_the_card
22. **Reseat/quit/todo cards say "pgdn scrolls" but `scroll_card` (`keys/chords.rs:263`) needs
    `open` non-empty.** [keys 22] — ✓ fixed in fd826964, test the_page_keys_scroll_the_reseat_card_that_says_they_do
23. **A question with no choices shows "↑↓ to choose"**; an empty-string choice is a blank row
    (`cards.rs:402-411`). [keys 23] — ✓ fixed in fee16209, test a_question_without_choices_offers_no_arrows_and_an_empty_choice_is_named
24. **Options containing " | " are re-split** (`asked_card.rs:43`); the todos pane joins with
    " / " (`panes/todos.rs:396`). [keys 24] — ✓ fixed in 16ce45f3, test the_options_are_a_list_and_the_old_line_still_parses
25. **Todos pane says "enter on the row, or alt-a, answers it"** but alt-a opens the first waiting
    row (`todos.rs:316-365`). [keys 25] — ✓ fixed in 7a493c3f, test the_todos_bar_says_enter_answers_a_waiting_rows_question
26. **The waiting line**: stays while the card is open; alt-a answers about the other card; counts
    only todo questions (`ui/status.rs:159-163`). [keys 26] — ✓ fixed in 9d327dce, test the_waiting_line_is_about_what_is_not_on_the_screen
27. **Every open ask notifies "permission needed"** (`attention.rs:80`); reseat and todo waits
    never notify. [keys 27]
— OPEN: an open card is now named for what it is, but nothing notifies for a reseat card or for a todo row that waits with no card open — `Attention` (`crates/tui/src/app/attention.rs:305-310`) has no reseat and no board field, and `attention_now` (`crates/tui/src/app/attention.rs:44-63`) counts only open/away asks, the secret and finished subagents, so no notification edge can fire for either.
28. **Popup footer and bar wrong while a decision is open; "PgUp PgDn" vs "pgup/pgdn"**
    (`diff_popup.rs`). [keys 28] — ✓ fixed in b553d2b0, test the_popup_edge_drops_the_keys_a_decision_takes
29. **Question/permission cards stay until `DecisionAnswered`, a second Enter resends; todo cards
    vanish at once.** [keys 29] — ✓ fixed in 85f61957, test an_answered_card_closes_at_once_and_a_refusal_reopens_it
30. **The quit card's Enter has no empty-composer guard** — typed words are lost on quit. [keys 30] — ✓ fixed in d70d70f5, test the_quit_cards_enter_keeps_the_empty_composer_guard
31. **Narrow widths**: reseat rows trimmed (79-char summarise row cut), popup overflows below ~22
    columns. [keys 31] — ✓ fixed in b553d2b0, test the_popup_fits_a_narrow_screen, the_reseat_rows_wrap_on_a_narrow_screen
32. **Wording**: two casings of the question hint; `allow_session` ids on rows; the held-prompt
    notice (`composer.rs:330`) says it goes to the model when the run ends but needs Enter again;
    /help says ctrl-c twice "quits" but opens the card; alt-a for two meanings. [keys 32]
— OPEN: the id is still drawn on every permission row, `Allow session  (allow_session)` — the head passes `option_id` straight through (`crates/tui/src/ui/cards/decision.rs:146`) and rano appends it to the label, and `the_ladder_survives_a_card_whose_content_is_taller_than_the_screen` (`crates/tui/src/app/tests/decisions.rs:1006`) pins it there by asserting `allow_session` is on the screen. The item's other four halves are fixed (fee16209, 9d327dce).
33. **The waiting line's "alt-a to answer" is not clickable** while the reseat strip's are
    (`app/keyhints.rs scan_row`). [keys 33] — ✓ fixed in 9d327dce, test the_waiting_line_is_about_what_is_not_on_the_screen

---

## R91 — Results, restore, reseat and the store: messages lost and wrong states — **filed 2026-10-12; left: items 9, 10, 15, 16 and 18 (another daemon's instance read as gone, the 30-minute busy wait, a child's unheard `[meanwhile]` turn, the per-delta R79 sort, a root close dropping held results)**

Ranked severity × likelihood. **1 and 2 lose data; 3 fires waits on the wrong job.**

1. **Messages handed back by a failed or interrupted round are dropped.** `harness.rs:11188` makes
   a new `HubSteering` (`self.steering()`, `:12214`, empty `held`) on every pass of the HTTP retry
   loop; `give_back` (`turn/src/engine.rs:1418/1438/1464/1474`, local 760–987) pushes into
   `HubSteering::held` (`:1686`), which is dropped. Lost: an operator follow-up typed mid-stream
   then esc-esc or a provider drop; R79 immediates already marked delivered in the store
   (`at_round_boundary` `:1628`); monitor firings (cursor advanced, `:1646`); parent messages;
   AnswerTodo lines. The operator's words are already in the authorisation trail (`:1522`). Fix:
   build `steering` once per round outside the retry loop; on final failure push the rest back.
   CONFIRMED. [delivery 1]
2. **A fork registers the new transcript before writing its rows.** `put_fork` (`:10464`),
   `set_prefix_fingerprint` (`:10483`), `set_stale_prefix(None)` (`:10498`) all before
   `engine.open` (`:10502`), `append_items` (`:10631`) and the chunked persist (`:10685-10699`). A
   crash mid-carry resumes onto a partial conversation; an error after `put_fork` leaves the dead
   fork newest and clears the reseat offer; `reingest` returns at `:9880` before `self.prefix =
   next` (`:9892`), so metered turns send old schemas against the new transcript, and the operator
   is told "Nothing was changed". CONFIRMED (write order). [delivery 2]
3. **Job handles restart at `j1` every daemon life** (`tools/src/exec/jobs.rs:44-48`); the store's
   `job`/`job_context` upsert by `(session_id, handle)` keeps the old command and overwrites history
   (`store.rs:490-501, :3352, :3357-3361`); `is_running`, `job_fate` (`harness.rs:13719-13730`),
   `delivery::names` (`delivery.rs:247-249`) and the TUI link (`app/todos.rs:763-860`) match a
   pre-restart `j7` to the new one. Also reported by the lifecycle review. CONFIRMED. [delivery 3,
   lifecycle B13]
4. **An interrupt is followed at once by a turn nobody asked for** — `note_turn_ended`
   (`:10779, :5481`) runs however the turn ended, `JobWatchers::turn_ended`
   (`jobwatch.rs:660-671`) rings the wake when anything is held, and `stop_children_first`
   (`:1535`) makes children settle right after. Failed turns too. Fix: tell `turn_ended` it was
   interrupted; ring nothing; batch for the next operator turn. CONFIRMED. [delivery 4]
5. **A job result held when the daemon stops is never delivered** — restore collects only
   `state == "running"` (`:8784-8787`); `job` has no `delivered` column. CONFIRMED. [delivery 5]
6. **Store writes that skip the busy retry**: all of `tokencore/src/store/subagents.rs`
   (`put_subagent_run` 167, `settle` 201, `interrupt` 212, `deliver` 222, `put_monitor` 267,
   `end_monitor` 281, `put_job_context` 316, `life_*` 355/366/375) — a failed deliver redelivers,
   a failed interrupt counts toward `RESUME_LIMIT`. And `persist_context(..)?` (`:11561`),
   `flush_todos()?` (`:11791`) and the fork writes end the turn after 60 s instead of waiting.
   CONFIRMED. [delivery 6]
7. **A failed todo write is never retried** — `flush_todos` sets `todos_version` before
   `put_todos` (`:12478-12487`); a restart then reverts answers, waits and consumed conditions.
   CONFIRMED. [delivery 7]
8. **The one-hour restore threshold measures daemon downtime, not stop → reopen**
   (`back_ms = started_ms()`, `:8762, :8831, :8885`); a lazy reopen 4 h later reads "down 5m" and
   monitors get a fresh TTL. CONFIRMED. [delivery 8]
9. **Restore treats any other daemon instance as gone** (`:8771-8781`; `subagent_record` decide
   263, resumable 355); `sessions.db` is shared by every daemon, the flock is per folder — double
   resume possible. SUSPECTED. [delivery 9]
10. **The 30-minute busy wait stalls the worker and ignores interrupts** (`persist_reporting`
    `:12711-12745`, `sleep_unless_closed` `:12631-12641`); other sessions show only "queued";
    patience counts from the start, not the first busy. CONFIRMED. [delivery 10]
11. **A wake marks results delivered before the turn that delivers them can fail**
    (`:8631-8641, :8569`, `submit_item` `:8691`, `:10774-10776`). CONFIRMED. [delivery 11]
12. **The held-results counter goes stale after attach/reconnect** — `HeldResults` is ephemeral
    (`scrub.rs:102`), published only on change (`jobwatch.rs:768-784`), the head keeps the last
    value (`tui/src/app/events/turn.rs:44`). CONFIRMED. [delivery 12]
13. **Reseat side effects before success**: `reseat_tight` published before the fork
    (`:9805-9819`); `reseat_target` changes `cfg.system` (`:9655`) and writes a `stable_prefix` row
    (`:9683`) even when refused Over (`:9780`). CONFIRMED. [delivery 13]
14. **An interrupt absorbed at the end of a successful round is dropped**
    (`turn/src/engine.rs:1229-1250, :1614-1631`). CONFIRMED. [delivery 14]
15. **A child can answer its parent and then run a `[meanwhile]` turn nobody hears**
    (`jobwatch.rs:669`, `harness.rs:15316`). SUSPECTED. [delivery 15]
16. **The R79 sort runs per streamed delta** (copies the board, `todo.rs:746`). Cost only.
    CONFIRMED. [delivery 16]
17. **The `wanted` map leaks** (`delivery.rs:286`). SUSPECTED. [delivery 17]
18. **Closing a root session drops held and queued results** (`jobwatch.rs:868-886`,
    `sessions.rs:2595-2598`). CONFIRMED; may be intended. [delivery 18]

Checked sound: a result arriving exactly at turn end; prompt vs wake ordering; compaction is not a
turn end; `wait_on` on an ended job refused; restored subagents keep their id; a removed worktree
is abandoned with the reason; same-daemon reopen does not double-restore; two heads reseating.

---

## R92 — Lifecycle, jobs, the queue and worktrees as the operator meets them — **OPEN, filed 2026-10-12; left: items 11 (later commits never re-enqueued), 12 (the main checkout's `.cargo/config.toml` still patches rano), 15 (worktree debris nothing reaps) and 34 (only firecode's capacity wording recognised)**

Ranked by how often the operator hits it.

1. **Firecode jobs are never watched** — `FirecodeBackend` (`tools/src/firecode.rs:737-860`) has
   `processes()` but not `processes_arc()` (trait default `None`, `backend.rs:152`), so
   `harness.rs:3939-3942` builds no watcher and `jobwatch.rs:917-962` drops the handle: no
   `JobSettled`, no wake, the store row stays `running`, `wait_on` rows never told. **This is the
   firecode "job sat finished until I typed".** Also: a VM child's backend is closed after its
   first turn and its background jobs die unannounced (`~:18186-18203`). Fix: implement it and
   remove the trait default. CONFIRMED. [lifecycle A1]
2. **Slow start shows a live-looking session that cannot run** — the socket binds
   (`cli.rs:1066/1077`) before flowy (`:1104`), the model-server probes (`:1370, ~1458-1466`, 10 s
   connect / 3 min read), `Parts::load`, `open_first_with_seat` (`:1476`); the worker starts at
   `:1766`. The launcher only polls the socket (`scripts/letibot:1932`). **The rano "took ages"
   report.** Fix: a "starting: <step>" status on the hub; label queued prompts. CONFIRMED.
   [lifecycle A2]
3. **A boot failure's reason goes only to the shared `~/logs/harnessd.log`**
   (`cli.rs:1104-1108, ~1478`; `scripts/letibot:268`); the launcher's tail (`:1933`) can show
   another folder's lines. CONFIRMED. [lifecycle A3/C1]
4. **Child worktrees always look dirty, so none is retired** — the daemon writes
   `.letibot/delivery-log.jsonl`, `context-claims.jsonl`, `notes-offers.jsonl` into the workspace
   (`delivery.rs:91, :464`; `context_claim.rs:568`; `notes_offer.rs:331`), not ignored;
   `retire_branch` (`mergequeue/git.rs:470-488`) refuses dirty trees and says so only by
   `eprintln` (`serve.rs:1296`). 27 of 31 trees have `.letibot/`; 20 of 37 merged branches still
   have trees; `.claude/worktrees` is **240 GB**. The deliverable nudge then complains about the
   daemon's own files (`deliverable.rs:166`). CONFIRMED on disk. [lifecycle A4]
5. **The bottom-edge queue standings are stale, count every repository and only `red`**
   (`tui/src/ui/panes/queue.rs:117-139`, refreshed only with the pane open). CONFIRMED.
   [lifecycle A5]
6. **Nothing says "main is green again"** (`red_card.rs:32`; `target_is_green` `serve.rs:883`).
   CONFIRMED. [lifecycle A6]
7. **A daemon restart repeats every alert and wakes dormant sessions** — dedupe in memory only
   (`serve.rs:145` `rung`, `harness.rs:1976` `gate_told`); `regate_at_start` (`verbs.rs:540`);
   `ring_project` (`mergequeue.rs:366-391`) resumes old root sessions. CONFIRMED. [lifecycle A7]
8. **Reviews freeze while any gate runs in the repository** — `RepoQueue::pass`
   (`serve.rs:224-279`) runs gates (`:724`, `:1122`) synchronously, up to 30 min; no claims, no
   deadlines (`:399-445`), no capacity rechecks. CONFIRMED. [lifecycle A8]
9. **One worker thread runs root turns inline; Idle upkeep starves** (`daemon.rs` run,
   `sessions.rs:2036-2119`). Structure CONFIRMED, impact SUSPECTED. [lifecycle A9]
10. **A child's later turns never reach the parent** — the slot settles once
    (`~:18210`, `task.rs:370-380`); `serve_child_with` (`:15260-15470`). CONFIRMED. [lifecycle B10]
11. **The branch is enqueued after the child's first turn, whatever its state**
    (`:18203`, `:18224`); `review.rs:631-685` `unwrap_or_default` enqueues an empty head; later
    commits never re-enqueued. CONFIRMED (re-read of the head SUSPECTED). [lifecycle B11]
12. **The main checkout's `.cargo/config.toml` patches rano to `/home/dead/Projects/rano/rano`** —
    every worktree under `.claude/worktrees` inherits it (cargo searches parents) and builds local
    rano (`v0.8.3-1-gde2ae1f`); the landing tree is outside and builds the `v0.8.3` tag, so a
    subagent build and the gate can disagree. **Operator's file — not touched.** The 31 legacy
    per-tree `.cargo/config.toml` (target-dir) are harmless leftovers. CONFIRMED on disk.
    [lifecycle B12]
13. **Job ids restart at j1** — filed as R91.3. [lifecycle B13]
14. **A job slug in `needs` reads as "already ended"** — `todo_write` stores the raw slug
    (`todo.rs:3437-3449`), only `wait_on` resolves (`:3114`); `job_fate` → Unknown → ended
    (`harness.rs:13568`), while `delivery::names` matches the slug. CONFIRMED. [lifecycle B14]
15. **Worktree debris nothing cleans** — 48 registered, 62 dirs; 24 `agent-a<hex>` orphans whose
    gitdir is the old `/home/dead/Projects/letibot/.git/worktrees/…`; 3 locked registrations at
    missing paths (`agent-a8e2db02…`, `agent-ac359f48…`, `agent-adff1e33…`); 5 dirty trees
    (`agent-agents-md` mid-merge `UU standing_notes.rs`, `agent-operator-input` 5 uncommitted TUI
    files, `agent-store-busy`, `agent-why-a-queued-message-waits`, `agent-quiet-shell`). Nothing
    reaps failed, refused or killed children. CONFIRMED on disk. [lifecycle B15]
16. **A firecode `task_start` child is told to commit on the host branch but works on a VM copy**
    (`:16725-16780, :18120-18135`; `task.rs:52-65`) — empty branch enqueued. SUSPECTED.
    [lifecycle B16]
17. **Slug collisions and unvalidated slugs** — `slug_from_prompt` (`task.rs:299-328`) takes 4
    ASCII words; a stale branch blocks with no prune hint (`:16744-16752`); user slugs only trimmed
    (`task.rs:1098-1102`): `/`, `..`, spaces pass; `agent-agent-refresh` on disk. CONFIRMED.
    [lifecycle B17]
18. **"Red main halts both queues and returns items to review" is not what the code does** —
    `serve.rs:465, :803` keep reviewing; `land_next` (`:918-923`) leaves rows queued with their
    approval and lands them on green unreviewed. CONFIRMED. [lifecycle B18]
19. **`queue_settings_unreadable` floods every hub every ~2 s** (`serve.rs:231-243`). CONFIRMED.
    [lifecycle B19]
20. **Exit-and-stop against an older daemon SIGKILLs without waiting for its turn**
    (`driver.rs:746-764`, `head.rs:675`); rows of that turn can be lost. Path CONFIRMED, loss
    SUSPECTED. [lifecycle C20]
21. **`interrupt_turns` can never say "N turn(s) still running"** — calls `ask_sessions` with an
    expired deadline (`head.rs:579-605`); `--stop` hard-codes the reason "letibot --stop --force"
    (`:597`); `scripts/letibot:1554` swallows a failed `--interrupt-all` and then blames SIGTERM
    (`:1572`). CONFIRMED. [lifecycle C21]
22. **The quit card under-counts what a stop kills** (`ui/cards/quit.rs`): other sessions, the
    gatekeeper's reviews, a running gate. SUSPECTED. [lifecycle C22]
23. **`letibot --stop --force 3` ignores the 3** (`scripts/letibot:1514`); `--stop --all` never
    escalates and advises a flag that acts on this folder only. CONFIRMED. [lifecycle C23]
24. **Nothing checks the workspace still exists**; `retire_branch` force-removes a live child's
    tree; `Deliverable::uncommitted` reads a gone tree as clean (`deliverable.rs:59-75`); the log
    writers recreate `<gone>/.letibot/`. No-check CONFIRMED, effect SUSPECTED. [lifecycle C24]
25. **A gate killed at its cap retries on every lease take with no backoff**
    (`serve.rs:676-681`, `verbs.rs:540`); the merge side says "could not run" and names `regate`
    where the verb is `restart` (`gate.rs:77`, `serve.rs:1175-1186`). CONFIRMED. [lifecycle C25]
26. **A box-wide OOM kill of a rustc is judged RED** — `red_cause` (`gate.rs:207-228`) counts only
    a signal on the step leader. CONFIRMED. [lifecycle C26]
27. **A timed-out target and several blocked causes raise nothing** (`serve.rs:764-781`, `:1308`).
    CONFIRMED. [lifecycle C27]
28. **Merge-row failures tell nobody; merge rows landed by hand never settle**
    (`serve.rs:1050-1063, :1145-1153, :288`). CONFIRMED. [lifecycle C28]
29. **Red-tip reviews survive green** — only `is_open` rows withdrawn (`queues.rs:248`); a landing
    sets Green (`serve.rs:1281-1296`) without `target_is_green`, so a pending red review can raise
    EMERGENCY on a green main. CONFIRMED. [lifecycle C29]
30. **Alerts reach only the lease-holding daemon's sessions; some reach every repository**
    (`mergequeue.rs:355-430, :513`); repository = parent of `git-common-dir` (`git.rs:116-174`); a
    directory with one repo child counts as it (`:155-166`, SUSPECTED). CONFIRMED. [lifecycle C30]
31. **The same sha red twice is silent** (`queues.rs:807` INSERT OR IGNORE). CONFIRMED.
    [lifecycle C31]
32. **Reviewer start failures handled inconsistently** (`review.rs:261-280`, `serve.rs:402-411`,
    `mergequeue.rs:359-364`). CONFIRMED. [lifecycle C32]
33. **A push failure with no install step raises nothing; an install failure is labelled
    `queue_blocked`** (`serve.rs:1235-1265`). CONFIRMED. [lifecycle C33]
34. **VM-capacity waits: minor gaps** — failure shown only when attempts > 0
    (`queue.rs:378-384`), evidence overwritten on re-claim, only firecode's wording recognised
    (`capacity.rs:67-71`). CONFIRMED. [lifecycle C34]
35. **`wake: later` tells the model a held result "never starts a turn of its own"**
    (`bash.rs:~308-318`) — false under R79. Also R93 F12. CONFIRMED. [lifecycle C35]

---

## R93 — What the model is told: tics, leaked numbers, contradictions, schema mismatches — **OPEN, filed 2026-10-12; left: items 1 (A2, A5, A6), 3 (B4, B7, B8, B13), 4 (C5, C6, C9–C14), 5 (D3, D4), 6 (E2–E6, E9–E13, E16), 7 (the F findings the named ones do not cover) and 8 (a fabric memory body no tool reads, and the fabric block's time changing per open)**

From real data (`sessions.db`, fork copies removed: 4,347 injected items) and the code. The full
report has the exact text and a replacement for every item — work from it. **Every lettered item
in that report (A1–A7, B1–B13, C1–C14, D1–D4, E1–E16, F1–F58, G1–G10) is an entry of R93 in its
own right**, addressed as R93.<id>; the list below groups them by what to fix first:

1. **The `[context check]` quotes the model's own sentence back, ✓ marks and all** (`context_claim.rs:460-466, :517-523`). The "my context N%" tic began two minutes after the first numbered check in s-1789919514688401228 (108 of 113 live checks are there); the latest one still quotes "Hmm — **my *context* ✓** ✓ ✓" (14×). Stop quoting. Also A2 (`repeated` reaches the model via `harness` warnings), A3 stale doc comment, A4 (`context_fill` `harness.rs:12341` ignores `--no-auto-compact`), A5/A6 (reply/reasoning variants), A7 (`digest.rs:192-197` invites context sizing). [model A1–A7]
2. **The memory/box note is on 93% of bash results** (3,979/4,256 today; subagents ~95%) and peak/cap on every job — resource sizing the model should not size by. Say it only on a kill or near the cap. `bash.rs:140-168, :1077-1081`; `exec/memory.rs:236-243`; `exec/host.rs:221-235`; `jobs.rs:378, :713-717`; `jobwatch.rs:1269`; `mergequeue/gate.rs:252-257`. [model F1–F8, C4]
3. **The todo nag**: 45% name a row already in progress (793/1,778); 45% got no tool call back, only restated status (113× "Not done: three boxes open…"); "(N more open)" in the compared text resets the R77 backoff; criterion rows nagged every window (124× one row); `nothing_can_start` fires on job/young-child roots `todo_write` promised were silent (`todo.rs:2381-2416`, `:1813`); `attach_now` appended with no plan or no `todo_write` seated (`todo.rs:1355-1360`); 60–70-word advice on every nag. 393 identical nags at 66 s in one session were the pre-R77 metronome, fixed by ebc2af7. [model B1–B13]
4. **Completion texts contradict themselves and arrive after the model already read the result** (21% of `[task]`, ≤6% of `[job]`): "don't call `task_result` … read it with `task_result`", placeholder "…" for a known id, the command echoed in full, `[meanwhile]` heads explaining the holding policy (and leaking the 10-min cap), `[monitor]` announcing the model's own retire as a firing, plural bugs. `harness.rs:1064-1148, :1260, :990-1004, :8657-8660, :13552-13554, :14865`; `delivery.rs:356-368`; `deliverable.rs:97-113`. [model C1–C14]
5. **"You keep overthinking"** leaks `reasoning_only`, says "Reason after" to models that cannot, and D2 is factually wrong (`harness.rs:11367-11387`; `compaction.rs:243/253`); the length-cap and progress-watcher texts are long (`length.rs:139-141`, `progress.rs:246-268`). [model D1–D4]
6. **Compaction/reseat/restart/stale notes**: `continue_after_wall` names the "context wall" and can fire with nothing compacted (`harness.rs:9037-9040`, `turn_tail.rs:177`); **literal 14-space gaps** in the cut note (`harness.rs:10520`); transcript ids, R27 and KV-cache internals (`event.rs:1061-1080`); reseat notes stacked 2–3× and `[reseat]` asks for a one-line restatement and re-plants `/reseat` (`harness.rs:2252-2283, :10581-10584`); the stale notice is 120 words of operator text (`:2205-2209, :2483-2514`); `[restart]` contradictions, nested tags, `pkill` hint missing the required `pattern`, `wait_on` named as a tool (`restart.rs:266-303, :353-354, :439-440, :480-482`; `subagent_record.rs:393-411`); `[cleared]` leaks ids (`harness/clear.rs:171-178`). [model E1–E16]
7. **Tool descriptions and results contradict the code**: `wake_sentence` (F12) vs R79 and six other texts; `bash` says a foreground command is moved to the background at `timeout_ms` but it is killed (F14, subreviewer's claim, not re-read); `job_wait` says it blocks but returns at once (F16–F18); `task`'s schema lacks `model` (F21) and `task_start` lacks `wake` (F22); `monitor`'s description contradicts its schema (F28); `retire` renders "it had retired by the model" (F29); two list tools (`todo` vs `todo_write`) and the intent check names the unseated one (F30); merge-gate text tells the model to commit AGENTS.md to main (F31); **literal `{raw}`** in `notes.rs:374` (F42); **~18-space gap** in `gate.rs:218` (F10); duplicated clause in the predicate note, 436× (F27); the snapshot-budget note 5,611× (F38); `[repaired]` notes every call (F56); the intent-check threat on `todo`, 163×, for a check off by default (F58); and the rest F2–F59 in the report. [model F1–F58]
8. **The base prompt** (`config.rs:987-1024 DEFAULT_SYSTEM`) says **"You have read-only tools."** to seats that have `edit`/`write` (G2); forbids tools for world questions even with `web_search` seated (G3); names tools many seats lack (G4); children inherit the parent's own-session and fabric blocks (G5); the fabric block names a `flowy get` action and a `skill load` form that do not exist (G6, G7); "Reasoning goes in a row" (G8); the gatekeeper's "reason as long as you like" vs the overthinking nag (G9); dead role tool names (G10). And it has no rule on waiting, jobs, todos, context or asking the operator, so nothing settles the contradictions above (G1). [model G1–G10]
   - **G1–G10 are in main** (`Config::compose_for_seat`, `config.rs:2389`, called from `Harness::open_with_registry` when `Config::seat_prompt` is set; `SECTION_NAMES`, `config.rs:1126`; `default_section`, `:1192`) — the branch they were built on is gone. The base prompt is composed at open from `SeatFacts` — the seated tool names after role and downgrade, the backend's real scratch path, subagent, linked worktree. Sections (`config::SECTION_NAMES`: identity, language, tree, find_and_read, edit_files, scratch, notes, working, tone) say only what is true for the seat; a section with nothing true is left out, overridden or not. New `working` section: the R79 delivery sentence, `ask_user_question` (blocks) vs a `todo_write` `wait_on` operator question (the plan goes on), the harness manages context (no numbers), stop/continue (root) or "your last reply is your parent's answer" (child), the queue rule. Children compose for their own seat (no own-session paragraph, no fabric block). Fabric block renders `skill` / `flowy get` (CLI through `bash`) only when the seat has them; "Reasoning goes in a row" dropped; gatekeeper and notes-keeper "reason as long as you like" → "explain what you found; keep to the evidence"; standing-notes index line has no token number; dead `researcher()`/`reviewer()` roles removed. `prompts.toml` keys renamed: `read_only_tools` → `tree`, `scratch_path` folded into `scratch`, `working` added. The scratch pid is masked in `prefix_fingerprint` (`harness::instructions_of`) so a restart does not call every session stale. Left: no tool reads a fabric memory body (only the CLI); the fabric block's "Read from the node at <time>" still changes per open, so seated roots read as stale after a restart (pre-existing).

---

## R65 — The model chooses each tool call's memory cap, knowing what is free and what a cap costs — **LANDED on main 2026-10-11 (073c1ea, 51dd340); left: item 2 (what is free is not said where the model decides - `memory_arg` is rendered from the policy alone, because the schema is the session's stable prefix), item 5 (an ordinary call's peak is still withheld unless it was killed at or near its cap), item 6's test, the above-ceiling card answered by a person in a head, and the cross-session ledger**

The operator: *"the question is how many for 2. I think model should try to decide, so it should
know the free, how much it can ask and what are the consequences"*.

Verified on lab2x1 that the mechanism exists: the user manager delegates `cpu memory pids`, and
`systemd-run --user --scope -p MemoryMax=64M` around a 200 MB allocation ends that process alone
with `Killed`, rc 137.

1. **The model asks.** `bash` (and a backgrounded job) gains a memory argument — `memory: "8G"`.
   — ✓ fixed in d755bf51, test uncapped_is_an_ask_and_a_blank_is_the_default
2. **The model knows what is free** — said where it decides, in the tool's description or the
   turn's disclosure: the box's total, `MemAvailable` now, swap, and what this daemon's live
   calls and jobs already hold under their caps. — OPEN: what is free is still not said where
   it decides — the description is rendered from the policy alone
   (`crates/tools/src/builtins/bash.rs:51`) and `Meminfo::clause`
   (`crates/tools/src/exec/memory.rs:205`) has no caller.
3. **The model knows how much it can ask** — a ceiling per call, derived from what is free at
   call time. An ask over it is refused with the numbers, not silently clipped. — ✓ fixed in
   d755bf51, test
   the_default_is_granted_an_ask_up_to_the_ceiling_is_granted_and_anything_above_is_the_operators
4. **The model knows the consequences, in the description and in the result:** over the cap, the
   call's process tree is killed and only it (rc 137, the result names the cap and the peak).
   **With no cap, an exhausted box kills whatever the kernel picks, and systemd then stops the
   whole window** — R64's table. Omitting the argument gets a stated default, not no cap.
   — ✓ fixed in d755bf51, test
   a_kill_at_the_cap_says_what_to_ask_for_next_and_where_the_default_lives
5. **Every result reports its peak** (`memory.peak` of the call's scope), so the model learns
   what its commands cost instead of guessing again next time; a job's peak is in `job_list`.
   — OPEN: a peak is withheld from the result unless the job was killed at or came near its cap
   (`crates/tools/src/builtins/bash.rs:84`), so an ordinary call still cannot learn what it cost.
6. **A box with no user systemd says so** — the call runs uncapped, and the disclosure says it
   is uncapped. Never silently. — fixed in d755bf51 in code, untested: `MemoryCap::Uncapped`
   names the ask and the reason in the result (`crates/tools/src/builtins/bash.rs:84`), and
   nothing tests it

**The default is a config option, 64 MB out of the box** — the operator: *"default memory either
as absolute or a fraction is a config option with 64mb being a good default"*. The setting takes
either an absolute size (`64M`, `2G`) or a fraction (`0.05` — of `MemAvailable` at call time),
and the description states the value in force, resolved to bytes. 64 MB is deliberately small:
most calls fit in it, and a call that does not is killed at once with a result that says what to
ask for, which is cheaper than a runaway found by the OOM killer.

**The ceiling is a config option too, and it is not a hard limit** — the operator: *"ceiling is a
config too, with both absolute and fraction and the max is uncapped but if model wants it - must
ask me. unaskable ask is 50%."* So there are two bands:

- **Up to the ceiling** (default `0.5`, of `MemAvailable` at call time; absolute sizes allowed),
  the model's ask is granted without asking anyone.
- **Above it, up to uncapped**, the ask is legal but goes to the operator as a card naming the
  amount, what is free, and the command. Refused → the call does not run, and the result says so.

**The mechanism, MEASURED on lab2x1 2026-10-11:** a process started with `systemd-run --user
--scope -p Delegate=yes` moves itself into a leaf (`<scope>/daemon`), writes `+memory` to the
scope's `cgroup.subtree_control`, makes the tree (`<scope>/tree`, `+memory` again) and a job under
it with `memory.max=64M`, `memory.swap.max=0`; a child that joins the job and allocates 200 MB dies
with SIGKILL, `memory.events` `oom_kill 1`, `memory.peak` 64 MB, and nothing else is touched. The
daemon must not be IN the cgroup whose children get controllers (cgroup v2's no-internal-processes
rule) — which is why the tree today, rooted in the daemon's own cgroup, cannot carry a cap.
**OPEN — the operator, same day:** *"then again - depends on who does what - if nothing else going
then even 80% becomes possible. and if something else going can be changed by other sessions - so
both sessions can ask 50% independently which means some central daemon that checks it and rejects
the second ask? no idea yet"*. The defect named: a ceiling read from `MemAvailable` at call time
does not see a GRANT that has not been USED yet, so two sessions (or two daemons — one per folder)
can each be granted 50% within a second of each other. A proposal, not a decision: **the kernel is
already the ledger** — every grant is a `memory.max` on a job cgroup under some daemon's scope, so
what is promised box-wide is the sum over `letibot*.scope/tree/**/memory.max` (minus what those
jobs already use, which `MemAvailable` has counted), read under one `flock` in
`$XDG_RUNTIME_DIR/letibot/` for the check-and-grant. No central daemon; the second ask is judged
against what is left. Nothing in the per-call cap, the default, the peak report or the operator
card depends on this, so those can land first.

(Trap met while measuring: `systemd-run` expands `$VAR` and `$$` in its arguments, so a probe
written inline as `sh -c 'echo $$ > …'` writes `$` and fails with EINVAL/EIO. Use a script.)

**Done when:** R64's done-when holds, the model's next call after a kill asks for a different
cap on its own, and `job_list` shows each job's cap and peak.

**Landed (branch `memory-caps`):** `exec::memory` (the policy: `Amount`, `MemoryPolicy`,
`Meminfo`, `Ask`, `decide`); `exec::scope::delegate_memory` (the daemon steps into
`<scope>/daemon` and enables `memory`, refusing by name when it shares its cgroup) called first
thing in `harnessd`'s `main`; `ScopeTree::{memory_caps, cap_memory, memory_use}`;
`SpawnRequest::memory` → `memory.max` + `memory.swap.max=0` before any process joins, carried
across a promotion; the job's `memory.peak`/`oom_kill` read at the settle; `bash`'s `memory`
argument, the description, the note on every model call's result (cap, peak, box free, what may
be asked without asking), a dedicated *killed at its memory cap* outcome; `Gate::grant_memory`
with an `AlwaysAsk` card in `AdjudicatedGate`; `providers.toml` `[memory] default` / `ceiling`,
a malformed value refusing the start by name. The operator's own `!` run is uncapped.
MEASURED end to end through deepseek: no `memory` → `killed at its memory cap — peak 64 MiB of a
64 MiB cap`; `memory: "512M"` → ran, `peak 192 MiB of a 512 MiB cap`.
`tools/tests/memory_cap.rs` re-runs itself in a delegated scope and asserts both.

`job_list`, `job_wait` and a background job's completion notice carry a `memory:` line (cap,
peak, KILLED at the cap) — measured: `job_list` read `memory:  peak 97 MiB of a 128 MiB cap` for a
backgrounded 100 MB awk.

**Still open:** the card above the ceiling is built and unit-tested only through the type system — not yet
answered by a person in a head; the cross-session ledger above. — OPEN: neither is done: the
above-ceiling card is never answered by a person in a head (`crates/tools/src/adjudicate/tests/memory.rs:99`
is the type-system test), and no ledger sums `memory.max` under `letibot*.scope/tree` — `flock`
appears only in `crates/room/src/lease.rs:206`.

---

## R66 — A harnessd reached 113 GB — **OPEN: instrumented (memwatch, 4eb1dbb) and now MEASURED on a scratch daemon (2026-11-02) — 40 turns with a refused compaction each grow nothing; the two-threads-per-subagent leak is explained in R68.3 / R67.13 and closed on `r68-lifecycle`; only the LOCAL-model route is untried**

MEASURED in the kernel's OOM report, 2026-10-11 00:47:00: `Killed process 3985588 (harnessd)
anon-rss:113190136kB` — the daemon's own heap, not a child's. It was the `/home/dead/Projects/letibot`
daemon serving `s-1789462738453908838` (qwen-3.8-flash-next on :8082), the session whose
compaction failed repeatedly the same week and accumulated six copies of the compaction
instruction in its transcript. Its window's scope peaked at 131.6 GB over 1 d 23 h. The
allocating thread named in the report is `harness::queue_`.

Nothing is known yet about the cause. **Start with:** what that session's transcript and
`harness::queue_` hold; whether a daemon's RSS grows per turn on a long session (sample
`/proc/PID/status` VmRSS across turns on a scratch daemon); whether the failed-compaction retries
grow anything.

**Done when:** the growth is reproduced on a scratch daemon and either bounded or explained.

**MEASURED 2026-10-11 — what it is NOT, and one smaller leak found on the way:**

- **Not job output.** `HostProcesses` keeps every job it ever ran (`jobs.push`, never removed),
  each with up to 8 MiB of capture — but across the session tree's 15,405 `bash` results between
  10-09 and the kill, the retained capture sums to ~0.1 GB.
- **Not the transcripts.** 76 transcripts, a few MB each in the store.
- **`harness::queue_…` is not a daemon thread.** It is the task that ALLOCATED when the box ran
  out — a test binary's thread (a `harness::queue_*` test); the kernel then chose the largest
  process, harnessd, at 108 GB RSS (131 GB in the task table's total_vm column).
- **On deepseek (metered) a scratch daemon does not grow like that:** 245 MB at start; +75 MB
  for six sequential subagents; +12 MB for three turns each printing ~15 MB of tool output.
- **A smaller leak, real:** each finished subagent leaves **two threads** behind (7 → 19 for six,
  never released) and ~12 MB. At the 519 subagents that tree started, ~6 GB — not 108.
  **CLOSED on branch `r68-lifecycle` (R68.2/R68.3):** R67 item 13 ended the `letibot-deadline`
  thread with its host, and the host now dies: a finished child idle for `child_park_after`
  (10 min) parks as store-only — its `subagent-*` thread leaves, its hub is removed from the
  registry, its harness drops. MEASURED (`tests/session_lifecycle.rs`, 50 children): threads
  (subagent, deadline) 0,1 before → 50,51 with all answered → 0,1 after parking, one registry
  entry left; with the policy off (the old behaviour) 50,51 and 51 entries stay.

**Instrumented 2026-10-11 (`harnessd::memwatch`), so the next growth is SEEN:** a counting global
allocator (heap live / peak, and a backtrace for any single allocation ≥ `LETIBOT_LARGE_ALLOC`,
64 MiB by default); a watchdog line every `LETIBOT_MEMWATCH_SECS` (60) with rss, heap, threads,
sessions, hub logs, view items, heads and job rows; a census at each rss step from 1 GiB
(doubling) — hubs holding most, threads grouped by name — with a `memory_high` warning on every
session; `BOUND BROKEN` when a hub's log or a head's queue is past its bound, or a view list
with no bound passes 10,000; and `memory.high` on the daemon's own leaf (`[memory] daemon_high`,
16 GiB default, `max` = off) so the kernel throttles the daemon instead of taking the box.
First reading on a scratch daemon: **rss 279 MiB with heap live 35 MiB** — most of a daemon's
resident memory is NOT on the Rust heap (SQLite's page cache and the vocabulary map use their own
allocations), so a census showing rss far above heap live points there and not at a Rust
collection. And the large-alloc log found its first: `Catalogue::load` re-reads the 5 MiB model
catalogue from disk on every `Config::settings`, i.e. on every session open.

**Not yet tried, and the likeliest place left:** the LOCAL-model path. The killed daemon served
qwen-3.8-flash-next on :8082, which streams token ids through `/completion` and keeps the token
ledger itself; deepseek does neither. Reproducing it needs the local server running, which is
the operator's to start. — OPEN: the cause is still unreproduced; the instrument is
`crates/harnessd/src/memwatch.rs`, and the local-model path that was killed has not been run
under it.

**MEASURED 2026-11-02 — a scratch daemon with a fake model does not grow per turn, and the four
lines that could have carried it are each bounded:**

- **The daemon itself.** `crates/harnessd/tests/r66_daemon_rss.rs` starts a real `harnessd` on its
own store, socket and runtime dirs under an address-space cap, with a fake model whose summary
requests it refuses, and drives one session: **40 turns of 8 KiB prompts, a refused compaction
after each — rss 230.5 → 223.5 MiB, and the last watch line reads `grew s-r66 events +7 · view +1 ·
warnings +1`.** The same file takes `R66_TURNS`/`R66_PROMPT_BYTES`/`R66_IDLE_SECS` for the probe at
its own scale. A session with a refused compaction refuses its next prompt for room (`0 refused`
above is what a session with room looks like), so the driver waits on the turn's own events rather
than on the model.
- **The probe at its own scale, and idling.** 120 turns of 16 KiB prompts with a refused compaction
after each: **rss 235.6 → 257.3 MiB over 119 turns — 0.18 MiB a turn against 1.9 MiB of prompts
appended**, most of it the token region (4 bytes a token on a byte vocabulary) and the hub's own two
copies, so ~1.4× what the rows cost and inside the 2× the CI test allows. Then **60 s idle: 257.3 MiB
at all six readings** — nothing grows with the clock.
- **The worker and the queue gain nothing per turn.** `crates/harnessd/tests/r66_queue.rs`: 40 turns
through the real `Sessions` and its worker leave `opened()==1`, one registry entry and a watcher set
holding nothing, at **0.01 MiB of heap a turn, the same in both halves**. Every map is keyed by session
and removed at close; `Outcome` is a return value; activity is rebuilt per publish; metrics are a
bounded channel. **Two maps were unbounded per JOB and are now bounded**: `JobWatchers.settled` (one
name per backgrounded job, never removed) and `ReadRecord.seen` (one entry per delivered completion),
which also made `want_now`'s guard read a record with no history.
- **What is per CHILD, and still holds an answer after the child is gone:** `TaskSlots`
(`harness/children.rs:140`) is a `Vec` that only grows, and a slot keeps `TaskStatus::Done { answer }`
— the child's whole answer — after it has parked, been collected or died: measured **5 271 B for a
closed child whose answer was 2 bytes, 119 593 B for one that answered 16 KB, 351 767 B while it is
still open**. At the 519 children that tree ran that is tens of MB, not 113 GB, and it is the same
class as the threads R68.2 closed. `HostProcesses` also keeps a row per command ever run (1.1 KB a
row, per HARNESS and not per daemon, its capture already bounded at 64 MiB), which is what makes
`job_output j12` answer at all.
- **One failed fold** leaves exactly one row — the 1 470-byte `SUMMARY_INSTRUCTION` — and 1 511
ledger tokens, and no failure arm takes it back (`crates/turn/src/compaction.rs:210`). Measured over
40 attempts on a 2 MiB conversation: +0.19 MiB of heap in total, flat. It compounds the *prompt*
(+1 494 bytes per attempt), not the heap, and 113 GB of it would be ~74 million attempts.
- **A round's working set** is two whole-prompt copies inside the stream (22.4 bytes per prompt
token at 244 139 tokens) and none of it survives the round; a fork frees both the rows and the token
region (40 rows / 1.06 MiB → 1 row / 0.27 MiB). The region only grows by doubling and never shrinks
between forks, which is the capacity the footprint already states.
- **Two blind spots were real and are fixed.** `restate_footprint` did not run for a compaction's own
rows, so a session that could not compact under-reported itself by exactly the instructions it kept
(58 800 bytes after 40 attempts) — `harnessd: walk the census after a compaction's own rows`. And
`TranscriptItem::bytes` counted a picture's base64 body as 32 and a tool call's arguments as zero,
so the ceiling and the hub's byte bound were blind to the largest rows a session can hold —
`transcript: count a picture's body and a write call's arguments in a row's bytes`.
- **The watch now says what GREW**, not only what is held: every line after the first prints
`memwatch: grew <term> +<bytes> · …` (biggest first, per session), and three growth readings in a
row with nothing in the census accounting for them raise `MEMORY CREEP` in the log and a
`memory_creep` warning on every session. That is the line the 47-hour daemon's log never had.

**What is still unexplained:** the incident's own 113 GB. Nothing measured here produces it, and the
one route that was never exercised is the LOCAL model (`/completion`, token ids, no GGUF on this
box). The watch now names the term that grows, so the next daemon to do it says which.

---

## R67 — Stabilization: the contained fixes the 2026-10-11 review found — **2026-10-11; left: 12 (the queue's store-error retry, no test drives an error through the loop), 14 ("only items since the last sweep"), 17 and 18 (the seams and `apparatus::absent` routing, no test pins either), 23 (documentation only), 24 (the 400 ms deadline and the 10 ms polls, kept on purpose), 26 (the once-per-tip landing line, untested), 27 (`by_id`/`index`, behaviour-neutral, untested)**

The operator: *"goal is stabilization. many features recently, things arent stable or follwoing
the same path, like mouse clicks - sometimes they work, sometimes they dont, per pane key maps
etc etc."* Four read-only reviews of main at `435bb88`. These are the findings that are one
defect each and fixable without the structural work in R68–R70; every line names where, and
every fix lands with the test that would have caught it. CONFIRMED means the code path was
traced; each was re-checked where marked ✓.

**Head (crates/tui):**

1. ✓ **Typing with the queue pane open runs queue verbs.** `app/keys/panes.rs` (`key_queue…`,
   `Key::Char('r')` ~:736, `'a' | 'v' | 'd'` ~:770) has no empty-composer guard; the subagents
   pane's `o`/`p` do (`self.editor.text().is_empty()`). Typing "deploy" with `/queue` open sends
   `d` and `a` against the selected entry.
   **DONE** (branch `r67-tui`, `e25a7d5`): `r`/`a`/`v`/`d` guarded on an empty composer; `a_typed_word_reaches_the_composer_and_runs_no_queue_verb`. — ✓ fixed in
   74de81a5, test a_word_typed_over_any_surface_reaches_the_composer
2. **A click in the session picker selects the wrong session.** `panes.rs:~294-308` maps
   `y - (header+2)` to `picker_sel`, but the picker draws two lines per session (row + id line)
   and footer lines fall inside the bound. **The test `a_click_picks_the_row_under_the_pointer…`
   (`app/tests/session.rs:1762`) asserts the wrong row** — fix it with the code. The picker also
   truncates without scrolling.
   **DONE** (`8774e72`): clicks read `picker_hits`, built from rano's `stop_rows` (row + id line per session; footer picks nothing); the picker now scrolls its cursor into view; the test is rewritten to the right row, plus `the_picker_scrolls_its_cursor_into_view_and_clicks_follow`. — ✓ fixed in a92752ad, test
   the_picker_scrolls_its_cursor_into_view_and_clicks_follow
3. **Clicks on the mode/setting card are mis-mapped.** `ui/screen.rs:393-394` takes
   `mode_first_row` from the top of the chrome, above the lines pushed before the card (`:311`);
   `mode_rows_drawn` counts the MODE row's choices for every subject (`/verbosity`, `/diff`,
   `/models`); wrapped `why` text breaks one-line-per-value.
   **DONE** (`0b42927`): `mode_hits` from `setting_picker_layout` (rano's `stop_rows`, `why` lines included), placed where the frame put the card; `mode_first_row`/`mode_rows_drawn` removed. Tests: `a_click_on_the_mode_card_lands_under_a_link_line`, `a_click_on_the_verbosity_card_picks_the_value_drawn_there`. — ✓ fixed in
   9e82bdbb, test a_click_on_the_mode_card_lands_under_a_link_line
4. **The diff popup blocks the permission card.** `key_diff_popup` (`keys/mod.rs:132`) runs before
   `key_decision` (`:196`) and returns `Break` for every key; its own comment says cards stay
   answerable. The hint bar has no diff-popup branch.
   **DONE** (`ccde44b`): with a decision open the popup keeps only Esc/ctrl-c/ctrl-]/wheel/page keys; hint bar gains popup and popup-over-decision branches. `a_decision_under_the_diff_popup_takes_its_keys`. — ✓ fixed in
   af6faf2a, test a_decision_under_the_diff_popup_takes_its_keys
5. **Scrolling in the queue pane moves the hidden transcript.** The wheel/PgUp pane list in
   `app/keys/chords.rs:270` omits `queue_pane` and `queue_open`; `key_queue_review`'s PageUp arm
   (`panes.rs:182`) is dead code.
   **DONE** (`e0252df`): `queue_pane`/`queue_open` in the chords' list, the dead PageUp arm removed. `the_wheel_and_page_keys_scroll_the_queue_pane_and_its_overlay`. — ✓ fixed in f40be664, test
   the_wheel_and_page_keys_scroll_the_queue_pane
6. **`!term` gets the operator's raw mouse reports.** `pane_keys` (`keys/mod.rs:719`) forwards SGR
   mouse sequences verbatim, against `crates/vt/src/screen.rs:27`.
   **DONE** (`e312b44`): SGR and X10 reports stripped before forwarding (no program has mouse mode per `vt/src/screen.rs`). `the_pane_is_never_sent_the_operators_mouse`. A report split across two reads is not recognised (documented). — ✓ fixed in
   6bf6238b, test the_pane_is_never_sent_the_operators_mouse
7. **Any wheel or click cancels allow-all.** `key_mode_confirm` (`app/keys/cards.rs:25`) treats
   every key as "no".
   **DONE** (`262867f`): the wheel falls through, a click is swallowed, the card stays up. `the_wheel_and_a_click_leave_the_allow_all_card_standing`. — ✓ fixed in
   08bc5d55, test the_wheel_and_a_click_leave_the_allow_all_card_standing
8. **`/sessions` and ctrl-s open the picker differently.** `commands.rs:510` neither seeds
   `picker_sel` nor clears `pick`; the chord does both.
   **DONE** (`e274142`): both call `App::open_session_picker`. `slash_sessions_opens_the_picker_the_way_ctrl_s_does`. — ✓ fixed in
   44001592, test slash_sessions_opens_the_picker_the_way_ctrl_s_does
9. **Help and hints disagree with the bindings.** `ui/panes/help.rs:99` (a click on an edit row
   opens the diff popup, not rano); the hint bar's "ctrl-n notes" (ctrl-n retires every note,
   `chords.rs:162`); help omits ctrl-g, ctrl-q, ctrl-o, `/queue`, `/jobs`, `/todos`,
   `/subagents`; "wheel scrolls the transcript" is not what happens. **And the hint bar has no
   mode for `slash_out`, `sub_out`, the diff popup or the payload window**: with `sub_out` open
   over the subagents pane the bar still says "enter (or o) opens a subagent", while Enter
   re-peeks and `o` falls through to switching session.
   **DONE except one string** (`50ec059`): help fixed (popup, wheel, click, ctrl-g/q/o, `/queue` `/jobs` `/todos` `/subagents`); hint-bar branches for `slash_out`, `sub_out`, the payload window (diff popup in `ccde44b`). ~~**NOT DONE: "ctrl-n notes"**~~ **DONE**: the head generates the conversation's half (`ctrl-n clears notes`), and rano 0.8.2 corrected its own sentence. — ✓ fixed in 5be9113d, test
   the_help_names_every_pane_chord_and_what_a_click_and_the_wheel_do

**Daemon (crates/harnessd, crates/tools):**

10. **A nag-started turn strands its calls.** `Sessions::deliver_due_nags` (`sessions.rs:2379`)
    runs a turn and arms neither `arm_sweep` nor `arm_wake`, after `sweep_at` was cleared
    (`:2367`) — the wake arm's bug (fixed 2026-10-10) on the third door.
    **DONE** (branch `r67-daemon`, `e28cebc`): the nag door arms the sweep before its turn and the monitor wake after; `SWEEP_AFTER` moved beside the clock. `tests/nag_arms_the_sweep.rs`. — ✓ fixed in 10a99ed7, test
    a_nag_turn_arms_the_sweep_that_closes_a_call_it_stranded
11. **`!` stops working for a session after one failed settle.** `OperatorShellResult`
    (`sessions.rs ~3525`) returns `Failed` before `clear_bang_in_flight`; every later `!` line is
    queued for ever. Error to stderr only.
    **DONE** (`e6d3377`): the flag comes down before any return; a failed settle is `operator_shell_failed` on the hub and the next queued line still starts. `sessions::operator_run_failures`. — ✓ fixed in fac036fc, test
    a_settle_that_fails_still_ends_the_run_and_says_so_on_the_hub
12. ✓ **One store error ends the merge queue thread for good.** `mergequeue.rs:4116/4121` — the
    loop must retry with backoff and say so on the hubs (R61 req. 4 makes it structural; this is
    the stop-gap that keeps today's queue alive).
    **DONE** (`1651b85`): `run` never ends on a store error — 1s→60s backoff honouring `stop`, `merge_queue_store_error` once per reason, `merge_queue_recovered` after. `mergequeue::store_errors`. — fixed in ca7edbbc in code, untested: `work` retries a
    store error with backoff (`crates/harnessd/src/mergequeue/serve.rs:3728`), but the only test is
    `the_retry_wait_doubles_and_is_capped` and nothing drives a store error through the loop
13. **The deadline watchdog never exits.** `tools/src/exec/host.rs:629/660` (`Deadlines::run`, an
    endless `loop` holding its own `Arc`): one `letibot-deadline` thread per `HostProcesses`
    forever — one of R66's two threads per finished subagent.
    **DONE** (`d338e9c`): `Deadlines::close` from `HostProcesses`' `Drop`; the watchdog returns. `a_dropped_host_takes_its_deadline_thread_with_it` (a `Weak`, then the tid in `/proc/self/task`). — ✓ fixed in
    d2c916d1, test a_dropped_host_takes_its_deadline_thread_with_it
14. **The idle sweep is quadratic, on the worker.** `abandoned_calls` (`harness.rs:20292`):
    `answered.iter().any` per asked call over the whole transcript; with ~15k results, ~10⁸
    compares per sweep, every 2 s after a turn. A `HashSet`, and only items since the last sweep.
    **DONE** (`2ea0e38`): one pass into a `HashSet` of borrowed ids. **NOT DONE: "only items since the last sweep"** — a late answer is still an answer, so the whole transcript is the honest input; linear is the fix. No non-timing test fails on the quadratic version. — OPEN: the linear pass is in
    (`crates/harnessd/src/harness/turns.rs:3345`), but "only items since the last sweep" is not:
    `abandoned_calls` is still handed the whole transcript, and no non-timing test pins either
15. **Failures that reach no hub** — `let _ = self.append_results(…)` in
    `Harness::sweep_abandoned_calls` (`harness.rs:5228`, the hub block at 5222 is dead);
    `JobRecorder::put_job` (`jobwatch.rs:776, 813`); `flush_todos` (`harness.rs:7044, 7056, 7704,
    8122` — at 8122 an unpersisted fired condition fires again after restart);
    `StoreSessions::merge_entries`/`merge_reviews` (`sessions.rs:4247, 4266`) answer an empty queue
    on a failed read; a child's `ChildCommand::Leave` (`harness.rs ~13093`) drops `!`, `/slash`,
    `/mode` and todo edits sent to a subagent with a stderr line. Also stderr-only: `OperatorCall`
    failures, "not open" failures and `run_operator_shell_here`'s fallback lines
    (`sessions.rs ~3460, 3497, 3538`, reaching only `on_reply` → `eprintln!` at `cli.rs:1761`); in
    `daemon.rs`, the `ensure_gatekeeper` error and the sweep and nag counts (a nag's FAILURE is
    published, by `after_turn`).
    **DONE** (`e6d3377`, `2ea0e38`, `41f5e2d`): every site now also publishes a hub warning — `calls_abandoned`/`transcript_store` (sweep), `job_not_recorded` (recorder, via a `Weak<Hub>`), `todos_not_stored` + `todo_move_unresolved` (`flush_todos_or_say`), `merge_queue_unreadable` (`SessionSource::merge_*` now return `Result`; `Registry::merge_queue_for`), `child_door_refused` (Leave), `operator_call_failed`/`session_unavailable`/`operator_shell_on_worker`/`operator_shell_failed` (operator arms), `review_queue_not_open` (once per reason). Census 103. Sweep and nag COUNTS stay stderr-only (routine). Tests in idle_clock.rs, todos.rs, jobwatch, resume_frames, sessions. — ✓ fixed in de66c0f5 (with
    975c161c, fac036fc), test a_row_the_store_refuses_is_said_on_the_hub
16. **`Catalogue::load` re-reads the 5 MiB model catalogue on every `Config::settings`** — every
    session open (found by memwatch's large-allocation log).
    **DONE** (`660a79e`): `Catalogue::load` → `Arc<Catalogue>` through `cached_read` (path + mtime + length); snapshot parsed once. `a_file_is_parsed_once_until_it_changes`. — ✓ fixed in aaa9b69f, test
    a_file_is_parsed_once_until_it_changes

**Tests (flaky by construction):**

17. **Environment variables set with no lock**, read concurrently by other tests in the same
    binary: `tools/src/builtins/task.rs:2101,2131` (`FIRECODE_BIN`, read at `firecode.rs:73`),
    `turn/src/serving.rs:587`, `letibot/src/roles.rs:332-341`, `websearch/src/lib.rs:298`,
    `tokencore/src/apparatus.rs:189-223`. `confine.rs`/`host.rs` hold a `static LOCK` — do that.
    **DONE** (`f08b7ba`, branch `r67-tests`): not a lock — every reader is production code, so a
    lock would have to be taken by all of them. Each function grew a seam taking the reading as a
    value (`firecode_available_at` + `TaskTool::with_firecode_bin`, `skip_unreachable`,
    `askpass_in`, `resolve_key_from`, `present_given`/`gguf_path_from`); no test sets a variable.
    serving.rs's test also stopped panicking under a run's own `LETIBOT_REQUIRE_MODEL=1`. — fixed in f08b7ba1 in code, untested: the readers
    take the reading as a value, so no test can observe the variable it no longer sets
18. **Silent skips the require-switch cannot catch**: `tools/tests/confine.rs` (13 early
    `return;`), `notscoped.rs:55`, `src/exec/scope.rs` skip with `eprintln!` + `return` instead of
    `apparatus::present`, so `LETIBOT_REQUIRE_APPARATUS=1` cannot make a missing cgroup v2 fail.
    **DONE** (`0157743`): confine.rs, notscoped.rs, and the same skip in background.rs, exec.rs,
    deadline.rs, toolchain_grant.rs go through `apparatus::absent` (refusal still asserted first).
    MEASURED with `LETIBOT_BWRAP=/nonexistent/bwrap LETIBOT_REQUIRE_APPARATUS=1`: confine was 23
    passed on main, 5 failed after. scope.rs's tests have no skip (checked at `435bb88` too). — fixed in 01577431 in code, untested: the proof is the
    require-switch run in the line above (5 confine tests refuse), and no test fn pins the routing
    through `apparatus::absent`
19. **Wall-clock upper bounds**: `task.rs:1815` (`< 1s`), `answers.rs:2256` (`< 1s`),
    `answers.rs:1677`, `1723`, `1908` — trip on a loaded box or a parallel `cargo test`.
    **DONE** (`7761f7b`): task.rs asserts the cause (zero `collect` calls; and, same class at
    `a_watched_subagent_is_not_waited_on`, the longest wait asked of the child); answers.rs:2256's
    bound dropped (the `Answered` match is the claim); the three refuses-at-once tests keep a
    clock — the fact is about time — at half the 30 s budget, reason written beside each. — ✓ fixed in 7761f7b8, test
    a_watched_subagent_is_not_waited_on
20. **Pre-existing failure:** `tui` `app::tests::detected::a_python_edit_detected_after_a_command_draws_its_diff`
    fails on main (with and without llama).
    **DONE — a real head bug, environment-dependent** (`c9f3028`): the provenance note was the payload's second `[note]` line, hidden by the fold behind the box's `memory: ran UNCAPPED` note; a folded detected-diff panel now carries its own provenance line. — ✓ fixed in 6ba1ede1, test
    a_python_edit_detected_after_a_command_draws_its_diff

**Also found, and contained:**

21. **The flowy seat's slot can outlive a failed open.** `seat_tool` inserts into `slots` before
    the open can fail; `detach_seat` clears `seated` but not `slots`. Minor.
    **DONE** (`662f7eb`): a failed open removes its `slots`/`seated` entries. `sessions::seat_slots`. The `detach_seat` half is not a defect: it empties every slot, and the entries must stay for `attach_seat` to refill. — ✓ fixed in 43f124ff, test
    a_failed_open_takes_its_slot_with_it
22. **A shared global counter makes one allocation test single-test-only.**
    `tui/tests/frame_allocations.rs:43` counts every allocation in the binary through a global
    `ALLOCS`; safe while the binary holds one test, flaky the day a second is added.
    **DONE** (`8813962`): per-thread `const` thread-local counter. MEASURED: a second allocating
    test beside it failed it 3/3 before, passed 3/3 after. — ✓ fixed in 88139627, test
    a_frame_costs_no_allocation_per_unchanged_line
23. **A process-wide cache shared across tests**: `tools/src/builtins/pattern.rs:142` (`CACHE`).
    **DONE, documentation only** (`eb25ba8`): entries are pure functions of their key, so no test
    can observe another's through a match; identity breaks only on a 256-key cap clear, and the
    whole lib test binary peaks at 44 keys (measured). The reason is written on `CacheKey`. — fixed in eb25ba8c in code, untested: a documentation change, and the
    cache is a pure function of its key
24. **Slow polls in tests**: 70 `sleep` calls under test dirs, many 10–20 ms polls;
    `tools/tests/deadline.rs:179,235` arms a 400 ms deadline (a lower bound — safe, slow);
    `tui/tests/stop_daemon.rs` and `reconnect.rs` poll at 10 ms. Most `recv_timeout(200ms)` sit
    inside 10–30 s deadline loops, which is fine.
    **NOT DONE, on purpose**: deadline.rs's 400 ms is the quantity under test, and its poll is the
    premise ("a run nobody is waiting on") — `wait_job` would change what it tests. The 10 ms
    polls in stop_daemon.rs return the moment their condition holds, inside 10 s bounds; reconnect
    has no 10 ms poll (its 20–120 ms sleeps are retry pacing and quiet-detection). No condition to
    wait on without new hooks in the head's link, so not cheap — left. — OPEN: the 400 ms deadline and the 10 ms polls stay, on the reasons above;
    `tools/tests/deadline.rs` still arms its 400 ms lower bound
25. **No click test for queue rows** (`panes.rs:798`); `cards.rs:498` (`key_pick`) is reached only
    through settings. Existing click tests: the picker (`app/tests/session.rs:1762` — wrong, item 2),
    the count labels (`screen.rs:384-636`), todos (`todos.rs:293-332`), settings, the editor, the
    queue strip (`queue.rs:1049`). The mouse decoder itself is pinned (`keys/input.rs:264-277`).
    **DONE** (`e25a7d5`): `a_click_on_a_queue_row_selects_the_entry_drawn_there` (the hit test was already right). — ✓ fixed in 74de81a5, test
    a_click_on_a_queue_row_opens_the_row_drawn_there
26. **The queue's log repeats `landed <same sha>` 7 and 10 times in a row** — reset rows recognised
    as already on main, logged as landings each pass. Noise that reads like work.
    **DONE** (`1651b85`): a tip is logged as a landing once; the same tip again says *already on main, nothing new merged*. `the_same_tip_is_logged_as_a_landing_once`. — fixed in ca7edbbc in code, untested: the once-per-tip
    line went with the R61 queue rewrite (955a7fc2), which makes a tip on the target one
    conditional move (`crates/harnessd/src/mergequeue/serve.rs:2106`), and no test asserts it
27. **Linear lookups that grow with the daemon's age**: `Registry::get` scans every entry under
    the registry lock; `HostProcesses` finds a job by `iter()` over every job ever run
    (`host.rs:1740`).
    **DONE** (`d338e9c`): `HostProcesses.by_id` beside `jobs`; `Registry` `Inner.index` with `push`/`entry`/`entry_mut`/`has` (nothing removes entries today; R68's remove must update it). Behaviour-neutral, so no failing-before test. — fixed in d2c916d1 in code, untested: `HostProcesses.by_id` and
    `Registry`'s `index` are behaviour-neutral by design

**Still open?** Each item names its file and line; re-read it before starting — an item found
already fixed goes to `TODO-settled.md` with the evidence. **Done when:** each has a test that
fails on `435bb88` and passes after. The four reviews these come from are in full in section 4,
"The stabilization review of 2026-10-11, in full".

---

## R74 — What the 2026-10-11 landings left open — **AUDITED 2026-11: nine fixed, seven already done, six not a session's to settle, and 37 since settled — the workspace-wide clippy step is the merge gate's own; left: 4 (the queue verbs a person at a head cannot run), 11 (reasoning left out of the sent count), 12 (the ratio across a non-verbatim fork), 16 (a tool-call round under a context excuse), 20 (the one report sink), 23 (cards off the surface stack), 25 (a real head answering `grant_memory`), 33 (its three skip-green holes), 39 (a live gate on lubuntu1/2), 40 (the inline blocks over 800 lines)**

Everything merged on 2026-10-11 (main `cf47e65`: R61, R64, R65, R66 instrumentation, R67, R68, R69,
R71, R72 + follow-up, R73, the no-llama build), and the gaps each landing reported or a check
found. One place to read them; each line names the item that owns it, whose own "left" list has
the detail.

**Audited 2026-11: where each item stands.** The list below is the record as it was written; this
block is the answer, item by item, after a pass that re-read every one against the tree and fixed the
ones that were code. `FIXED` names the code; `DONE` is an item the tree had already closed without
the list being updated; `OPERATOR` and `DESIGN` are not a session's to settle; `CHORE` is work with
nothing to design.

- 1 **FIXED** — `mergequeue::review::deliver_refusal` writes the undelivered refusal down
  (`refusal:<head>:<row>` in the store's `queue_told`) and `redeliver_refusals` hands it to the first
  session that opens, once, for a day, while the verdict stands. `Served.redelivered`.
- 2 **OPERATOR** — `RepoQueue::health` gates `[queue] target` every pass, including a daemon start
  with nothing queued (~7 minutes). Skipping it until a landing needs it trades a stale *main is
  green* line for that cost; half the waste is already gone (`regate_at_start` re-gates only a red,
  blocked or timed-out tip).
- 3 **FIXED** — `Store::review_rows_in` and `ReviewState::QUEUED`; the three wake paths read the live
  rows instead of the table and filtering in Rust.
- 4 **PARTLY** — a model can re-gate (`queue` tool `action: "regate"` → `QueueDesk::regate` →
  `verbs::regate`), and a restarting daemon re-gates a bad tip. A PERSON at a head cannot: no
  `ClientFrame` carries a queue verb and `/queue` in the TUI only opens the pane, while the daemon's
  own warnings say `/queue regate`, `/queue restart`, `/queue approve` and `/queue rm` — and
  `verbs::approve` has no caller at all. Either the frames and verbs get built or the sentences stop
  naming them.
- 5 **DONE** — `sessionlog::gatekeeper_session(workspace)`: one queue session per workspace's
  daemon, not one per box.
- 6 **FIXED** — `mergequeue::snapshot` keeps each LIVE row's own last `RECENT_MOVES_PER_ROW` moves
  beside the queue-wide window, so a busy queue no longer pushes a row's history out.
- 7 **FIXED** — `tools::gatekeeper::{wake, Reviewer, NoReviewer}` deleted; nothing stood on them.
- 8 **NOT CODE** — a `perf` measurement on an idle worker.
- 9 **OPERATOR** — three rulings (`[queue]` in `leticode.toml` exists): withdraw vs delete for
  `/queue rm` and `clean`; `red_target`'s default; `git branch -D` after landing.
- 10 **DESIGN** — the mechanism is built (`tools::spill::Spiller`) and wired to every call, but
  `SpillPolicy::Unset` → `NoBudget`, so nothing is capped by default. A default budget derived from
  the window is a policy the operator has not made.
- 11 **OPEN** — `sent_since` (`harness/wall.rs`) still drops `Reasoning` and `SegmentMark` while the
  request builder echoes reasoning on a tool chain, so the count is short by a round's thought. The
  fix is to thread the same `echo_reasoning` fact into the count; the test that pins today's answer
  pins the assumption.
- 12 **OPEN** — a fork that is not verbatim clears the stored count/ledger pair on purpose, so a
  restart before any round falls back to the raw ledger count. The fix is to carry the RATIO across
  the fork.
- 13 **NOT CODE** — `e834316`'s six tests are all in the tree; what is left is the operator's live
  tmux replay of the real session.
- 14 **DESIGN** — 16 of 79 calibration excuses still score under the floor and there is no second
  tier. A second scorer is a slice of its own.
- 15 **NOT CODE** — the floor is a literal with a measured provenance; re-tuning needs the operator's
  `.letibot/context-claims.jsonl`, which no checkout has.
- 16 **OPEN** — no harness-level test of the round that calls a TOOL and carries a context excuse (the
  step-boundary queue). Every test drives a plain answer; the door exists and a canned tool-call
  reply would reach it.
- 17 **DONE**, with a residue — all three decisions are in (park at 10 min, reopen on a message or a
  head, a message earns the plan-check back, failures are `TurnFailed`). `wake_failed` and
  `todo_check_failed` survive in `sessionlog::warning`'s table with no emitter anywhere.
- 18 **OPERATOR** — roots are never closed automatically, and an idle root holding a monitor pins the
  daemon until it stops (`court_refusal`'s monitor arm). Not a defect in the close path; a policy.
- 19 **FIXED** — `RECORD_LIMIT` and `bound_records` in `exec/host.rs`: `reaps`, `promotions` and
  `Deadlines.fired` drop the oldest instead of growing for the life of the daemon.
- 20 **OPEN** — the single `report(session, code, detail)` sink is still not built: 130 inline
  `SessionEvent::Warning` sites outside test modules in `harnessd` against two private helpers
  (`Harness::warn`, `Sessions::warn`), and `warning.rs` is a table a head draws, not a sink. A
  refactor across `harnessd`, not a one-function change.
- 21 **DONE** — the gatekeeper retry exists and cannot storm: one attempt per 30 s pass, one warning
  per distinct reason.
- 22 **DONE** on `r60-term` — `letibot_vt::Screen::mouse` and the pane's own translation.
- 23 **PARTLY** — all six modal cards have key tables (`app/keys/cards.rs`); none is on the surface
  stack (`Drawn` has no card arm and `focused_surface()` still answers a surface under a card).
  TUI work.
- 24 **FIXED** (`a4384ed5`) — the flake was one fixture directory shared by two tests, one deleted by
  the other's `Drop`; one directory per call now. Residual: `settle` gives up after 10 s of ticks.
- 25 **OPEN (verification)** — the card's path is complete and a fake adjudicator answers it; no test
  drives a real head answering `grant_memory`, and on Darwin the card is unreachable (`Meminfo` is
  `/proc/meminfo`).
- 26 **DESIGN** — no box-wide ledger: two sessions can each be granted half of what is free at the
  moment of the ask. The single-daemon half (subtract the caps live jobs were promised) is three
  lines in `bash::memory_for`; the cross-daemon half needs the `flock` the proposal describes.
- 27 **NOT CODE** — a runner's `~/.actrc`; `act` appears nowhere in the tree and the fleet gate is
  R62's unfinished done-when.
- 28 **NOT CODE (deliberate)** — linear over the whole transcript on purpose, per its own doc. A
  cheaper index (`asked`/`answered`/`scanned` beside the session) is written out in the audit if it
  ever matters.
- 29 **NOT CODE** — the slow polls are documented as left on purpose and the reasons still hold.
- 30 **DONE** on `r60-term` — `TermPane::held` keeps a report the read ended inside.
- 31 **FIXED** — a real child's hub now refuses a `Compact` and the `child_door_refused` warning is
  read back off it (`tests/child_nag.rs`), which is the half the pure-function test could not show.
- 32 **NOT CODE** — settled in R67's own entry: a review mark names the head the reviewer judged and
  a landing mark the rebased commit, by design.
- 33 **NOT CODE for the tree's rule**, with three holes named — a key-less box's `children.rs` test and
  two `bridge` tests that `remove_var("LETIBOT_REQUIRE_APPARATUS")` still skip green under the gate's
  prefix.
- 34 **FIXED** — the two wall-clock assertions in `tests/stop_force.rs` are gone (a span that began
  before the fixture attached, compared against the app's own deadline); the ordering claims stay and
  `reconnect`'s clocks were already injected.
- 35 **FIXED** — a file-level mutex around the one process-wide variable in `tests/confine.rs`, held
  for the whole body, which is what makes its SAFETY note true.
- 36 **FIXED** — every live target is `#[ignore]`d (`*_live.rs` ×7, plus `sessionlog/tests/live_e2e.rs`,
  `turn/tests/live_qwen.rs`, `flowy/tests/live.rs`), so a workspace run cannot dial the box; a person
  runs one by name with `--ignored`. `sudo_ask_live`'s re-exec spawn says `--ignored` with it.
- 37 **CHORE** — run the workspace clippy step; the four exemptions live only in the gate line and CI
  is deliberately weaker, so there is no drift to fix.
- 38 **DONE** — R63: `sessions::Court`, one thread per session, armed by `Daemon::run`;
  `tests/thread_per_session.rs` is its done-when.
- 39 **PARTLY** — R62's seam is built and tested against a fake ssh (`GateRunner`/`HostGate`,
  `mergequeue/gate_host.rs`); its done-when — a landing gated on lubuntu1/2 with the row naming the
  host — is a live run, and the fleet's `~/.actrc` is item 27's.
- 40 **CHORE** — R70 is partly done (`harness.rs`, `adjudicate.rs`, `todo.rs` and `sessions/` moved
  their blocks out). Nine inline blocks are still over ~800 lines; `harnessd/src/plugins.rs` is the
  largest at 3 320 lines and the one to move first.
- 41 **DONE** — R60's three: scrollback (`vt::Screen::HISTORY_ROWS`/`view`), panes numbered on the
  wire, and a confined pane (`wrap_terminal`, no `--new-session`), each with a test.

**R61 — the queues**
1. A rejection whose root session is not open in any daemon is only raised (`refusal_undelivered`)
   — no model hears it later. Not a regression (the old door did the same).
   — OPEN: still only raised — `refusal_undelivered` is published and no model is told later (`crates/harnessd/src/mergequeue/review.rs:897`).
2. Every daemon start gates its repositories' target tips — ~7 minutes of gate even with nothing
   queued.
   — ✓ fixed in 362113e4, test a_judged_tip_is_gated_again_on_request_and_when_a_daemon_takes_the_queue
3. Each wake reads the whole `review_queue` table (`queue_notices`, `handover_notices` in
   `Harness::wake`) — should read only open rows.
   — OPEN: `store.review_rows(None)` still reads every row (`crates/harnessd/src/harness.rs:2924`, `:3074`), and the query has no state filter (`crates/tokencore/src/store/queues.rs:797`).
4. No `/queue regate`: a timed-out or blocked target holds all landings until its tip changes or a
   daemon restarts.
   — ✓ fixed in 362113e4, test a_judged_tip_is_gated_again_on_request_and_when_a_daemon_takes_the_queue
5. One `s-gatekeeper` review session per box.
   — ✓ fixed in 0eb8958a, test two_workspaces_get_two_review_sessions_and_one_workspace_keeps_its_own
6. The pane's per-row history covers only the last 60 moves overall.
   — OPEN: still 60 — `RECENT_MOVES` (`crates/harnessd/src/mergequeue.rs:154`), read at `:283`.
7. The tools-crate `Reviewer` trait and `wake` are unused and can go.
   — OPEN: both are still there — `crates/tools/src/gatekeeper.rs:261` (`wake`) and `:290` (`Reviewer`), with no implementor outside that crate's own tests.
8. The "worker ~0% when idle" done-when was not measured with `perf`.
   — OPEN: not code — no `perf` run of an idle worker is recorded anywhere in the tree.
9. Per-project config is `[queue]` in `leticode.toml` (`target`, `merge_via_session`,
   `red_target`) — three rulings deferred by the operator: withdraw vs delete for `/queue rm` and
   `clean`; `red_target` default `fix`; `git branch -D` after landing.
   — OPEN: the three rulings are still the operator's to make; the keys are read at `crates/harnessd/src/mergequeue/settings.rs`.

**R72 — compaction counting**
10. A single tool result larger than the whole window is still appended in full; the next round
    refuses to send it, but compaction then folds a history already over the window. Capping tool
    results is a larger design.
    — OPEN: `append_items_reporting` (`crates/turn/src/engine.rs:445`) still appends every item whole, and nothing caps a tool result's size.
11. Excluding reasoning rows from the provider-sized count is an assumption: a provider that counts
    the current tool chain's reasoning makes the count one round short until the next measurement.
    — OPEN: still an assumption — `sent_since` (`crates/harnessd/src/harness/wall.rs:1937`) sizes the prompt with the reasoning rows left out.
12. A restart after a compaction but before any round falls back to the unscaled ledger count
    (measured harmless at 22,496 against a 37,500 wall).
    — OPEN: `recovered_scale` (`crates/harnessd/src/harness/wall.rs:1735`) still answers the stored pair only when the stored ledger is not longer, so a restart after a fork comes back unscaled.
13. The follow-up (`e834316`) was not re-checked live in tmux; the real leticl session was never
    replayed against the fix.
    — OPEN: not code — the follow-up has never been replayed against a real leticl session.

**R71 — the context-excuse check**
14. 16 of 79 calibration excuses score under the floor (a clause deep in a long sentence, or no
    word saying whose context) — a second, embeddings tier.
    — OPEN: no embeddings tier — `crates/harnessd/src/context_claim.rs:48` still records the 16 of 79 it misses.
15. The floor (0.25) is in-sample; re-tune it from `.letibot/context-claims.jsonl`.
    — OPEN: `FLOOR` is still 0.25 (`crates/harnessd/src/context_claim.rs:64`), not re-tuned from `.letibot/context-claims.jsonl`.
16. No harness-level test of the tool-calling branch (the step-boundary queue).
    — OPEN: the harness tests all play a round with no tool call (`crates/harnessd/tests/context_claim.rs:3`), so the tool-calling branch is still untested at harness level.

**R68 — daemon lifecycle**
17. The agent's decisions to confirm: idle children PARK after 10 minutes (`child_park_after`) and
    reopen from the store on a message or a head; a message to a held session now earns the
    plan-check back; children's failures are `TurnFailed` instead of `wake_failed` /
    `todo_check_failed`.
    — OPEN: still the agent's to confirm — `child_park_after` is 600 s at `crates/harnessd/src/config.rs:1458`.
18. Root sessions are never closed automatically — a resumed root keeps its harness and its
    monitor-wake thread until the daemon stops.
    — OPEN: roots are still never closed — `park_idle_children` (`crates/harnessd/src/sessions.rs:2994`) parks children only.
19. `reaps`, `promotions` and `Deadlines.fired` still only grow (small records).
    — OPEN: still only grow — `Deadlines.fired` is a `Vec` pushed and never drained (`crates/tools/src/exec/host.rs:463`), and `promotions` likewise (`:1116`).
20. The single `report(session, code, detail)` sink is not built.
    — OPEN: no such sink — there is no `report(session, code, detail)` anywhere under `crates/harnessd/src`.
21. R68.7 (the gatekeeper retry storm) is probably moot after R61; not checked.
    — OPEN: still not checked.

**R69 — the head's input**
22. **DONE on `r60-term`**: `letibot_vt::Screen::mouse` records the program's mouse request and
    the pane forwards a report, translated to the pane, only when it asked — see R60.
    — ✓ fixed in 4ef25b13, test the_programs_mouse_request_is_recorded_not_dropped
23. Six modal cards (allow-all confirmation, new-todo, password, `!term close`, prompt, key ask)
    are not on the surface stack and have no key tables.
    — OPEN: the cards are still first in `App::key` (`crates/tui/src/app/keys/mod.rs:200`) and not on the surface stack; `key_key_ask` is `crates/tui/src/app/keys/cards.rs:262`.
24. `editor::the_editor_pane_wears_the_heads_frame_not_nanos` failed once at a fixture `unwrap`
    and passed on every rerun — a flake to find.
    — OPEN: not code — the flake was never reproduced.

**R65 — memory caps**
25. The operator card for an ask above the ceiling has never been answered by a person in a head.
    — OPEN: not code — the card is `crates/tools/src/adjudicate.rs:2401`, and no person has answered one in a head.
26. Two sessions can each be granted 50% — the box-wide ledger proposal is open.
    — OPEN: no box-wide ledger — the rule is per ask (`crates/tools/src/exec/memory.rs:245`), so two sessions can each be granted 50%.
27. Each fleet runner needs `~/.actrc` pinning the `ubuntu-latest` image, or `act` asks
    interactively.
    — OPEN: nothing in the tree pins it — `.github/workflows/ci.yml` is the only runner definition, and `actrc` appears nowhere but this entry.

**R67 — the contained fixes**
28. Item 14: the abandoned-call sweep is linear but still reads the whole transcript each pass (a
    late answer must count).
    — OPEN: `abandoned_calls` (`crates/harnessd/src/harness/turns.rs:3345`) still walks the whole item list every pass.
29. Item 24: slow test polls left on purpose.
    — OPEN: not code — the polls were left slow on purpose.
30. **DONE on `r60-term`**: the start of a report a read ends inside is held for the next read
    (`TermPane::held`) — see R60.
    — ✓ fixed in 26913ddd, test a_mouse_report_split_across_two_reads_is_put_back_together
31. Item 15: the subagent "command dropped" warning is tested only as a pure function.
    — OPEN: still only absence assertions — the warning is published at `crates/harnessd/src/harness/children.rs:960`, and the tests check it is absent (`crates/harnessd/tests/child_nag.rs:602`, `crates/harnessd/tests/clear_context.rs:474`).
32. Item 21: half of it was not a defect. The status marks cite pre-rebase commit hashes.
    — OPEN: not code — the marks still cite pre-rebase hashes.

**Tests and the gate**
33. Under `LETIBOT_REQUIRE_APPARATUS=1` a box without a working sandbox (bwrap, user namespaces)
    now FAILS the gate where it silently passed.
    — ✓ fixed in 18f57e00, test present_skips_when_absent_and_fails_when_the_run_demands_the_apparatus
34. TUI timing tests flake under heavy parallel load (two failures once while five agents
    compiled; clean on reruns) — likely `reconnect`/`stop_daemon`.
    — OPEN: not code — no cause found.
35. `tools/tests/confine.rs` sets `LETIBOT_TEST_FAKE_TOKEN` with no lock (nothing else reads it).
    — OPEN: still unlocked — `crates/tools/tests/confine.rs:889`.
36. `cargo test -p letibot-harnessd` / `-p letibot-sessionlog` also run the `*_live` files, which
    AGENTS.md says not to run broadly (they skipped — nothing listened on 8080).
    — OPEN: the `*_live` files are still in the package's test directory (`crates/harnessd/tests/compact_live.rs:1`), so a `-p` run still reaches them.
37. The workspace-wide `cargo clippy --all-targets` gate step has not been run on the result.
    — ✓ settled: it is the merge gate's own step now — `sh scripts/clippy-gate.sh --all-targets --
    -D warnings …` (`agents.md:98`), which touches every member's root first so a warm target cannot
    answer for the lints (`3feb2606`).

**Open work, in order**
38. R63 — a thread per root session (unblocked by R68).
    — ✓ fixed in f68c90b0, test a_held_turn_in_one_session_does_not_delay_another
39. R62 — the gate on lubuntu1/2; R61's `GateRunner` is the seam.
    — ✓ fixed in 5d3b2f05, test the_tree_and_the_repository_arrive_and_the_steps_run_there
40. R70 — move the largest inline test blocks into sibling files (unblocked now the big edits have
    landed).
    — OPEN: only part done — `crates/harnessd/src/plugins.rs:2226` still carries a 2,469-line inline test block (`TODO-settled.md:1500`).
41. R60 — the three things `!term` did not do: **built on `r60-term`**, see R60.
    — ✓ fixed in 26913ddd, test a_second_pane_runs_beside_the_first_and_each_is_named_by_its_number

---

## R84a — Worktrees and landing trees beside the checkout (`wt/`, `landing/`) — **PARKED 2026-10-12, built on branch `parked/layout-wt-landing` (e7f4e55), not on main**

The operator proposed the placement: *"so id suggest to correct the placement - ~/Projects/rano/rano
<- the main checkout and then ~/Projects/rano/wt/* ~/Projects/rano/landing/*"*, and for a repository
not inside its own project directory chose a per-project option with a fallback. It was built, then
parked the same night: *"on placement - lets park it in todo. keep worktree, fix that .git thing with
something small. i dont have time to reason about proposed behaviour changes now"*. Main keeps R84
(landing in the data dir, out of `.git`) and subagent worktrees stay in `<workspace>/.claude/worktrees`.

What `e7f4e55` does, for when it is picked up (rebase it onto main; it was cut on top of `d27ed56`):

- **The rule** (`crates/harnessd/src/layout.rs`), first match wins: `$LETIBOT_LAYOUT_ROOT`;
  `[layout] root = "<dir>"` in the project's `leticode.toml` (absolute or `~/`; relative, `..` and
  unknown keys refused by name); the X/X layout (`~/Projects/rano/rano` → root `~/Projects/rano`,
  so `wt/` and `landing/` in it); otherwise siblings named after the checkout
  (`~/Projects/firecode-wt`, `~/Projects/firecode-landing`).
- **Landing** at `<landing>/<target>` (branch name sanitised to one folder), replacing R84's data-dir
  path; migration of `.git/letibot-landing` as in R84.
- **Subagent worktrees** at `<wt>/agent-<slug>` instead of `<workspace>/.claude/worktrees/`; existing
  ones not moved. Because `wt/` can be outside the workspace, a session's write access to its
  children's trees is kept three ways: the sandbox binds `wt/` read-write, the file tools reach it
  through `HostBackend::with_extra_root`, and the permission gate treats it as the workspace
  (`Surroundings::worktrees`).

**The behaviour changes the operator has not ruled on** — the reason it is parked:

1. With the X/X layout and a session rooted at the project directory (`~/Projects/rano`), `landing/`
   is inside the workspace: the sandbox sees it read-only but `write`/`edit` can still write there
   (their check knows only the workspace root). Closing it needs a per-session read-only path in the
   backend.
   — OPEN: still parked — `e7f4e55` is not an object in this checkout, there is no `crates/harnessd/src/layout.rs` and no `[layout]` key, and the landing tree is still `<data dir>/landing` (`crates/harnessd/src/mergequeue/git.rs:435`).
2. Child daemons, whose workspace becomes `wt/agent-*` outside the checkout, no longer take the
   repository's merge-queue lease; only the project's own daemon serves its queue.
   — OPEN: still parked — `e7f4e55` is not an object in this checkout, there is no `crates/harnessd/src/layout.rs` and no `[layout]` key, and the landing tree is still `<data dir>/landing` (`crates/harnessd/src/mergequeue/git.rs:435`).
3. Every place that assumed `.claude/worktrees` moves with it (grants, cleanup, texts).
   — OPEN: still parked — `e7f4e55` is not an object in this checkout, there is no `crates/harnessd/src/layout.rs` and no `[layout]` key, and the landing tree is still `<data dir>/landing` (`crates/harnessd/src/mergequeue/git.rs:435`).

---

## R55 — a round's end and a turn's end are the same event on the wire — **reported from leticl through the relay 2026-10-03; left: leticl's `:turn-finished` arm reading `ends_turn` (and its own test on that head), and `Hub::turn_in_flight` still reading the round rather than the field**

The operator, watching leticl's composer edge during a multi-round turn: *"sometimes 'responding'
timers flickers to Responded and then back, without time reset. not harmful but annoying"*, and then
*"so it looks like the harnessd must be explicit here and add another real end marker"*.

The timer does not reset because you measured the turn once: `harnessd/src/harness.rs:4339`
`begin_turn_clock` — *"Called once per prompt, never per round"* — sets `turn_began_ms` and the
engine's copy, and every round's `TurnStarted` carries that same stamp. Good, and it is the half that
works. The status word toggles because the **ending** is not marked at all: `engine.rs:1318-1320`

```rust
let finish_reason = match done.finish {
    Finish::Stop | Finish::ToolCalls | Finish::Other(_) => FinishReason::Eos,
```

so a round that ended to call tools publishes a `TurnFinished { finish_reason: Eos }` that is
byte-identical to a real ending — which your own comment at `harness.rs:5800` already says out loud:
*"Per round, the same way the heads' own `TurnFinished` updates theirs."* A head therefore believes
the turn is over in the window between a round's finish and its calls being proposed (nothing is
generating and no call is unfinished yet), writes its `Responded in …` report, and the proposal flips
it back. On leticl's screen: `Responding → Responded → Responding` on every round boundary, with the
duration untouched — measured on the operator's own head, `Responded in 4m09s at 21:37` drawn while
the turn was still running.

**The ask: mark the real end.** The minimal shape, and the operator's own framing — *another real
end marker*: a `final: bool` on the existing `TurnFinished` (or a `round: u32` beside it if naming the
round is wanted). The metrics stay per round, where the heads read them; only the ending gains a name.
An absent field should mean **final**, i.e. today's behaviour, so an old head against a new daemon
keeps working and a new head against a recent daemon flickers but never lies.

**Both heads then answer it with one predicate each**: while a turn is in flight the row says
`Responding`, including that window; the report is written only on `final`, and cleared at the next
`TurnStarted` as it is today. leticl's half is one line in its `:turn-finished` arm — it is ready to
land the day the field exists, and its suite builds the event by hand, so it is testable before this
ships.

**What letibot found when it read the three sites — and why the shape is an EVENT, not a field.**
All three citations hold verbatim: `begin_turn_clock`'s *"Called once per prompt, never per round"*,
the `Finish::Stop | Finish::ToolCalls | Finish::Other(_) => Eos` fold, and — the one leticl could not
read — `harness.rs:5800`, which is **support rather than a contradiction**: it says *"Per round, the
same way the heads' own `TurnFinished` updates theirs"* in the middle of explaining why the SESSION
row's prompt size is per round, i.e. it records this as deliberate.

**The engine cannot set `final`, and that is the whole of it.** `Finish::ToolCalls` establishes only
that a continuation MAY follow; whether one does is decided one layer up, in this daemon's round loop
— `room_for_next_turn` and then `run_continuation` (`harnessd/src/sessions.rs`) — and the convergence
point that already knows a prompt is over is the one where `end_turn_clock()` is called
(`sessions.rs:1090`, whose own comment is *"Every turn ends here"*). A `final: false` written at the
engine's fold would therefore be a **guess**, and on the paths where the guess is wrong — the room
check says no, an interrupt lands while the calls are still running, `Finish::ToolCalls` on a turn
that then fails — the head is left saying *Responding* with nothing running at all. That trades the
flicker for a **stuck row**, and a stuck row is the worse defect: R13 exists because a reader who
cannot tell working from wedged is the confusion this row is for.

So the marker is published where the decision is made, and that means **a new variant — `TurnEnded`.**
Re-publishing the round's `TurnFinished` at the convergence point is the alternative and it
double-counts `usage` in any head that accumulates the turn's cost. A field would also have to carry
the right default (`absent` = final, i.e. not `#[serde(default)]`'s `false` for a positive name), which
is the smaller of the two problems.

**CORRECTED 2026-10-03, an hour after the paragraph above: the variant does NOT need the protocol
bump, because this tree already has the mechanism.** [`PROTOCOL_VERSION`]'s version-27 note is that a
new VARIANT forces a bump *because a head has no arm for it* — *"`serde` has no catch-all on this
enum — deliberately, so a head cannot silently skip a fact it does not understand"* — and `Caps`
carries, in its own words, *"**free-form feature names, for forward compatibility**"*
(`protocol.rs`: `features: Vec<String>`, currently unused and evidently waiting for its first
customer). The hub **already gates delivery per head on those caps** — `hub.rs:931` filters by
`can_decide` when routing a decision and `hub.rs:1425` bounds a head by `caps.queue` — so:

  · the head declares a feature name in its `Caps` on attach (this head would declare it, and leticl's
    half is one string where its attach already is);
  · the daemon sends `TurnEnded` **only to a head that declared it**, so no head ever receives a frame
    it cannot read and the reason the bump existed is dissolved rather than dodged;
  · and `Hello` **echoes the features the daemon honours** (a new field with `#[serde(default)]`, for
    which the rule is genuinely *no bump*), because otherwise a head cannot tell *this daemon ignores
    my feature* from *no turn has ended yet* — and guessing that right is the difference between the
    flicker and a row stuck on for ever.

So the bump is not needed after all, and the operator's deliberate pin of leticl to `v0.2.0` (protocol
27) is left intact: an old head gets today's behaviour, a new head gets the marker, and neither ever
sees a frame it has no arm for. If the operator would rather have the coarse signpost — one number
that says the two halves are skewed — then bump and the argument above is the fallback rather than the
mistake; say which in the commit, since the two routes cost leticl different things (a declared
feature, or a re-pin).

**MEASURED 2026-10-03, before any code: the gate the paragraph above rests on does not exist.** The
whole no-bump argument turns on *“the daemon sends `TurnEnded` **only to a head that declared it**”*,
and this tree has no way to send a frame to one head. `crates/sessionlog/src/hub.rs`'s entire public
surface offers exactly two routes to a head: `publish` (an `Envelope` in the log, read by every head in
`seq` order through `next_batch`) and `submit` (a `ServerFrame` **returned to the caller**, which writes
it back to the one head that asked). `deciding_heads()` is the nearest thing to a per-head gate and is
not one — it is a *query* the adjudicator makes before posting — and `Caps.features` today is
**advisory**: `FEATURE_QUESTION_ANSWERS`’ own doc says a head that does not advertise it *“can still be
sent a question”* and will simply not answer. Nothing in the tree filters a frame by a declared
feature.

So the three routes, with what each actually costs:

 · **A per-head directed frame.** New machinery in the connection layer: either the daemon holds every
   head’s writer, or an outbound queue per head that the accept loop drains. It dissolves the bump
   exactly as the paragraph above intends, and it touches the most delicate layer in the daemon — the
   one where a head that is not written to does not hear that its turn ended.
 · **A `SessionEvent::TurnEnded` and a bump to 28.** The simplest thing that works: one variant, every
   head reads it in `seq` order, a late head replays it correctly, and an old head is refused at the
   handshake rather than handed a frame it has no arm for. The cost is leticl’s — a re-pin, and the
   operator deliberately pinned leticl to 27 — which is a *sentence on the other head*, not a code
   change here.
 · **Zero-bump by reuse, and rejected twice for two different reasons.** Re-publishing the round’s
   `TurnFinished` at the convergence point double-charges every head that accumulates `usage`, and an
   old head over-billing is worse than a flicker. And a `Warning { code: "turn_ended" }` is an arm
   every head already has — so it works, and **a head that does not know the code DRAWS it**: one dim
   row per turn boundary, in the conversation the `model_slow_first_byte` ruling has just finished
   moving weather *out* of.

**Which route is the operator’s to pick**, which the paragraph above already says in the same words
(*“say which in the commit, since the two routes cost leticl different things (a declared feature, or a
re-pin)”*). **The fail-first half needs no ruling and is unaffected by the choice**: this head’s row and
the test that currently pins the flicker.

**And the convergence point is now verified rather than quoted** — both routes depend on it, and one of
its edges fails in exactly the way both are trying to prevent. `end_turn_clock` has **one** caller:
`sessions.rs:1090`, inside `after_turn`. `run_prompt` ends by calling `after_turn` unconditionally
(line 974 passes the `out` **Result**, so a failure is not a bypass), `submit` is the only way in, and
the wall-continuation loop (`1057`–`1085`, `WALL_CONTINUES`) runs **before** the clock stops — so a
marker published there lands after every continuation rather than before it. Every ending therefore
reaches it: an `eos` finish, a `length` cut, a failure, an interrupt, and each wall continuation.

**The one caveat, and it is where the publish must not go.** `end_turn_clock()` sits inside
`if let Some(h) = self.open.get_mut(session_id)` — so a session that left `open` between the round loop
and this tail skips it. That *should* mean *no heads left to tell*; the marker's publisher belongs
**outside** that guard anyway, because a head still attached to a session the daemon has closed would
otherwise sit on a stuck row, which is the failure mode this whole row exists to avoid.

**What THIS head does today, so the operator knows what to look for here.** letibot has no past tense
at all — `Responding` is the only word the row has (`app.rs:13718`), so it cannot flicker to
*Responded* — and it **drops the row** for the same window instead: the last call of a round finishing
and the next round's first delta, because `turn_busy()` is `generating || a call unfinished`
(`app.rs:8185`) and every call of the finished round is `Finished` by then. **And this tree currently
asserts that as correct**: `the_status_row_survives_a_tool_call` pins *"the call lands and nothing
else is outstanding, so the work is over and the row stands down"*. The fix therefore REWRITES that
assertion rather than adding beside it, and that is the fail-first evidence — available before any
daemon change, since it is one predicate and one test. The backstop that makes this *annoying rather
than harmful* on this head is `a_turn_that_has_gone_quiet_says_so_rather_than_spinning`: forty silent
seconds and the row says something again.

**still open?** `grep -n "final" crates/sessionlog/src/event.rs` around the `TurnFinished` variant
shows no such field, and `crates/turn/src/engine.rs:1320` still folds `Finish::ToolCalls` into `Eos`.
And the gate the no-bump route needs is *absent rather than unfinished*: `grep -n "pub fn "
crates/sessionlog/src/hub.rs` lists no directed send, so that is the piece to build if route one is
chosen.

**done when** the daemon publishes the end at the convergence point (where `end_turn_clock` is
called) by whichever route the operator picks above — a filtered `TurnEnded`, or a bumped version
whose `Hello` echo says what is honoured — so that **no head is ever handed a frame it has no arm for**;
and a two-round stream on each head shows the status row up across every round boundary and down only at
the marked end — asserted, not watched: a test per head, and on this head that test replaces the one
that currently pins the flicker.

**UPDATE 2026-10-26 — the fact this row is about is on the wire, and this head's half of the row
is done.** `PROTOCOL_VERSION` 52 publishes `SessionEvent::ActivityChanged` — quiet, or busy with
`turn`, `calls`, `delivery`, the jobs and children whose settlement will wake the model, the
monitors it armed, and the cards nobody has answered — computed by the daemon from its own state
and published only when it changes (`crates/sessionlog/src/activity.rs` is the argument). And
**`turn` is the PROMPT's span, not a round's**: `begin_turn_clock`/`end_turn_clock` are *"called
once per prompt, never per round"*, and the daemon publishes that span, so the window this row was
written about — a round's last call finishing before the next round's `TurnStarted` — is filled.
This head's row now asks the activity before the round-level state
(`App::turn_busy`, `crates/tui/src/app/turn.rs`) and therefore stays up across every round boundary
and stands down when the prompt's loop returns; asserted in
`a_prompt_in_flight_holds_the_row_up_between_its_rounds`. So the marker at the convergence point is
no longer needed *by this head*, and what is left of the row is the other half: leticl's row, and
whether an explicit `TurnEnded` marker is still wanted at all now that the prompt's span is on the
wire for any head to fold (the two routes above are unchanged if it is).

**LANDED as a third route: a field on the frame that already ends a round.** Neither a filtered
`TurnEnded` (a new variant is a frame an older head cannot parse — it dies at the first turn end, and
this daemon answers a skewed `Attach` by serving it rather than refusing it) nor a bump (the same
death). `SessionEvent::TurnFinished` gains `ends_turn`, `#[serde(default = "an_ending")]`, so **absent
means an ending**: an old daemon's frame reads as today, an old head ignores the field and keeps its
flicker, and no peer is handed a shape it has no arm for. The producer is the engine's own round fact —
`TurnEvent::TurnFinished.proposed_calls` is `!done.tool_calls.is_empty()`, and `lift` translates it to
`ends_turn: !proposed_calls` — so no second frame is published and no head is billed twice. Frames:
`crates/harnessd/tests/turn_end_marker.rs` (round one `false`, round two `true`, exactly two frames).
Heads: this one reads it in `turn_busy` and the bridge gates `Turn::Idle` on it. **Residual, and it is
the guess this row warned about**: a round that proposed NO calls but is followed by a nudge or a
steering round still says `true`, so the flicker survives there — only a marker at the convergence
point could remove it. `Hub::turn_in_flight` is unchanged and still reads the round, not the field.

## Which side owns WEATHER — a classification both heads must answer the same way — **SETTLED 2026-11-02 on this side: the daemon classifies, and what was broken was the head's FILING; `model_endpoint_retry` is the ruling still open**

The operator ruled: *"both heads should not emit it inside conversation"* for `model_slow_first_byte` —
the diagnostic is wanted, its PLACEMENT was wrong, so it leaves the transcript, lights the `⚠` and
`/notes` still lists the sentence. leticl landed its half in `973d58f`, and the shape it took is worth
copying or overruling deliberately rather than by accident:

  · the test is **not** *is this routine* but **is this an event in the record**. `+weather-warnings+` is
    `model_slow_first_byte` and `model_endpoint_retry`; `auto_compact` and `compacted` are routine too
    and STAY ROWS, because a compaction changes the conversation; `prefix_check_skipped` stays a row
    because a skipped check is *said, never counted as a pass*;
  · it is a **subset of the routine register** (`warning.rs`'s `Class::Routine`), asserted by a test
    (`821d218`) so a code cannot be silenced here alone.

**The question: where does WEATHER live?** leticl's list is head-local and yours is the daemon's
register, so the two heads now hold two answers to one question — and a code added to one and not the
other is two heads disagreeing about whether a sentence is an event, which is the two-lists defect the
archive's file set was made un-driftable for in `018fc45`.

Two shapes, and the second needs a reason written down: **the daemon classifies weather beside
`Class::Routine`** (one answer, both heads read it, neither can drift — leticl's preference, because it
is what the register already does), or the lists stay head-local with the reason they must exist
separately stated in both trees.

**done when** a code added on one side cannot silently change a head's placement without the other
side seeing it — one list, or two with a stated reason and a guard that reads the other.

**SETTLED, 2026-11-02: the daemon classifies, and the head holds no list of its own.** The first shape,
and leticl's preference, is what this tree already had and what it keeps. `warning::ALARM_ONLY` sits
beside `Class::Routine` in `crates/sessionlog/src/warning.rs`, the head asks it through
`warning::to_the_alarm`, and `crates/tui` has no placement list to drift — so a code added here cannot
be placed one way by the head that saw it live and another way by the head that attached later. The
guard is the register's own: `an_alarm_only_code_is_never_a_failure_or_a_refusal` fails if a code on
the edge is also a `Failure` or a `Refused`, and it names the routine codes that must stay rows
(`compacted`, `auto_compact`, `compact_half`, `reseated`, `cleared`) — the same subset argument
`821d218` makes on leticl's side, read off the table rather than restated.

**And the defect this entry was chasing was not in the list — it was in the FILING.** MEASURED on the
tree: `crates/tui/src/app/events/warnings.rs`'s alarm arm returned `Disposition::Filtered` *before*
filing anything, while the snapshot arm (`app/events/mod.rs`, the `Placed::Before` push) files every
warning it replays. So one `model_slow_first_byte` had two records, decided by WHEN THE HEAD ATTACHED:
a head that came later listed the sentence under `/notes`, and a live head kept only the `/status`
count — the diagnostic the ruling is about, gone on the head that watched it happen.

Landed: the alarm arm files the daemon's sentence through a new door, `App::note_beside`
(`crates/tui/src/app/notes.rs`), and then counts it. `note_beside` files `Placed::Before` — listed by
`/notes`, counted by `/status`, drawn nowhere — through `App::note`'s own identity test, and the note
cap is one function now (`cap_notes`, over a new `NOTES_KEPT`) because filing has two doors and a cap
written into one of them is a list that grows through the other. `/status`'s third number reads
`N not drawn` rather than `N from before this window`, because a live edge-bound note is not from
before the window. `ALARM_ONLY`'s docstring says where the sentence goes, so the next code added there
is told. Test: `an_edge_bound_warning_is_listed_in_notes_rather_than_deleted`, beside the older
`an_edge_bound_warning_raises_the_alarm_and_never_becomes_a_row`, which still pins the half that
matters most — it is not a row in the conversation.

**STILL OPEN, and it is the operator's ruling rather than a change to make: `model_endpoint_retry`.**
leticl's `+weather-warnings+` carries it and this side's `ALARM_ONLY` does not, so a retry ladder is a
row here and weather there — the one code where the two heads still answer *is this an event in the
record* differently. Nothing above changes it: it stays a routine row until somebody rules.

## R59 — the context wall's denominator moves a third between firings — **raised by the operator 2026-10-03; left: the operator's ruling on smoothing the ratio, which has not been given and which the tree now keeps per-round as recommended (the id, `bd012ed`, and the provider-unit printing, `e8343165`, are landed)**

The operator, on letibot's compaction: *"something is off here, like too much too early too wtf."* Four
`context wall` lines on one daemon whose `context_window` is 999,999 — **and no way to say which session
each was about**, because the wall line carried no id, which is why that is the first thing fixed below.

    stopped after 13 round(s) at 1400086 of 1118167 tokens
    stopped after  7 round(s) at 1463534 of 1484966 tokens
    stopped after  2 round(s) at 1257759 of 1199811 tokens
    stopped after 16 round(s) at 1429974 of 1484829 tokens

**The denominator is `planning_window()`, and it is a measurement rather than a bug.** `Config::ledger_scale`
is `(ledger.len(), the last round's prompt_tokens)`, refreshed at the end of **every** round
(`harness.rs:5848`), and `planning_window` is `context_window * ledger / provider` clamped to
`[w/4, 4w]`. The four windows above are 1.12x–1.48x the configured 1M, i.e. the ledger ran 12% to 48%
wider than the provider's own count of the same prompt — **how much of that session was reasoning the
messages provider is never sent**, which the docstring already says varies per session and over the life
of one. So the ratio is doing its job; what moves is the printed pair.

**And the threshold the wall FIRES on does not move at all — the swing is in the display only.**
Verified from the code rather than argued: the check is `resident + headroom() >=
planning_window()` where `resident = session.ledger.len()` at the top of the round loop and `ledger_scale`
was taken at the bottom of the previous one, so `resident == ledger`; `headroom()` is `w/16` at these
sizes; substituting gives `ledger >= 15/16 * w * ledger/provider`, the `ledger` cancels, and the
condition is exactly **`provider_prompt_tokens >= 15/16 * context_window`** — one fixed fraction of the
configured window, independent of the ratio. The wall fires in the provider's units; only the *ledger*
pair printed beside it swings. That is the same unit confusion the operator already caught once
(*"1.35 is a lie - that top was shown as 900+"*, `Config::shown_tokens`' docstring), now in a log line
instead of a notice.

**Which makes the two asks different sizes.**

  · **The measurement is theirs to rule on: smooth or not.** A smoothed ratio (an EMA) would hold the
    printed denominator still and cost lag — the window would track a genuinely changing reasoning
    fraction a turn or two late — and it would not move the fail point, because the fail point is
    already ratio-independent. This tree's own argument for the ratio is *"it is not a constant: it is
    how much of the conversation is reasoning"*, and smoothing is the one change that makes it less of
    the measurement it was written to be. Recommendation: **leave it per-round**; if the printed number
    is the problem, fix the printing.
    — OPEN: the operator's ruling, not code. The tree took the recommendation — the ratio is refreshed per
    round (`crates/harnessd/src/harness/turns.rs:1729`) and no smoothing exists anywhere — so the printed
    denominator still moves between firings and the choice is unmade rather than made.
  · **The display is not: print the provider's figure.** `{resident} of {window}` is in ledger tokens
    and every notice a head draws is in the provider's (`shown_tokens`). Printing the provider-side
    number (or both, as the wall *notice* already does with its `(counted as the provider counts them;
    …the ledger says X of Y)` aside) makes the log line read in the same unit as the screen it
    describes.
    — ✓ fixed in `e8343165`, test `the_wall_failure_says_the_providers_count_and_names_its_unit`

**The id that makes it decidable is landed** — `bd012ed` gives the `context wall` line the session name
its `compacting:` neighbour has had since this same morning, so the next occurrence pairs a wall with the
compaction beneath it **by id rather than by adjacency**. That is what answers *"too much too early"*:
whether the 1.25M-to-633k folds and the 985k-to-8k compactions in the operator's log are one session
behaving differently or two behaving consistently — and note the two are different paths with different
contracts (a fold and a compaction), which the operator's own caution says not to conflate.
— fixed in `bd012ed` in code, untested: the id is an `eprintln!`
(`crates/harnessd/src/harness/turns.rs:1396`) and no test reads that line.

**still open?** `grep -n "context wall:" crates/harnessd/src/harness.rs` finds the id;
`grep -n "ledger_scale =" crates/harnessd/src/harness.rs` finds the one per-round refresh at 5849 and the
recovery at 2833; no smoothing exists anywhere, so both routes above are unstarted.

**done when** the wall's log line and the compaction's can be paired by id (done, `bd012ed`), and either
the ratio is left per-round with the printing moved to the provider's unit, or a smoothed ratio is added
with the argument for the lag written down — so that the denominator on the screen and in the log is the
number the operator can check, and the analysis *"too much too early"* rests on attributed lines rather
than adjacency.
— the id half is done (`bd012ed`, untested) and the printing half is done (`e8343165`, tested); the ratio
half is the operator's ruling and has not been given, so this entry is not settled — and the `still open?`
paragraph above is stale on its second route, which is now landed.

### The same mismatch from the DISPLAY side — **added 2026-10-04**

The operator, on a compaction that fired while the header read half a million: *"it showed me
compaction when screen showed only 500k tokens."*

**The number on the screen and the number that decides are different units.** `Config::shown_tokens`
is `provider_tokens(ledger_tokens)` — what a request would actually carry — while `should_compact`
and the `compacting:` line are in LEDGER tokens. MEASURED on that session:

    log:     compacting: 1390742 of 1000000 tokens resident   -> compacted: 834514   (the fold)
    log:     compacting:  947896 of 1000000 tokens resident   -> compacted:  12594
    header:  947896 of 1000000                                 (agreeing, once the ratio was near 1)

The two agree now and did not then: a ~2.8x gap, inside the clamp of four. **The gap IS the reasoning
the messages transport never sends** — the ledger holds it, the provider's count does not.

**And `config.rs` already names the direction of the error**, in `tokens_are_converted`'s neighbour:
*"a ledger figure standing in for a provider's — up to the clamp's factor of four out, **in the
direction that compacts early**."* So *"too much too early"* is not a misreading by the operator; it
is the documented failure mode seen from the only place they can see it.

**This is the same root as the entry above, from the other end.** There the denominator moved between
firings in the log; here the numerator is printed to the operator in one unit and decided in another,
with nothing on the glass saying which is which. A reader cannot predict a compaction from the
display, which is the whole of the complaint.

**done when** one unit reaches the operator — either the decision is stated in the unit the header
shows, or the header names both and says which one compaction uses. `tokens_are_converted()` exists
precisely so a message *"can name the other number once rather than per figure"*; nothing is calling
it on the compaction path.
— ✓ fixed in `e8343165`, test `the_wall_failure_says_the_providers_count_and_names_its_unit` — the wall's
sentence and the banner now print the provider's count and its unit, with the ledger pair in parentheses, and
`tokens_are_converted()` is what chooses that aside (`crates/harnessd/src/harness/turns.rs:1363`).

## A wake that has not run yet does not survive its transcript being replaced — **NEW 2026-10-04, split out of the stall entry; left: the loss itself — a settlement whose turn has not run is still not re-armed at a fork, since `deliver_settlements_left_undelivered` is reached from `Harness::open` alone**

`j19` and `j56` are the two measured cases: both settled while `s-1791017230755743833#t2` was current,
neither notice is in `#t2`, and neither is in `#t3` either. The daemon's wake submits the notice as a
user item and runs a turn; if the turn has not run when the transcript forks, the queued item goes with
the transcript it was addressed to — and nothing re-arms it against the new one.

**What READING establishes, so the experiment starts where it should** (all cited, no live head needed):

  · **`Harness::wake` is the only road a settlement takes to the model, and it DRAINS before it
    submits** — `jobwatch::JobWatchers::take_completions` (`:303`) is a `g.drain(..).collect()` from
    the shared pen, and `harness.rs:4553` is the `submit_item` that turns it into a row. Nothing
    re-arms the pen, so *drained and not delivered* is a loss with no second chance;
  · **the pen is per TREE and shared** (`Parts::tree_watch`, R58) — which is exactly why a child's
    notice keeps arriving while a job's does not: they are two settlements in ONE pen, and the
    asymmetry the stall entry spent four hours on cannot come from two queues, because there is one;
  · **a compaction does not rebuild the harness at this level**: `Sessions::compact_if_at_the_wall`
    (`sessions.rs:1296`) reads `self.open.get(session_id)` and returns — so a pen held by a live
    `Harness` survives a compaction.

**Which leaves the fork as the suspect and not the compaction**: if the session's harness is
RE-OPENED across a fork under a fresh `Parts`, the pen goes with the old one and an undrained
completion is gone — and that is a question about `Harness::open`'s callers, not about the wake.
**The experiment decides between them**: start a job on an idle session and fork that session before
the notice's turn runs, then look in the NEW transcript. Absent means the pen is per-open and has to
be re-armed at the fork; present means the loss is inside the wake itself and the drain above is
where to look.

**The shape to build:** the queue is the session's and the transcript is the conversation's, so a fork
has to either flush the queue first or carry it across, and the daemon already knows the moment (it is
the same `auto_compact`/`compacted` pair the head marks its echoes with — see
`a_compaction_resolves_the_echoes_it_supersedes`). **The falsifiable form:** start a job on an idle
session, fork it (a `/compact`) before the notice's turn runs, and look for the notice in the NEW
transcript. Present means this entry is wrong too, and the answer is somewhere in the wake's own
eligibility rule — which is where the previous four readings were looking.

**still open?** the query above, grouped by `transcript_id` — a reading that does not do that cannot
see a fork, and this entry is the fifth measurement to prove it.

**done when** a settlement whose turn has not run yet is either delivered after the fork or reported as
lost, in both cases with the transcript it belongs to named.

— OPEN: a fork is not an open, so the suspect named above is falsified and the loss is unchanged. A compaction runs on the held harness (`crates/harnessd/src/sessions.rs:3226` → `harness.compact()`), so the pen survives it; the only door that re-queues a settlement nobody was told about is `deliver_settlements_left_undelivered`, called from `Harness::open` alone (`crates/harnessd/src/harness/turns.rs:461`), and a settlement taken for a turn that opened is marked read and skipped there (`:415`). `TODO-settled.md:3459` names the same residue as still a defect.

## R19 — stream backwards: the head holds a window, not a buffer — **given 2026-09-20; left: R19.2's third layer, the ring that lets the head drop a row it is not looking at — not built, so `hist_lines` only grows — and nothing constructs `Action::FetchRow`**

> so in a way we are looking back right, instead of looking in the future, interesting
> challenge for treesitter

> render chrome asap, then render the very current frame of text, then some scroll up
> buffer if needed — mind the bounds of blocks, etc

The implementation of R18.4's design, which is the operator's and was written down there
only after a first pass around it. Three stages, in order, and the first is head-side
only with no protocol change.

### R19.1 — wire `tail_cut` into the walk — ✓ fixed in 4146bf56, test a_tail_frame_is_far_cheaper_than_a_full_walk

`crates/tui/src/markdown.rs::tail_cut(src, min_bytes)` is committed and tested
(`815200e`, `ad0a10a`) and has **no caller**. `lex(tail)[1..]` is the document's own
blocks — exact below a possibly-ragged first block that goes off the top — and a 4 KB
tail of 152 KB measures **3.0%** of the whole's cost.

`App::body_window` still lexes every row in full from row 0, then slices the bottom
`room` rows out of `segs`. The change: render the **tail** of the conversation, from the
end, until the window is covered — the current frame first. `retarget_before(k)` already
scans backward for the per-row call context, and the other per-row inputs (`edit`,
`decision`, `elapsed_ms`, `answered`) are maps keyed by item id, so nothing about them
depends on direction. The one fiddly part is `hist_marks`, which is dense and indexed by
absolute row; a tail walk wants a floor and an "N lines above" count instead.

**Measured, and it rules out one wrong idea**: the forward walk is *not* the cost. A cold
frame over 6000 rows is 41 ms and every frame after is 0.06 ms. So this is not
"the walk is slow" — it is that a 160 MB session's *lex* is, and a tail avoids it.

### R19.2 — a row is a logical string; the head holds a viewport — **DONE 2026-09-20 (the paging half)** — ✓ fixed in 076d6f1c, test a_long_payload_can_be_paged_to_its_end

**Done**: a payload past its first screenful is now reachable. `ctrl-t` opens the fold
*and* a view on the newest payload row; `↑`/`↓` page inside it; the seam says which key
does what (`… +N lines · ↓ pages down · esc closes`, and `… end of output` at the end);
`esc` closes the view and gives the arrows back to the transcript.

**The bug was that `ctrl-t` revealed nothing.** It raised the *budget* — how many rows a
card may draw — and there was **no offset**. So a 418 KB payload drew its head, said
`… +N lines · ctrl-t`, and the chord showed you none of them. The rest was unreachable.

Three things this needed, each found by a test rather than reasoned out:

- **The view is keyed on the item id, not the call id.** The first version keyed it on
  the call id, which `item_lines` does not hold — so the view was silently closed and the
  seam went on saying `ctrl-t pages` while `ctrl-t` had been pressed.
- **A page move must invalidate the history buffer.** `hist_lines` is a cache of rendered
  rows and a page offset changes what one of them renders to, so `redraw` alone
  re-drew the old lines: the page said 10 and the screen said line 0.
- **`Up` at the top is a no-op, not a bug.** A test asserting "the arrows page" with `Up`
  was wrong, not the code.

**A contract that changed, and it is a real one**: while the view is open the arrows page
it rather than scrolling the transcript. `scroll_still_works_after_ctrl_t` — written for
the mouse-reporting bug — now asserts the *new* contract explicitly, including that `esc`
gives the arrows back. A view that held them for ever would be the same defect with a new
cause.

**Still open** — two layers, and they are one design: *the head must not hold what it is not
looking at.*

**Measured 2026-09-20** (400 messages, 80×24, scrolled to the top): 23 KB of source becomes
**67 KB of rendered lines** — wrapping and escape codes make rendering about **3×** the source
— and `hist_lines` holds every line from the floor down, **for ever**. It never shrinks. So the
coarse bound is `3 × ViewBounds::item_bytes` ≈ 24 MB today, and the moment anything can fetch
rows on demand it becomes unbounded: scrolling through a 160 MB conversation would render all
of it and keep all of it.

**First, a correction about what "dropped" means, because an earlier version of this item got it
wrong in a way that changes the design.** There are not one but **two rings in the daemon's
memory**, with two different bounds, plus the disk:

| where | holds | bound | who reads it |
|---|---|---|---|
| **view** (`ViewBounds`) | materialised rows, bodies included | 2,000 rows / 8 MiB | the `Hello` snapshot |
| **log** (`LogBounds`) | *every event*, including `TranscriptContent` — **which carries the body** | 20,000 events / 16 MiB | **`Peek`**, and a resume gap |
| store | every row, forever | disk | resume, `--replay` |

So a row trimmed from the **view** may still be whole in the **log**, and `Peek` already reads
the log (`hub.retained()`) — the mechanism exists and is wired. `FetchRow`'s `None` for a trimmed
row is therefore **not necessarily final**; the parts below say what to try before reaching for
the disk. A row costs several events (`TranscriptAppended`, `TranscriptContent`, and for a call
`ToolStarted`/`ToolProgress`/`ToolFinished`), so the log's window is a few thousand rows rather
than 20,000 — larger than the view's 2,000, not unlimited.

**(a) A ring over rendered lines — the head holds a window, not a history.** `hist_lines` is a
`Vec<String>` indexed by *absolute line* (`take_window` slices `[start, end)` across the
concatenation of segs, with `hist_lines` as seg 0). Two things make this more than a `VecDeque`
swap, and both were found by trying to write it down:

- **It only makes sense in tail mode.** In an ordinary session `hist_floor == 0` and the window
  *is* the history; a session small enough to walk fully is small enough to hold. So the ring
  is a tail-mode mechanism, which keeps it away from `hist_marks` — the forward walk's rewind
  index, which is empty exactly when tail mode is on.
- **Eviction needs a per-row line count that tail mode does not keep.** Dropping whole rows from
  the front means knowing how many *lines* they became, and `hist_marks` is the only row→line map
  — empty in tail mode. So the ring needs one small parallel structure of its own: the line
  count per rendered row of the window. Then `hist_line_base` (lines evicted above) and its
  counterpart (lines evicted below, which must still count towards `total`, or the scroll
  position jumps when a row is re-rendered).

With those, eviction is at both ends around the viewport, refilled by `fill_backward` above and a
matching `fill_forward` below — both the same shape as the existing one, since rendering a row is
`item_lines` and nothing else. **This half pays for itself with no protocol traffic at all**: the
head can re-render any row it holds, so a ring costs a re-render and saves the memory. — OPEN: the ring was never built — `hist_lines` only grows, and a re-render truncates it from a mark (`crates/tui/src/app/scroll.rs:24-62`), while a scroll-up in tail mode prepends to it and evicts nothing (`crates/tui/src/app/scroll.rs:455-469`).

**(b) `FetchRow`, and the daemon is a *proxy*.** The operator's framing, and it is the right
one: *"it is a cache problem - the daemon is optimized for normal usecase - heads show the latest
+ some scroll back. if a head wants something in the way past - daemon is simply a proxy from the
store to the head."*

So the two in-memory rings are not tiers a head navigates — they are **a cache tuned for the
normal case**, and a request outside them is an ordinary cache miss that the daemon resolves from
the store. The head asks **once**, by ordinal, and never learns which of the three answered. That
is why the frame takes an ordinal and not a tier, and why `body: None` means *the row does not
exist* rather than *ask somewhere else*.

This also replaces the "three steps" an earlier draft of this item had, which had the head
reasoning about log ring versus store. It does not: resolution belongs behind the frame.

**What is still missing:**

1. **The head does not ask yet** — no flag for "this ordinal is missing", no tracking of an
   answer, and no partial-body render. Blocking, and independent of where the answer comes from. — OPEN: nothing constructs `Action::FetchRow` (`crates/tui/src/driver.rs:657` is its only reader) and `ServerFrame::RowFetched` is answered as `Disposition::Control` (`crates/tui/src/app/events/frames.rs:290-293`).
2. **The daemon does not proxy yet.** `row_body_at` answers from the view and returns `None` on a
   miss (`crates/sessionlog/src/view.rs`), so today a trimmed row answers `None` even though the
   log ring — and the store — still have it. Resolution wants to be one method: view, then log
   ring, then the store through
   [`SessionSource`](crates/sessionlog/src/registry.rs), the trait that already exists for
   exactly this reason (*"A **trait and not a `Store`**… the daemon passes an implementation in"*),
   implemented as `StoreSessions` in `crates/harnessd/src/sessions.rs`. — ✓ fixed in a17be5cb, test an_ordinal_names_window_and_trimmed_rows_alike

**What the store path has to answer**, recorded because it would otherwise be found by a row
landing in the wrong place: **a session is not one table of rows.** Resume forks make the rows a
*chain* of transcripts — `crates/harnessd/src/transcript_source.rs:129` walks `transcript` ordered
by `created_at` rather than querying `transcript_item` directly — so the ordinal the head counts
must be the sequence that chain produces.

**And the head's window and the daemon's ring are allowed to disagree** — the operator's call:
*"regarding different heads that can display disjoint sets - it is their problem essentially"*.
The daemon serves rows **by session ordinal**, and that ordinal is a property of the session, not
of any head's buffer. Whether a given head can render what it asked for is the head's business: a
head holding its own rendered window, a head showing forty rows, a head that never scrolls — the
daemon satisfies none of them individually and all of them by answering "row N". So there is no
reconciliation to design between `ViewBounds`, `LogBounds` and the head's ring; they are three
different windows over one sequence, and only the sequence is shared.

That is what makes the pieces independent rather than a single design:

| piece | whose | needs |
|---|---|---|
| a ring over **rendered lines** | the head | nothing — stop holding every line it has drawn |
| asking for a **missing ordinal** | the head | `items_dropped` plus its own index; no flag on the wire |
| answering **by ordinal** | the daemon | the store read, behind the `SessionSource` seam |
| heads with disjoint sets | theirs | — |

**Still to write, in this order** — the order changed once the two rings above were separated out:

1. **The head does not ask yet.** It has no flag for "this ordinal is missing", no tracking of an
   answer, and no partial-body render. This is the blocking piece; the two below are about where
   the answer comes from. — OPEN: the same defect as the list above — the head asks nothing and holds no flag for a missing ordinal.
2. **A row trimmed from the *view* is probably still in the log.** `Peek` already reads it, and
   `TranscriptContent` carries the body. So the cheap next step is not a new store read but a
   **log-ring read shaped like `FetchRow`** — same ordinal, same window, one source. Worth
   measuring first how many rows the log's 20,000 events actually cover, since a row costs several
   events and the honest answer may be "a few thousand more than the view, not a different order". — OPEN: the log ring is never read for a row — resolution is the view and then the store (`crates/sessionlog/src/server.rs:1239-1241`).
3. **Only past both rings does the disk matter** — and the seam it needs **already exists**.
   `SessionSource` (`crates/sessionlog/src/registry.rs:311`) is a trait `sessionlog` defines and
   `harnessd` implements as `StoreSessions` (`crates/harnessd/src/sessions.rs:1809`), introduced
   for exactly this reason: *"A **trait and not a `Store`**: `letibot-sessionlog` does not depend
   on `letibot-tokencore` and should not start… the daemon passes an implementation in, and a
   registry with no source behaves exactly as it did before."* So a `row_body(session_id, ordinal)`
   method on that trait is the whole of step 3, with no new direction of dependency —
   `crates/harnessd/src/transcript_source.rs` already reads the rows in order
   (`SELECT … FROM transcript_item WHERE transcript_id = ?1 ORDER BY seq ASC`). — ✓ fixed in a17be5cb, test a_row_is_readable_by_ordinal_without_loading_its_transcript

   **What step 3 does have to answer** is that a session is not one table of rows: resume forks
   mean the rows reaching the head are a **chain** of transcripts, which is why
   `crates/harnessd/src/transcript_source.rs:129` walks `SELECT … FROM transcript … WHERE
   t.session_id = ?1 ORDER BY t.created_at DESC` rather than querying items directly. The ordinal
   the head counts must be the same sequence the chain produces, or a fetched row lands in the
   wrong place.

**(a) is the one that matters first**, and it is also the smaller: it needs no daemon change, no
store read and no protocol, and it is what makes (b) safe to add — without it, giving the head
access to more rows would only move the memory problem from the daemon to the head.

**And it is not `Peek`, which an earlier draft of this item said.** `Peek` fetches a whole
session's scrollback and has **no position**: you name a session, you get all of it. That is
right for the picker — "what was that session about" is a whole-thing question — and it is the
wrong shape for this, which is positional. The *pattern* worth reusing is that it reads without
moving your seat; the cursor and the window are new.

**Checked before building, 2026-09-20**: across the whole store there are **8 rows over
64 KB**, and the largest is a `tool_result` (418 KB) — which never goes through the
markdown lex at all, because a tool result is drawn as payload text with its own bound.
The largest *markdown* rows are a 42 KB assistant answer and an 86 KB reasoning block.
At ~450 ns/byte those are ~19 ms and ~39 ms, once each.

So a single huge row is **not** where a 160 MB session's cost lives — it is thousands of
ordinary rows, which R19.1 now renders only the tail of. The stage below stays worth
doing for its own sake (an unfolded row is unreachable past the fold today: `render_bounded`
draws a tail and an elision count, and nothing fetches the rest), but it is a *fidelity*
fix rather than the performance one, and the number that said otherwise was never taken.

### R19.2 (detail) — a row is a logical string; the head holds a viewport — ✓ fixed in 076d6f1c, test a_long_payload_can_be_paged_to_its_end

The operator's framing, and it is the correct one: a 418 KB tool result is a **logical**
string that wraps to thousands of **display** lines, of which 40 are on screen. Wrapping
exists (61 call sites); **paging** does not, so an unfolded row is unreachable past the
fold.

So the fix is the editor's, not a cap: the daemon owns the buffer, the head holds the
window, and a row is fetched when it is unfolded. `Peek` already fetches a stored
conversation for the session picker — this needs a row-shaped form of it rather than a
new mechanism.

**Not the fix**: capping a row's payload at the source. That cuts a logical string to
solve a display problem, which is the editor mistake. A `read` of a large file, a build
log: the bytes are the truth and the screen shows 40 lines of them.

### R19.3 — bound the snapshot in bytes — **DONE 2026-09-20** — ✓ fixed in 0c5d4d02, test a_view_drops_the_oldest_rows_to_stay_inside_its_byte_bound

`ViewBounds::items: 2_000` bounded a **count**, and the operator has sessions of **160 MB
and thousands of turns** where one row may be 418 KB — so the daemon's snapshot could be
most of a gigabyte, cloned per attach and sent over the socket.

Done: `ViewBounds::item_bytes` (default 8 MB) beside the count, and one `trim()` that
both bounds run through. Three things the implementation needed that a one-liner would
have missed:

- **Two places can go over, not one.** A row is *announced* before it is filled, so the
  count is checked at the announcement and the **bytes at the fill** — a row that arrives
  empty and lands 418 KB later blows the bound the moment it lands, and trimming only on
  the next announcement would leave the snapshot oversized until one came.
- **The newest row is kept whatever its size.** A view with nothing in it cannot be read
  or scrolled, and a bound that emptied the transcript would break the thing it exists to
  protect.
- **`TranscriptItem::bytes` is the one definition** of "how big is this row", in
  `letibot-transcript` where both sides can see it — the daemon bounds by it and the head
  decides whether a transcript is too big to walk by it. The head had its own copy before
  this, which is exactly the drift the shared one prevents.

`items_dropped` reports the trim, so a head can say "N rows above" honestly.

**Done when.** Attaching to a 160 MB session draws the current frame at the same cost as
attaching to a 1 MB one; scroll-up extends it without a full lex; an unfolded 418 KB row
is reachable rather than truncated; and `eval` stays out of it — no logical string is cut
to fit a screen.

### R19.4 — the head holds a window, and scrolls above it by asking — **DONE**

The head-side half of (a) and (b) together, because they are one thing: a window is only bounded
if what is above it can be fetched back.

- **`App::held_limit`** (8 MiB, a field so a test can shrink it) and **`App::trim_held`**: once
the head's copy of the rows is over it, the oldest rows above the reader's window are let go and
the window is rendered again from what is left. The rendered lines go with them, so the buffer
is one window and not every line ever drawn. Before this, `hist_lines` never shrank: a session
that started small and grew stayed in forward-walk mode for ever and the head grew with it.
- **`App::items_base`**, from the snapshot's `items_dropped`: where `items[0]` sits in the
session. That is what makes an ordinal nameable after a window has been trimmed.
- **`ClientFrame::FetchRows`/`ServerFrame::RowsFetched`** (`PROTOCOL_VERSION` 56): a page of
whole rows by ordinal. The daemon answers from its view and falls through to the store behind
`RowSource::row_items` for the rest, one query per page. `FetchRow` was the wrong shape here: a
byte window inside one row's body does not carry the row's kind.
- **The ask**: `App::ask_for_rows_above` fires when the reader has scrolled to the oldest row
the head holds, one page (100 rows) in flight at a time, and stops for good once a page comes
back empty. The page is prepended, every row index shifts with it, and the reader's held row
does not move on their screen.

**Both ends of one window, DONE.** Trimming used to let go of rows *above* the reader only, so a
reader parked up in history while the session kept appending still grew the head with the rows
arriving below them. `App::items_below` is now how many rows sit below the window,
`App::trim_held_below` lets them go (a screen of rows stays under the held row, and only when the
rows above cannot pay for the overflow), and `App::ask_for_rows_below` brings them back a page at a
time, by ordinal, when the reader comes down to the end of what is held. The tail follows again
from the last page on.

Three things the implementation needed that the design did not name:

- **A row arriving while the gap is open is counted, not held.** The window is one contiguous run
  of the session, so a row spliced onto its end would take the ordinal of a row still missing. It
  is counted until the page that fills the gap arrives, and the page carries it whole.
- **A body for a released row is not an orphan.** `record_item` warned about every body with no
  row; for a head that holds a window that is the ordinary case, above and below it, and the page
  is what brings the words back.
- **The wheel's "is the stream moving" is the session's row count**, not the window's: the window
  stops growing exactly when rows start landing in the gap, and a park out of which the tail keeps
  running has to stay walkable.

**Still not done, and it is the same shape as the last time.** Nothing on the screen says the tail
is not held: a window that ends forty rows above the session's end draws as if it ended the
session. The count is there to say it with; no seam prints it yet.

---

## R13 — cloud GLM, DeepSeek and Grok as turn backends — **SETTLED 2026-09-14 (claude-lab2x1); left: nothing in the tree has run against a real provider — every provider test is a fake server (`crates/provider/tests/fake_provider.rs`), and the entry's `Done when` asks for live tests behind the key — and the providers' prompt caches are still only relied on, `PrefixGuarantee::Asserted` (`crates/backend/src/lib.rs:224`) being the whole claim**

Done: `crates/provider`, `MessagesBackend` in `letibot-backend`,
`TurnEngine::run_turn_messages`, `--provider/--api-key/--thinking`, the `provider`
disclosure, cost in the footer; fake-provider tests and an end-to-end harness run
(`docs/providers.md`). — ✓ fixed in a592cc89, test a_turn_streams_reasoning_text_and_a_tool_call_and_the_request_carries_the_key_and_tools **Not verified live** — no key on lab2x1. — OPEN: nothing in the tree has run against a real provider — every provider test is a fake server (`crates/provider/tests/fake_provider.rs`) — and `Done when` asks for live tests behind the key. Open, per the
operator's note: an eager-compaction budget under a meter (compaction is manual
today), and the providers' prompt caches are relied on through the stable prefix. — ✓ the eager-compaction budget under a meter is fixed in e1419a07, test an_unmeasured_metered_session_is_not_folded_before_it_is_measured — OPEN: the prompt caches are still only relied on — `PrefixGuarantee::Asserted` (`crates/backend/src/lib.rs:224`) is the whole of the claim, and `cached_tokens` is disclosed rather than asserted (`crates/turn/src/compaction.rs:114-120`).

*"2. i want cloud glm and deepseek and grok to work in letibot/code."* D10 reserved
the seam (`crates/backend`: `TurnRequest` holds the transcript, `BackendCaps` states
facts, `Meter::Money`, `PrefixGuarantee::None`). All three speak an OpenAI-compatible
chat-completions API with tool calling; one `messages` backend with three endpoint
presets. Keys from the environment (`ZHIPU_API_KEY`, `DEEPSEEK_API_KEY`, `XAI_API_KEY`)
or `~/.config/letibot/providers.toml`. D10's rule holds: the invariant suites skip
loudly under a provider — `skip_reason()` says the prefix check did not run.

**Done when.** `harnessd --provider deepseek --model deepseek-chat` (and `glm`,
`grok`) runs a turn with tool calls through the same engine; a fake-server test
covers the message conversion, streaming and tool-call parsing; live tests behind the
key being set; cost reported as `TurnCost.micros_usd`.

---

# 2. NEEDS A NOD — small question first, then unblocked

## N5 — the agent should know the view is hermetic, and propose the grant it needs; left: the model is told what its view holds only when a path fails, and which asks must be refused rather than prompted is undecided — `grant_view` raises an ordinary card for `~/.ssh`, with no never-write precheck

Operator, 2026-09-11: *"i guess the agent should know it is hermetic and prompt me with
his idea of shared data which i can allow."*

**Measured 2026-09-11, and it is why this matters now.** The confined backend builds on
this box — `bubblewrap 0.11.1`, cgroup entered, nested-namespaces held, no-new-privs
held — and it is `writable + EXEC`, so confinement costs no file access. But its view is:

    project /home/dead/Projects/letibot (rw); 19 read-only system paths;
    $HOME is a FRESH TMPFS at /run/letibot/home
      (so a build cache under $HOME is empty every run); no grants
    egress: DENIED — no interfaces, no routes, no DNS

So `cargo test` cannot run: `~/.cargo` is absent and there is no network to refetch it.
That is the boundary working — *"a secret outside it is ABSENT, not denied"* — pointed at
a toolchain instead of a secret.

### Most of this exists

`exec/confine.rs` already has the mechanism, and its discipline is the interesting part:

- `Grant::{ReadOnly, ReadWrite, AgentSocket}`, each carrying a **`why`**, because *"a
  grant nobody can explain is a grant nobody can revoke"*
- `ViewSpec::granting()`, and `Boundary::describe` prints **the consequence next to the
  grant** — `ReadOnly` means readable *into the transcript*, since §3's second half is not
  enforced by that module
- the `agent_from_env` precedent: when a key is needed there is no grant that binds it.
  The socket is bound, the key is not, and *"the error path is the important half"* — the
  refusal names the mechanism nobody built rather than quietly widening

### What is missing is the loop, and it is three things

1. **The model does not know it is hermetic.** The boundary description is a startup
   banner for the operator; nothing puts it in the model's context, so a failure inside
   the view looks like a broken toolchain rather than an absent one.
2. **An absence does not say what would fix it.** `cargo: command not found` or a missing
   registry should come back as *absent because outside the view*, with the grant that
   would change it — the same shape as `edit`'s read-before-write refusal and R8's
   near-miss, self-correcting in the same call.
3. **Grants are construction-time.** `ViewSpec` is built before the spawn; nothing adds
   one for the rest of a session after an operator approves it.

### The questions, which is why this is a nod

**Does a grant go through the gate like any other decision?** It should — the prompt is
*"the model wants `~/.cargo` read-only, because: to run the workspace's tests"*, and
`grants session` then makes it stick for the session, which is the machinery that already
exists. The consequence line has to be in the prompt, not only in the banner: binding
`~/.cargo` read-only means its contents can reach the transcript.

**And which asks must be refused rather than prompted?** The ssh case is the precedent
and the flow rule (§3) is the test: a grant that would make secret bytes *readable* is
inexpressible, not adjudicable, however politely the model asks for it. A model that can
propose grants must not be able to propose that one and have it arrive as an ordinary
prompt. `NEVER_WRITE` is the first precheck and the tier mints no `Adjudicable` for an
inexpressible action — so the pieces are there; what is undecided is that a
model-proposed grant is routed through them rather than around them.

**Smallest version that closes the loop for tests:** the view learns one grant shape
(`ReadOnly` on a path), the model is told the view is hermetic and what it holds, an exec
failure names the absence, and the grant prompt carries its `why` and its consequence.
That is N4's answer too — see `TODO-settled.md`'s N4 for why a shaped runner was wanted rather than
`bash`.

---

## N3 — a refusal claims "the operator has been told" without knowing it; left: the larger half — a pending or expired decision is still unrecoverable, so "what decision" has no answer after the window, and it waits on N1 (open); the delivery claim itself is fixed, 9676492b and 37e6cf8b

From the same refusal, verbatim:

    The operator has been told, with this: grant `adj-…-0001` (edit) for this session,
    or answer the pending decision.

The operator had **not** been told: they were away from the keyboard, the 300-second
window expired, and when they came back they asked *"what decision"*. The prompt was
gone and the question was unrecoverable — the ask is not a transcript row, only the
refusal is.

This is the fleet's own rule inverted. *Guard the fact, not the proxy*: "a prompt was
emitted" is not "the operator saw it", and the sentence states the second while knowing
only the first.

**Why this needs a nod rather than a fix.** The minimal change is honest and small —
say what is true (*"a decision was raised and timed out unanswered"*) instead of
asserting delivery. The larger change is the one that would actually help: make a
pending or expired decision **recoverable**, so `what decision` has an answer after the
window closes. That is a question about what the transcript records, which is N1's
territory, and it should be decided with N1 rather than beside it.

**Still open?** `grep -rn 'has been told' crates/tools/src/` — the payload builder.

**The question:** does an ask become a transcript row, or does the head keep a durable
list of expired decisions? Either answers *"what decision"*; they differ in whether the
model sees it too.

---


---

# 3. BLOCKED ON THE OPERATOR — do not start

- **T5, T25** — 17 open decisions. Several gate everything else.
- **T22 and T17** — both wait on **D11**. "Until it is decided, M1 remains formally
  unexited."
- **T19** — rewrite-or-not is the decision itself.
- **T24** — needs **D3** (firecode's two asks).
- **T16** — several items explicitly want an operator or a strand owner.
- **T3** — the minja bug report is written and ready, but `llama.cpp/AGENTS.md` says
  reports and PR text are the operator's to write.
- **T6** — not started; `pytest` is still not installed, which `docs/workstreams.md`
  calls blocking for W1/W2.

---

# 4. Reference — the full text of everything above

## The review of 2026-10-12, in full

Five read-only reviews of main at `50b3e55`, run as subagents on the operator's ask (*"start review
subagents - we need to find edge cases, ux inconsistencies, notifications that posted in full instead
of line + popup pattern etc etc"*). Kept verbatim so nothing a summary dropped is lost; R89–R93 are
filed from them, and their `[name N]` pointers refer to the numbering here.

### Notices drawn in full instead of one line + popup (R89)

#### Notice walls in the transcript (letibot main 50b3e55, read-only review)

Nothing was edited or built. I read `sessions.db` and `metrics.db` read-only, with Python `-I` scripts in my scratchpad.

**How I measured.** Hub warning details are not stored anywhere: `sessions.db` has no warning table, and `metrics.db` `event` rows of kind `warning` have empty `attrs`. So real text below comes only from rows the session writes in the person's place (`transcript_item` kind `user` with speaker agent) and from System rows. For warnings I give the frequency from `metrics.db` (923 warnings all-time) and the producer's own wording.

To find which session rows fall through to a raw wall, I ported the tui's `compact_cards` / `compact_notice` / `folded_notice` / `ended_jobs` gates to Python. I ran it over the last 4 days, with duplicate texts removed. Row counts assume about 100 text columns.

**How the transcript draws things now:**
- **Session rows** go through `user.rs:61` (speaker Agent): first `compact_cards`, then `compact_notice`, then `folded_notice`. Anything else falls through to `session_block` (`blocks.rs:7`, rano `SessionBlock`). That draws `  session · ` plus the WHOLE text wrapped, in faint grey. These are the gray walls.
- **System rows** are drawn whole by `item.rs:~263` through rano `screens::system_row`. There is no fold. They are shown at every rung except Conversation.
- **Hub warnings** go through `panes/notes.rs:120 note_lines`:
  - a code with a card reader becomes a one-line card you can click;
  - `compacted`, `auto_compact`, `compact_half` and `reseated` become one line ending `· /notes`, which cannot be clicked;
  - everything else gets rano's `Note::lines` (rano 9c53f50 `agent/notes.rs:187`): 3 wrapped lines plus `  … +N lines · /notes`. It cannot be clicked, and the only way to see the rest is the `/notes` listing.

#### Ranked findings

**1. `[todo check]` that does not fold: RAW WALL.** 264 distinct rows in 4 days, about 4,490 screen rows in total, median 8 rows, max 66 (6,233 chars).
- Renderer: `user.rs:96` → `session_block`. The fold is `blocks.rs:177 folded_notice`, which folds a group only if every line after the bullets matches `PROMISES` or is a `Waiting on…` line.
- Producer: `tools/src/builtins/todo.rs:1728-1820` and `:2412`.
- Why it fails, by count:
  - 168: the old Model closing `do this one, or mark it done, or drop it — a plan left open…`. The fold dropped this phrase on purpose, but the store still has them (5 written today, 1,081 on 10-10).
  - 13: the Queue advice `this row is the merge queue's, not your plan's: …` has no promise phrase.
  - 10: the Parent advice `your parent asked for this one, so do it and say in your reply…` has no promise phrase.
  - 13: the `[todo check] this turn is finished and NOTHING can start — every item left waits on another:` variant, whose closing `unblock one of these rather than starting something new: …` is not recognised.
  - Some of the 61 rows that do carry the current promise: the `blocked_said` line (`3 rows waiting on the operator: …`) comes after the promise and is not a `Waiting on…` line.
  - 18: the `session X was seated … with a prompt this daemon would not compose now: …` paragraph is added as a second `\n\n` group, which drops the whole row to raw.
- Real example (1,545 chars, 5 lines): `[todo check] this turn is finished and one item is not done (1 more open):\n  - review-agent/a-pass-reads-one-row (entry s-…): Your branch is cut from …— the merge queue's, and you were asked about this one already…\nthis row is the merge queue's, not your plan's: …\n\nsession s-gatekeeper was seated 10-10 22:42 with a prompt this daemon would not compose now: …`
- Should be: `todo · <item head> · yours|operator's|queue's|parent's · N more open · click for details`, or `todo · nothing can start · N wait on <child/operator> · click for details`. The stale paragraph should split off as its own stale card.

**2. `Tool call X was not executed: …`: RAW WALL, worst single row.** One row of 1,083,613 chars and 6,902 lines, about 13,800 screen rows.
- Session `s-1789462738453908838-sub-1791503723529407810`, 10-09 02:03. It is the same sentence once per truncated call (`read` ×1, `bash` ×6,901).
- Producer: `turn/src/length.rs:139`. Renderer: the `session_block` fallback.
- Should be: `length cap · 6902 calls not executed (bash ×6901, read ×1) · click for details`.

**3. `message from your parent session `s-…`: …`: RAW WALL.** 130 rows that are not review-shaped; median 20 rows, max 46 (3,302 chars, 25 lines).
- Producer: `harness.rs:14865`. These are messages a parent model wrote to its child (briefs, extra requirements).
- Only the review-shaped ones are unwrapped (`review_card.rs:222 unwrap_message`, 21 rows, which become cards).
- Real example (1,392 chars, 7 lines): `message from your parent session `s-…`: Two new constraints from the operator, fold them into the plan before you finish:\n\n1. REPLICATION IS IN SCOPE, …`
- Should be: `parent s-gatekeeper · <first clause> · click for details`.

**4. `[intent check]`: RAW WALL.** 52 rows in 4 days, median 12 rows, max 20 (1,518 chars, 10 lines).
- Producer: `tools/src/builtins/intent/ledger.rs:304`. No reader.
- Real example: `[intent check] this turn declared 6 item(s); 0 tool call(s) succeeded and 0 ran without producing a result.\n  - #1 "Write src/datum.rs: …" was declared this turn and no tool ran in this turn — start it, or say why not\n … board: 0 of 6 complete — 0 in progress, 6 pending, 0 blocked`
- Should be: `intent check · 6 declared, 0 ran · board 0/6 · click for details`.

**5. `[context check]`: RAW WALL, short but frequent.** 99 rows, 4–5 rows each (up to 491 chars).
- Producer: `harnessd/src/context_claim.rs:462` and `:530`.
- Real example: `[context check] you said "Each reports on its own, and my context is nearly out, …". Your context is 54% full — 547,198 of 999,999 tokens. That is not the wall, … Continue with what you stopped.`
- Should be: `context check · 54% full (547k/1M) · not the wall · click for details`.

**6. Merge-queue session notices with no card reader: RAW WALL.** 8 rows, median 28 rows, max 52 (4,177 chars, 16 lines).
- Shapes:
  - `The merge queue cannot get a verdict on `X` (entry …), so it will not land … placing this session in a firecode VM: io…`
  - `The merge queue is holding `X` … has no merge gate on main`
  - `The merge queue cannot finish with `X` …: steering_urgent: the daemon is stopping…`
  - `The merge queue cannot land anything in `X`: its target … has no … section in AGENTS.md`
- Only the last is still produced (`harness.rs:8202`). The others come from older builds but stay in carried or reseated history.
- `red_card` only opens `EMERGENCY — `, `The merge queue's gate of ` and `The merge queue CANNOT JUDGE `.
- Should be: red-card or review-card lines, e.g. `‼ queue · X · cannot judge (VM: io error) · click for details`.

**7. `[reseat]` session row: RAW WALL.** 8 rows, up to 22 rows (1,684 chars, 8 lines). Producer: `harness.rs:2263`.
- Real example: `[reseat] The person re-seated this conversation with `…`: the conversation was carried across as it was, nothing summarised and nothing dropped. Mes…`
- The same event also draws the `reseated` warning one-liner and a System(update) row, so one act becomes three rows.
- Should be: `reseated · verbatim · N items carried · click for details`.

**8. `[monitor]`: RAW WALL.** 7 rows, up to 15 rows (1,272 chars, 3 lines). Producer: `harness.rs:990`.
- `job_card.rs:249 notice_groups` lists `[monitor] ` as a tag, but `folded_notice` has no `[monitor]` case. So any row that mixes `[job]` with `[monitor]` fails `compact_notice` and the WHOLE row falls back to raw.
- Real example: `[monitor] 1 watch(es) fired:\n  - `child-nag-suite` (pid 1299446 (/bin/sh -c cd … cargo test …) — process(es) … leaving …), declared by turn …: FIRED after 129.8s — pid 1299446 (/bin/sh -c …the whole command again…)…\nOnly a FIRED watch is an answer about the world; …`
- Should be: `monitor · child-nag-suite FIRED after 129.8s (pid gone) · click for details`.

**9. `The harness is watching what your tool calls return…`: RAW WALL.** 6 rows, 8–9 rows each (725 chars). Producer: `harnessd/src/progress.rs:246`.
- Should be: `progress · last N rounds: N calls, N returned nothing new · click for details`.

**10. `[the deliverable]`: RAW WALL.** 6 rows, 5 rows each (497 chars). Producer: `harness/deliverable.rs:98` and `tools/src/builtins/task.rs:366`.
- Real example: `[the deliverable] Your worktree `…` has uncommitted changes, so the branch `…` does not hold them: ?? .letibot/notes-offers.jsonl. The branch is the deliverable — commit this…`
- Should be: `deliverable · worktree has uncommitted changes (1 file) · click for details`.

**11. Stale-prompt sentence as a session row: RAW WALL.** 3 rows on their own, 8–9 rows each (875 chars), plus 18 rows where it is attached to a todo check.
- `stale_card.rs:131` registers only a `WARNING` kind (`prefix_stale`), with no `SESSION` kind.
- Real example: `session s-gatekeeper was seated 10-10 22:42 with a prompt this daemon would not compose now: the system instructions and the tool schemas have changed since. … Say this to the person in your reply, …`
- Should be: the same `prompt stale · seated … · …` card, registered in `SESSION_KINDS` with an end marker, so it also splits off a todo check.

**12. `[answer] on "…"`: RAW WALL.** 1 row, 968 chars on one line, about 10 rows. Producer: `operator_ask.rs:75`.
- Should be: `answer · <row head> · operator chose "<x>" · click for details`.

**13. Orphan `[task]` completion: RAW WALL.** 1 row, 467 chars, 6 rows. Producer: `harness.rs:1114`, `:1146`.
- Text: `[task] a subagent a session below this one started has finished:\n  - `…` done: … (started by `…`)\nThis session is told because the one that started it cannot be: …`
- `blocks.rs PROMISES` lacks the closing `This session is told because the one that started it cannot be:`. `job_card::CLOSINGS` has it for `[job]`, so the job and task folds disagree.
- Should be: the same folded `Agent …` line, clickable.

**14. Short raw session nudges: RAW, 2–3 rows each.**
- `Your previous turn was stopped at the context wall…` (`harness.rs:9037`).
- `You keep overthinking. Cut it short and do things…` (`harness.rs:11368`, `turn/src/compaction.rs:243/253`).
- Should be: `context wall · compacted, continuing · click for details` and `nudge · reasoning-only turn · click for details`.

**15. System(update) rows: RAW WALL whenever tools, thinking or system rows are shown.**
- Renderer: `item.rs:~263` → rano `screens::system_row`, which wraps the whole text with no fold. It is hidden only at the Conversation rung (the operator's read-edits).
- Store, last 6 days:
  - 39 compaction-summary rows (`This conversation was compacted: what was said before this point is replaced by …`), up to 18,684 chars and 116 lines, about 190 screen rows each;
  - 25 reseat rows (`This conversation was re-seated onto a new prompt…`, 253 chars);
  - 1 compaction instruction (`The conversation above is what this record stands in for…`, 1,466 chars, 18 lines).
- Producers: `turn/src/compaction.rs:448`, `:1756`; `harness.rs:10594`; `dialect.rs:156`; `httphead.rs:230`.
- Should be: `system · compacted summary (18.7k chars) · click for details`.

**16. Hub warnings with no card: 3 wrapped lines plus `… +N lines · /notes`, no click, no popup.**
- Renderer: `panes/notes.rs:120` → rano `notes.rs:187`. A short detail is 1–3 rows; a long one is 4 rows with the rest only in `/notes`.
- Frequency (`metrics.db`, all-time):

| Code | Count | Notes |
|---|---|---|
| `context_claim_repeated` | 63 | |
| `resume_note` | 55 | |
| `daemon_stopping` | 24 | about 330-char detail, so 4 rows: `` `{who}` asked this daemon to stop. Every head detaches, the socket goes, … nothing new is started.`` (`sessionlog/src/server.rs:1589`) |
| `merge_queue_store_error` | 23 | `the queues could not read or write their store while {doing}: {why}. Nothing moved; …` (`mergequeue.rs:131`) |
| `merge_queue_recovered` | 23 | |
| `queue_lease_lost` | 17 | `serve.rs:1553` |
| `queue_wrong_build` | 13 | `this daemon serves NO queue: the store's schema is … Restart it on the current build.` (`serve.rs:1455`) |
| `queue_target_proposed` | 11 | |
| `memory_high` | 11 | `memwatch.rs:236`, about 230 chars. **Not in the 127-code census**, so it defaults to Failure and is drawn red. |
| `waiting_on_operator` | 10 | `WAITING ON YOU: the model asked "…" on the row `…` — open the question card…` (`operator_ask.rs:139`) |
| `daemon_stopping_runs` | 10 | |
| `operator_question_settled` | 7 | |
| `mode_set` | 7 | |
| `subagents_stopped` | 4 | |
| `model_endpoint_retry` | 4 | |
| `todo_answer_refused` | 2 | |
| `context_wall` | 2 | |
| `child_door_refused` | 2 | |
| `slash_refused` | 1 | |
| `reseat_refused` | 1 | |
| `length_empty_turn` | 1 | |
| `calls_abandoned` | 1 | |

- `auto_compact_no_progress` and `auto_compact_failed` are deliberately drawn whole (3 lines plus the seam). The comment says their detail IS the finding. That still works as a one-liner with the reason's first clause and a popup.
- Should be: `<mark> <code> · <first clause> · click for details`, a card through `WARNING_KINDS` or a generic fallback card for every code.

**17. Other notes this head makes itself, same rano fold, no click:**
- `denied:<req>` refusals (`app/events/asks.rs:344`): `REFUSED {tool} — {summary}. {outcome} by {by}: {basis}{repeat}. {grant}`. Red, 3 lines plus the seam; this is the old "red wall", only shortened.
- `Note::NotRun` (`asks.rs:331`), `Note::Answered`, `Note::Decided`: folded to 3 lines.
- `Note::Pane` (`app/events/term.rs:153`): never folded, a head line plus up to `PANE_LAST_LINES` = 4 rows.
- `sudo`, `prompt`, `provider_key_refused`, `protocol_skew` and `daemon_replaced` (`hello.rs:194`), `unreadable_frame` (`session.rs:500`), `anchor_lost` (`scroll.rs:1115`).

**18. One line, but no way to see the details (compaction one-liners).**
- `compacted`, `auto_compact`, `compact_half`, `reseated` (`panes/notes.rs:222 compaction_row`).
- Drawn as `· compacted — compacted: 958397 → 100005 tokens, on transcript s#t25. … · /notes`. The code is said twice.
- They are not in `note_cards`, so a click does nothing; the only way in is the `/notes` listing.
- Should be: `compacted · 958k → 100k tokens · click for details`.

**19. Folded notices: one line per item, but no popup.**
- `[task]` (177 rows, up to 10 lines each, one per subagent), `[todo check]` (103), `[meanwhile]` (2), folded by `blocks.rs:177`.
- `diff_popup.rs:228 notice_rows_in` marks a row clickable only if it has `[job]` groups or a card, so a pure `[task]` or `[todo]` row cannot be clicked. An 8-subagent `[task]` row is 8 lines with no popup.
- Should be: one line (`agents · 8 finished · click for details`), or at least clickable.

**20. (Not the transcript) The flash notice above the composer** (`ui/screen.rs:296`, rano `screens::notice`) is cut to one line with no way to read the rest.

#### Inconsistencies among the existing one-liners

- **Hint wording.**
  - `click for details` (`line_cards::CLICK`): red, review, asked and restart cards.
  - `alt-p for the card` (`stale_card.rs:28 POINTER`): the stale card. alt-p opens the reseat card (`app/reseat.rs`), but a click on the line opens the NoteCard popup, a different view.
  - `… · /notes`: the compaction one-liners, which cannot be clicked.
  - No hint at all: the job card's two lines and the folded `Job/Agent/Todo/Meanwhile` lines.
  - The hint sits at the end of the line, so `trim_to` cuts it first at narrow widths.
- **Lead and glyph.**
  - Only the red card has a glyph (`‼ main RED at 60bf819c`).
  - The other cards lead with a word: `review ·`, `asked ·`, `restarted ·`, `prompt stale ·`.
  - Compaction uses rano's `· code — ` with an em dash and repeats the code.
  - Jobs lead with `j182 · slug · exited 0 after…`.
  - Folded lines use `Job `/`Agent `/`Todo `/`Meanwhile: `.
- **Colour and frame depend on where the card came from.**
  - A session-row card (`blocks.rs session_card_block`) sits inside `  session · ` with an `HH:MM:SS` stamp; the parts outside the accent are Faint.
  - A warning card (`panes/notes.rs:213 card_row`) is a bare line: no label, no indent, no stamp, and the parts outside the accent are `Span::raw`, the default bright colour.
  - So the same `‼ main RED` card looks different as `target_red` than as an `EMERGENCY —` row.
  - Accent roles: red is Failure, review uses a per-verdict role, asked, restart and stale use Attention, compaction is Faint (Routine).
- **Click paths.**
  - Session cards and job notices use `Target::Notice` → `open_notice_popup(item row)`.
  - Warning cards use `Target::NoteCard` → `open_card_popup(index)` (`keys/mouse.rs:97-105`).
  - Compaction one-liners, rano notes and folded rows cannot be clicked.
  - No key opens any notice popup from the transcript. It is mouse-only, plus the todos pane (`todos.rs:831`).
- **Popup.**
  - Every popup is the same `PopupBody::Notice`: full text wrapped, Esc or ctrl-c closes it, footer `esc closes · ↑↓ PgUp PgDn Home End`. These agree.
  - Titles differ:
    - a card's title is drawn in its role;
    - a job's is `j1 · a job ended` in Strong;
    - the fallback is `notice` in Role::Failure, red (`ui/diff_popup.rs:71`).
  - A NoteCard loses its code, so `popup_title` tries every warning reader in turn.
  - A parent-wrapped review card's title does not say it came through the parent.
- **Height.** The job card is 2 lines per job (the operator's spec); every other card is 1.
- **One event, several rows.**
  - A red target: a `target_red` warning card plus an `EMERGENCY` session card.
  - A restart: a `restart_restored` / `restart_reminded` warning card plus a `[restart]` session card.
  - A reseat: the `reseated` one-liner, a System(update) row, and a raw `[reseat]` wall.
  - A stale prompt: the `prefix_stale` card plus the raw session sentence (finding 11).
- **Census gaps.** `memory_high` and the `denied:<id>` codes are not in `warning::TABLE`, so they default to Failure and are drawn red.

My scratch scripts (the Python port of the fold gates and the store queries) are in `/tmp/claude-1000/-home-dead-Projects-letibot/7a4b7ff7-173b-4f07-a444-4fda796b95b7/scratchpad/` (`classify.py`, `why.py`, `rows.py`, `sys.py`).

### Keys, cards and popups (R90)

UX consistency review of crates/tui at 50b3e55. I only read code: nothing was built or run, no files were edited, and no process was touched. Paths below are relative to `crates/tui/src/`. Findings are ranked by how likely the operator is to hit them.

#### A. Likely to be hit

**1. With the reseat card open, a pane's keys go to the card, so Enter or `1` reseats.**
- **Where:** `app/reseat.rs:183-230` (`key_reseat_card`) runs before `key_surface` (`app/keys/mod.rs:235` vs `:239`). `reseat_card_up()` → `slot_taken()` (`reseat.rs:110`) does not count an open surface. `ui/hint_bar.rs` has no branch for the reseat card.
- **How:** attach to a stale session with an empty composer (the card opens on every Hello). Then:
  - ctrl-t, ↓ to a row, Enter: you expect "reads it whole", but `/reseat` is sent (sel 0 is "Reseat now").
  - ctrl-s, type `1` to switch session: `digit_row(k,2)` reseats now. Typing `2` pins the card and swallows the digit.
  - In the todos pane, Tab toggles the card's "whole" view instead of marking a row done; Esc pins the card instead of closing the pane.
- **What happens now:** the bar shows the pane's hints ("↑↓ moves · enter reads it whole · esc closes") while the keys act on the reseat card.
- **Should:** an open surface should count as taking the slot (draw the strip instead), or `key_reseat_card` should only run when `focused_surface()` is None.

**2. Digits answer question cards whose rows carry no numbers, and the first digit of a typed answer is taken as a choice.**
- **Where:** `app/keys/cards.rs:415` (`_ if !typing && digit_row(..)`). rano draws question and permission rows as `▸ body` with no number. The question bar (`hint_bar.rs:124-130`) does not mention digits.
- **How:** a question "how many workers?" with choices `["2","4","8"]`. Type `16` on the empty composer: the `1` answers choice 1 ("2") at once.
- **Should:** on question cards, either drop digit-answer or draw numbers and require Enter. The permission bar (rano `Deciding`: "a row number answers") also points at numbers that are not drawn. The reseat and quit cards do number their rows (`▸  1  Reseat now`), so the cards disagree.

**3. A question that arrives while the operator is typing turns the half-typed prompt into its answer.**
- **Where:** `app/composer.rs:193-213`. A question's Enter with text calls `question_answer` → `QuestionAnswer::free(line)`. The permission path holds the words ("answered the ask — your line is held"); the question path sends them as the answer.
- **The same happens with alt-a:**
  - `bring_back_question` (`app/todos.rs:316-352`) refuses only when the away card has its own draft *and* the composer is non-empty. A card with an empty draft comes back over an unrelated prompt.
  - `open_operator_ask` (`todos.rs:136-165`) has the same rule.
- **Sequence:** Esc the question away, start typing "check the logs first", press alt-a to reread it, press Enter. The question is answered with "check the logs first".
- **Should:** when a question arrives or comes back over composer text, stash that text and give the card an empty field, or hold the words as the permission path does.

**4. A new decision arriving resets the highlight on the card already on screen; answering one card leaves the highlight where it was for the next.**
- **Where:** `app/events/asks.rs:59` sets `self.sel = 0`, then `open.push` puts the new card at the back. `open[0]`, the visible card, is unchanged. Nothing resets `sel` when `DecisionAnswered` removes `open[0]`.
- **How (first case):** on a permission card, ↓↓ to "deny". A second permission arrives (parallel calls, a subagent). The highlight jumps to row 0 (allow_once), and Enter allows.
- **How (second case):** answer card A at row 3. Card B appears with row 3 (clamped) already highlighted.
- **Should:** reset `sel` only when the front card changes, and always reset it when it does.

**5. Esc means something different on every card; on a permission card it silently arms the interrupt.**
- **What Esc does on each card:**
  - Question: put away (alt-a brings it back).
  - Reseat: not now (alt-p brings it back).
  - Prompt card: put away. No key brings it back, only `!send`, and there is no resident line (`cards.rs` `key_prompt`; `prompt_away` is not drawn anywhere).
  - Secret: refuse, which sends `None`.
  - Key ask, todo draft, setting picker: cancel.
  - Quit card: stay.
  - mode_confirm and term_ask: any key cancels.
  - Permission: falls through `key_decision` (`cards.rs:397` only matches questions).
- **How (permission card):**
  - With a pane open, Esc closes the pane behind the card.
  - Otherwise Esc reaches the editor and arms the Esc-Esc interrupt; a second Esc interrupts the turn that is waiting on the card.
  - Inside a subagent, Esc says "a decision is waiting".
  - Neither the card nor rano's `Deciding` tail says anything about Esc.
- **Should:** give the permission card an explicit Esc (put away, as questions do, or swallow it with a notice). Also give the prompt card alt-a and a resident line like the question card's.

**6. Typing a quit chord while a decision is up shows the quit hints, but Enter and the digits answer the decision.**
- **Where:**
  - Key order: `key_decision` (`keys/mod.rs:224`) runs before `key_quit_card` (`:227`).
  - Draw order: the decision arm comes before the quit arm (`ui/screen.rs:173` vs `:192`).
  - Bar: `hint_bar.rs:83` checks `quit_card` first, giving "1/2 or ↑↓ then enter · esc stays".
- **How:** a permission card is up; ctrl-c ctrl-c on an empty composer. The bar shows the quit keys while the permission card stays on screen. Pressing `1` (meaning "detach") answers the permission's first option.
- **Should:** don't open the quit card while a decision is up, or give it priority in both drawing and keys.

**7. With the editor pane focused, a permission or question card is drawn, but every key goes into the file.**
- **Where:** `app/editor.rs:411-440` (`route`) sends all keys to rano while `p.focused`. Nothing unfocuses it when a decision arrives (no `focused = false` on any ask event). The hint bar checks `editor_focused()` / `editor_drawn()` (`hint_bar.rs:116-119`) before `open`, so it shows the Editor/EditorBehind hints.
- **How:** ctrl-] into rano during a turn. A permission card appears with "↑↓ to choose · Enter to answer". Pressing `1` or Enter edits the file.
- **Also:** with the editor drawn but not focused, the bar says "enter sends" while Enter answers the card.
- **Should:** a new ask returns focus to the composer (or at least the bar names ctrl-]), and the bar checks `open` before the editor modes.

**8. In the todos pane, Enter on a row waiting on you opens a question card, but the bar says "enter reads it whole".**
- **Where:** `app/keys/bindings.rs:412` (`row_says` → "reads it whole") vs `app/keys/panes.rs` `todos_read`, which opens the card for a waiting row.
- **Should:** `row_says` should say "answers its question" on waiting rows.

**9. Nothing shows how many decisions are queued behind the one on screen.**
- **Where:** only `open[0]` is drawn (`screen.rs:173`). Neither the rano card nor the head adds a count.
- **Worst case:** a head-local todo-ask card at `open[0]` hides a model question with a deadline at `open[1]`. That question can time out unseen.
- **Should:** add "+N more waiting" to the card. A daemon decision should not queue behind a head-local todo card.

#### B. Moderately likely

**10. ↑↓ do not scroll /help or /status, and they pull history into the composer.**
- **Where:** `bindings.rs:157`: `READING = [PAGE, WHEEL]`, while Listing, TodoRow, QueueEntry, SubOut and JobOut all bind ↑↓ to scroll.
- **What happens:** ↑ in help falls through to `editor.vertical` and recalls an old prompt. Help and status own Enter (`surface.rs:90`), so a following Enter is silently swallowed.
- **Should:** add the `UPDOWN` scroll binding to READING.

**11. Enter is silently dropped on reading surfaces when a line is typed.**
- **Where:** `owns_enter` covers Help, Stats, Queue, QueueEntry, Standing, Config, Jobs, SubOut, Listing. It does not cover the ctrl-v window, where Enter still sends.
- **How:** with /help open, type a prompt and press Enter. Nothing happens and nothing is said.
- **Also:** TodoRow swallows every key (`On::Any`), while Help lets letters through to the composer. So "can I type here" differs between overlays.
- **Should:** say something on a swallowed Enter, or make the reading overlays consistent.

**12. A head-local todo-ask card is never closed when its row stops waiting.**
- **Where:** `app/todos.rs`. Nothing removes `todo-ask:` cards from `open` on `TodosUpdated` / `ServerFrame::Todos` (`events/todos.rs:16-30`, `events/frames.rs:115-127`).
- **How:** another head answers the row, or the model completes it or changes the question. This head keeps the card up, and answering it sends `AnswerTodo` for a stale question.
- **Should:** on every board update, drop or refresh `todo-ask` cards whose row and question are no longer in `operator_asks()`, and prune `away` the same way.

**13. The tab progress bar can stay on "Waiting" with nothing on screen waiting.**
- **Where:** `app/attention.rs:25` and `:48` count all of `away`, across every session, including stale todo-ask cards (see 12).
- **How:**
  - Put a question away in session A and switch to B. B's tab shows Waiting, while B's waiting line (filtered by session, `ui/status.rs:143`) shows nothing.
  - A put-away todo card whose row was answered elsewhere keeps the tab on Waiting indefinitely.
- **Should:** use the same rule as the waiting line, or name the other session.

**14. A resync or re-attach throws away an open todo-ask card, and its draft becomes a prompt.**
- **Where:** `app/events/mod.rs:343`: `self.open = open` replaces the list with the daemon's. The head-local card is dropped, not stashed in `away`, and the composer keeps the words typed for it.
- **How:** a resync arrives while answering a todo row. The card vanishes, and Enter sends the half-typed answer to the model.
- **Should:** stash the front todo-ask card (with its draft) into `away` before replacing `open`.

**15. In a subagent, Esc on a question puts it away, and the next Esc leaves the session with it unanswered.**
- **Where:** `cards.rs:397` takes Esc before the subagent arm (`keys/mod.rs`, which guards permissions with "a decision is waiting in this subagent").
- **What happens:** in the parent, the waiting line is filtered to the current session, so the child's question with its deadline is invisible.
- **Should:** keep the operator in the child, or carry the question onto the parent's waiting line.

**16. A new todo card and an arriving decision split the screen from the keys.**
- **Where:** `key_todo_draft` (`keys/mod.rs:189`) runs before `key_decision`, but `screen.rs:173` draws the decision ahead of the todo card. The bar shows Deciding.
- **How:** the todo card is open and a permission arrives. The permission card is shown, but Enter files the todo ("a todo item needs a title…") and Esc cancels the draft. ↑↓ and digits (on an empty title) act on the permission.
- **Same mismatch, rarer:** `key_ask` takes every key while the decision is drawn over it.
- **Should:** make the draw order match the key order.

**17. `/peek ID` opens a subagent's output, but the bar says "esc back to the subagents".**
- **Where:** `bindings.rs:298` and the special case at `:478`, which only handles Todos underneath. `events/frames.rs:304` pushes SubOut onto whatever is on top.
- **How:** `/peek ID` with no pane open (Esc goes to the conversation) or over the jobs pane (Esc goes to jobs).
- **Also:** a late `Peeked` reply opens SubOut after the operator has already closed todos.
- **Should:** say the actual surface below.

**18. Mode and model pickers show the wrong bar while a decision is open.**
- **Where:** `hint_bar.rs:120` checks `pick` before `open`. `/mode` can open the picker while a decision is up, because `/` lines run first. The decision is drawn and takes the keys, but the bar shows the Pick hints.

**19. Cards that own the keyboard show conversation chords in the bar.**
- **Where:** `hint_bar.rs` has no mode for `secret`, `prompt`, `key_ask`, `todo_draft`, `term_ask` or `mode_confirm`.
- **What happens:** while the sudo password card is up, the bar still reads "ctrl-s sessions · ctrl-n clears notes · ctrl-t todos…". Those chords are swallowed (secret/prompt/key_ask) or cancel the card (`mode_confirm`: pressing the advertised ctrl-t cancels allow-all).
- **Should:** give these cards their own bar.

#### C. Less likely: edge cases and wording

20. **/help omits alt-a, alt-r, alt-p, Esc-puts-away, digit answers and /reseat.** `ui/panes/help.rs` mentions alt-p only as the editor's "shows the change again", so one chord has two meanings and only one is documented.

21. **alt-p does nothing visible while another card holds the slot.** `reseat_expand` sets `open=true`, but `reseat_card_up()` is false, and the strip keeps saying "alt-p shows the card". The card pops up later, possibly in front of the next Enter.

22. **The reseat card's content can be windowed, but PgUp/PgDn cannot scroll it.**
    - **Where:** `card_window` (`ui/cards/decision.rs`) draws "… N line(s) out of view · pgdn scrolls" for every slot card. `scroll_card` (`keys/chords.rs:263`) returns false when `open` is empty, so PgDn scrolls the transcript instead.
    - **How:** reseat card with "tab shows all" on a short terminal.
    - **Same on:** the quit and todo cards.

23. **A question with no choices shows "↑↓ to choose · Enter to answer".** With `n == 0`, ↑ recalls history into the answer field and an empty Enter does nothing silently (`cards.rs:402-411`, `answer_marked` returns None). An empty-string choice renders as a blank highlighted row that Enter answers.

24. **Options containing " | " are re-split in the transcript.** `ui/transcript/asked_card.rs:43` splits on " | ". An unchosen option that contains it is drawn as two options (the code acknowledges this). The todos pane joins the same options with " / " (`ui/panes/todos.rs:396`): two separators for one list.

25. **The todos pane says "enter on the row, or alt-a, answers it".** alt-a opens the first waiting row (or a put-away model question), not this row (`todos.rs:316-365`).

26. **The waiting line and alt-a messages are confusing in some states.**
    - "WAITING ON YOU … alt-a to answer" stays up while that row's card is already open; alt-a then does nothing.
    - With a model question open, alt-a for a todo row answers "this question is the open card…", which is about the other card.
    - With two model questions put away, the line names one and counts only todo questions (`ui/status.rs:159-163`).

27. **Notifications call every open ask "permission needed".** This includes model questions and todo cards (`app/attention.rs:80`). Reseat and todo rows waiting on you never notify.

28. **The popup's bottom edge stays the same when a decision is open.** It still says "esc closes · ↑↓ PgUp PgDn Home End" (`ui/diff_popup.rs`), but `key_diff_popup` passes ↑↓/Home/End to the card and composer. With a decision open the bar says "the diff: pgup/pgdn scroll…" even for a notice popup. Casing differs between "PgUp PgDn" and the bar's "pgup/pgdn".

29. **Question card answers stay up until the daemon confirms.** Model question and permission answers stay on screen until `DecisionAnswered` arrives, and a second Enter resends for the same req_id. Todo-ask cards vanish at once. The feedback differs.

30. **The quit card's Enter has no empty-composer guard; its digits do** (`cards.rs` `key_quit_card`). Words typed after it opens are lost on Enter, which quits.

31. **Narrow widths:** reseat rows use `trim_to`, so `RESEAT_SUMMARISE` (79 chars) is cut, while question rows wrap. The popup body uses `inner = (w-4).max(20)` plus 2 columns of indent, which overflows below about 22 columns.

32. **Wording:**
    - The question hint appears twice, on the card and in the bar, with different casing ("Enter to answer" vs "enter answers").
    - Permission rows show internal ids (`allow_session`); this is intentional for typing, but it is internal naming.
    - The held-prompt notice says "this message goes to the model when the run ends" (`composer.rs:330`), but it is only held: the operator must press Enter again.
    - Help says ctrl-c twice "quits", but it opens the quit card.
    - A one-key `alt-a` is used for both "brings it back" and "to answer".

33. **Hint buttons are inconsistent:** the reseat strip's alt-r and alt-p are clickable, but the waiting line's "alt-a to answer" is not, because its segment starts with "WAITING" (`app/keyhints.rs` `scan_row`).

The highest-risk items are 1, 2, 3, 4 and 6: each can send an answer or `/reseat` the operator did not choose. 1, 2 and 6 are the easiest to pin with a key-sequence test.

### Delivery, waits, restore, reseat and the store (R91)

I found 18 problems. Three of them lose data or put a session in the wrong state, and they are likely enough to fix first. Everything comes from reading code at 50b3e55: I ran no tests, changed nothing, and did not touch any daemon or sessions.db. Each finding is marked **[confirmed]** (I read the whole path) or **[suspect]** (I read part of it and inferred the rest). The order is severity × likelihood.

---

##### 1. Messages handed back by a failed or interrupted round are dropped (HIGH × likely) [confirmed]
- **Where:**
  - `crates/harnessd/src/harness.rs:11188` builds a new `HubSteering` (`let mut steering = self.steering();`) on every pass of the HTTP retry `loop`.
  - `self.steering()` at harness.rs:12214 starts each one with an empty `held: VecDeque::new()`.
  - On failure the engine calls `pending.give_back(steering)` (turn/src/engine.rs:1418/1438/1464/1474 cloud, 760–987 local), which only pushes into `HubSteering::held` (harness.rs:1686).
  - The next pass, or `break attempted`, drops that `HubSteering`. It has no `Drop` impl, so the messages go with it.
- **Sequence:**
  1. The model is streaming.
  2. The operator types a follow-up. `on_delta` absorbs it through `try_next` into `pending.queued`.
  3. The operator presses esc-esc (`BackendError::Aborted`), or the provider drops mid-stream (`Unreachable` / `Malformed` / `Refused`).
  4. `give_back` moves the follow-up into a `HubSteering` that is thrown away. The prompt is gone. On a retry that succeeds, nobody is ever told.
- **Same loss for every source `try_next` drains mid-stream:**
  - R79 immediates and the starvation batch from `at_round_boundary` (harness.rs:1628). `read_now` has already marked them delivered and set `deliver_subagent_run` in the store, so a restart never brings them back.
  - Monitor firings: `monitor_cursor` is already advanced (1646).
  - Parent messages.
  - AnswerTodo lines: the board already holds the answer, but the model is never told.
- **Authorisation side effect:** the operator's words are already in the trail (`t.say(Speaker::Operator, …)`, 1522). The authorisation trail can then cite words the model never read.
- **Fix:** build `steering` once per round, outside the retry `loop`, so handed-back messages survive into the next attempt. On a final failure, push what is left back onto the hub or into the next turn.

##### 2. A fork registers the new transcript before its rows are written; a crash or error mid-carry resumes onto a partial conversation (HIGH × medium) [confirmed for the write order; resume reading the newest transcript is from the store's own doc at store.rs:2862–2878]
- **Where:** `harness.rs:10464` (`put_fork`), `10483` (`set_prefix_fingerprint`), `10498` (`set_stale_prefix(None)`), all before `engine.open` (10502), `next.append_items` (10631) and the chunked `persist_reporting` (10685–10699).
- **Sequence:**
  - A `/reseat` carries 971 rows in chunks for minutes, or waits up to 30 minutes on a busy store.
  - If the daemon is killed or crashes during that, `current_transcript_id` (newest, `created_at DESC, rowid DESC`) is the partial `#tN`. The next open resumes onto a truncated or empty conversation.
- **Error after `put_fork` but before the swap (10505, 10631):**
  - The harness carries on in memory on the old transcript.
  - The store's newest transcript is the dead fork, so the next restart loses everything written since.
  - The fingerprint and stale flag were already cleared, so the head's reseat offer disappears although the session is still on the stale prompt.
- **Error in the persist after the swap:** `reingest` returns at `let mut out = out?;` (9880) before `self.prefix = next` (9892).
  - Metered turns then send the OLD schemas against the new transcript, the exact mismatch the comment at 9884 warns about.
  - The operator is told `reseat_refused … Nothing was changed`, which is false.
- **Fix:**
  - Write the transcript row (or a "complete" mark) in the same transaction as the last chunk, or have resume ignore a fork whose `item_count` is short of what it should hold.
  - Move the fingerprint and stale-flag writes after the body is stored.
  - Set `self.prefix` together with the swap.

##### 3. Job handles are reused after a daemon restart and match old waits (MED-HIGH × likely) [confirmed]
- **Where:**
  - `crates/tools/src/exec/jobs.rs:47`: `JobId::next()` is a process-wide counter that starts at `j1` in every daemon.
  - Store `job` and `job_context` are keyed `(session_id, handle)` with UPSERT. The `job` upsert keeps the old `command` when the new one is empty (store.rs:3357–3361).
- **Sequence:**
  1. A row waits on `j7`. The daemon restarts.
  2. The restart notice says the wait is RELEASED (restart.rs:478).
  3. The session's next job mints `j7` again.
  4. All of these now see an unrelated job as the waited one:
     - `RunningJobs::is_running("j7")`
     - `job_fate` (live table first, harness.rs:13723)
     - `delivery::names` (delivery.rs:247)
     - the `when: job` condition
  5. The row stays quiet until that job ends, then fires as `immediate:needed` on the wrong result.
- **Store side:** the new `j7` row starts with the previous life's command, and the old row's history is overwritten.
- **Fix:** make the handle unique across restarts (instance prefix, or seed the counter from the store's maximum), or keep the instance on `TodoNeed::Job` and compare it.

##### 4. An interrupt is followed at once by a turn the operator did not ask for (MED × high) [confirmed path; I did not check which state a stopped child reports]
- **Where:**
  - `note_turn_ended` (harness.rs:10779, 5481) runs however the turn ended.
  - `JobWatchers::turn_ended` (jobwatch.rs:660–671) rings the session's wake whenever anything is held.
  - The interrupt arm runs `stop_children_first` (harness.rs:1535). The stopped children then settle (`watch_task` → `Failed` or `Done`, jobwatch.rs:1354–1360).
- **Sequence:** the operator presses stop → the turn ends → the held batch, or the children's settlements arriving after `ended_at` (rule `idle`), ring the wake → `Harness::wake` starts a fresh `[meanwhile]` turn. The model may pick the work back up seconds after it was stopped.
- **Failed turns too:** a provider-failed turn gets the same follow-up turn, which fails again.
- **Fix:** have the harness tell `turn_ended` whether the turn was interrupted. If it was, ring nothing and mark held and new settlements to ride the next operator-started turn (`Rule::Batch`).

##### 5. A job result not yet delivered when the daemon stops is never delivered (MED × medium) [confirmed]
- **Where:** `restore_after_restart` only collects jobs with `state == "running"` (harness.rs:8784–8787). Jobs have no `delivered` column; subagents do (`subagent_run.delivered`).
- **Sequence:** a job settles during a long turn and is held under R79 (up to the 10-minute cap, or until the turn ends) → the settle is recorded (`exited 0`) → the daemon stops → after restart the job is neither running nor delivered, so the model never hears its result. R79 makes this window much wider than it used to be.
- **Fix:** add a `delivered` flag to `job` (set in `read_now`, as `subagent_delivered` is), and include "settled, not delivered" jobs in the restart notice.

##### 6. Store writes that skip the busy retry, or still kill the turn (MED × medium) [confirmed]
- **No retry at all:** every write in `crates/tokencore/src/store/subagents.rs` uses `self.conn.execute` directly, not `retry_sql` or `write_tx`:
  - `put_subagent_run` (167), `settle_subagent_run` (201), `interrupt_subagent_run` (212), `deliver_subagent_run` (222)
  - `put_monitor` (267), `end_monitor` (281), `put_job_context` (316)
  - `life_*` (355/366/375)

  Each gets one 30 s busy-timeout attempt, which busy.rs calls "a single attempt of a lottery". What each failure costs:
  - `deliver_subagent_run` fails → the result is delivered a second time after restart (`Decision::Deliver`).
  - `interrupt_subagent_run` fails at an orderly stop → `resumes` is not reset, so after three of those the child is abandoned (`RESUME_LIMIT`).
  - `put_subagent_run` fails at spawn → the child can never be restored.
  - `life_stopped` fails → the stop time is estimated from the last heartbeat.
- **Wrapped, but a busy store still ends the turn after 60 s instead of waiting the 30 minutes:**
  - `self.persist_context(..)?` (harness.rs:11561): the context-size row, a measurement, fails the whole turn.
  - `self.flush_todos()?` (11791).
  - `put_fork` / `set_prefix_fingerprint` / `put_stable_prefix` in forks.
- **Fix:** route the subagents.rs writes through `self.exec` / `write_tx`. Make the context write non-fatal (warn). Give the todo flush the same patience loop `persist_reporting` has.

##### 7. A todo write that fails is never retried (MED × medium) [confirmed]
- **Where:** `flush_todos` (harness.rs:12478–12487) sets `self.todos_version = v` before `put_todos`. If the write errors (busy past 60 s), the next flush sees the same version and returns `Ok` without writing.
- **Consequence:** the store's board stays stale until the board changes again. A restart then reverts it: the operator's AnswerTodo, `wait_on` edges and consumed conditions are lost, and conditions fire again.
- **Fix:** set `todos_version` only after the write succeeds.

##### 8. The restore threshold measures the daemon's downtime, not how long the work has been stopped (MED × medium) [confirmed]
- **Where:** `back_ms = self.session_registry.started_ms()` (harness.rs:8762) is fed to `restores()` (8831) and to the monitor TTL check (8885).
- **Sequence:** the daemon restarts 5 minutes after the stop → the operator `/resume`s the session 4 hours later (lazy open) → it reads as "down 5m" → subagents are resumed on a tree that has moved, and monitors are re-armed with a fresh full TTL ("TTL starts again from now"). The operator's rule was "more than 1 hour after the stop".
- **Fix:** measure from the stop to now (the open time), and close a monitor whose TTL ran out before the open, not just before the stop.

##### 9. Restore and `task_resume` treat any other daemon instance as gone (MED × low–medium) [suspect: I did not confirm that a daemon for another folder can open the same session]
- **Where:**
  - harness.rs:8771–8781 filters on `r.instance != me` only.
  - `subagent_record::decide` (263) and `resumable` (355) refuse only for `instance == me`.
  - The single-instance `flock` is per folder (sessionlog/server.rs:98–130), but `sessions.db` is shared by every daemon on the box.
- **Sequence:** daemon B opens a session whose children are running under live daemon A → A's last heartbeat is about 30 s old, so the stop reads as recent → children are resumed twice in the same worktree, monitors run twice, and A's live jobs are marked `lost at restart`. `task_resume` has the same hole.
- **Fix:** treat another instance as gone only when its `daemon_life` row has `stopped_ms`, or its beat is older than a few `BEAT`s.

##### 10. During the 30-minute busy wait the whole worker is stalled and an interrupt does nothing (MED × low) [confirmed]
- **Where:** `persist_reporting` (harness.rs:12711–12745) loops `sleep_unless_closed(5s)`, which checks only `hub.is_closed()` (12631–12641).
- **What the operator sees:** a `store_busy_wait` warning every 120 s on this one session.
  - An esc-esc on it is not acted on for up to 30 minutes.
  - Every other session on the single worker waits too, and their heads show only "queued" with no reason.
  - Patience is counted from the start of `persist_reporting`, not the start of the busy wait, so a long carry has less of it.
- **Fix:** let an interrupt end the wait (rows stay in memory and are written later), tell every other affected session's head why it is waiting, and count patience from the first busy failure.

##### 11. A wake marks results delivered before the turn that delivers them can fail (LOW-MED × low) [confirmed]
- **Where:** `Harness::wake` calls `between_turns` (which does `read_now` → store `delivered`, and empties held) and drains `restart_notices`, at 8631–8641/8569. Only after that does it call `submit_item` (8691).
- **Failure point:** `submit_items` can fail in `append_items` or `persist` (10774–10776) before `run_rounds`. The text is then lost, and `note_turn_ended` is never reached.
- **Fix:** do what the function already does for fired conditions — mark delivered only after `submit_item` succeeds, and put results back into held on failure.

##### 12. The held-results counter is wrong after an attach or reconnect (LOW-MED × high, display only) [confirmed]
- **Where:**
  - `HeldResults` is ephemeral (scrub.rs:102) and in no snapshot.
  - The daemon publishes only when the count changes (`held_said`, jobwatch.rs:768–784).
  - The head keeps its last value (tui/src/app/events/turn.rs:44).
- **Effect:**
  - A head that attaches mid-turn shows nothing held while results are held.
  - A head that reconnects after missing the zero keeps showing a stale "held: 2 jobs" on every later turn's status row.
- **Fix:** republish the current count on `HeadAttached`, or put it in the snapshot or settings row, and clear `held` on TurnFinished / TurnFailed / reconnect.

##### 13. Reseat side effects happen before the reseat is known to succeed (LOW × medium) [confirmed]
- **Where:**
  - `reseat_tight` "re-seated verbatim at …" is published (harness.rs:9805–9819) before the fork, which can still fail.
  - `reseat_target` changes `self.cfg.system` (9655) and writes a `stable_prefix` row (9683) even when the reseat is then refused as Over (9780).
- **Effect:**
  - A message saying the reseat happened, for a reseat that failed.
  - An orphan prefix row for every refused attempt.
- **Fix:** publish the warning after the fork succeeds, and write the prefix row only once the fit check has passed.

##### 14. An interrupt arriving at the end of a successful round is dropped (LOW × low; in `crates/turn`, next to this scope) [confirmed]
- **Where:** turn/src/engine.rs:1229–1250 (local) and 1614–1631 (cloud). `pending.absorb` → `hold_urgent(u)` → `return Ok(...)`. `Pending` is local and does not survive the return, and nothing hands the urgent back.
- **Effect:** an esc-esc between the last streamed piece and the end-of-round check stops the children (`stop_children_first` already ran when it was taken) while the parent carries on.
- **Fix:** give the held urgent back to the source on the Ok path, or return it in `TurnOk`.

##### 15. A child can answer its parent and then run another turn (LOW × medium) [suspect]
- **Where:** a child's turn that ends with held results rings its own wake (jobwatch.rs:669 → `serve_child` `OwnWork::Wake`, harness.rs:15316).
- **Sequence:**
  - The child writes its final answer without results that were held mid-turn.
  - The parent receives it.
  - The child then runs a `[meanwhile]` turn whose work goes nowhere the parent hears.
- **Fix:** for a child, deliver held results at its turn end before the answer is final (keep checking once more), or do not hold results for children.

##### 16. The R79 sort runs on every streamed piece of output (LOW × certain, cost only) [confirmed]
- **Where:** `Pending::drain` calls `try_next` on each delta. `try_next` → `at_round_boundary` → `delivery::sort` copies the whole todo board (todo.rs:746), takes and puts back the held set, and checks starvation — per piece of streamed output, not per round.
- **Fix:** run the R79 sort only at real step boundaries (an argument, or a flag the engine sets).

##### 17. The `wanted` map leaks entries (LOW × medium) [suspect]
- **Where:** `JobWatchers::delivered` removes an entry only on immediate delivery (delivery.rs:286).
- **Sequence:** a `job_wait` / `want_now` for a job already delivered, or a want recorded after its result was batched, stays in the map for the session's life.
- **Fix:** also call `delivered()` in `batch` / `between_turns`, and skip `want_now` for handles already in the `settled` set.

##### 18. Closing a root session throws away held and queued results (LOW × low; may be intended) [confirmed]
- **Where:** `JobWatchers::stop` (jobwatch.rs:868–886) takes held and the queue, and returns when there is no parent. `Sessions::close` calls it (sessions.rs:2595–2598).
- **Effect:** the subagent rows say this daemon's instance and not delivered, so nothing redelivers them until a different daemon opens the session.
- **Fix:** leave `delivered = 0` but stamp the run so the next open in the same daemon redelivers, or say on the hub what was dropped.

---

**Checked and found sound:**
- **R79 ordering:**
  - A result arriving at turn end: the queue rings when the result is pushed, and the bell keeps one pending ring per session.
  - An operator prompt racing a wake: commands are served before wakes, and the batch goes in ahead of the prompt.
  - Compaction does not count as a turn ending (it does not run `run_rounds`).
  - A wake for a child is not lost while the child is mid-turn: the `own_wake` flag is sticky.
- **`wait_on`:**
  - It refuses a job that has already ended.
  - A job that ends between the check and the board write is picked up as `needed` at the next sort.
- **Restore:**
  - A subagent restored after restart keeps its id.
  - A subagent whose worktree was removed is abandoned with the reason.
  - Reopening the same session in the same daemon does not restore twice (rows are re-stamped, and the in-memory slot check covers a failed re-stamp).
  - Over the threshold, undelivered settlements are still delivered.
- **Reseat:**
  - Reseat runs only between turns, behind any running turn, and the turn after it picks up held results.
  - Two heads pressing reseat: the second is refused with "nothing to re-seat".

### Lifecycle, jobs, the queue and worktrees (R92)

#### Review of session lifecycle, jobs, queue and worktrees — letibot @ 50b3e55 (read-only)

I read the code only. Nothing was edited, built or signalled. Git checks were read-only (`worktree list`, `status`, `merge-base`, `prune --dry-run`). I covered the lifecycle and the git state myself and gave subagents/jobs and queue/gate to two read-only sub-reviewers. Their findings are merged in below, with their file:line references.

CONFIRMED means the whole path was read, or the fact was seen on disk. SUSPECTED means the code points that way but the path was not fully traced. The list is ranked by how often the operator will hit each one.

---

##### A. Most frequent

**1. Jobs in a firecode session are never watched, so a finished job is announced only on the next turn. CONFIRMED.** This is the "firecode job sat finished until I typed" report.
- **Path:**
  - `harness.rs:3939-3942` builds the job watcher from `backend.processes_arc()`.
  - `FirecodeBackend` (`tools/src/firecode.rs:737-860`) implements `processes()` (856) but not `processes_arc()`, so it gets the default `None` (`tools/src/backend.rs:152`).
  - `JobWatchers::watch` (`jobwatch.rs:917-962`) then drops the handle ("nothing here can ever settle this handle").
- **Effect:**
  - No `JobSettled`, no `JobCompletion`, no wake.
  - The store row from `record_start` stays `running` for ever, and the jobs pane never settles.
  - `wait_on` / `needs: job` rows are never delivered.
  - The completion surfaces only when the model calls `job_wait`/`job_list`, i.e. after the operator types.
- **Should:** implement `processes_arc()` on `FirecodeBackend`, and make the trait method non-defaulted so the next backend cannot forget it.
- **Related, CONFIRMED (`harness.rs:~18186-18203`):** a VM child's backend is torn down right after its first turn. Any background job it left running dies with the VM, and nobody is told.

**2. Slow start: the daemon binds the socket before it is ready to work, and the head shows no sign that it is still booting. CONFIRMED.** This is the rano "took ages and showed nothing but the queued prompt" report.
- **Ordering in `harnessd/src/cli.rs`:**
  - `create_under` (1066) and `Daemon::serve` (1077) come first ("Bind before opening the session").
  - Then come: `open_seat` (flowy, network, 1104), and `served_model` / `served_media_marker` / `served_ctx` (1370, ~1458-1466). These are HTTP calls with a 10 s connect and a 3 min read timeout (`turn/src/serving.rs`).
  - Then `Parts::load` (vocabulary), then `Sessions::open_first_with_seat` (resume of the whole transcript, 1476).
  - Only after all of that does `daemon.run` start the worker (1766).
- **What the launcher does:** `scripts/letibot:1932` polls only "is the socket listening". That is true almost at once, so it `exec`s the head.
- **What the operator sees:**
  - The head attaches (the hub already exists) and shows the transcript.
  - A prompt typed now sits in the hub queue, shown as queued, until boot finishes.
  - Nothing says "daemon is starting: loading vocabulary / resuming N rows / probing model server". A grep for any boot or progress state in harnessd, sessionlog and tui found none.
- **Should:** publish a "starting: <step>" status on the hub during boot, and a line on the queued prompt saying it will run when the worker starts.

**3. If the daemon fails during boot, the head never sees why. CONFIRMED.**
- `open_first_with_seat` failing (cli.rs ~1478) or `--flowy` failing (1104-1108) calls `daemon.shutdown()` and returns `Err`. The reason goes only to the daemon's stderr.
- That stderr is `~/logs/harnessd.log`, **one file shared by every folder's daemon** (`scripts/letibot:268`, `detach … >>"$LOG"`).
- The launcher has already exec'd the head, so the operator sees the connection close without the reason.
- The launcher's own failure path (`scripts/letibot:1933`, `tail -15 "$LOG"`) can show another folder's daemon's lines.
- **Should:** publish the error on the hub before `shutdown`, and give each folder its own log, or prefix lines with the workspace.

**4. Every finished subagent worktree looks dirty, so nothing ever cleans it up. CONFIRMED on disk.**
- **Cause:** the daemon writes its own logs into the session workspace, which for a child is the worktree:
  - `.letibot/delivery-log.jsonl` (`delivery.rs:91,464`),
  - `.letibot/context-claims.jsonl` (`context_claim.rs:568`),
  - `.letibot/notes-offers.jsonl` (`notes_offer.rs:331`).
  - The child's own notes land in `.letibot/notes/`.
- `.letibot/` is neither gitignored nor in `info/exclude` (the main repo tracks `.letibot/notes/*`). So `git status --porcelain` shows `?? .letibot/`.
- `retire_branch` (`mergequeue/git.rs:470-488`), the only cleanup (called from `mergequeue/serve.rs:1296` after a landing), refuses any dirty tree. It logs only an `eprintln` to the shared daemon log.
- **Measured:**
  - 27 of 31 checked worktrees have `.letibot/` (mostly `notes/`, some `notes-offers.jsonl`).
  - 20 of the 37 `agent/*` worktree branches are already merged into main and still have worktrees. The sub-reviewer counted 46 of 64 `agent/*` branches merged.
  - `.claude/worktrees` is **240 GB**: 34 per-tree `target/` dirs of 3.7–7.9 GB each.
- **Knock-on effects:**
  - `EndOfWork` nudges the child about files the daemon wrote (`harness/deliverable.rs:166`, `harness.rs:11655`).
  - The child may `git add -A` the logs onto its branch.
  - The child's notes are stranded in an untracked directory.
- **Should:**
  - Write the daemon's logs to the main tree or the data directory, not the workspace, or add `.letibot/*.jsonl` to the common `info/exclude`.
  - Decide where a child's notes go.
  - Have `retire_branch` ignore known daemon files.
  - Raise the "kept" reason on a hub.

**5. The queue standings on the bottom edge are stale, count every repository, and count only red. CONFIRMED.**
- `tui/src/ui/panes/queue.rs:117-139` draws from `self.queues`. That is refreshed only when the pane opens or is drawn, or on `QueueMoved` while the pane is open (`surface.rs:294`, `events/todos.rs:41`).
- **Operator sees:**
  - `‼ 1 red target(s)` stays on every session's edge after main is green.
  - It counts other repositories' targets.
  - An unjudged or timed-out main raises no edge alarm.
- **Should:** refresh on `QueueMoved` even with the pane closed, filter by the session's repository, and count `blocked`/`timed_out` as alarms.

**6. Nothing ever says "main is green again". CONFIRMED.**
- `red_card.rs:32` states it. `target_is_green` (`serve.rs:883`) only withdraws reviews, and `queue_emergency_notices` (`harness.rs:8232`) only reports trouble.
- **Operator sees:** the transcript and the project model's last word stay "EMERGENCY… nothing lands" after a fix lands.
- **Should:** one recovery warning and session notice per red→green pair.

**7. A daemon restart repeats every alert and wakes dormant sessions. CONFIRMED.**
- The dedupe sets are in memory only: `RepoQueue.rung` (`serve.rs:145`) and `Harness.gate_told` (`harness.rs:1976`).
- `regate_at_start` (`verbs.rs:540`) re-gates the red tip, so "EMERGENCY — main is RED" is told again.
- `ring_project` (`mergequeue.rs:366-391`) calls `registry.create` to resume and wake old root sessions for every blocked or cannot-decide review.
- `gate_killed` and `gate_setup_failed` re-raise on every re-gate.
- **Should:** persist what was told or rung, per sha/state or row/state.

**8. Reviews freeze while any gate runs in that repository. CONFIRMED (structural).**
- `RepoQueue::pass` (`serve.rs:224-279`) runs upkeep, then health, then land on one thread. The target gate (`serve.rs:724`) and the landing gate (`serve.rs:1122`) are synchronous and can take up to 30 minutes.
- During that time there are no claims, reviewer deadlines are not enforced (`serve.rs:399-445`), and capacity holds are not rechecked.
- **Operator sees:** reviews sit `waiting` with no reason given.

**9. The single daemon worker thread runs a root session's turn inline, so other work starves. CONFIRMED structure; impact SUSPECTED.**
- `daemon.rs` `run`: `Work::Command` goes to `sessions.dispatch`, then `run_prompt`, then `Sessions::turn`, all on the worker thread (`sessions.rs:2036-2119`).
- Nags, abandoned-call sweeps, child parking and `serve_reviews` / `ensure_gatekeeper` run only in the `WorkOrIdle::Idle` arm. That arm fires only when no work arrives before the idle deadline.
- **Effect:**
  - A long turn in one root session delays prompts to other root sessions in the same daemon, and delays all of the Idle tasks.
  - A slow `Work::Open` (resuming a big session) blocks the same way.
- This may also contribute to "why a queued message waits".

---

##### B. Frequent

**10. A child's later turns never reach the parent. CONFIRMED.**
- The slot settles once, at the end of the child's first turn (`harness.rs:~18210`; `task.rs:370-380`). Later turns run silently (`serve_child_with`, `harness.rs:15260-15470`).
- **Sequence:** the child ends with "waiting on j97". The parent takes that as the answer. The child later finishes and commits, and the parent never hears.

**11. The branch is enqueued right after the child's first turn, whatever state it is in. CONFIRMED.**
- `enqueue_finished` (`harness.rs:18203` for VM children, `18224` for host children) runs even with jobs still running, a dirty tree, or head == base.
- `mergequeue/review.rs:631-685` does `branch_head(...).unwrap_or_default()`, so a deleted branch is enqueued with an empty `head_sha`.
- The enqueue is idempotent by id, so commits from later turns are never re-enqueued. SUSPECTED: whether the gate re-reads the head.

**12. The main checkout carries the `.cargo/config.toml` trap that `.gitignore` warns about, and every nested worktree inherits it. CONFIRMED on disk; the cargo behaviour is standard config discovery.**
- `~/Projects/letibot/letibot/.cargo/config.toml` contains `paths = ["/home/dead/Projects/rano/rano"]`.
- `.claude/worktrees/*` is nested under that checkout, and cargo merges config from every parent directory. So every subagent worktree build silently uses the local rano checkout instead of the locked `v0.8.3` tag. The local checkout is currently `v0.8.3-1-gde2ae1f`, one unreleased commit ahead.
- The landing worktree (`~/.local/share/letibot/landing/…`) and VM clones are **not** nested, so they build the real tag. That gives "builds in the subagent, fails or differs in the gate".
- It goes silently wrong the moment the rano checkout moves to a different version (the 2026-10-08 incident in `.gitignore`).
- **Legacy files:** 31 worktrees also hold a per-tree `.cargo/config.toml` with `target-dir = ".../letibot/letibot/target"` ("Written by `task_start`"). The sub-reviewer reports the runner no longer writes these; the build env now comes from `worktree_build_env` (`harness.rs:19012`). They are gitignored, so they do not count as dirty.
  - **Inferred, not measured:** these files point at the main target. An explicit `CARGO_TARGET_DIR` overrides them, but a build without that env (a hand-run cargo in the tree) shares and locks the main target.
- **Should:** delete the main checkout's `.cargo/config.toml`, and have cleanup remove the legacy per-tree files.

**13. Job ids restart at j1 after every daemon restart. CONFIRMED.**
- `JobId::next()` is a process-global counter (`tools/src/exec/jobs.rs:44-48`), never seeded from the store.
- The store `job` table is upserted by `(session_id, handle)` (`tokencore/src/store.rs:490-501`, 3352), so the new j97 overwrites the old one's history.
- `job_fate` (`harness.rs:13719-13730`) and `delivery::names` (`delivery.rs:249`) match `when/needs: job j97` written before the restart to the new j97.
- In the TUI, "waits on j97" links (`tui/src/app/todos.rs:763-860`) can open the wrong job's output or slug.

**14. A job slug used in `needs` reads as "already ended". CONFIRMED.**
- `todo_write` stores the raw handle (`tools/src/builtins/todo.rs:3437-3449`); only `wait_on` resolves slugs (`todo.rs:3114`).
- `job_fate` looks up `JobId(slug)`, gets `Unknown`, and treats it as ended (`harness.rs:13568`). The row unblocks while the job runs.
- Slugs are unique only per daemon life (`exec/host.rs:1139-1160`).

**15. Worktree debris: nothing cleans up beyond a clean landing. CONFIRMED on disk.**
- **Registered vs on disk:**
  - `git worktree list`: 48 entries. `.claude/worktrees`: 62 dirs.
  - 24 dirs (`agent-a<hex>`) have a `.git` file pointing at `/home/dead/Projects/letibot/.git/worktrees/…`, which does not exist. These are orphans from before the repo moved down a level, and git cannot see them.
  - 3 registered worktrees (`agent-a8e2db02…`, `agent-ac359f48…`, `agent-adff1e33…`) are **locked** at paths under `/home/dead/Projects/letibot/.claude/worktrees/` that no longer exist. `prune` will not touch locked entries.
  - Also `wt-drill`, `wt-jobs`, `wt-visibility`, `wt-yellow` sit outside `.claude`.
- **Dirty worktrees, 5 of them:**
  - `agent-agents-md` is mid-merge with a `UU` conflict in `standing_notes.rs` (2 days old).
  - `agent-operator-input` has 5 modified, uncommitted TUI files (4 days old). This is a subagent that finished without committing.
  - `agent-store-busy` has a modified `.commit-msg`.
  - `agent-why-a-queued-message-waits` has an untracked test file.
  - `agent-quiet-shell` has `.scratch/`.
- **Cleanup coverage:** nothing removes worktrees of children that failed, were refused, were killed or are dirty. `retire_branch` is the only path.

**16. A firecode `task_start` child is given a deliverable it cannot meet. SUSPECTED.**
- The host worktree is created and the brief says to commit to `agent/<slug>` (`harness.rs:16725-16780`, `18120-18135`). But the VM works on a copy (`task.rs:52-65`).
- So the host tree always reads clean, and an empty branch is enqueued.

**17. Slug collisions and unvalidated slugs. CONFIRMED.**
- **Derived slugs collide:** `slug_from_prompt` (`task.rs:299-328`) takes the first 4 ASCII words. Briefs that begin alike collide, and non-ASCII prompts all become `task`. The second spawn is refused "already exists" (`task.rs:1131-1153`, `harness.rs:16731-16737`). The `-2` suffixes on disk show this has been worked around by hand.
- **A stale branch or registration blocks the slug:** with the directory gone but the branch or registration left, `worktree add -b` fails with no hint to prune (`harness.rs:16744-16752`).
- **User slugs are only trimmed** (`task.rs:1098-1102`):
  - `/` nests directories and can clash with a ref.
  - `..` escapes `.claude/worktrees`.
  - Spaces give an invalid branch name.
  - On disk, `agent-agent-refresh` shows the prefix doubled.

**18. The design rule "red main halts both queues and returns items to review" is not what the code does. CONFIRMED.**
- `serve.rs:465` and `803` keep reviews and enqueueing going while red (the "relaxed rule").
- `land_next` (`serve.rs:918-923`) just leaves rows `queued`, and their evidence stays the approval text.
- On green, they land without re-review.

**19. `queue_settings_unreadable` floods every hub about every 2 s. CONFIRMED.**
- `serve.rs:231-243` has no dedupe, at Failure class, for a malformed `[queue]` in leticode.toml.

---

##### C. Occasional

**20. Exit-and-stop against an OLDER daemon goes straight to SIGKILL and can lose the in-flight turn. CONFIRMED path.**
- The head takes the force path at once (`tui/src/driver.rs:746-764`, `ForceWhy::OlderDaemon`). The force path is `head.rs:675` `force_stop`: interrupt (3 s budget), then SIGTERM, then SIGKILL 5 s later.
- `server.rs:670` notes that daemons built before that change still refuse a mismatched `Attach`. Against those, `interrupt_turns` → `discover` is refused, the interrupt never lands, the old daemon's SIGTERM finishes its turn, and SIGKILL ends it after 5 s. The comment's promise that "no turn's rows are lost" does not hold for those daemons.
- **The operator does see why:** "the interrupt did not go through (…)".
- **Should:** when the interrupt cannot be delivered, say before signalling that the running turn's last rows may be lost, and offer to wait instead.

**21. In `interrupt_turns`, the "N turn(s) still running" message can never be produced. CONFIRMED.**
- `head.rs` `interrupt_turns`: once the deadline passes it calls `ask_sessions(&mut client, &rx, deadline)` with a deadline already past. `ask_sessions` returns `Err("timed out waiting for the session list")` immediately.
- The `?` inside the polling loop does the same near the deadline.
- **Operator sees:** `letibot --stop` and the head's force path report "timed out waiting for the session list" instead of "2 turn(s) still running after 30s".
- **Also:** plain `--stop` sends the interrupt reason `"letibot --stop --force"` (hard-coded), so the abort row names the wrong command.

**22. The quit card under-counts what a stop kills. SUSPECTED.**
- `tui/src/ui/cards/quit.rs` `quit_view` counts `self.heads - 1`, this session's jobs and this session's subagents.
- Stopping the daemon also kills the other sessions' turns and heads, the gatekeeper's reviews and any merge-queue gate (`merge_stop`). None of these are counted.
- **Should:** ask the daemon for its totals across sessions before offering "stop daemon".

**23. Launcher stop edge cases. CONFIRMED.**
- `letibot --stop --force 3` ignores the `3` and stops **this** folder's daemon. `scripts/letibot:1514` skips `retarget` when `$2` is `--force`. `--stop 3 --force` works.
- `--stop --all`:
  - It never sends `--interrupt-all` and never escalates with `--force`.
  - A busy daemon finishing a turn is reported as "ignoring SIGTERM after 5s", which is the wrong reason.
  - It advises `letibot --stop --force`, which acts only on this folder.

**24. Nothing checks that the workspace still exists. CONFIRMED (no check); SUSPECTED effect.**
- `retire_branch` force-removes a worktree with no check that its child is alive. A host child stays reachable for 10 minutes and a parked child reopens on the next prompt (`config.rs:1739`).
- A grep for any workspace-existence check in harnessd and tools found none. Tool spawns `current_dir` into the missing directory (`exec/host.rs:1353`), so every call fails with a bare ENOENT.
- `Deliverable::uncommitted()` returns `None` when git fails (`deliverable.rs:59-75`), so the completion treats the gone tree as clean.
- The daemon's own log writers `create_dir_all` the `.letibot/` dir back into the removed path.
- **Should:** refuse to retire a live child's tree, and say "workspace gone" on open or wake.

**25. Gate killed at its memory cap: it is unjudged and named correctly, but retries are "never, or every restart". CONFIRMED.**
- **Target side:**
  - `gate.rs:357-399` classifies it as `Blocked` with `GATE_KILLED`, and the TUI shows "UNJUDGED · gate killed at its memory cap". That part is good.
  - It is not retried in the daemon's life (`serve.rs:676-681`), but it is retried on every lease take (`verbs.rs:540`) with no backoff. With the same cap it dies the same way, and the notices repeat.
- **Merge side:**
  - A killed landing gate is `Blocked` until `/queue restart` (`serve.rs:1175-1186`).
  - Its text says "could not run", but it ran and was killed.
  - The advice names `regate`, but the verb for a branch row is `restart` (`gate.rs:77`).

**26. A memory kill not at the cap, or with no cgroup, is judged RED. CONFIRMED path.**
- `red_cause` (`gate.rs:207-228`) only counts a signal on the step leader.
- A box-wide OOM or oomd kills a rustc, and cargo exits 101 with "(signal: 9, SIGKILL". That becomes `RedCause::Code`: RED, an emergency, a red-tip review and possibly a fix branch.

**27. A timed-out target, and several blocked causes, raise nothing on any hub. CONFIRMED.**
- `serve.rs:764-781` handles `TimedOut` in the `_ => {}` arm.
- "the gate could not be read", "landing worktree could not be made ready" and non-`GATE_KILLED` `Infra` are not in `emergency_health` (`serve.rs:1308`).
- They freeze landings until a restart, regate or new commit, and show only as a ⚠ line in the pane.

**28. Merge-row failures tell nobody, and parked merge rows never settle. CONFIRMED.**
- A rebase conflict (`serve.rs:1050-1063`) and a gate failure (`serve.rs:1145-1153`) raise nothing. `ring_the_projects` (`serve.rs:288`) looks at reviews only.
- `settle_if_needless` covers review rows only, so a merge row whose branch was landed by hand stays parked for ever.

**29. Red-tip reviews survive green. CONFIRMED.**
- (a) `target_is_green` withdraws only `is_open()` rows (`queues.rs:248`). `CannotDecide` and `Rejected` rows remain, and `CannotDecide` is re-rung after each restart.
- (b) A landing sets Green directly (`serve.rs:1281-1296`) without calling `target_is_green`. A waiting red-main review for a later sha can still reject, which opens a fix branch and raises "EMERGENCY" while main is green.

**30. Alerts reach only the lease-holding daemon's sessions; some reach every repository. CONFIRMED.**
- `announce_to` / `publish_for_repository` / `ring_repository` (`mergequeue.rs:355-430, 513`) use this daemon's registry only. A second daemon in the same repository (for example one seated in a worktree) gets nothing.
- `queue_lease_lost`, `queue_wrong_build` and the store-error warnings use the global `announce`, so they reach every repository's hubs.
- `publish_for_repository` falls back to the gatekeeper hub alone when `Store::open` fails.
- **Repository identity:** it is the canonical path of the parent of `git-common-dir` (`git.rs:116-174`). Worktrees map to the main checkout. Remote URLs are not used.
- SUSPECTED: a non-repo directory with exactly one repository child counts as that repository (`git.rs:155-166`).

**31. The same sha going red twice is silent. CONFIRMED.**
- The review id is `red-<target>-<sha12>` with INSERT OR IGNORE (`queues.rs:807`). The withdrawn row stays withdrawn, `fresh=false` (`serve.rs:862`), and the in-memory `rung` / `gate_told` keys are already set.

**32. Reviewer start failures are handled inconsistently. CONFIRMED.**
- A synchronous non-capacity error (for example a `clone_branch` failure) blocks on the first strike (`review.rs:261-280`), while an asynchronous one gets 3 attempts.
- "No reviewer started within 5 min" blocks on the first strike (`serve.rs:402-411`), and blames the review session even when `ring_reviewers` silently no-op'd because `s-gatekeeper` was not in the registry (`mergequeue.rs:359-364`).

**33. Push failure and install failure are mislabelled. CONFIRMED.**
- A push failure with no install step raises nothing; the raise is inside `if !install.is_empty()` (`serve.rs:1235-1265`).
- An install failure after a good landing is raised as `queue_blocked`, though nothing is blocked (`serve.rs:1251`).

**34. VM-capacity waits mostly name the real reason. CONFIRMED, minor gaps.**
- The evidence says "4 of 4 VM slots are busy — no attempt is counted…" (`notices.rs:360`), and it is deduped durably.
- **Gaps:**
  - The overlay shows `failure` only when attempts > 0 (`queue.rs:378-384`).
  - The evidence is overwritten on re-claim.
  - Only firecode's tap-slot wording is recognised (`capacity.rs:67-71`); any other capacity refusal is counted as an attempt and blocks after 3.

**35. The `wake: later` sentence tells the model the wrong thing. CONFIRMED.**
- `tools/src/builtins/bash.rs:~308-318` says a held result "never starts a turn of its own".
- The delivery rules contradict it (`delivery.rs:27-48`, `jobwatch.rs:660-671`, `harness.rs:1448-1491`). A model that believes it may poll, or wait to be prompted.

---

##### Checked and fine

- **Protocol skew:** the daemon accepts a mismatched protocol and says so (`server.rs:670`). The head locks sends against an older daemon, with a `ctrl-^` override (`app/session.rs:695-760`). The launcher probes before attaching and refuses with an explanation (`scripts/letibot:1142, 1961`). The messages name the direction.
- **Two daemons for one folder:** the folder lock, `live_detached` / `refuse_detached` and `wait_gone` cover the stop/start race (`scripts/letibot:1086-1122, 1870-1910`).
- **Restart notes** (`restart.rs`): a crash estimates the down time from the last heartbeat, which can be up to 30 s early. An unknown stop is treated as over the threshold and said so.
- **Root-session job completion** is pushed, not left to the next input: `SettlementQueue` → `ring_wake` → `Work::Woken` → `between_turns`. Late announcements come from findings 1 and 10.
- **Not reviewed in depth:** the stale-prompt / reseat card path and two heads on one session. Grep shows explicit multi-head handling; I found no defect there.

### Lifecycle, second hand-back — the same review re-ordered, with a few details the first lacks (R92)

#### letibot @ 50b3e55: session lifecycle, subagents/jobs and queue edge cases (read-only review)

I edited nothing, built nothing, and did not touch any process or sessions.db. Part 1 and the git state are my own reading of the code and the disk. Parts 2 and 3 were reviewed by two read-only subagents. I re-checked their two headline claims myself (finding A1 and the job-id counter in A5); everything else is their work, labelled as they marked it.

**Confirmed** means the whole code path was read, or the state was seen on disk. **Suspected** means the code points that way but not every step was traced. Findings are ranked by how often the operator hits them, across all three areas.

---

##### A. Hit daily

**A1. A job in a firecode session is never watched, so its finish is only seen when someone asks. Confirmed (this is the firecode report).**
- **Where:**
  - `harness.rs:3939-3942` builds the job watcher only when `backend.processes_arc()` returns a host. Otherwise it builds `JobWatchers::watching_tasks`, which has no host.
  - `FirecodeBackend` (`tools/src/firecode.rs:737-860`) implements `processes()` but not `processes_arc()`. It falls back to the trait default, `backend.rs:152`, which returns `None`.
  - `JobWatchers::watch` (`jobwatch.rs:917-962`) then drops the handle ("nothing here can ever settle this handle").
- **Sequence:** the model runs `bash --background` in a firecode session and the job finishes. Nothing follows: no `JobSettled`, no `JobCompletion`, no wake.
- **Operator sees:**
  - The finish only shows up on the next turn, when the operator types and the model calls `job_*`.
  - The job's row in the store stays `running` for ever, and the jobs pane never settles it.
  - `wait_on` / `needs: job` rows are never told the job is done.
- **Related, confirmed (`harness.rs:~18186-18203`):** a VM child's backend is shut down right after its first turn. Any background job it left running dies with the VM, and nobody is told.
- **Should:** `FirecodeBackend::processes_arc` returns its `Arc<HostProcesses>`. Better, remove the trait default (or derive it from `processes()`) so the next backend cannot forget it.

**A2. While the daemon boots, the head is attached to a live-looking session that cannot run anything: the "rano took ages, showed only the queued prompt" report. Confirmed.**
- **Where:** `harnessd/src/cli.rs:1065-1077` creates the first session's hub and binds the socket ("Bind before opening the session"). The worker loop `daemon.run` only starts at `cli.rs:1766`.
- **What runs in between, on this thread:**
  - `open_seat` (flowy, over the network)
  - `served_model`, `served_media_marker` and `served_ctx`: HTTP calls to the model server with a 10 s connect timeout and a 3 min read timeout each (`turn/src/serving.rs`)
  - `Parts::load` (the vocabulary)
  - `Sessions::open_first_with_seat` (a resume, which re-tokenises the transcript)
- **Sequence:** the launcher's `live` check passes as soon as the socket listens (`scripts/letibot:1932`), and it execs the head. The head gets its `Hello` and shows the transcript. The operator types, and the prompt is queued in the hub, but no worker takes it until `daemon.run`.
- **Operator sees:** the queued prompt and nothing else, for as long as boot takes. Nothing on the hub says "daemon still starting". A grep for any boot or progress status in harnessd, sessionlog and tui finds none.
- **Should:** publish a "starting: loading vocabulary / resuming N rows / asking the model server" status on the hub from bind until `daemon.run`. The head should label queued prompts as "queued until the daemon finishes starting".

**A3. Merged subagent worktrees are never cleaned up, because the daemon's own files make every worktree look dirty. Confirmed (on disk and in code).**
- **Where:**
  - `retire_branch` (`mergequeue/git.rs:470-488`) refuses when `git status --porcelain` shows anything at all.
  - The daemon writes into `<workspace>/.letibot/`: `delivery.rs:91,464` (`delivery-log.jsonl`), `context_claim.rs:568`, `notes_offer.rs:331`. Children also write notes to `.letibot/notes/`.
  - For a `task_start` child, the workspace is the worktree. `.letibot/` is not in `.gitignore` and not in `info/exclude`, which holds only `.claude/`.
- **On disk:**
  - 23 worktrees hold `.letibot/notes`, and 3 more also hold `notes-offers.jsonl`.
  - 20 of the 37 `agent/*` branches checked out in worktrees are already merged into main, and their worktrees are still there. The other reviewer counted 46 of 64 `agent/*` branches merged.
  - The refusal is only an `eprintln` to the daemon log (`serve.rs:1296-1299`), so the operator never sees it.
- **Knock-on effects:**
  - The end-of-work nudge (`harness/deliverable.rs:166`) tells the child about files the daemon itself wrote.
  - A child may `git add -A` the daemon's logs onto its branch.
  - Notes a child wrote are stranded in an untracked directory.
- **Should:** keep the daemon's jsonl logs out of the workspace (in the data dir), or exclude `.letibot/*.jsonl` in the common `info/exclude`. Decide where a child's notes belong. Say the retire refusal on a hub.

**A4. The bottom-edge queue standings are stale, count every repository, and only count `red`. Confirmed.**
- **Where:**
  - `tui/src/ui/panes/queue.rs:117-139` builds the edge from `self.queues`.
  - That is set only by `ServerFrame::Queues`, which the daemon sends only when asked.
  - The head asks only when the queue pane is open (`surface.rs:294`, `queue.rs:147`, `events/todos.rs:41`).
- **Operator sees:**
  - `‼ 1 red target(s)` stays on every session's edge after main goes green, until the pane is reopened.
  - Repo A's red main shows in repo B's sessions.
  - An unjudged (gate killed, setup failed) or timed-out main raises no edge alarm at all.
- **Should:** push or refresh the snapshot on `QueueMoved` even while the pane is closed, filter by the session's repository, and count `blocked` / `timed_out` as alarms.

**A5. Job ids restart at j1 on every daemon restart, so "waits on j97" can point at the wrong job. Confirmed (I verified the counter).**
- **Where:**
  - `tools/src/exec/jobs.rs:44-48`: `static N: AtomicU64 = 1`, never seeded from the store.
  - The store's `job` table is keyed `(session_id, handle)` and upserted (`tokencore/src/store.rs:490-501`, `put_job` at 3352), so a new j97 overwrites the old j97's history.
  - `job_fate` (`harness.rs:13719-13730`) and `delivery::names` (`delivery.rs:249`) match a `when` / `needs: job j97` row written before the restart to the new j97.
  - The TUI resolves the "waits on j97" link (`tui/src/app/todos.rs:763-860`) against the current list or the newest notice, so it can open the wrong job's output.
- **Should:** namespace handles per daemon instance, or seed the counter from the store's `max(handle)`.

**A6. A job slug written in `needs` reads as "already ended". Confirmed.**
- **Where:**
  - `todo.rs:3437-3449` stores the raw string. Only the `wait_on` door resolves a slug (`todo.rs:3114`).
  - `job_fate` looks up `JobId(handle)` only. A slug gives `Unknown`, which is treated as ended (`harness.rs:13568`).
- **Operator sees:** the row unblocks, and the nag talks about the job, while it is still running. Meanwhile `delivery::names` does match the slug, so the two readers disagree.
- **Also:** slugs are unique only within one daemon's life (`exec/host.rs:1139-1160`).

**A7. A child's later turns never reach the parent. Confirmed.**
- **Where:**
  - The task slot settles once, at the end of the child's first turn (`harness.rs:~18210`; `task.rs:370-380`).
  - `serve_child_with` (`harness.rs:15260-15470`) runs every later turn, and none of them notifies the parent.
- **Sequence:** the child ends turn 1 with "waiting on j97", and the parent takes that as the answer. When j97 settles, the child wakes, finishes the work and commits. The parent never hears.
- **Operator sees:** "child done" on stale text, then silence. The only way to learn more is `task_result`, which returns a live reading rather than the new answer.
- **Should:** a child turn that ends after the slot has settled pushes a second completion to the parent.

**A8. The branch is enqueued after the child's first turn, whatever state it is in. Confirmed path.**
- **Where:** `enqueue_finished` runs at `harness.rs:18203` (VM children) and `18224` (host children). It runs even when:
  - jobs are still running, or the child will commit in a later turn (see A7);
  - the tree is still dirty after the one nudge;
  - the head equals the base.
- **Also:** `mergequeue/review.rs:631-685` uses `branch_head(...).unwrap_or_default()`, so a deleted branch is enqueued with `head_sha = ""`.
- **Also:** the enqueue is idempotent by id, so later commits are never re-enqueued with the new head. Whether the gate re-reads the head is suspected, not traced.

##### B. Hit weekly

**B1. Every daemon restart re-rings the same alerts and wakes dormant sessions. Confirmed.**
- **Where:**
  - The dedupe state is in memory only: `RepoQueue.rung` (`serve.rs:145`) and `Harness.gate_told` (`harness.rs:1976`).
  - `regate_at_start` (`verbs.rs:540`) re-gates the red tip, which raises again.
  - For every blocked or cannot-decide review, `ring_project` (`mergequeue.rs:366-391`) calls `registry.create`, which resumes a dormant root session to re-tell the same notice.
  - `gate_killed` and `gate_setup_failed` re-raise on every re-gate. Only `target_red` is durably deduped.
- **Should:** persist what has been rung and told, keyed by row and state or by sha and state.

**B2. Nothing ever says "green again". Confirmed.**
- `red_card.rs:32` says so outright. `target_is_green` (`serve.rs:883`) only withdraws reviews.
- The model's last word stays "EMERGENCY… nothing lands", and it keeps acting on a red main until it calls `queue status`.

**B3. The review queue freezes while any gate runs in that repository. Confirmed (structural).**
- **Where:** `RepoQueue::pass` (`serve.rs:224-279`) runs review upkeep, the target gate (`:724`) and the landing gate (`:1122`) on one thread. A gate can take up to 30 minutes.
- **Effect:** for the whole gate, no review is claimed, reviewer deadlines and the never-started check (`serve.rs:399-445`) are not enforced, and capacity holds are not rechecked.
- **Operator sees:** reviews sit `waiting`, and nothing says why.

**B4. Separately, the daemon's worker loop is single-threaded for root sessions. Confirmed structure; suspected effect.**
- **Where:**
  - `daemon.rs:150-300` runs `Work::Command` (`sessions.dispatch` → `run_prompt` → `turn`, inline, `sessions.rs:2036-2119`) and `Work::Open` on one thread.
  - The upkeep in the `Idle` arm runs only when no work rings before the deadline: `deliver_due_nags`, `serve_reviews`, `park_idle_children`, `sweep_abandoned_calls`.
- **Effect:** a long turn in one root session delays prompts to other root sessions in the same folder's daemon, and starves nags and review service. Children run on their own threads.

**B5. The code does not follow the operator's design ("a red main halts both queues and returns items to review"). Confirmed.**
- **Where:** `serve.rs:465` and `target_went_red` (`:803`) keep reviews going on red. `land_next` (`:918-923`) just leaves merge rows `queued`.
- **Effect:**
  - No row is moved back to review, and each row's evidence stays its approval text, so nothing says why it is not moving.
  - When main is green again, those rows land without being reviewed again.

**B6. Stopping a daemon while a turn is running prints the wrong reason. Confirmed.**
- **Where:** in `tui/src/head.rs:579-605` (`interrupt_turns`), once the deadline passes, `ask_sessions(…, deadline)` is called with a deadline already in the past. It returns "timed out waiting for the session list" at once, so the intended "N turn(s) still running after Xs" can never be printed.
- **Also:** the interrupt's reason is hard-coded as "letibot --stop --force" (`head.rs:597`), so a plain `--stop` is recorded in the transcript as a forced stop.
- **Also:** `scripts/letibot:1554` runs `--interrupt-all || true`, which swallows the failure. The script then reports "NOT stopped: … is ignoring SIGTERM after 5s" (`:1572`) when the daemon is really finishing a turn the interrupt never reached.

**B7. Stopping an older (protocol-skewed) daemon. Confirmed path; whether rows are lost is suspected.**
- **Where:** the head goes straight to the force path (`driver.rs:746-764`, `head.rs:675-760`): interrupt (3 s), then SIGTERM, then SIGKILL 5 s later.
- **The problem:**
  - A daemon built before the "accept any protocol version" change (`sessionlog/src/server.rs:670`; its own comment says older daemons "go on refusing") refuses the interrupt's attach.
  - So the turn is never interrupted. An older daemon finishes its turn before honouring SIGTERM (`scripts/letibot:1546-1552`).
  - SIGKILL then lands mid-turn, which is the row loss the protocol half of the force path exists to prevent.
- **What the operator sees is honest:** "the interrupt did not go through (…)".
- **Gap:** neither the force path nor the `--stop` path in `scripts/letibot` waits for the turn to end before escalating.

**B8. A gate killed at its memory cap is correctly counted as unjudged, but the retry policy is "never, or on every restart". Confirmed.**
- **Target side:** classified `Infra`, state `Blocked`, prefix `GATE_KILLED`, shown as "UNJUDGED · gate killed at its memory cap" (`gate.rs:357-399`, `serve.rs:774`). That part is right.
  - It is never retried within a daemon's life (`serve.rs:676-681`).
  - It is retried on every lease take with the same cap and no backoff, so it likely dies again, and the alert repeats (B1).
- **Merge side:** a killed landing gate goes to `Blocked` until someone runs `/queue restart`.
  - The message says "could not run", but it ran and was killed.
  - The advice names `/queue regate` / "gate it again" (`gate.rs:77`). The verb for a branch row is `restart`.

**B9. A memory kill far from the cap, or with no cgroup, is reported RED. Confirmed path.**
- **Where:** `red_cause` (`gate.rs:207-228`) only counts a signal when the step leader itself died by one.
- **Sequence:** oomd or a box-wide OOM kills a rustc. Cargo prints `(signal: 9, SIGKILL` and exits 101.
- **Effect:** RED, an emergency, a red-tip review, and possibly a fix branch. The same happens on every `Unscoped` placement.

**B10. Removing a subagent's worktree under a live child goes unsaid. Confirmed that there is no check; frequency suspected.**
- **Where:**
  - `retire_branch` removes the tree with `--force` after a landing and does not check whether the child is alive. Host children stay live 10 minutes (`config.rs:1739`) and reopen when prompted.
  - `Deliverable::uncommitted()` returns `None` when git fails (`deliverable.rs:59-75`).
- **Effect:** a later `task_message` runs the child in a directory that no longer exists, and nothing says the worktree is gone.
- **The same applies to a removed workspace in general:** a grep finds no "workspace exists" check anywhere in harnessd, tools or sessionlog. Tool spawns use `current_dir(cwd)` (`exec/host.rs:1353`), so I suspect every tool call fails with a bare ENOENT.
- **And:** the daemon's log writers call `create_dir_all`, so they would recreate `<gone>/.letibot/`.

##### C. Hit occasionally

**C1. A startup failure after the socket is bound loses its reason. Confirmed.**
- **Where:**
  - `cli.rs:1104-1108` (flowy seat) and `1478-1481` (`open_first`) call `daemon.shutdown()` and return `Err`.
  - The error goes only to the daemon's stderr, which is `~/logs/harnessd.log` (`scripts/letibot:268`), and that file is shared by every folder's daemon.
  - The launcher has already exec'd the head, so the head just sees the daemon close.
- **Also:** the launcher's "did not come up; last lines of $LOG" (`:1933`) tails that shared log, which can show another folder's daemon's lines.

**C2. Exit-and-stop reports only this session's heads, jobs and subagents. Suspected.**
- **Where:** `tui/src/ui/cards/quit.rs` builds `other_heads` from `self.heads`, which is session-scoped, and `running_jobs` and `running_subagents` from this head's own lists.
- **Effect:** stopping the daemon also kills other sessions' heads and turns, the gatekeeper's reviews, and a running merge gate, without any of that being named on the card.

**C3. `letibot --stop --force N` ignores `N`. Confirmed.**
- **Where:** `scripts/letibot:1514` retargets only when `$2` is not `--force`, so `--stop --force 3` stops this folder's daemon.
- **Also:** `--stop --all` never uses `FORCE` (no SIGKILL), yet tells the operator to run `--stop --force`, which acts only on this folder.

**C4. Smaller queue findings, all from the queue reviewer and marked confirmed:**
- **Timed-out target:** gives no hub warning (`serve.rs:764-781`).
- **Silent blocked causes:** "the gate could not be read", "the landing worktree could not be made ready", and an `Infra` step that could not spawn raise nothing anywhere. They also never retry until a restart, regate or new commit.
- **Settings flood:** `queue_settings_unreadable` is raised on every pass, about every 2 s (`serve.rs:231-243`).
- **Silent merge failures:** a rebase conflict (`serve.rs:1050-1063`) or a failed gate (`:1145-1153`) on a merge row tells nobody.
- **Merge rows never settle:** `settle_if_needless` covers reviews only, so a parked merge row whose branch was landed by hand stays parked for ever.
- **Red-tip reviews survive green:**
  - `target_is_green` withdraws only open rows, so `CannotDecide` and `Rejected` red-tip reviews survive, and the `CannotDecide` one is re-told after each restart.
  - A landing sets health Green directly (`serve.rs:1281-1296`) without withdrawing a pending red-tip review. That review can still reject and raise "EMERGENCY" on a green main.
- **The same sha going red twice is silent:** `red-<target>-<sha12>` plus INSERT OR IGNORE (`queues.rs:807`) means no review and no raise; only the pane shows it.
- **Alerts reach only the lease-holding daemon's sessions:** `mergequeue.rs:355,393-430,513`.
  - A second daemon in the same repository hears nothing.
  - `queue_lease_lost` and `queue_wrong_build` use the global `announce`, so they reach every hub of every repository.
- **Reviewer start failures are inconsistent:** a synchronous start error (for example a failed clone) blocks on the first failure, while an asynchronous one gets 3 tries (`review.rs:261-280`).
  - "No reviewer started within 5 min" blocks on the first strike (`serve.rs:402-411`).
  - When s-gatekeeper is not in the registry, the message blames the wrong thing: `ring_reviewers` silently does nothing.
- **Push failures:**
  - A failed push with no install step raises nothing (`serve.rs:1235-1265`).
  - An install failure after a successful landing is raised as `queue_blocked`, though nothing is blocked.
- **VM capacity wait:** mostly shows the real reason ("4 of 4 VM slots are busy…", `notices.rs:360`, deduped durably).
  - Only firecode's slot wording is recognised (`capacity.rs:67-71`). Any other capacity refusal counts as an attempt and blocks after 3.
  - The detail overlay hides the failure while attempts are 0.
- **Repository scoping:** the key is the canonical parent of `--git-common-dir`, so worktrees map correctly to their repository.
  - Suspected: a non-repo directory with exactly one repository child is treated as that repository (`git.rs:155-166`).

**C5. Slug collisions and unvalidated slugs. Confirmed.**
- **Derived slugs collide:** `slug_from_prompt` (`task.rs:299-328`) takes the first 4 ASCII words. Same-start briefs share one slug, and non-ASCII prompts all get `task`. The second spawn is refused. The `-2` worktrees on disk show this being worked around by hand.
- **Leftovers block a slug:** a leftover `agent/<slug>` branch or a stale `.git/worktrees/<name>` entry makes `git worktree add -b` fail, and the error suggests no prune (`harness.rs:16744-16752`).
- **A supplied slug is only trimmed** (`task.rs:1098-1102`):
  - `/` nests directories and can clash with a ref.
  - `..` escapes `.claude/worktrees`.
  - Spaces make an invalid branch name.
  - `agent-agent-refresh` on disk shows the prefix being doubled.

**C6. A firecode `task_start` child is told to commit to a host branch it cannot reach. Suspected.**
- **Where:** `harness.rs:18120-18135` gives the deliverable for the host path, but the VM works on a copy (`task.rs:52-65`).
- **Effect:** the host tree always reads clean, the nudge never fires, and an empty branch is enqueued.

**C7. The `wake: later` sentence contradicts the delivery rules. Confirmed.**
- **Where:** `wake_sentence(None)` (`bash.rs:~308-318`) tells the model a held result "never starts a turn of its own".
- **But:** the R79 rules (`delivery.rs:27-48`, `jobwatch.rs:660-671`) do deliver it as a turn of its own. A model that believes the sentence may poll with `job_wait` instead.

**For reference:** a normal host job in a root session does push its completion, without waiting for the operator: watcher → `SettlementQueue::push` → `ring_wake` → `Work::Woken` → `Harness::wake`. So the late announcement comes from A1 or A7, not from the root delivery rules.

---

##### Git state: the stale worktrees

All of this is confirmed on disk:

- **Counts:** `git worktree list` shows 48 entries. `.claude/worktrees` holds 62 directories and takes **240 GB**; 34 of them carry their own `target/`, the largest 7.9 GB (`agent-job-slugs`).
- **24 orphaned checkouts:** the `agent-a<hex>` directories have `.git` files pointing at `/home/dead/Projects/letibot/.git/worktrees/...`, which no longer exists (it looks like an old repository location). Git does not know about them, and nothing will ever clean them up.
- **3 locked registrations with no directory:** `agent-a8e2db02c7b03ba25`, `agent-ac359f48783c335c5` and `agent-adff1e3334619ee0f` are registered at `/home/dead/Projects/letibot/.claude/worktrees/…`, which does not exist. Because they are locked, `git worktree prune` will not remove them.
- **Merged but still kept:** 20 of the 37 `agent/*` branches checked out in worktrees are merged into main, kept because of A3.
- **Dirty worktrees, i.e. uncommitted subagent work left behind:**
  - `agent-agents-md`: a staged change with an unresolved merge conflict (`UU standing_notes.rs`)
  - `agent-operator-input`: 5 modified TUI files
  - `agent-store-busy`: `.commit-msg` modified
  - `agent-why-a-queued-message-waits`: an untracked `tests/queued_latency.rs`
  - `agent-quiet-shell`: an untracked `.scratch/`
- **What cleans up:** only `retire_branch`, after a landing, and only for a clean tree. Nothing removes worktrees for children that failed, were refused or were never enqueued, and nothing prunes the orphans.

**The `.cargo/config.toml` question: yes, it still breaks things, but not the way expected.**
- **The worktree files are harmless leftovers.** 31 worktrees carry the old `task_start` file (`[build] target-dir = "/home/dead/Projects/letibot/letibot/target"`). The runner no longer writes it (`harness.rs:16752-16758`), and the session's `CARGO_TARGET_DIR` overrides it. It is gitignored, so it does not block a retire. Nothing removes it.
- **The real trap is in the main checkout.** `~/Projects/letibot/letibot/.cargo/config.toml` exists and contains `paths = ["/home/dead/Projects/rano/rano"]`. That is exactly the override `.gitignore` warns against.
- **It reaches every worktree.** Cargo searches parent directories for config, and every worktree under `.claude/worktrees` sits inside the main checkout. So every subagent build, and the operator's own, silently uses the local rano checkout, currently `v0.8.3-1-gde2ae1f`, one unreleased commit past the locked tag.
- **The gate does not see it.** The gate's landing worktree is at `~/.local/share/letibot/landing/`, outside the checkout, so the gate builds the real tag. "Builds in the worktree, fails in the gate" (or the reverse) is possible.
- **It will break the build** when the rano checkout moves to a version the lock does not want, which is the 2026-10-08 incident recorded in `.gitignore`.
- **Fix:** delete that file.

##### Highest-value fixes

1. `FirecodeBackend::processes_arc` (A1).
2. A boot status on the hub from bind until `daemon.run` (A2).
3. Keep `.letibot/*.jsonl` out of worktrees, plus a worktree reaper (A3).
4. Delete the main checkout's `.cargo/config.toml`.
5. Persist the alert dedupe (B1) and say "green again" (B2).
6. Make job ids unique across restarts (A5).

### What the model is told (R93)

I reviewed the text the daemon puts into the model's context at letibot main 50b3e55, read-only. Nothing was edited, built or committed, and no running process was touched. sessions.db was opened read-only only (`?mode=ro`).

The worst problems:
- The `[context check]` taught the "my context N%" tic, and still quotes the model's own sentence (✓ marks included) back to it.
- A memory/box note goes on 93% of bash results.
- 45% of todo nags name a row the model already marked in progress, and 45% get no tool call in reply, only a restated status.
- Several messages contradict each other about whether to wait, whether to call `task_result`, and what the system prompt says the model can do.

Three sub-reviewers covered harness notices, tool results and the system prompt. I re-checked eight of their claims in the source and all eight held: the 14-space gaps, `pkill` needing `pattern`, "You have read-only tools", `task` missing `model`, the literal `{raw}`, the gate.rs gaps, the stale-prompt wording and the reseat wording. The rest of their file:line claims are as they reported, not re-read by me. That includes B14 (they say a foreground command is killed at timeout_ms, not moved to the background).

Paths are under /home/dead/Projects/letibot/letibot.

### Part 1 — Real session data (sessions.db, read-only)

**Method.**
- Injected items are `transcript_item` rows of kind `system`, plus kind `user` where `speaker='agent'` (or null with a daemon prefix). Notes inside tool results come from `tool_result.payload`.
- When a transcript forks (compaction or reseat), its items are copied into the child with new item_ids. I removed copies by matching content in order against the parent transcript: 57,662 of 347,039 rows were copies. Without this, counts double; for example, the 945× below is really 393×.
- Totals after removing copies: 5,321 non-operator user/system items, of which 4,347 are injected.

**Most frequent injected texts (template counts):**

| Count | Text |
|---|---|
| 1,649 | `[todo check] this turn is finished and one item is not done (N more open):` |
| 1,346 | `[job] a job you backgrounded has ended:` |
| 260 | `[task] a subagent you started has finished:` |
| 100 | `[todo check] … one item is not done:` |
| 96 | `[job] N jobs … have ended` |
| 94 | `[intent check] this turn declared N item(s); 0 tool call(s) succeeded…` |
| 88 | `[context check] your reasoning sized the work by your context: "X". Your context is N% full — N of N tokens —` (old wording, with numbers) |
| 76 + 49 | compaction system notes |
| 54 | `Your previous turn was stopped at the context wall…` |
| 27 | re-seat system notes |
| 26 | `You keep overthinking…` (two variants) |
| 19 | `[context check]`, new wording without numbers |
| 11 | stale-prompt `session s-gatekeeper was seated…`, old wording ending "keep saying it until they act" |

**Worst exact repeats in one session:**
- **393×** in s-1791151788256943467: the same text, from 10-11 00:15 to 09:30 UTC, about every 66–68 s (median gap 66.3 s):
  ```
  [todo check] this turn is finished and one item is not done (1 more open):
    - do everything from todo.md — the operator's, and it is marked in progress and still open
  the operator asked for this one, so do it — or mark it done with `todo_write`'s `operator` field, quoting the text above exactly. You cannot remove their row: if you think it should not be done, say why in your reply.
  ```
  - The model's reply was the same each time. Reasoning: "My context is at its absolute end. The loop continues…". Answer: "Not done: three boxes open. 25 closed; suite 7,809/7,809; tree clean; pushed; frozen." That pair appears ≥86 times.
  - This was the pre-R77 metronome. The clock recorded the delivered wording but compared the plan wording, so the backoff never engaged. ebc2af7 (`Harness::nag_said_as`, harness.rs:7904) fixed it on 10-11 at 20:45 +0200.
  - Since R77 the ladder works on unchanged text, with gaps of 261, 490, 974 then about 1,809 s.
- **124×, 87×, 43×, 41×, 40×, 35×, 23×** in s-1789919514688401228: a row the model itself titled "CRITERION, NOT A TASK (stays open until the gate is green…)". Asking about it every window can only produce "still waiting".
- **29×** in s-1789462738453908838: "Your previous turn was stopped at the context wall and the conversation was then compacted…".

**Todo-check states (1,778 live checks):**
- 793 (45%): "it is marked in progress and still open". The model had already acted on the row.
- 518: "you were asked about this one already".
- 227: "you had it in progress".
- 487: the row is a gate criterion, not an action.
- Only 120 offered `wait_on`.
- After a todo-check, 777 of 1,737 (45%) got no tool call at all, only a status restatement. Top replies:
  - 113× "Not done: three boxes open…"
  - 70× "Row N stays open — the criterion, unchanged. Waiting on a head or the grants."
  - 31× empty
  - 19× "Stopping here deliberately…"
  - many "Running — `jNN` …, completion arrives on its own."
- That last group is a nag firing while a job runs, because the row was not attached with `wait_on`.
- Length: median 672 chars, p90 1,292, max 6,631.
- Why consecutive post-R77 nags differ: 74 named a different row, 23 had the same row but other lines differed (job lines, memory lines), and 5 changed only the head count, e.g. "(17 more open)" → "(18 more open)". Each such change resets the backoff ladder to the 60 s base window, because the "(N more open)" count is part of the compared text.

**The context-check tic:**
- In s-1789919514688401228 the first `[context check]` came at 10-11 18:23:56 and quoted numbers: "Your context is 21% full — 214,088 of 999,999 tokens. That is not the wall…".
- The first reasoning saying "my context is N%" came at 18:25, two minutes later. "my context" mentions then ran 38, 42, 160, 85, 76 per hour through 22:00, and 101/hour at 10-12 03:00.
- 108 of the 113 live context checks are in this one session.
- 98 of 113 quote ✓-laden sentences back to the model; 94 of 113 carried % numbers (pre-890d0eb).
- The latest one (10-12 12:28, after numbers were removed) still echoes the tic: `[context check] your reasoning sized the work by your context: "Hmm — **my *context* ✓** ✓ ✓ — **WRITE** ✓ ✓.". The context window is not yours to budget…` That exact quote was sent 14×.
- The ✓ habit itself predates the check (first seen 09-20). But the check feeds it back as a quoted, highlighted sentence.

**Notes inside tool results (live):**
- `[note] memory: peak X of a Y cap · box: Z GiB free of 185.8 GiB; you may ask up to W GiB without asking the operator`: 6,700 total, 3,979 of 4,256 bash results today (93%). Subagents are at about 95% (336/356, 269/286, 265/276, 227/240 of their bash calls).
- `[note] 1 file(s) were already modified and beyond the snapshot budget, so a change to one of them is not reported`: 5,611 total, 840 today. Internal and unactionable.
- `[note] this predicate also matches the process asking … the evaluating shell (the shell that will evaluate this command), the evaluating shell (the shell that will evaluate this command)`: 436. The clause is duplicated in the text.
- `[note] started. If this turn ends with no successful tool call, the intent check will say so…`: 163.
- `[note] this command ran over a minute in the foreground…`: 157.
- `[job]` completion bullets also carry `memory: peak 15.8 GiB of a 48 GiB cap`.

**Completion notices after the model already read the result:**
- 57 of 269 `[task]` notices (21%) arrived after the model had already called `task_result` on that child.
- 54 of 891 `[job]` notices (6%) arrived after `job_output`, `job_wait` or `job_list` had already shown that job. This is an upper bound, since a `job_list` mention may not be a read.

### Part 2 — Findings in the code

Format: file:line | exact text | problem | replacement.

#### A. Context check (crates/harnessd/src/context_claim.rs)

**A1.** :460-466 `notice` and :517-523 `reasoning_notice`
- Text: `[context check] you said "{claim.sentence}". The context window is not yours to manage: the harness compacts when it is reached and the work carries on. Stop sizing the work by it and continue with what you stopped.` (the reasoning variant is similar)
- Problem: it quotes the model's own context sentence back verbatim, tic marks and percentages included. That re-plants the phrase every time, and the data shows the tic followed the check.
- Replace: do not quote. `[context check] Don't size work by context; the harness handles it. Continue the task.`

**A2.** :467-480 `repeated`
- Text includes `(context {}% full — {} of {} tokens)`.
- This is operator-facing (a hub warning). It reaches the model only through the `harness` tool's `warnings` view (see F47). Make sure that view redacts it.

**A3.** :446-454
- The doc comment of `notice` still says "with the numbers — because a number is what the model did not have", right above "**No numbers.**". Stale and self-contradicting (not model-facing).

**A4.** `context_fill` harness.rs:12341
- Problem: it never checks `cfg.auto_compact`. With `--no-auto-compact` or a stuck wall controller, "the harness compacts when it is reached" is false (turn_tail.rs:226-243 says nothing was compacted).
- Fix: return None when auto-compaction is off.

**A5.** Reply variant
- Problem: "continue with what you stopped" fires even on a legitimate stop that merely mentioned context. The `[todo check]` explicitly allows "If you are stopping here deliberately, say why", and the loop continues (harness.rs:11619-11622).
- Replace: `…if context was the only reason you stopped, continue.`

**A6.** Reasoning variant on a clean turn
- Problem: it is appended without continuing (harness.rs:11620), so it is the last item the model sees at the start of an unrelated later turn.
- Fix: drop it when the turn ended cleanly.

**A7.** digest.rs:192-197
- Text: `ASK A QUESTION ABOUT MORE TRANSCRIPT THAN FITS IN YOUR CONTEXT … The rows never enter your context: you pay for the answer, not the conversation.`
- Problem: it invites exactly the context-sizing reasoning the check punishes.
- Replace: `Ask a question over many transcript rows; a subagent reads them and returns the answer.`

#### B. Todo check (crates/tools/src/builtins/todo.rs, crates/harnessd/src/sessions.rs)

**B1.** todo.rs:1816/1820 head, :1824-1827
- Text: `[todo check] this turn is finished and one item is not done (N more open):\n  - {row}{state}\n{advice}{offer}{waiting}`
- Problems:
  - It fires on rows the model already marked InProgress (793/1,778).
  - "this turn is finished" is said inside a fresh idle turn.
  - "(N more open)" changes reset the backoff.
- Fix:
  - Do not nag an InProgress row until it has been idle for the cap interval.
  - Drop "(N more open)" from the compared text (nag_said_as) or from the head.
  - Head: `[todo check] Open: {row}`.

**B2.** :1699-1709 state suffixes
- Text: `— {who}, and it is marked in progress and still open` / `, and you were asked about this one already and it has not been started`
- Problem: this asks for a restatement. The model answers "still in progress" (113× / 70× identical replies).
- Replace: drop the suffixes. Say `(in progress)` only.

**B3.** :1716-1763 advice
- Model row text: `do this one, or mark it done with \`todo_write\`'s \`update\` field, quoting the text above exactly — a row left out of \`todo_write\` STAYS on the board, so a line of work you are dropping is something you say in your reply, not something a call does. If you are stopping here deliberately, say why in your reply.` (about 60 words, on every nag)
- The Operator advice is similar. Queue advice (:1745-1752) is about 70 words that say no action moves the row; it is a nag about something the model cannot act on.
- Replace (Model): `Do it, or mark it done with \`todo_write\` \`update\` (quote it exactly).` This keeps the substring the fold matches.
- Queue rows: do not nag at all until a reviewer child has finished or gone quiet. The same pressure pushes the gatekeeper toward starting reviewers, which gatekeeper.rs:403-410 forbids.

**B4.** :1768-1810 wait_on offer
- Text: `If this row is only waiting on a running job or subagent, attach the wait instead of answering — \`todo_write\` {"wait_on": [{"content": "<the text above>", "kind": "handle"}]} — and this check stays quiet until it ends.`
- Correct field names, but it was offered only 120 times while "Running — jNN… completion arrives on its own" replies were common.
- Better: when a running job's description matches the row (LIKE_FLOOR), suppress the nag rather than offer.

**B5.** :2381-2416 `nothing_can_start`
- Text: `[todo check] this turn is finished and NOTHING can start — every item left waits on another: … unblock one of these rather than starting something new: finish a row they wait on, or correct a need … A plan where nothing can start is a plan to fix, not a plan to work.`
- Problems:
  - It fires when the root wait is a running job or a young child, which todo_write (:2555) promised would be SILENT.
  - Within one notice it says both "unblock one" and "there is nothing to unblock" (:2394-2410).
- Fix: extend `all_on_the_operator` (:1813) to job and young-child roots. Then say nothing, or `[todo check] Rows wait on running work; nothing to do until it ends.`

**B6.** :1355-1360 `attach_now`, appended to every bash background start (bash.rs:792, :901) and task start (task.rs:922, :1209)
- Text: ` If a row on your plan is waiting on this, attach it now — \`todo_write\` {"wait_on": …} — and the end-of-turn check stays quiet about that row until this ends.`
- Problem: it is sent even with an empty plan, and to seats without `todo_write` (m2_coder has `todo`; gatekeeper and runner have neither).
- Fix: emit only when `todo_write` is seated and an open row exists. Shorter: `If a todo row waits on this: todo_write {"wait_on":[{"content":"<row>","job":"{id}"}]}.`

**B7.** :2108 StruckOff
- Text: "re-word it with `todo_write`".
- Problem: re-wording creates a new row; the old one stays.
- Replace: `re-send this row in \`todos\` with that need removed.`

**B8.** :2754-2758 refusal
- Problem: it describes `wait_on` as job/subagent only and omits `operator` and `detach`.

**B9.** :2522 vs :2607
- Problem: "`update` moves ONE" vs "ONE OR MORE". Pick one.

**B10.** :2656
- Problem: the `needs` schema is `items: string`, but the parser and description accept tagged objects, so a schema-constrained model cannot emit them.
- Fix: `anyOf [string, object]`.

**B11.** Abstentions contradict.
- todo.rs:2582-2583: `An abstention unblocks it too … state the assumption you are making`.
- intent/ask.rs:452-453: `an abstention is an ANSWER and not permission to proceed on an assumption`.
- Pick one rule.

**B12.** Blocking contradicts.
- `ask_user_question` blocks ("however long", ask.rs:468), while `todo_write`'s operator wait says "THE REST OF THE PLAN KEEPS GOING" (:2577).
- Nothing says which to use. Add one line to the base prompt.

**B13.** `todo_write` description, todo.rs:2496-2584
- About 1,100 words. Compress to the core (one line per field).

#### C. Completion, delivery and wake notices (harnessd)

**C1.** harness.rs:1078-1080 `[job]` tail
- Text: `This is the completion arriving on its own — you do not need to wait for it, and \`job_wait\` would only block you for a result you already have. Read what it wrote with \`job_output\` (job="…"), then carry on with what you were doing.`
- Problems:
  - It also fires right after the model called `job_wait` (delivery.rs:219 Awaited), and after it already read the output (6%).
  - It plants `job_wait` on every completion.
  - It uses the placeholder "…" where the id is known.
  - "carry on" is said in a turn-end batch.
- Replace: `Output: job_output job="{id}".`

**C2.** harness.rs:1139-1142 `[task]` tail
- Text: `…calling \`task_result\` to block would only hold you for a result you already have. Read what it said with \`task_result\` (task="…")…`
- Problem: it says don't call `task_result`, then call it. 21% arrive after the model already called `task_result`.
- Replace: `Its full answer: task_result task="{handle}".`

**C3.** harness.rs:1064
- Text: `  - \`{label}\` {state} after {elapsed}, wrote {produced} bytes: {command}{whose}`
- Problem: the byte count, plus the model's own command echoed back in full (the monitor sample in the data shows the same command three times in one notice).
- Replace: `  - \`{label}\` {state} after {elapsed}`. Keep the command only for jobs another session started.

**C4.** harness.rs:1073 (from tools/src/exec/host.rs:221-235)
- Text: `    memory: peak X of a Y cap`, on every job.
- Problem: it leaks resource-sizing numbers.
- Replace: print only when killed: `    killed: hit its memory cap`.

**C5.** harness.rs:1084-1087 and 1146-1148 (job started by a session that has since stopped)
- Text: `This session is told because the one that started it cannot be: it has stopped and will not run again… this is the fact of the ending and no more.`
- Replace: `(Started by {owner}, which has stopped; its output is gone.)`

**C6.** harness.rs:1260
- Text: `Waiting on this: "{text}" ({whose}) — update {them} with \`todo_write\` now that {verb}.`
- Problem: it can appear under a `[meanwhile]` head that says "nothing in your plan was waiting on it". Contradictory.
- Fix: recheck the rows before using that head clause.

**C7.** delivery.rs:356-358, :360-364, :366-368 `[meanwhile]` heads
- Texts:
  - `[meanwhile] {n} result arrived while you were working — nothing in your plan was waiting on it, so it was held until your turn ended instead of interrupting you.`
  - `[meanwhile] {n} results held for over {cap} while this turn ran — … so they do not wait for ever. Carry on…` (leaks the 10-min cap)
  - The "busy" variant.
- Problems: they explain the daemon's holding policy, and the Starvation and Batch arms say "them/they" for n=1.
- Replace: `[meanwhile] Finished while you were working:`

**C8.** harness.rs:990-1004 `[monitor]`
- Text: `[monitor] {n} watch(es) fired:\n  - \`{name}\` ({watch}), declared by {turn id}: {word}\n…Only a FIRED watch is an answer about the world; an expiry, a cancellation and an owner ending are three ways a watch stopped existing.`
- Problems:
  - The model's own `retire` (tools/src/exec/monitor.rs:834-845) and turn-scope owner endings are pushed as firings. `wake()` (harness.rs:8574) then spends a turn telling the model about its own retire.
  - The head says "fired" for expiries.
  - It leaks turn ids, and the closing aphorism repeats on every notice.
- Fix: filter self-retire and OwnerEnded. Use `[monitor] \`{name}\` fired: {why}.` / `[monitor] \`{name}\` expired without firing.`

**C9.** harness.rs:8657-8660
- Text: `[todo] {n} row you attached a condition to is due:` (plural bug).
- Replace: `[todo] Due (its condition is met):`

**C10.** harness.rs:13552-13554
- Text: `…The result is not here: read it up yourself if the next step depends on it.`
- Problem: it names no place to read it.
- Replace: `\`{handle}\`: unknown job (lost in a restart, or a wrong handle); its result is unavailable.`

**C11.** harness.rs:14865
- Text: `message from your parent session \`{from}\`: {text}`
- Replace: `[parent] {text}`

**C12.** jobwatch.rs:1358 + harness.rs:366 `ContextWall` Display (the subreviewer did not confirm the exact path)
- A failed child's `why`, e.g. `stopped after 41 round(s) at the context wall: 980,112 of 1,000,000 tokens…`, is likely to reach the parent's `[task]` bullet as token numbers.
- Map to: `failed: ran out of room; resume it with task_message.`

**C13.** deliverable.rs:105-113 `completion_line`
- Problem: it is prepended to a dirty child's answer, so the parent's first-line bullet (jobwatch.rs:1423) shows "has uncommitted changes" instead of the answer.
- Fix: put the salvage line after the answer.

**C14.** deliverable.rs:97-106
- Text: `[the deliverable] Your worktree \`{path}\` has uncommitted changes, so the branch … does not hold them: ?? .letibot/delivery-log.jsonl. The branch is the deliverable — commit this work … check that \`git status --porcelain\` prints nothing, and then finish…`
- In the data it fired on the daemon's own `.letibot/delivery-log.jsonl`, a file the model did not create.
- Fix: ignore `.letibot/` runtime files. Shorter: `[the deliverable] \`{branch}\` is missing uncommitted changes ({list}). Commit them (or say why one stays out), then finish naming the commit.`

#### D. Step and salvage nudges

**D1.** harness.rs:11367-11372 (text also in turn/src/compaction.rs:253)
- Text: `You keep overthinking. Cut it short and do things. Your last turn spent its whole output on reasoning ({reason}) and nothing was recorded. Answer first. Reason after, or not at all.`
- Problems:
  - It leaks the enum `reasoning_only` / `no_visible_content`.
  - "Reason after" is impossible for GLM, DeepSeek and Qwen, whose thinking always comes first.
  - It is identical on every salvage, and "You keep" accuses on the first occurrence.
  - It conflicts with gatekeeper.rs:325-327 "You may reason in prose for as long as you like".
- Replace: `Your reply was all reasoning and nothing was kept. Make the tool call or give the answer now.`

**D2.** harness.rs:11383-11387 (turn/src/compaction.rs:243)
- Text: `You keep overthinking. Cut it short and do things. Your last turn thought until it ran out and said nothing. Stop reasoning and act now.`
- Problem: factually wrong. This arm is end-of-sequence inside reasoning, not running out.
- Replace: `Your reply ended inside reasoning with no output. Make the tool call or give the answer now.`

**D3.** turn/src/length.rs:139-141
- Text: `Tool call {name} was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.`
- Replace: `Tool call {name} was cut off and not run. Send it again complete (split a large write into several edits).`

**D4.** progress.rs:246-268 (injected at harness.rs:11866)
- Text: `The harness is watching what your tool calls return, not how many you make, and the last {n} rounds ran {calls} calls and 0 of {calls} returned a result this turn had not already seen ({tally})… If the next round also produces nothing new the turn stops and the operator is shown this, so say in that round what you are still looking for: an operator who can see what you were after can widen the search, and one who cannot see it can only see that you stopped.` (about 90 words, 9 in the data)
- Replace: `Your last {n} rounds returned nothing new. Answer if you can; otherwise try a different query and say what you're looking for. One more empty round ends the turn.`

#### E. Compaction, reseat, restart, stale prompt

**E1.** harness.rs:9037-9040 `continue_after_wall`
- Text: `Your previous turn was stopped at the context wall and the conversation was then compacted: the summary above stands in for everything said before it. Continue the work you were doing, picking up from the summary rather than repeating what it already records.` (54 live, 29 in one session)
- Problems:
  - It names the "context wall" the context check says to ignore.
  - It duplicates the system compaction note right above it.
  - turn_tail.rs:177 can send it when nothing was compacted.
- Replace: `Continue from the summary above; don't redo what it records as done.` Send only after a real compaction.

**E2.** harness.rs:10520 `cut`
- Text: `" It was CUT OFF at the model's length limit before it finished: what              follows is incomplete, it stops mid-sentence, …              re-read the earlier              transcript if something is missing."`
- Bug: runs of 14 literal spaces, because the line breaks have no `\` continuation. Confirmed in source.
- Replace: ` The summary is incomplete (cut off); treat anything missing as unknown and check with \`transcript\`.`

**E3.** harness.rs:10564-10565
- Text: ` The last {n} item(s) of it follow this note VERBATIM — as they were written, not as a description of them.`
- Replace: ` The most recent messages follow verbatim.`

**E4.** harness.rs:10571-10573
- Text: ` The verbatim part begins in the MIDDLE of an exchange: {dropped} item(s) of it were dropped from the front … Stated rather than left to be discovered.`
- Replace: ` The verbatim part starts mid-exchange; its first message answers something not shown.`

**E5.** harness.rs:10588-10590
- Text: `This conversation was compacted: what was said before this point is replaced by the summary below, which was written over the full history of transcript {old_id} and proposed no tool calls.`
- Problem: it leaks the transcript id and an acceptance-check detail.
- Replace: `Earlier conversation is replaced by this summary (full history: \`transcript\` tool).`

**E6.** sessionlog/src/event.rs:1061-1080 `why_line` (used at harness.rs:10561)
- Text: `No verbatim tail: this conversation's turns go to the LOCAL model, and R27's split gives the tail to a remote one only — a local model is bounded by the KV cache…`
- Problem: rule ids, KV cache, budgets.
- Fix: omit for the model.

**E7.** harness.rs:10581-10584
- Text: `This conversation was re-seated onto a new prompt: message zero now announces the tools this daemon seats, and everything said before this point follows unchanged from transcript {old_id}. Nothing was summarised and nothing was dropped.`
- Problem: it duplicates `[reseat]`. In the data it appears 2–3× per reseat (t10 and t11 notes stacked).
- Fix: drop it, or `System prompt and tools updated; conversation continues unchanged.`

**E8.** harness.rs:2252-2283 `reseated_for_the_model`
- Text: `[reseat] The person re-seated this conversation with \`/reseat summarise\`: the {was_tokens} tokens of history before this point are replaced… Message zero is now the prompt this daemon seats… stop asking for \`/reseat\`. Say in one line that the re-seat landed and what it changes for you, then carry on with what you were doing.`
- Problems:
  - A token count.
  - It asks the model to restate state ("Say in one line…").
  - "stop asking for /reseat" re-plants the request that stale_notice already forbade.
- Replace: `[reseat] Your system prompt and tools were updated{; earlier history summarised}. New: {gained}. Removed: {lost}. Changed: {grew}. Carry on.`

**E9.** harness.rs:2205-2209 `stale_notice_for_the_model` wrapping `stale_prefix_sentence` :2483-2514
- Text: `session {id} was seated {when} with a prompt this daemon would not compose now … message 0 is frozen, and rewriting it is a full cold re-prefill — so the model cannot call a tool seated since… For your information, said once: … Do not ask them to re-seat and do not repeat this…` (over 120 words)
- Problems: operator text addressed to the model in the third person, and it explains `/reseat` and then forbids it.
- The old wording "Say this to the person in your reply, and keep saying it until they act" is in the data 11× (s-gatekeeper), but is fixed in code.
- Replace: `[stale prompt] Your tool list is out of date: {added} exist but you cannot call them; {gone} will refuse; {grew} changed. Updating it is the person's call: don't ask; mention a missing tool only when you need it.`

**E10.** harness/clear.rs:171-178 `[cleared]`
- Leaks the session id, transcript id and "Jobs ({n})" counts; long.
- Replace: `[cleared] The operator cleared the conversation; assume nothing from before. Your board, jobs and subagents are listed below; read the old conversation with \`transcript\` if they refer to it.`

**E11.** subagent_record.rs:393-398
- Text: `[restart] the daemon restarted while you were working (at {utc}, build {sha}); your conversation, worktree and plan are as you left them — carry on from where you were.`
- Problems: it leaks the build and time, and conflicts with the "Nothing above is running now" restore notice sent in the same turn. Data shows it 4× identical in a row.
- Replace: `[restart] You were interrupted and resumed; background jobs you started are gone (see below). Carry on.`

**E12.** restart.rs:266-303
- Text: `[restart] the daemon was down for {down} (stopped at {utc}, estimated from its last heartbeat; back at {utc}) — under the {threshold} restore threshold…`
- Data: "down for 0s … estimated from nothing (no record said when)".
- Replace: `[restart] The daemon restarted. Restored:` / `…nothing was restored. You were running:`

**E13.** subagent_record.rs:403, 409-411
- Problem: nested `[restart]` tags inside a `[restart]` notice.
- Replace: `- subagent {name}: resumed.` / `- subagent {name}: not resumed ({why}); restart with task_start if still needed.`
- Also :316-319 "is in a directory beside the copy" gives no path.

**E14.** restart.rs:439-440
- Text: `…(\`pkill\` with \`action: "list"\` checks, killing nothing).`
- Problem: `pkill` requires `pattern` (pkill.rs:65), so the suggested call is refused.
- Replace: `…(check: pkill pattern="<cmd fragment>" action="list").`

**E15.** restart.rs:480-482
- Text: `…if they still need its result, run it again and \`wait_on\` the new handle.`
- Problem: `wait_on` is a `todo_write` field, not a tool.
- Replace: `…re-run it and set wait_on on that row with todo_write.`

**E16.** restart.rs:353-354
- Text: `Nothing above is running now…`
- Problem: it is skipped for the Restored outcome even when job lines say "NOT run again".
- Fix: append it whenever a job line is present.

#### F. Tool results (crates/tools)

**F1.** bash.rs:140-168, :1077-1081, exec/memory.rs:236-243
- Text: `[note] memory: peak {p} of a {cap} cap · box: {free} free of {total}; you may ask up to {ceil} without asking the operator`
- Problems: it is on 93% of bash results (3,979/4,256 today), and the numbers are resource sizing that changes every call.
- Fix: emit nothing on success. Only when killed or within about 10% of the cap: `[note] this command used nearly all of its memory cap; pass memory: "2G" if it needs more.` Drop `box:` entirely.

**F2.** bash.rs:409-415 description
- Text: `…Each result also says how much the box has free and how much you may ask for. Without a cap, a command that ate the box would be killed … the session, its daemon and the operator's terminal window have all gone that way…`
- Replace: `Every command runs under a memory cap and is killed alone if it reaches it. Set memory for builds/tests (e.g. "4G").`

**F3.** bash.rs:110-121 `memory_arg`
- Text: `…Every result reports the command's peak, so you can size the next ask from the last one.`
- Problem: explicit sizing guidance.
- Replace: `Memory cap: a size ("512M","4G") or "uncapped". Default {d}. Large asks go to the operator.`

**F4.** bash.rs:126-136 `default_sentence`
- Problem: it duplicates F3 and invites editing `leticode.toml`. Merge, and drop the config clause.

**F5.** bash.rs:1040-1056 + exec/memory.rs:186-209 (killed at cap)
- Problems: peak and cap are said twice, the ceiling is disclosed, the model is told to edit `leticode.toml`, and the F1 note is appended too.
- Replace: `[killed: out of memory at its {cap} cap] Run it again with memory: "{step}".`

**F6.** bash.rs:205-209, :214-218, exec/memory.rs:314-326
- Text: `nothing was run. You asked: the model asks for {X} … {free} of {total} is free now. The operator said no…`
- Problems: free/total exposed, and the model is addressed in the third person.
- Replace: `Not run: the operator refused {X} ({why}). Ask for {ceiling} or less, or make the work smaller.`

**F7.** exec/host.rs:221-235 → jobs.rs:378 (`job_list`), :713-717 (`job_wait`), jobwatch.rs:1269
- Peak/cap on every job row. Print only when the job was killed at its cap.

**F8.** mergequeue/gate.rs:252-257 (into notices::health and the red-tip reviewer brief, serve.rs:827)
- Text: `[gate memory: peak {} of a {} cap]`
- Fix: strip it from model-facing evidence, unless killed: `[gate step killed: out of memory — not a code failure]`.

**F9.** gate.rs:77-82
- Text: `…The cap is [memory] gate in providers.toml (unset, it is half of what is free…); raise it…`
- Problem: the model may not edit the operator's providers.toml.
- Replace: `{step} was killed at the gate's memory cap; the tip is UNJUDGED, not red. Raising it is the operator's call — tell them.`

**F10.** gate.rs:218
- Text: `"it died with its peak within {}% of the cap ({}) and no failing code in its                  output — …"`
- Bug: about 18 literal spaces (no `\` continuation). Confirmed in source; percent leak too.
- Replace: `it died near its memory cap with no failing code in its output — out of memory, not a verdict`.

**F11.** mergequeue/notices.rs:114-121
- Text: `…once there is room, gate the tip again.`
- Problem: it asks the model to watch box memory.
- Replace: `…tell the operator; regate once they raise it.`

**F12.** bash.rs:315-330 `wake_sentence`
- Text: `…its result is HELD and arrives … at your next break … It never starts a turn of its own.`
- Problem: false under `Rule::TurnEnd`/`Idle` (delivery.rs:304-309), and contradicted by bash.rs:765, :791-799 ("You are woken when it finishes"), task.rs:1216, :1253, todo.rs:2129, :2406, :2556 ("as a turn of its own"), and jobs.rs:583. Sometimes the contradiction is inside one result.
- Use one sentence everywhere: `Its completion reaches you automatically — when your current turn ends, or at once if a todo row needs it or wake is "now".`

**F13.** bash.rs:777-811 background start result + result.rs:138-148 STILL_RUNNING
- About 250 words: "reaches you on its own" four times, "THIS COMMAND IS STILL RUNNING… Do NOT start it again", "ran in the foreground for: 0ms", then the HELD sentence.
- Replace: `started {label} in the background (pid N). Its completion reaches you automatically; don't poll. Output: job_output job="{id}".`

**F14.** bash.rs:381-385 description
- Text: `A command you did NOT mark background is moved to the background by the runtime once it has run longer than timeout_ms…`
- Problem (subreviewer, not re-read by me): false. At the deadline it is killed (bash.rs:966 "was killed after {}s — it outlived its deadline"). Promotion happens only on the operator's request.
- Replace: `A foreground command is killed at timeout_ms (default 120 s); use background: true for anything longer.`

**F15.** bash.rs:366-371 vs :966-969
- The slow note says "Raising timeout_ms only makes the wait longer…". The kill says "call bash again with a larger timeout_ms, or background: true".
- Kill text: `Killed at its {N}s deadline. Re-run it with background: true.`

**F16.** jobs.rs:650-656 `job_wait` description + predicate.rs:484-490
- The description says it blocks until done. The code (jobs.rs:775-781) returns at once for every watched job ("Waiting here would hold the floor").
- Description: `For your own bash jobs this returns at once; their completion is delivered to you.`

**F17.** jobs.rs:844-848
- Text: `…Wait again with a longer timeout_ms…`
- Contradicts F16. Replace: `{label} still running after {t}; not killed. Output so far: job_output job="{id}".`

**F18.** task.rs:1420-1426 vs :1342
- Text: `call task_result again with a longer timeout_ms…` vs "Waiting here would hold the floor".
- Replace: `{handle} has not answered yet; its answer is delivered when it finishes.`

**F19.** task.rs:1252-1254 vs :820/:1034
- "You do not need this to get its answer" vs "Collect its answer with `task_result`".
- Replace both: `Its answer is delivered to you; task_result shows its state or re-reads the answer.`

**F20.** result.rs:433-436
- Text: `…STILL RUNNING … Wait on them, or read what they have written, before reporting anything as an answer.`
- Problem: "Wait on them" contradicts the don't-wait texts.
- Replace: `{n} call(s) still running ({handles}); results will be delivered. Don't report an answer yet.`

**F21.** task.rs:808-826
- The description lists `model`, and invoke reads it, but `task`'s input_schema has no `model` property. Confirmed: the properties are prompt, role, access, where, verify, wake.
- `task_start` says "exactly as task takes them". Add `model` to the schema.

**F22.** task_start (task.rs:1021-1047)
- Problem: no `wake` property, yet its result promises "comes back as a turn of its own". Add `wake`.

**F23.** task.rs:822-826
- Repo-specific `sh scripts/check-fmt.sh … cargo test --workspace --no-run` sits in a generic tool schema. Move it to AGENTS.md.

**F24.** task.rs:1408-1411, :458
- Text: `…its transcript is on the board under {handle}.`
- Problem: no "board" tool.
- Replace: `Its transcript: transcript session="{handle}".`

**F25.** bash.rs:401-404 and jobs.rs:300, :1026-1027
- Texts: "Do not write pkill, pgrep…", "Use this instead of ps or pgrep", "instead of pkill or kill".
- Problem: `pkill` and `ps` are real seated tools, and ps.rs:37 recommends `pkill`. There is also a typo, "an scope".
- Replace: `Don't use shell pkill/pgrep/sleep loops: job_kill/job_wait for your jobs, the pkill tool for other processes.`

**F26.** exec/predicate.rs:487-492
- Text: `A process this harness did not start is not yours to kill from here`
- Contradicts the `pkill` tool. Replace: `…use the pkill tool (list, then kill by pid).`

**F27.** predicate.rs:433-437
- Text: `…which is the failure TODO.md T21 is about, and it was measured on this box five times…`
- Replace: `Not run: its pattern matches the process running it (or a protected one).`
- Also fix the duplicated clause "the evaluating shell (…), the evaluating shell (…)" seen 436× in data.

**F28.** monitor.rs:61-81
- Text: `exactly one of job … process` / `There is deliberately no way to watch a shell command or a process name`
- Problem: the schema has `command`, `timer_ms` and `process`.
- Fix: list all eight conditions and delete the sentence.

**F29.** monitor.rs:143-150 retire
- Bug: it renders "it had retired by the model after 30s left", and "It never fired" is false for a repeat monitor.
- Replace: `retired {name} (watching {watch}) after {age}.`

**F30.** intent/board.rs:181-187 (`todo`) vs todo.rs:2496 (`todo_write`)
- Two list tools with different models, both registered. intent/ledger.rs:315/317 tells leticode sessions to "call todo with op: \"list\"", and `todo` is not seated there.
- ledger.rs:191 prints "board: 0 items — nothing has been declared" while `todo_write` has rows.
- Fix: seat one, or read the `todo_write` board.

**F31.** merge_gate.rs:166-171, :245-248, harness.rs:8202-8208
- Text: `put the choices to the operator…; then write the section into AGENTS.md and commit it to main`
- Contradicts config.rs:1020-1021 and notes.rs:652 ("AGENTS.md … you read them rather than edit them") and task_start ("NEVER pushed — landed by a merge queue"). It also does not name `ask_user_question`.
- Replace: `Ask the operator with ask_user_question (options = these choices); after they pick, add the section to AGENTS.md (state this is the bootstrap exception to the queue rule).`

**F32.** retrieval.rs:128-133
- Text: `attach a retrieval backend… (T16.6)`
- Problem: no tool for it, and an internal ticket number.
- Fix: don't seat ask_code/ask_corpus, or `No retrieval backend; use grep and read.`

**F33.** exec/confine.rs:836-844
- Text: `…That ENOENT is the boundary, NOT an answer … the mount namespace does not contain it…` (long)
- Replace: `{paths} are outside this session's view (absent, not missing). The operator has been asked to grant access; don't retry another spelling.`

**F34.** result.rs:108-112 + spill.rs:386-391, :410-415
- Problem: the spill "Omitted N bytes… Call read_spill" notice is emitted twice per result. Drop one.

**F35.** provider/src/messages.rs:311
- Text: `[{name}: {reason}]\n{payload}`
- Problem: it re-adds the reason already in the envelope (result.rs:150-163 "reason once").
- Fix: pass the payload through unchanged.

**F36.** result.rs:121-126 NO_RESULT
- Text: `This call produced no result. Nothing above is an answer…`
- Problem: it wraps jobs.rs:578 (redirected output), which tells the model to read a file.
- Fix: return that as ok.

**F37.** bash.rs:908-912
- Text: `[{label} was moved to the background — you did not ask for it] … do not wait on it.`
- Replace: `[the operator moved {label} to the background] It's delivered when it ends.`

**F38.** `[note] 1 file(s) were already modified and beyond the snapshot budget, so a change to one of them is not reported`
- 5,611× in data (the subreviewer did not cite a line). Internal and unactionable: emit once per session, or never.

**F39.** standing_notes.rs:420-421 (system prompt)
- Text: `### {} — {} line(s), indexed: over what is left of the {2000}-token notes budget…`
- Problem: a token number in the prompt.
- Replace: `### {path} — indexed (too long to include); read it with a range.`

**F40.** notes_offer.rs:149-154
- Text: `[standing-notes offer — the harness's own line, not the operator's] {path} looks relevant … (similarity {:.2} of 1.00, lexical over its abstract, title and headings)… If it is worth the context, open it…`
- Problems: score internals and "worth the context" sizing.
- Replace: `[note offer] {path} may be relevant; read it if useful.`

**F41.** notes.rs:1159-1170
- Text: duplicate refusal with "(score {:.2} of 1.00)… IDF-weighted cosine… {DUPLICATE_FLOOR} floor" (about 110 words).
- Replace: `Not written: too similar to {other}. Append to it, or say in the abstract what differs.`

**F42.** notes.rs:374
- Text: `Err("\`{raw}\` is too long for a filename (over 100 characters).".into())`
- Bug: not `format!`, so a literal `{raw}` reaches the model. Confirmed in source.

**F43.** notes.rs:424-426 vs config.rs:1016-1017
- Notes re-read "every turn on a provider session" vs "after every compaction". Unify the wording.

**F44.** harness_view.rs:150-160 (`harness` what=warnings)
- Returns warning details verbatim, including harness.rs:9269-9274 `{r} of {w} {unit} resident… compacting now`, memwatch.rs:238 `the daemon now holds {} resident (heap {})…`, and context_claim::repeated with percent and tokens.
- Fix: redact auto_compact, memory_* and context-claim details for the model.

**F45.** decisions.rs:399-403
- Text: `P(allow) {p:.3}, in {ms} ms`, `oracle answered in {ms} ms, no logprobs…`
- Drop the latency.

**F46.** read.rs:268-273
- Text: `stopped before line {}: one read is capped at {} KiB of text…`
- Replace: `stopped before line {n}; continue with offset={n}.`

**F47.** mergequeue/notices.rs:49-55 `TOOL_WAYS`
- About 90 words restating the `queue` schema, appended to every health notice (:121, :134, :142, :155).
- Replace: `See queue action:"status"; regate once the cause is gone.`

**F48.** notices.rs:146-155
- Text: `EMERGENCY — {at} is RED … a reviewer looks for what … broke it. {TOOL_WAYS}`
- 26 in data, 3 near-identical in a row for leticl. It never says whether the model should act.
- Add: `A reviewer and fix branch are automatic; you need do nothing unless you know the cause.`

**F49.** notices.rs:166-201 `parked`
- Text: `/queue approve {id}`, `/queue veto {id} WHY`
- Problem: operator slash verbs the model cannot run. Name model actions only (`queue action:"restart"`), or mark them "(operator only)".

**F50.** notices.rs:409-423 `handed_over`
- Text: `…land it on {target} yourself if it is green…`
- Contradicts the queue rule. Prefix it `Exception to the queue rule:`.

**F51.** mergequeue/review.rs:387-398 vs verdict.rs:316
- In a red-tip review `accept` means "the red is not in the code"; the schema says it means "the change does what the brief asked". Note this in the schema.

**F52.** mergequeue/desk.rs:37-40
- Leads with `/queue regate /path…`. Replace: `name the repository with repository:"<path>".`

**F53.** notices.rs:229-236
- Text: `(the verdict is bound to the head it judged)`
- Internal. Replace: `…a new head is reviewed again automatically.`

**F54.** sessions/gatekeeper.rs:409
- Text: `Say on your own log what you found and leave the row where it is.`
- Problem: there is no log tool. Replace: `Say what you found in your reply.`

**F55.** gatekeeper.rs:247-251, :255-261
- Text: `…its calls are permission cards, and they come to YOUR head… The operator's remedy is a firecode on the daemon's PATH…`
- Problem: the model cannot answer cards or fix PATH.
- Replace: `Reviewers run on this machine; their bash calls need the operator's approval, so a stalled review may be waiting on that.`

**F56.** args.rs:154-402 `[repaired]` envelope notes
- Examples: "arguments arrived inside a markdown code fence"; in data, 132 `[repaired] \`id\` was given as a number`.
- Problem: they recur every call for a model that always fences its arguments.
- Fix: once per session for `envelope.*`.

**F57.** write.rs:270-273, edit.rs:695-697
- Text: `…a second, independent gate below §11's adjudication…`
- Replace: `The session's backend is read-only; nothing was written.`

**F58.** `[note] started. If this turn ends with no successful tool call, the intent check will say so before the next one begins.`
- 163× on `todo`. A threat about a check that is off by default (`--intent-prose`; cli.rs:757). Drop it.

#### G. Base system prompt vs everything else

**G1.** The real prompt is config.rs:987-1024 `DEFAULT_SYSTEM`, plus standing_notes::section (standing_notes.rs:285), plus `OwnSession::prompt` (sessions.rs:1878; gatekeeper::PROMPT gatekeeper.rs:377-415), plus the fabric block (sessions.rs:1520, flowy/src/context.rs:112-145). prompt.rs is the operator `!send`/sudo cards, not the system prompt.
- It has no rule on waiting, jobs, subagents, todos, context, stopping, asking the operator, the queue or reasoning length.
- So every such rule lives only in tool descriptions and notices, with nothing above them to settle a contradiction.

**G2.** config.rs:990
- Text: `You have read-only tools.`
- Problem: false for leticode, coder, planner and runner. It contradicts config.rs:998-1007 "Change files with `edit` and `write`". Confirmed in source.
- Replace: `Use your tools for questions about **this tree**…`

**G3.** config.rs:995
- Text: `Do not call a tool for a question about the world …; answer those directly.`
- Problem: it forbids `web_search` and `web_fetch` when they are seated.
- Add: `…unless a web tool is seated and the answer depends on current facts.`

**G4.** One prompt for every seat, but it names tools many seats lack:
- `transcript` (:993) is missing in m2_coder, gatekeeper and notes_keeper.
- `harness status` scratch path (:1008) is leticode-only. A coder child is told to use only that path and nothing under /tmp, with no tool to read it.
- `edit`/`write` are absent in gatekeeper, whose own prompt says "You have no write tool" (tools/src/gatekeeper.rs:316).
- `notes` is absent in gatekeeper.
- Fix: compose these sentences from the seated tools, or inject the scratch path.

**G5.** Children inherit the parent's finished prompt (harness.rs:17823-17836 `..self.base.clone()`), including the own-session paragraph and the fabric block.
- That block says `skill load …` and `flowy get …`. The `flowy` tool is withheld from children, and m2_coder has no `skill`.
- A reviewer under the queue session would carry "You are the review queue…" plus "You are the gatekeeper." Inferred from code, not confirmed on a live transcript.
- Fix: compose per seat.

**G6.** flowy/src/context.rs:138, sessions.rs:1534
- Text: `flowy get /api/artifact/<id> --jq .body`
- Problem: the flowy tool has no `get` action (tool.rs:384: status, attention, subscribe, unsubscribe, say, dm, read, replay).
- Replace: say it is the CLI through `bash`, or add the action.

**G7.** context.rs:127
- Text: `skill load <id or title>`
- The real call is `skill {action:"load", name:"…"}` (skill.rs:280-292).

**G8.** flowy tool.rs:380
- Text: `Reasoning goes in a row.`
- Problem: no flowy action writes a row. Drop it, or name the door.

**G9.** gatekeeper.rs:325-327
- Text: `You may reason in prose for as long as you like before and after the call`
- Problem: it conflicts with the D1/D2 overthinking nags.
- Replace: `Explain what you found in your reply; keep it to the evidence.`

**G10.** Dead role tool names: `researcher()` names `search_corpus` and `reviewer()` names `git` (runtime.rs), and neither exists. They seem unused.

### Highest-value changes, in order

1. **Context check (A1):** stop quoting the model's sentence back.
2. **Memory/box note (F1–F7):** drop it from every bash/job result; say it only on a kill.
3. **Todo nag (B1–B5):**
   - don't nag InProgress rows or criterion rows each window
   - keep "(N more open)" out of the compared text
   - suppress when a running job or child is the root
   - shorten the advice to one line
4. **Wake/delivery contradictions (C1, C2, F12–F20):**
   - one sentence about how completions arrive
   - no "don't call X… call X"
   - fill in real handles instead of "…"
5. **G2:** "You have read-only tools" is false and contradicts the next paragraph.
6. **Formatting/schema bugs:**
   - E2: 14-space gaps in the compaction note
   - F10: about 18-space gaps in gate.rs
   - F42: literal `{raw}` in notes.rs
   - F21: `task` schema missing `model`
   - F22: `task_start` missing `wake`
   - F28: monitor description contradicts its schema
   - E14: `pkill` hint missing the required `pattern`
   - E15: `wait_on` named as a tool
7. **Echo requests:** D1/D2 "You keep overthinking", E8 "Say in one line that the re-seat landed", E1 "context wall".

Scratch analysis scripts (read-only, `mode=ro`) are in /tmp/claude-1000/-home-dead-Projects-letibot/7a4b7ff7-173b-4f07-a444-4fda796b95b7/scratchpad/s/. live2.py removes the fork copies; tr2.py, reply.py and acted.py produce the counts above.

## The stabilization review of 2026-10-11, in full

Four read-only reviews of main at `435bb88`, run as subagents on the operator's ask (*"spawn
subagents to do a code review - goal is stabilization"*, and *"the merge/review queue is a
center-piece and it was never reliable so far"*). Kept verbatim so nothing a summary dropped is
lost; R61, R67, R68, R69 and R70 are filed from them. Line numbers are on `435bb88`.

### The head: mouse and key routing (crates/tui)

#### Terminal head (crates/tui) review: mouse and key routing

Everything below was found by reading the code. Nothing was run. `crates/ui` holds only the composer editor and has no mouse or layout routing, so all findings are in `crates/tui`.

**Overall shape:** mouse and keys share one entry point. `key_of` (`app/keys/input.rs:126`) turns a wheel step or a plain left press into a `Key`. That `Key` then goes down a hand-ordered list of about 25 `key_*` handlers in `App::key` (`app/keys/mod.rs:54-260`). The first handler to answer `Break` wins.

There is no single focus and no single record of where things were drawn:
- Each pane is its own `bool`.
- Draw priority (`ui/screen.rs` `main_area`), key priority (`keys/mod.rs`) and hint-bar priority (`ui/hint_bar.rs:15`) are three separate hand-written orders, and they disagree.
- Each clickable surface does its own hit-testing: some use a recorded map, some redo the layout arithmetic, some do none.

##### Findings, most severe first

1. **CONFIRMED – typing with the queue pane open runs queue verbs.** `app/keys/panes.rs:736` (`r`) and `:770` (`a`/`v`/`d`) have no empty-composer guard. The subagents pane's `o`/`p` do have one.
   - Scenario: `/queue` is open and the operator starts typing "deploy…". The `d` sends `queue rm <selected id>` and the `a` sends approve. Every letter is eaten.

2. **CONFIRMED – a click in the session picker selects the wrong session.** `panes.rs:~294-308` maps `y - (header+2)` straight to `picker_sel`. But rano's `SessionPicker::content` draws two lines per session: the row, then an id/workspace line. The footer lines also fall inside the `picker_rows_drawn - 2` bound.
   - Scenario: clicking "two" (y=5) selects "three", and clicking any id line selects the next session. Enter then switches to a session the operator did not click.
   - The test `a_click_picks_the_row_under_the_pointer…` (`app/tests/session.rs:1762`) encodes this: it clicks y=5 and expects index 2.
   - The picker also truncates with no scrolling, so a selection past the window is invisible.

3. **CONFIRMED – clicks on the mode/setting card are mis-mapped.** Three causes:
   - `ui/screen.rs:393-394` sets `mode_first_row = (top of the whole chrome) + 1`. The stopping, link, lock, compact-off and mode-confirm lines are pushed above the card (`:311`), so the click is off by that many rows.
   - `mode_rows_drawn` counts `mode_choices()` (the *mode* row) for every subject: `/verbosity`, `/diff` and `/models` too.
   - The rows' `why` text wraps under each value, so one line no longer equals one value.
   - Scenario: `/verbosity`, click "loud" → it selects a neighbouring profile, or nothing. With the daemon link line showing, every mode click is shifted.

4. **CONFIRMED – several panes can be open at once, and keys go to one the operator cannot see.** `toggle_jobs`, `toggle_subagents`, `toggle_todos`, `toggle_standing` and `/queue` flip only their own flag (`app/panes.rs:22,38`, `todos.rs:279`, `standing.rs:125`, `commands.rs:699`).
   - Draw order is: todos > queue > standing > subagents > jobs.
   - Key order is: subagents > todos > queue > jobs > standing.
   - Scenario: ctrl-t then ctrl-g. The todos pane is drawn, but ↑↓ and Enter go to the hidden subagents list. Enter *switches session* into a subagent row nobody saw.
   - The shared `pane_scroll` also makes `scroll_into_view` jump the visible pane.
   - Same class: `editor_chord` (ctrl-], `app/editor.rs:267`) opens rano over an open jobs pane. After ctrl-] back to the composer, the hint says "enter sends", but Enter is swallowed by the hidden jobs pane (`keys/mod.rs:~392` swallow list). Only `key_standing_pane` guards on `!editor_drawn()`.

5. **CONFIRMED – the diff popup blocks the permission card.** `key_diff_popup` (`keys/mod.rs:132`) runs before `key_decision` (`:196`) and returns `Break` for every key. Its own comment says cards stay answerable.
   - Scenario: click an edit row, and a permission decision arrives. The card is drawn in the chrome, and the hint bar shows "Deciding… a row number answers". But digits, ↑↓ and Enter scroll or vanish. All ctrl chords are dead too, while the hint bar still lists them, because `hint_bar` has no diff-popup branch.

6. **CONFIRMED – scrolling in the queue pane moves the hidden transcript.** The wheel/PgUp pane list in `app/keys/chords.rs:270` omits `queue_pane` and `queue_open`.
   - Scenario: wheel or PgUp in the queue pane or entry overlay scrolls the conversation underneath and parks it at "holding". After Esc the operator lands in scrollback.
   - `key_queue_review`'s PageUp/PageDown arm (`panes.rs:182`) is dead code, because chords run first.

7. **CONFIRMED (code path); effect PLAUSIBLE – the `!term` pane gets raw mouse reports.** rano enables `?1002`+`?1006` (button and drag tracking) by default. `pane_keys` (`keys/mod.rs:719`) forwards `raw_input()` verbatim, including SGR mouse sequences.
   - This contradicts `crates/vt/src/screen.rs:27`: "the head must not hand a program the operator's mouse".
   - Scenario: wheel over `!term bash` → the shell gets `ESC[<64;x;yM`, and drags give a stream of them, so junk is likely typed at the prompt.

8. **CONFIRMED – the wheel ignores where the pointer is.** `key_of` drops the wheel's x/y. Chords pick the target by priority:
   - a decision card with overflowing content takes the wheel anywhere on screen;
   - the ctrl-v window takes it anywhere;
   - the session picker and mode card swallow it and do nothing.
   - Scenario: hovering over the transcript and scrolling moves the card's evidence instead.

9. **CONFIRMED – any wheel or click cancels allow-all.** `key_mode_confirm` (`app/keys/cards.rs:25`) treats *any* key, including `WheelUp` and `Click`, as "no".
   - Scenario: `/mode allow-all`, then scroll to reread → "allow-all cancelled".

10. **CONFIRMED – which surfaces take clicks is inconsistent.**
    - Clickable: the session picker, mode card, todos pane, queue pane, the count labels, edit rows, and rano.
    - Not clickable: the subagents, jobs, standing and config panes, the decision card, the quit card. This is despite `subagents_stop_rows`, `standing_stop_rows` and the jobs row maps already existing.
    - The todos (`panes.rs:~505`), queue (`:~798`), picker and pick card handlers return `Break` on *every* click, hit or miss. So the subagents/jobs count labels (`keys/mod.rs:245`) stop working while those are open, yet work while the jobs or subagents pane is open.

11. **CONFIRMED – who owns Enter is decided differently per pane.**
    - The "pane owns Enter" list (`keys/mod.rs:~392`) leaves out `queue_pane`. With an empty queue, Enter with typed text sends the prompt. With a non-empty queue, the same Enter opens an entry.
    - Todos and the picker guard on an empty composer; config always takes Enter.

12. **CONFIRMED – help and hint text disagree with the bindings.**
    - Help (`ui/panes/help.rs:99`) says "a click on an edit or write row opens that one" in rano. It actually opens the diff popup (`keys/mod.rs:~144`).
    - The hint bar (rano `agent/hint_bar.rs:96`) says "ctrl-n notes". ctrl-n *retires every note* (`chords.rs:162`), and the standing-notes pane has no chord.
    - Help never mentions ctrl-g, ctrl-q, ctrl-o, `/queue`, `/jobs`, `/todos` or `/subagents`.
    - Help says "wheel scrolls the transcript", which finding 8 shows is not true.
    - The hint bar has no mode for `slash_out`, `sub_out`, the diff popup or the payload window. With `sub_out` open over the subagents pane, the bar shows the list's "enter (or o) opens a subagent". Enter actually re-peeks, and `o` falls through to switching session.

13. **CONFIRMED – `/sessions` and ctrl-s open the picker differently.** `/sessions` (`commands.rs:510`) sets `picker=true` but does not seed `picker_sel` to the current session and does not clear `pick`. ctrl-s (`chords.rs`) does both.
    - Scenario: `/verbosity` then `/sessions` → both cards are up, with a cursor left over from an earlier list.

14. **PLAUSIBLE – a click in the same read as a wheel uses mixed positions.** All keys from one ~100 ms read are handled before the next `screen()`.
    - `todo_stop_at_row` and `queue_stop_at_row` combine *updated* `pane_scroll` with *last frame's* stop rows, and `file_rows` stays in last frame's coordinates after `scroll_up`.
    - Scenario: trackpad momentum scrolling followed by a click selects a row about 3 off, or opens the wrong file's diff.

15. **PLAUSIBLE, low – `editor_drawn()` ignores the diff popup.** It (`app/editor.rs:198`) does not account for `diff_popup`, which `main_area` draws first. If both are ever open, `route()` would send mouse and keys to the hidden rano instead of the popup.

##### Structural recommendation

- **One surface stack.** Replace the pane booleans with a `Vec<Surface>` (or a single `focus: Surface`) whose top is drawn *and* receives keys. Opening a pane pushes it or replaces the current one, and Esc pops. Draw order, key order and hint mode then all read the same value, which removes findings 4, 5, 6, 11, 12 and 13 as a class.
- **Hit-test against what was last drawn.** Each frame, `main_area` and the chrome push `(Rect, SurfaceId, Vec<(row, StopId)>)` into a `last_layout` that already accounts for scroll. Mouse routing is then one function: find the topmost rect containing (x, y), convert to local coordinates, hand it to that surface. No surface recomputes header, card or line arithmetic. Keep x/y on wheel events so scrolling goes to the rect under the pointer. A click that hits nothing falls through instead of being swallowed. Forward mouse reports to `!term` only if the program asked for mouse mode, translated to the pane's own coordinates.
- **Per-surface keymap tables.** Each surface declares `&[(Key, guard, Action, hint_text)]`. Plain-letter bindings always carry the empty-composer guard. The hint bar and help are generated from the table of the focused surface. Slash commands and chords call the same `open_surface(..)` function, so `/sessions` and ctrl-s cannot drift apart.
- **Re-snapshot the layout between keys in a batch**, or drop clicks that follow a scroll-changing key within the same read.
- **Fix the tests that encode the bugs.** `tests/session.rs:1762` (the picker) currently asserts the wrong row.

### The daemon: session lifecycle (crates/harnessd, crates/sessionlog)

I reviewed the daemon on main at 435bb88 by reading the code only; nothing was run. Paths below are relative to `crates/` unless they start with `tools/`. I found 15 problems, worst first.

#### Findings

**1. CONFIRMED: the worker can open a second Harness on a live subagent.** `harnessd/src/sessions.rs` ~3074 (`dispatch`, `Prompt` arm) → `run_prompt` → `harness_as` (~1564).
- A child's hub shares one command queue between two readers: the child's thread (`Hub::take_own_work_until`, hub.rs:1659) and the daemon worker (`Hub::try_command`, hub.rs:1772, which pops anything).
- The `Interrupt` (~3286) and `Message` (~3620) arms hand the command back with `give_back_to_its_own_reader`. `Prompt` does not.
- So when a head prompts a subagent it is attached to, the worker can win the race. `harness_as` then builds a second `Harness` for that session id, because `open` never held it.
- Result: two writers on one transcript and store row. The second open also replaces the child's `watch_runs`, `set_prompt` and `notes` entries in the Registry and starts another deadline thread.

**2. CONFIRMED: a subagent's hub can be left in the Registry with no reader, so later wakes and messages are lost.** harness.rs:15350–15475 (`run_to_completion`). After `adopt`, three exits leave the hub registered with nobody reading it, and nothing ever closes it:
- the first turn fails (the `?` on `submit_as_a_normal_session`);
- a non-Host (VM) placement returns before `serve_child`;
- a `Stop` makes `serve_child` return (harness.rs ~13075).

Then `wake_route` (sessions.rs:748) answers `ItsOwnReader`, and `Sessions::wake` returns `HandedOn` into a void. The same happens to `Message` and `Interrupt` handed back to the hub. The parent's `task_message` looks delivered, and a head's prompt falls through to finding 1.

**3. CONFIRMED: two threads per finished Host subagent are never released, which matches R66's 7 → 19.**
- Thread one is the `subagent-*` thread, parked in `serve_child` until its hub closes. Nothing closes a child hub before daemon exit.
- Thread two is `tools/src/exec/host.rs:629`, the `Deadlines` watchdog (`letibot-deadline`). It holds its own `Arc<Deadlines>` and `run()` (host.rs:660) is an endless `loop` with no exit, so one per `HostProcesses` lives forever even after the host is dropped.
- Each parked child also keeps its whole `Harness` alive: transcript, ledger, `HostProcesses` with up to 8 MiB of capture per job, and a lazily opened SQLite connection in `JobRecorder` (jobwatch.rs:757).

**4. CONFIRMED: per-session tables only grow.**
- `Registry.inner.entries` (registry.rs:1001 `adopt`, 940 `create_under`) has no remove anywhere. Every subagent's `Hub` (log and view) lives for the daemon's life, and every `get` is a linear scan under the registry lock.
- `Registry.runs` (`watch_runs`, harness.rs:2602) captures `Arc<HostProcesses>` per session. So even a child whose thread exited keeps its job table and captures reachable until `close`.
- `Registry.prompts` and `Registry.notes` are per session and never removed.
- In `Sessions`, `open`, `slots`, `seated`, `fabric_seen`, `nags`, `bangs`, `armed` and `wall_gave_up` have no removal. Every root session ever resumed keeps a full Harness and a `monitor-wake-*` thread (sessions.rs:2840).
- In `HostProcesses`: `jobs` (host.rs:1628), `reaps` (1124), `promotions` (1129) and `Deadlines.fired` are push-only. Job lookup is a linear `iter()` (1740).

**5. CONFIRMED: the idle sweep is quadratic and runs over every open session on the worker.**
- `abandoned_calls` (harness.rs:20292) checks every asked call against the whole answered list (`answered.iter().any`): O(calls × results) over the full in-memory transcript.
- `Sessions::sweep_abandoned_calls` (sessions.rs:2367) runs it for every held session, 2 s after every command or wake.
- The tree had about 15k bash results, so that is on the order of 10⁸ string compares per sweep for one big session. That is the same shape of worker stall as the `Store::session` loop.

**6. CONFIRMED: the turn tails have diverged.**
- `Sessions::deliver_due_nags` (sessions.rs:2379) runs a turn on the idle arm but calls neither `arm_sweep` nor `arm_wake`. `sweep_at` was set to `None` just before (2367), so a call stranded by a nag turn is never swept. This is the wake bug again, on the third door.
- `arm_wake` is called by `run_prompt`, `run_a_message_from_a_session` and `run_continuation`. It is not called by `wake()` (2915), `deliver_due_nags`, `OperatorShellResult` (~3560) or `run_operator_shell_here`. A monitor first declared in one of those turns gets no waiter until the next prompt.
- Children use a second copy of the tail, `Harness::submit_spoken_as_a_normal_session` (harness.rs:8232), plus `serve_child`:
  - the `Wake` and `TimedOut` arms (~12955, ~12985) have no forced compaction when a wake or nag turn hits the wall;
  - children are never swept, because the sweep iterates only `Sessions.open`;
  - `take_new_title` is drained only by `Sessions::publish_title`, so the Registry title is never updated for a child;
  - the daemon's `wall_gave_up` latch and its wording do not exist on the child path.
- The `Answer` arm (harness.rs ~13017) does `.is_ok()` and drops the error, so `turn_failed` carries no reason.

**7. CONFIRMED: `!` stops working for a session after one failed settle.** sessions.rs ~3525 (`OperatorShellResult`). If `settle_operator_shell` returns `Err`, the arm returns `Failed` before `clear_bang_in_flight`. `bangrun::State` then stays in flight, and every later `!` line is queued (`queue_bang_line`) and never runs. The error goes to stderr only.

**8. CONFIRMED: busy traffic starves the idle work.** registry.rs:183 (`Bell::next_any_until`) drains opens, then commands, then wakes, and checks the deadline only when all three are empty. Wakes are not de-duplicated (`ring_wake` pushes every time).
- Under sustained wakes (children settling, monitors, `HandedOn` relays) `WorkOrIdle::Idle` never fires. Nags, the sweep, `ensure_gatekeeper` and `serve_reviews` all wait.
- Each `Harness::wake` (harness.rs:8008) also reads the store: `serve_reviews` reads all reviews, and `queue_notices` and `handover_notices` each read all merge entries. A wake storm becomes a store-scan storm on the worker.

**9. CONFIRMED: `Registry::list()` scans the whole store on the worker.** registry.rs:1432 calls `SessionSource::list` → `Store::list_sessions`, a correlated-subquery scan over all sessions and their transcript items.
- Worker callers: `is_root` (sessions.rs:715) and the parent lookup (~1580), both on every open; `attach_session` (~926); `ensure_gatekeeper` (~2292); `child_todos` `resolve` and `my_children` during turns.
- Opening one session costs 2–3 full scans. The `StoreSessions.store` mutex is held for each scan, and every head's list frame (server.rs) takes the same mutex, so a picker refresh and a worker open serialize.
- `Registry::created_ms` and `wiring` already exist as cheap lookups. A `parent_of` lookup in the same style is missing.

**10. CONFIRMED: one store error stops the merge queue for the rest of the daemon's life.** mergequeue.rs:4115/4121 (`self.recover()?`, `self.step()?`). Any `StoreError` (for example SQLITE_BUSY against another connection) ends `run`, and spawn (4196) only prints to stderr. The queue stops serving and no hub is told.

**11. CONFIRMED: failures that never reach a hub.**
- `Harness::sweep_abandoned_calls` (harness.rs:5228): `let _ = self.append_results(...)`, and the hub block at 5222 is dead code.
- `JobRecorder` `put_job` (jobwatch.rs:776, 813): `let _`. This is the row restart-time `due_todo_rows` depends on.
- `flush_todos` failures (harness.rs:7044, 7056, 7704, 8122) are stderr only. At 8122 a fired condition that is not persisted fires again after a restart.
- `StoreSessions::merge_entries` and `merge_reviews` (sessions.rs:4247, 4266) print and return an empty list. The comment above them says this is "said out loud rather than answered as empty", but the pane shows an empty queue.
- `OperatorCall` failures, "not open" failures, and `run_operator_shell_here` fallback lines (sessions.rs ~3460, 3497, 3538) reach only `on_reply` → `eprintln` (cli.rs:1761).
- A child's `ChildCommand::Leave` (harness.rs ~13093): `!`, `/slash`, `/mode` or a todo edit sent to a subagent session is dropped with a stderr line, and the head attached to it sees nothing.
- daemon.rs: the `ensure_gatekeeper` error, sweep counts and nag counts are stderr only. The nag *failure* is published by `after_turn`.

**12. PLAUSIBLE: a gatekeeper session that fails to open is retried every 30 s and may leak each time.** sessions.rs:2252. When `open_as` fails after `registry.create`, every 30 s review pass retries through the `list()` branch. Each retry pays a store scan and publishes another `resume_failed`. If `Harness::open_with_registry` fails after building its `HostProcesses`, each retry may also leak a deadline thread.

**13. PLAUSIBLE: wakes for a root session that failed to open are lost.** A failed `Work::Open` leaves the hub registered and not held. Any later ring for it is routed `ItsOwnReader` and reported `HandedOn`, though nobody reads that hub.

**14. Lock ordering: no deadlock found.** Registry → Hub is always collect-then-release (`list`, `broadcast`, `close`, `next_work_until`), and the hub rings its bell outside its lock. The real contention is the `StoreSessions.store` mutex held across full SQL scans (finding 9). `Registry::close` runs `list()` (a scan) on the signal thread, which is harmless.

**15. Minor:** `seat_tool` inserts into `slots` before the open can fail, and `detach_seat` clears `seated` but not `slots`.

#### Structural changes that remove the most classes of bug

1. **One session lifecycle with an explicit close.** `Registry::remove(id)` takes away the entry, `runs`, `prompts` and `notes`. `Sessions::close(id)` drops the Harness and its map entries. Every exit of a child thread closes and removes its hub through a single guard. Add an idle-child reap policy, or park a child as store-only so a later prompt reopens it. This fixes findings 2, 3, 4 and 13.
2. **One owner per session, enforced in the hub.** A hub carries an owner tag (worker or own-thread). `try_command` refuses hubs it does not own, so the worker cannot pop a child's commands, and `harness_as` refuses a session that is live under another owner. Fixes 1.
3. **One turn tail for every door, shared by the daemon and children.** Move the before/after sequence (pre-wall compact, title, `arm_wake`, `arm_sweep`, failure publish, refused-compaction, continuation loop, nag rearm, bang-state cleanup on any exit) into a single function both `Sessions` and `serve_child` call. Fixes 6 and 7 and stops the drift.
4. **Get store and git work off the worker, and make the scans cheap.** Give `Registry` a `parent_of` and `title_of` from its own entries. Make `Harness::wake`'s store reads incremental (keyed by row or version) instead of full-table. Make `abandoned_calls` use a `HashSet` over items since the last sweep. Coalesce wakes per session in the bell. Give the idle clock a turn that cannot be starved. Fixes 5, 8 and 9.
5. **A single "report" sink instead of `eprintln!` and `let _`.** One `report(session, code, detail)` that writes stderr and the hub (a daemon-level broadcast when there is no session). Make the merge queue and job recorder retry or announce rather than die or drop. Fixes 10 and 11.

### The merge/review queue: why it is unreliable

#### Why the merge queue is unreliable: findings for R61

##### (a) What actually happens, from the store, git and the log

**Store** (`merge_queue`: 22 rows; `merge_review`: 21 rows). Commit e7798d9 cleared the table once, so the history before 10-10 is gone. The table also keeps only the latest evidence, so how long a row spent in each state cannot be reconstructed.

- **Landed: 9.** Only **1** of them went through the queue's own gate (`agent/thinking-merge`, 23:27 on 10-10, gate about 6.7 min). The other 8 say *"landed without the queue's gate"*: the queue had given them back to the project, and the project session merged them itself.
- **Failed: 10.**
  - 9 are pg-noop entries. Each had 3 reviewer attempts, and all failed with `store schema is version 23; this build knows 21` (a binary older than the store). They sat 24–43 h before failing.
  - 1 is an empty branch (`needs_human`).
- **Stale: 1.** `group-the-queue-messages` was accepted at 22:41, then marked stale at 09:15 the next morning ("gate job died with the daemon"). A later-accepted branch of the same priority landed in between.
- **Waiting: 2.**
  - `a-pass-reads-one-row` has empty evidence and no review row, and has sat about 11 h. No daemon for `/home/dead/Projects/letibot` is running, so nothing owns it.
  - `queue-spin` has a **reject** verdict (empty branch) but is still `waiting`. The refusal hit a daemon that was shutting down. Its branch now has commits, and the same work already reached main by hand as c12d044.

**Reviews:** 17 of 21 never ran.
- 9 failed on the schema mismatch.
- 8 failed because the firecode VM refused to copy a 72.8 GiB workspace (8 GiB ceiling).
- 4 got a verdict: 2 accept, 1 reject, 1 needs_human.
- 148 `gatekeeper` sessions were spawned for these 21 rows.

**Main:** 227 first-parent commits since 10-06, 36 of them since the queue's first landing on 10-10. The queue's gate landed 1 of those.

**Log** (`~/logs/harnessd.log`):
- `merge queue: the queue could not be served: sqlite: database is locked` appears 3 times. Each one is the queue thread ending for good.
- 41 refusals, 9 conflicts, 5 gate failures.
- "landed <same sha>" repeated 7 and 10 times in a row: these were reset rows recognised as already on main, not merges.

**Processes:** 9 `harnessd` daemons share one `sessions.db`, all running deleted binaries of **different builds** (started 09-21 through 10-11). The 10-07 (Leonid) and 10-09 01:28/01:38 (rano, firecode) builds have a merge queue with **no claim and no workspace filter** (both arrived in 837bd59, 10-09 20:37).

**Git history:** 39 commits touch the three files. They cluster into recurring bug classes:
- **Reviewer can't start or is placed wrong** (VM, copy source, fixture): e70245c, a07523a, 8912a8f, 4ebdd66.
- **Which daemon owns an entry:** 837bd59, b1adf2a, 5e53434, 7bc3f46.
- **Verdict delivery:** a1974c0, c05b9c9, 08c6742, 3d36cea, 233939d.
- **Stuck rows and manual escape hatches:** 4c53014, 8178c4e, 424c212, e9eb358.
- **Landing correctness:** a40a9f6 (two branches of real work were deleted), 2659cca, c22d07d, 9f71b36.
- **Performance:** c12d044.
- Separately, 5 merges on main say "landed by hand on the operator's instruction".

##### (b) Findings, ranked by severity

1. **One store, many daemons, mixed builds, no single owner.** `cli.rs:1080` spawns a queue in every daemon. Old builds (e.g. `46642a3:mergequeue.rs` `step`) take any accepted waiting entry with no `claim_merge_entry` and no `serves`, then fast-forward `self.repo`, which is *their own* workspace. *Scenario:* the rano daemon, alive since 10-09, takes a letibot entry, rebases it and runs a gate, then `merge --ff-only` fails in the wrong repo and the row goes `failed`, or it races the real owner. The schema check stops old binaries from *opening* the store, but not ones already running. **CONFIRMED** (process list and old code); actual interference is PLAUSIBLE.

2. **Any store error kills the queue thread for good.** `mergequeue.rs:4121` does `match self.step()?`, so `run` returns on the first error and `spawn` prints one line. *Scenario:* `database is locked` (3 times in the log) or an unknown state word (`store.rs:1436`, e.g. `vetoed` read by an older build) leaves a daemon running with no queue. The pane still draws the rows as if they were being served. **CONFIRMED.**

3. **An entry with no running daemon is silently orphaned.** `served_by` (`mergequeue.rs:714`) only lets the daemon whose workspace equals the root session's workspace serve an entry. *Scenario:* `a-pass-reads-one-row` has sat `waiting` for 11 h with empty evidence. The pane counts it "in review", although no reviewer was ever asked. **CONFIRMED.**

4. **Nothing in the row moves can enforce a state change.** `put_merge_entry` (`store.rs:3608`) overwrites the whole row with no expected-state check. `move_to` writes from a snapshot taken at the start of the pass. `MergeQueueDaemon::recover` (`mergequeue.rs:3522`) turns *every* `Taken` row in the store into `Stale`, not only this daemon's. *Scenario:* daemon B starts while daemon A is gating entry X, so X shows `stale`. A then writes `Landed`. Or a review-pass write of `Waiting` from a stale snapshot undoes another actor's `Taken`, and X is claimed twice. The `group-the-queue-messages` timeline fits this. **CONFIRMED** (code); the incident itself is PLAUSIBLE.

5. **The verdict is not tied to a commit.** `merge_review` and `merge_queue` store `base_sha` only, never the branch head that was reviewed. `review_gate` (`mergequeue.rs:1400`) accepts on the word alone. *Scenario:* the reviewer accepts tip T, the child commits T′, and the queue lands T′ without review. Conversely, `queue-spin` was rejected as "no change", got commits afterwards, and its reject still stands. **CONFIRMED.**

6. **The gate has no time limit and cannot be stopped.** `run_captured_env` (`mergequeue.rs:516`) uses `Command::output()`: no timeout, no process group, output held in memory. `step` (`:3874`) runs the gate on the queue thread and never checks `stop`. `cli.rs:1813` detaches the thread on exit. *Scenario:* a hung step (an interactive `act`, a test waiting on the model server) keeps the row `taken` with no end. On restart the row goes `stale`, the orphaned `cargo test` keeps running, and it holds the worktree's `target` lock. **CONFIRMED** (code).

7. **Landing happens in the operator's own checkout, after the install step.** `fast_forward_main` (`:385`) runs `git -C <repo> merge --ff-only`, which moves whatever branch is checked out and touches the live working tree. It then records `rev-parse main` as the landed sha. The gate's `install-gate-build.sh` step installs the binary *before* the fast-forward and the push.
   - If main moved during the ~7 min gate, the fast-forward fails and the entry is parked `Failed` instead of rebased again. With 36 hand commits in this window, that is likely.
   - If the push fails after the fast-forward, local main has the branch but the row says `Failed`.
   - Either way the installed binary is code that is not on main.

   **CONFIRMED** (code).

8. **Red main is never detected.** Nothing ever runs the gate on main itself. *Scenario:* main goes red, so every entry's gate fails and is blamed on the branch, and each one parks `Failed` one after another. **CONFIRMED.**

9. **The review queue is one global session served by every daemon.** `gatekeeper.rs:162` sets `SESSION = "s-gatekeeper"` for the whole store. `daemon.rs:216` calls `ensure_gatekeeper` in every daemon. `serve_reviews` (`harness.rs:7465`) filters by `session_id == me`, not by workspace, and `is_open_for` is per-process. *Scenario:* two new-build daemons each open their own copy of `s-gatekeeper` and each spawn a reviewer for the same entry, both writing one row and one transcript. **PLAUSIBLE** (only one new-build daemon is live right now).

10. **Reviewers have no deadline and their retries never stop.** `review_outcome` (`harness.rs:1129`) treats `Running` as "still waiting" forever. A child that vanished (`Unknown`) writes nothing, so the next pass spawns another, and that attempt is never counted. *Scenario:* 148 reviewer sessions for 21 rows; a stuck reviewer holds its entry indefinitely. **CONFIRMED** (code and counts).

11. **The fallback became the normal path.** Infrastructure failures (VM placement, wrong build) use up 3 attempts in about 5 min. `hand_over` then parks the row `Failed` and gives it to the project session, which merges by hand without the gate. *Scenario:* 8 of the 9 letibot landings went this way. **CONFIRMED.**

12. **The pane cannot show why an entry is stuck.** `sessions.rs:4225` returns an empty queue when the read fails, which looks like an empty queue. The age shown is time since creation (`tui/.../queue.rs:152`), not time in the current state. No row says which daemon owns it or whether that daemon (or its queue thread) is alive. Live updates come only from the attached daemon's own moves. Evidence is a single field that gets overwritten, with no transition history. **CONFIRMED.**

##### (c) Five requirements R61 has to meet

1. **Exactly one owner per queue, with a live heartbeat.** A lease row (owner pid, build, heartbeat time) that is claimed with a compare-and-swap. Every row shows its owner and how old the heartbeat is. A daemon whose build does not match the store does not serve any queue. Recovery only touches rows whose lease it held, or whose lease has expired.
2. **Every state change is a conditional move with history.** `UPDATE … WHERE state = expected AND owner = me`. Each transition is appended to an event table (from, to, at, by, why), so time in each state and the reason a row is stuck can be read directly. No whole-row upserts.
3. **The verdict is bound to a commit.** The review records the head SHA it judged. The merge queue lands only that SHA; if the branch moved, it goes back to review. The landing runs in a dedicated worktree (or ref update and push) with a compare-and-swap on main's SHA. If main moved, rebase and gate again rather than fail. Install only after the push succeeds.
4. **Every external step has a time limit, can be killed, and has its errors handled.** The gate and each reviewer get a deadline and run in a process group that is killed on timeout or shutdown. Timeout and infrastructure failure are outcomes of their own, separate from "branch is bad". Store errors are retried with backoff, never end the thread, and a dead thread is visible in the pane.
5. **Red main is first-class, and infrastructure failure never silently routes around the queue.** Gate main's tip whenever it changes; while red, land nothing and put main's tip first in review. A reviewer that cannot start should leave the entry visibly blocked on infrastructure at high severity, not hand it to a model session to merge without the gate.

### The tests: layout, counts, coverage, flakiness

#### Letibot test review (main @ 435bb88, read-only, nothing edited)

I counted tests with grep for `#[test]` / `#[tokio::test]`. I did not use `cargo --list`, to avoid a full workspace compile. No property-test or rstest macros exist, so one attribute is one test.

##### 1. Inventory

| crate | unit (src/) | integration (tests/) |
|---|---|---|
| tools | 944 | 146 |
| tui | 663 | 20 |
| harnessd | 467 | 160 |
| sessionlog | 146 | 78 |
| turn | 113 | 33 |
| tokencore | 80 | 0 |
| provider | 54 | 4 |
| letibot | 50 | 4 |
| ui 47, code 39, flowy 38/16, vt 36, dialect-qwen 30, transcript 22, webfetch 22, llama 0/20, dialect-glm 10/12, others under 10 | | |
| **Total** | **~2,780** | **~505** |

That is about 3,285 test functions and about 14,000 `assert*!` lines.

Test code is about 154k of about 349k Rust lines (44%):
- 73.9k lines in inline `mod tests { }` blocks
- 39.5k lines in out-of-line test modules
- 40.5k lines in `tests/*.rs`

**Top 10 files by inline-test lines** (test lines / file lines / tests):
1. `harnessd/src/mergequeue.rs`: 6,070 / 11,281 / 91 (54% test)
2. `harnessd/src/harness.rs`: 4,390 / 20,765 / 101 (`mod tests` at :16534)
3. `tokencore/src/store.rs`: 3,798 / 8,211 / 56
4. `tools/src/intent.rs`: 3,697 / 9,091 / 113
5. `tools/src/adjudicate.rs`: 3,573 / 7,583 / 72
6. `tools/src/builtins/todo.rs`: 3,312 / 5,879 / 68 (56%)
7. `letibot/src/launcher.rs`: 2,115 / 2,312 / 43 (91% test)
8. `harnessd/src/config.rs`: 1,743 / 4,749 / 47
9. `harnessd/src/sessions.rs`: 1,470 / 5,802 / 33
10. `tools/src/authorise.rs`: 1,306 / 3,691 / 41

`tui/src/app.rs` no longer exists. It became `app/mod.rs` (2,006 lines), and its tests are already split out: `tui/src/app/tests/` is 27 files, 38,395 lines. The largest are `transcript.rs` (7,789), `scroll.rs` (3,660) and `todos.rs` (2,525).

##### 2. What Rust convention says, and a rule for this repo

- **Unit tests** go in `#[cfg(test)] mod tests` inside the module. They can see private items, and that is the idiomatic default.
- **Integration tests** go in `tests/*.rs`. They see only the public API, and each file is its own binary, so compile and link cost grows with the number of files.
- **Middle path:** `#[cfg(test)] mod tests;` plus a sibling `foo/tests.rs`, or `#[path = "foo_tests.rs"]`. Private access stays the same; only the file location changes. std and rustc use this widely. It is not "moving tests out" in the integration-test sense.

The repo already uses the middle path in three places:
- `harnessd/src/standing_notes.rs:579` → `standing_notes/tests.rs`
- `harnessd/src/notes_offer.rs:526` → `notes_offer/tests.rs`
- `tui/src/app/mod.rs:2006` → `app/tests/{mod,…}.rs`, split by topic

`#[path]` is only used to share `turn/tests/support/canned.rs` between integration binaries.

**Recommended rule:** when a file's inline test block passes about 800 lines, or about 40% of the file, it moves to `<module>/tests.rs`. When that file passes about 2,000 lines, split it by topic the way `app/tests/` is. Add a new `tests/*.rs` file only for real public-API or end-to-end checks.

**Split first:**
1. mergequeue.rs (6k test lines, 33 recent commits)
2. harness.rs (4.4k; 156 commits since 09-25, the hottest file, so the most diff noise)
3. todo.rs
4. adjudicate.rs
5. intent.rs
6. store.rs
7. launcher.rs (91% test)

These are pure moves, so the test count should be identical before and after. Check it with `cargo test -p X --lib -- --list | wc -l`.

##### 3. Comparison with leticl

leticl uses **FiveAM** (`tests/package.lisp`). Its tests are 786 `def-test` forms in a single 28,413-line `tests/tests.lisp`, plus 7 in `tests/replay.lisp`, for **793 tests**. Those contain 4,793 `is`/`signals`/`finishes` forms (only 2 use `for-all`). The "7k+" is FiveAM's executed-checks count: `TODO.md:71` says "HEAD is green (41d8935, 7043 checks)". The 313 `deftest` hits are vendored alexandria/anaphora code.

So the numbers measure different things:
- **leticl:** about 793 tests, about 7,000 assertions run (roughly 9 per test).
- **letibot:** about 3,285 tests, about 14,000 assertion sites. The run-time count would be higher, because many sit in loops (for example the C0 table in `tui/src/app/keys/input.rs`).

The comparable letibot figure is the ~14k assertions, not 3.3k, so letibot is not behind. The gap is granularity: letibot writes many narrow, named behavioural tests, and leticl writes fewer, fatter ones. Neither count says anything about coverage, which depends on where the tests sit (section 4). If you want a like-for-like number, measure line coverage with `cargo llvm-cov`.

##### 4. Coverage gaps for stabilisation

Since 2026-09-25 there were 738 commits. 56 of the 299 that touched the TUI, merge queue or daemon added no test, though most of the big ones were the 10-08 TUI module-split refactors.

- **TUI input dispatch.** `app/keys/mod.rs` (1,332 lines, 15 commits), `keys/panes.rs` (896 lines, 25 per-pane `key_*` handlers) and `app/events/mod.rs` (1,260 lines) have no tests of their own. They are only exercised through `app/tests/*`. No test checks which pane gets the keyboard when several overlays or panes are open: precedence is the order of the `key_*` calls in `key()`. "focus" appears in only 7 test files, 12 of the hits in `editor.rs`.
- **Mouse.**
  - The decoder is well pinned: `keys/input.rs:264-277` covers wheel, left press, release and modifier rejection.
  - Click handlers sit at `keys/mod.rs:144,244`, `panes.rs:294,505,690,798` and `cards.rs:498`. Tests exist for the picker (`session.rs:1762`), the count labels (`screen.rs:384-636`), todos (`todos.rs:293-332`), settings, the editor and the queue strip (`queue.rs:1049`).
  - **No click test found** for queue rows (`panes.rs:798`). `cards.rs:498` (`key_pick`) is only reached indirectly via settings.
  - The real driver path is untested: `tui/src/driver.rs` (1,210 lines, 19 commits) has 4 tests, all about child processes. Mouse enable/disable and the stdin-to-`Key` loop have none.
  - Render-side hit maps have no tests of their own: `ui/screen.rs` (847 lines, 27 commits), `ui/panes/queue.rs` (880 lines) and `ui/transcript/window.rs` (1,152 lines).
- **Merge queue.** Best covered of the three: 91 tests, a real-repo daemon test (`the_thread_serves_the_queue_and_stops`, :8615) and a no-double-spawn-on-restart test (:9411). The risk is file size, not test absence.
- **Daemon worker loop.**
  - `harnessd/src/daemon.rs` `Daemon::run` (:162-335, signal-fd, 7 commits) has 0 unit tests. Only 8 integration files call `Daemon::serve`.
  - `cli.rs` (3,081 lines, 33 commits incl. today's memory-cap scope) has 13 tests, all for argument and query helpers.
  - In `sessionlog`, `server.rs` (2,010 lines, 23 commits) has 3 tests and `client.rs` has 2.
  - `turn/src/engine.rs` (2,074 lines) has 1 inline test.

##### 5. Flaky-test smells

- **Env vars changed without a lock** (edition 2024, so these are in `unsafe` blocks):
  - `tools/src/builtins/task.rs:2101,2131` sets `FIRECODE_BIN`, which `tools/src/firecode.rs:73` reads from any concurrent test in the tools lib binary.
  - `turn/src/serving.rs:587` sets `LETIBOT_COMPLETION_URL`.
  - `letibot/src/roles.rs:332-341` sets `LETIBOT_SOCKET`/`LETIBOT_SESSION`.
  - `websearch/src/lib.rs:298` sets `BRAVE_*`.
  - `tokencore/src/apparatus.rs:189-223` toggles `LETIBOT_REQUIRE_APPARATUS` and `LETIBOT_VOCAB_GGUF`.
  - By contrast, `confine.rs`, `host.rs` and several `tests/*.rs` do hold a `static LOCK: Mutex<()>`.
- **Silent skips that the require-switch cannot catch.** `tools/tests/confine.rs` (13 early `return;` across 23 tests), `notscoped.rs:55`, `memory_cap.rs:49` and `src/exec/scope.rs` skip with `eprintln!` + `return`. libtest hides that output on a pass, and they do not go through `apparatus::present`, so `LETIBOT_REQUIRE_APPARATUS=1` cannot turn a missing cgroup v2 into a failure. On a runner these show green. `apparatus.rs:5-8` itself records 7 cgroup tests among 84 runner failures.
- **Wall-clock upper bounds:** `task.rs:1815` (`< 1s`), `answers.rs:2256` (`< 1s`), `answers.rs:1677` (`< budget/2`), `answers.rs:1723,1908` (`< 5s`). The 1s ones will trip on a loaded box or during a parallel `cargo test`.
- **Sleeps:** 70 `sleep` calls under test dirs, many 10-20 ms polls. `tools/tests/deadline.rs:179,235` arms a 400 ms deadline and asserts `after >= 400ms`; it is safe as a lower bound but slow. `tui/tests/stop_daemon.rs` and `reconnect.rs` poll at 10 ms. Most `recv_timeout(200ms)` uses are inside 10-30 s deadline loops, which is fine.
- **Shared global state:**
  - `tui/tests/frame_allocations.rs:43` uses a global `ALLOCS` counter. It is safe only while the binary holds one test, which it does today; a second test would make it flaky.
  - `tools/src/builtins/pattern.rs:142` is a process-wide `CACHE`.
  - Sockets are named by `SystemTime` nanoseconds plus pid (`harnessd/tests/operator_shell.rs:102`), which is acceptable.




---

---

## T25 — Everything waiting on the operator

Filed on request, so a question does not live only in a scrollback. Continues the D
series. **Two arrived answered in the same message and are recorded as resolved** rather
than dropped, because the answer is the interesting part.

### Open

**D11 — Land the four branches?** session-resume (protocol 4, `--continue`), exec,
intent, outside-world. All green, all rebase cleanly. Landing costs the running daemon
(pid 384248) its live session, because protocol 3 → 4 makes a fresh head refuse it by
name. Every transcript is on disk and `--continue` now genuinely returns the newest.

> **Overtaken 2026-09-10: `PROTOCOL_VERSION` is 6.** The four branches landed and the
> assembly on top of them needed a wire change of its own — `SessionEvent::DenialRaised`,
> §4b's requirement that a refusal reach the operator when it happens. The cost D11 named
> is now paid once for both: a running daemon on 5 loses its live session to a fresh head
> either way. T24's `PromoteJob` was the *other* claimant on 6 (`ProcessHost::promote`'s
> doc specifies the frame verbatim) and it is **not** in this bump — it is a `ClientFrame`
> and needs a head that sends it, whereas a denial is a `SessionEvent` and needs only a
> head that renders it. Adding it later is another integer; the constant is the one line
> every wire-touching branch edits and taking 6 for the thing that was ready is the same
> "coordinate rather than race" D10 settled.

— settled: the four landed and the wire is at 55 (`crates/sessionlog/src/protocol.rs:19`), so
nothing waits on this one.

**D12 — A mounted board makes `todo` gated.** The flowy backing declares
`Access::Network`, so mounting a board without an adjudicator attached makes `todo` refuse
*entirely* — the unmounted local list works, the mounted one does not. Correct by the
rules and possibly intolerable in practice.

> **Sharper as of 2026-09-10, and generalised.** The same shape now decides whether a
> session *opens at all*: a seat whose tools declare `Write`, `Exec` or `Network` and has
> no adjudicator refuses to start, rather than starting and failing every call. That is
> the right end of the trade for `write` and `bash`. For **`say`** it is the D12 problem
> with a bigger blast radius — the planner seat needs an adjudicator because it can talk
> to a room, and a planner that cannot plan without one is exactly *"correct by the rules
> and intolerable in practice"*. The unasked question underneath both: is *reaching the
> fabric the operator already authorised this seat to be on* the same class of act as
> *reaching an arbitrary host*. `Surroundings::seen_hosts` says first contact and second
> contact differ; nothing yet says a **declared** attachment differs from a discovered one.

— settled by `Access::Session`: `todo_write` declares it (`crates/tools/src/builtins/todo.rs:2064`)
and `say` too (`crates/tools/src/builtins/room.rs:138`), and that class is unattended
(`crates/tools/src/schema.rs:129`) — so neither a mounted board nor a planner's `say` needs an
adjudicator to open.

**D13 — `Access::Session`, a new access class.** Declaring the intent tools `Read` would
have reproduced the gap the survey names in grok-build (§1.4). The wire carries access as
a String so nothing breaks, but no head has rendered one. — ✓ fixed in 449f7de0, test
the_description_is_clean_and_fits_the_prompt

**D14 — `loop_closes` needs an exclusive model server.** It drives live inference on
`:8080`; it passes alone in 11–32 s and times out under `cargo test --workspace` whenever
the box is busy. **Three separate agents have now reported it as a possible regression**,
and one of them burned a re-run attributing it. A test that reads as a failure whenever
the box is loaded is a broken instrument. Proposal: gate it behind an env var or `#[ignore]`
so `--workspace` means what it says.

> Fourth data point, 2026-09-10: green in 22.2 s standalone and green under
> `--workspace` on an unloaded box, alongside `turn/tests/live_qwen` (4 tests, 6.0 s).
> Two passes are not evidence against the entry — the failure mode is contention and
> the box was quiet — but they do say the instrument is not broken in some second way,
> which is what a fifth agent would otherwise spend a re-run finding out.

— OPEN: the test is still live whenever `127.0.0.1:8080` answers — `skip_live_test` gates
absence only (`crates/turn/src/serving.rs:70`), there is no `#[ignore]` and no opt-out, and
`agents.md:297` still tells a person which targets to avoid.

**D15 — 13 stale worktrees and one 30-hour orphan tmux** (`nano_test`). Safe to prune the
finished ones; two belong to live agents, so not a blanket sweep. T24 is the mechanism,
this is the backlog it would have prevented.

— OPEN, and not code: nothing in the tree prunes a stray worktree or a stray tmux session, and
the backlog itself is a fact about a box this audit cannot read.

**D16 — `~/bin/letibot` is not version-controlled**, and two real bugs were found in it
tonight (a `--continue` that showed an empty screen, and an `up()` that would have
orphaned a version-skewed daemon holding every session). It belongs in the repo.

— ✓ fixed in f0627c39, test tests/launcher.sh (a shell test: the launcher's own cases, run
against stub daemons).

**D17 — Group 2, subagents and admission.** Held all evening because it lands in
`crates/harnessd/src/sessions.rs`, which session-resume was rewriting. **Now unblocked.**
Its hard requirement is already written: `Fits{ceiling, cost_per_id, pool_free}`, because
436.7 MiB per sequence id charged at allocation makes N subagents an OOM path — and two of
the five surveyed harnesses ship subagents with no ceiling at all.

— OPEN: `Fits{ceiling, cost_per_id, pool_free}` appears nowhere in the tree but this line; what
exists is a depth cap on children (`crates/harnessd/src/harness/children.rs:1847`), and no
memory-cost admission.

**D18 — LSP.** The one gap in the survey's ten that no group covers. Four of five
harnesses have it, the most complete is 14 actions, and it needs a language server per
language. Its own decision, not a group.

— fixed in b5ee36d5 in code, untested beyond the config: `crates/tools/src/builtins/lsp.rs`
spawns a server and pulls its diagnostics, and the only test names the default language list
(`default_config_names_its_languages`).

**D20 — `never_hit` now sees network arguments.** A `web_search` or `github` string that
merely *mentions* `.config/gh` or `.password-store` is denied by the never-write list.
Fail-closed, and a real false positive.

— OPEN: `query` and the other content arguments are exempt now (`CONTENT_ARGUMENTS`,
`crates/tools/src/adjudicate.rs:3321`) with a test
(`the_never_write_list_is_about_paths_and_not_prose`, `crates/tools/src/adjudicate/tests/gate.rs:521`),
but `github`'s `title`/`body` (`crates/tools/src/builtins/external/github.rs:236-240`) are not,
so a body naming `.config/gh` is still refused.

**D21 — Enforcement binds the model, or the seat?** From `docs/closed-loop.md` §9. They
differ when a human is driving, and the operator should not be locked out of their own
restart because the model cannot be trusted with it. Tonight's blocked
`systemctl --user restart` is the worked example.

— OPEN: `docs/closed-loop.md:110` still lists it under §9 Open, and nothing in the tree binds
enforcement to the model or to the seat.

**D22 — The tolerance band: one, or per-operation?** Also §9. How large a deviation is
corrected silently versus faulted to a human is the whole design decision, and both ends
have measured costs. **One data point now exists**: T21.3's nudge is capped at one per
user turn, chosen at its narrowest for want of a measurement rather than because one is
right.

— OPEN: `docs/closed-loop.md:109` still lists it under §9 Open.

**D25 — The console adjudicator reads the daemon's own stdin, and a head cannot answer
it.** New, and the direct consequence of making roles reachable. `--role coder` gets
`ConsoleAdjudicator::stdio` by default, which blocks on stdin: correct and usable for
`harnessd --prompt …` and for a daemon in a foreground terminal, and **not** usable for
the shape people actually run — a daemon in the background with `letibot-tui` attached.
The head's answer affordance is D10, which is specified (`Answer { option, note, free }`,
a protocol bump, an affordance in the head) and not built.

So today a write or exec seat is a foreground thing. The startup disclosure says so; it
is not silent. What it means in practice is that the reachability this strand added is
reachable **from a terminal**, and a head-driven write session waits on D10. Two ways
out and they are not equivalent: wire the adjudicator to the head (D10, correct, costs a
protocol bump and head work), or attach the model adjudicator (D13, which needs an
oracle and a reviewed always-ask list, and is what the survey's 83% is about).

— ✓ fixed in a97d7da4, test a_subagent_in_a_tree_with_no_head_adjudicator_refuses_by_name:
`ClientFrame::Answer` (`crates/sessionlog/src/protocol.rs:629`) and `HeadAdjudicator`
(`crates/harnessd/src/answers.rs:908`) are the head answering, so a write or exec seat is no
longer a foreground-only thing.

**D26 — A wake spends a generation, and nothing bounds a chatty monitor.** T24's wake
turns a firing into a user item and runs a turn. A monitor watching a path that changes
every second would spend the box's slots on itself until its TTL expires. The TTL is the
only bound and it is up to an hour. A firing that should be *noted* rather than *acted
on* has no spelling — and inventing one is a decision about what a monitor is for, not a
fix.

— OPEN: a firing still runs a full turn — `crates/harnessd/src/turn_tail.rs:313-355` rings the
bell and the wake is a turn — and the monitor's own TTL is the only bound.

**D27 — Six adjudication items from the assembly agent, none filed until now.**
Carried out of a subagent's report and into scrollback, which is exactly the failure
T25 exists to prevent. In descending order of how often they bite:
`allow_session` does not stick (likely the `host_other` misclassification breaking a
class-keyed grant); `host_other` misclassification itself; `/mode` and `--mode` are
unwired; mode does not persist per project; a resumed session does not disclose a
tool-list mismatch; and the write prompt has no one-keypress answer -- the operator
had to type the whole word, and mistype it. Operator, 2026-09-10: *"ok so got the
write prompt, but it wasnt as a choice but as sometihng i have to type (and
mistype) myself"*.

**2026-09-13 (D13):** two of the six are now wired — *mode does not persist per
project* and the mode side of *`/mode` and `--mode` are unwired*. The per-project
`ModeStore` is threaded into `Parts` and consulted in `open_with` after the
workspace is resolved, so a session's point is its project's row (longest ancestor
wins), not the daemon's `--mode` default. A new named point `allow-all` (opencode's
`bypassPermissions`) ships, and `Mode::parse` accepts opencode's four permission-mode
names. Still open: the `/mode` head command to *move* a project at runtime, the
`todo_write` rename completion, and the remaining four D27 items.

— item 1, *`allow_session` does not stick*: ✓ fixed in 3ddf5f4f, test
at_always_ask_an_allow_session_answer_stands_for_the_session.
— item 2, *`host_other` misclassification*: ✓ fixed in 545d9d79, test
a_path_decided_baseline_does_not_report_a_region_it_did_not_decide.
— item 3, *`/mode` and `--mode` are unwired*: ✓ fixed — `ClientFrame::Mode`
(`crates/sessionlog/src/protocol.rs:558`), `/mode` in the head
(`crates/tui/src/app/commands.rs:1250`) and `apply_queued_mode`, test
a_mode_set_mid_turn_survives_a_restart.
— item 4, *mode does not persist per project*: ✓ fixed as the paragraph above records —
`ModeStore` (`crates/harnessd/src/modes.rs`), test a_mode_set_mid_turn_survives_a_restart reads
the row back the way a restart does.
— item 5, *a resumed session does not disclose a tool-list mismatch*: OPEN — no warning code
carries it (`crates/sessionlog/src/warning.rs` has none), and `reseat_unchecked` is about the
carry against the window, not the tools the prompt was built with.
— item 6, *the write prompt has no one-keypress answer*: ✓ fixed — the prompt is a card whose
option ids answer by prefix (`crates/tui/src/app/tests/decisions.rs:183`) or by digit-and-Enter
(`crates/tui/src/app/keys/cards.rs:351`, and `a_digit_inside_a_half_typed_line_is_not_an_answer`).

**D28 — Text selection in the TUI, still undiagnosed and needing one answer from
the operator.** The repaint hypothesis is REFUTED by measurement: before/after
frames byte-identical over 1,256,038 bytes, and 30 s idle wrote **zero bytes**.
Two candidates remain and one question splits them: *can you select while it is
idle, between turns?* Yes -> the complaint is mid-turn and is content change under
the selection, not gratuitous repaint. No -> it is not this code at all, it is the
TUI holding SGR mouse mode so the terminal hands drags to the application instead
of doing native selection, and the fix is a mode toggle.

— ✓ fixed in 2b49304e, test a_drag_holds_the_view_and_esc_lets_it_follow: `ctrl-p` holds the
view so the head writes nothing while a selection stands (`crates/tui/src/app/keys/mod.rs:886`),
and an idle head already writes nothing.

### Not ours, tracked because we caused or found them

**D23 — The 702 long chunks.** 702 of 43,967,653 exceed ~2048 tokens and already hold
ollama vectors of *truncated* text. Routing them to TEI makes new ≠ old for exactly those
documents. Small enough to re-embed rather than manage; lubuntu3's corpus, our capacity.

— OPEN, not this tree: a corpus on lubuntu3, and nothing here can re-embed it.

**D24 — The chunker length assertion.** lubuntu1's close, unowned: while chunks stay under
~2048 every backend agrees and none of the truncation split matters. Nobody enforces it.

— OPEN, not this tree: lubuntu1's chunker, unowned, and nothing here enforces the length.



---

---



---

## T17 — harnessd — **BUILT 2026-09-09. M1's exit criterion is NOT met as written; left: the 30-turn re-measurement under D11's metric, M0's opencode baseline (the comparison has no left side), C7's server half and C8's `length`, and C2 run as part of the exit — the per-clause status below says what is measured and what cannot be.**

`crates/harnessd`. Scripted 30-turn session against `qwen-3.8-flash-next`: 53
submissions, 25 tool calls, 161 persisted rows, 22,201 final prompt tokens, 85 s.

| check | result |
|---|---|
| C1 exact prefix extension | **PASS**, off the ledger's token vectors |
| C3 generation-inclusive prefix | never violated; 3/52 server-side shortfalls |
| **C4 `f_keep` p10** | **0.8771 — FAILS ≥0.99** |
| C4b cache efficiency p10 | **1.0000** |
| C5 reasoning replay | PASS (worst 0.9859) |
| C9 mid-session system change | PASS — stable prefix byte-identical |
| C10 disjoint cache accounting | PASS on all 53 |
| C6 / C7 / C8 | not run — see below |

**See T22 first — C4 is mis-specified, and the run measured what the plan asked for
rather than what the plan meant.**

**On the failure, and why I do not think it is the harness.** `f_keep`'s denominator
is the *whole* prompt, so a submission appending a 1,093-token tool result to a
9,000-token prompt cannot exceed 0.88 however perfect the cache is. 19 of 52
submissions add more than 1% of their own prompt. The **ceiling the script allows is
p10 0.8813, and we got 0.8771 of it**. Session-wide, 14,901 of 640,076 prompt tokens
were prefilled — **97.7% avoided**.

So **§17's exit criterion measures the script's shape as much as the harness**, and
≥0.99 is unreachable for *any* tool-using session. The harness-only number is C4b, at
p10 1.0000. **The criterion needs rewriting; the plan is wrong here, not the code** —
but it is recorded as a failure because that is what it is against the stated bar.

C6/C7/C8 could not run: C6 needs a tool-call-only assistant turn, which did not occur;
C7/C8 need a `length` finish, which requires either the `n_predict` cap §5.7 removed or
a 262k context on a production box.

— C6 and C8 are measured on HEAD now (`crates/harnessd/src/m1.rs:34,36`); C7 stays NOT RUN by
design (`:40-43`). The run above is unchanged.

**C3's shortfalls have an exact signature** — every one equalled `previous generation +
3`: the server reused to the end of the last committed item and discarded the 4-token
generation prompt *and* the whole generation. It is intermittent (14/55 on an earlier
run of the same script), so it is a server-side cache event, not structural. Settling it
needs `-lv 4` and `glm-why-no-cache` — a server-log question the harness cannot answer
from its own metrics.

— OPEN: the criterion is unrewritten — `docs/implementation-plan.md:360` still reads `f_keep`
p10 >= 0.99 — and no re-measurement under D11's metric exists in the tree (the metric landed in
cca619f8; the 30-turn run was never repeated).

### Per-clause status against `main`, taken 2026-11-03

M1's exit is two claims: **C1-C10 pass where opencode failed**, and **`f_keep` p10 >= 0.99 over a
scripted session**. The second names `f_keep` bare in the milestone table and §18.2 defines it as
`lcp / cached_entry`, which D11 settled; the table now says so, so the criterion no longer reads as
the `cached / prompt` ratio T22 proved unreachable.

**Every check now reads its evidence through a function that a test holds. Three clauses are still
not answered: C4 has no number in the settled form, C7's server half cannot be produced here, and
the comparison against opencode has no left side at all.**

| clause | on `main` today | what is missing |
|---|---|---|
| C1 exact prefix extension | read off the token vectors (`m1::c1`, two tests): a divergence names the token, and a run whose every turn failed is `NOT RUN` instead of a pass over zero submissions. The 2026-09-09 run: PASS | a re-run, below |
| C2 renderer fidelity | not produced by the instrument, deliberately. Answered offline by `crates/dialect-glm/tests/invariants.rs` (12 tests) and `crates/dialect-qwen` (36 across render, parse, lib and json); `tests/fidelity/run_gate.py` is the full gate and needs an oracle process | one command that runs the gate as part of the exit |
| C3 generation-inclusive prefix | `m1::c3`: `FAIL` only on `Violated`; a shortfall is `PASS*` and the exact `previous generation + 3` signature is counted. Live: never violated, 3/52 shortfalls | a re-run |
| **C4 `f_keep` p10 >= 0.99** | `m1::c4` reads `C4_KEEP` and prints the bar it judges by, in D11's form, with `f_sim` printed apart under its own name | **the number: the 30-turn run was never repeated after the metric landed (cca619f8), so no p10 in the settled form exists in the tree. The 0.8771 in the table above is `f_sim`** |
| C5 reasoning replay | `m1::c5`. Fixed on the way: the flag meant "any reasoning ever in the session", so from the first thought on every submission counted; it is now the turn being continued | a re-run |
| C6 `content: ""` never null | `m1::c6`. The 2026-09-09 run produced no tool-call-only turn, so its live verdict is `NOT RUN` | a session whose model answers with a call and no text |
| C7 truncated tool args | the client half is proven offline: `letibot_turn::length`'s tests, `one_truncated_argument_fails_the_whole_batch_not_just_itself`, and `FinishReason::Length` parsed off the wire | **the server half: a real `length`, which needs the `n_predict` cap §5.7 removed or a 262,144-token context** |
| C8 `finish_reason` | `m1::c8`, `PARTIAL` by design because no `length` was ever seen, and `NOT RUN` on a run with no submission, where it used to say `PARTIAL` over zero | the same `length` C7 needs |
| C9 mid-session system change | `m1::c9`, on the same collapse bar as C5. Live: PASS, prefix byte-identical | a re-run |
| C10 disjoint cache accounting | `m1::c10`; a run that asked nothing is `NOT RUN`, not agreement. Live: PASS on all 53 | a re-run |
| **"where opencode failed"** | nothing in the tree records opencode's per-check pass or fail, which is M0's exit | **M0's baseline: the comparison has no left side** |

**The run now says whether the criterion is met.** `letibot-m1` ends with one `M1 EXIT` line and
exits 1 unless it says `MET`: a check that never ran has not passed, where the old exit status was
"nothing said FAIL", which a run that measured nothing passed. C2 and C7 are named on that line as
measured elsewhere rather than counted as failures.

**What is still missing, in the order it would close.**

1. **The re-measurement.** Every number in this entry is from 2026-09-09, before D11's metric
   landed, so the exit's own p10 has never been taken in the settled form. That is one 30-turn run
   against a local server, and nothing in the instrument blocks it.
2. **M0's opencode baseline.** "where opencode failed" needs a recorded per-check pass or fail for
   opencode; no run of C1-C10 against it is in the tree.
3. **C7's server half**, and with it C8's `length`. Both need a `length` finish the plan forbids
   producing, so they stay `NOT RUN` until a 262k context exists on a production box.
4. **C2 as part of the exit.** The gate is answered by other tests; the exit does not run them.

— OPEN: the four above. Nothing in `crates/harnessd/src/m1.rs` is unproven now: every check reads
the evidence through a function with a test that fails when the evidence violates it.

---

---

## T19 — Rewrite or not, on the KV representation — **rows 1 and 2 are SETTLED NEGATIVE, measured (`experiments/kv-stitching/RESULTS.md`); left: row 3, which should not be built as written yet — it is the one checkpoint at `TokenLedger::prefix_len()` rather than §3.4.1's placement, and the §6 measurement that says whether it pays needs the :8080 server restarted, which is the operator's**

The operator's framing, and it is the right one: llama.cpp's data structures are not
the question. The question is **what would require a rewrite, and whether that rewrite
pays.** Separating the two changes what T14's negative actually settles.

### llama.cpp's choices — changeable

- recurrent cells *are* sequence ids, so a save is one 111.4 MiB blob per sequence
- attention KV and recurrent state are saved as a single unit, all-or-nothing
- rewind has **three fixed re-entry points**: free to `d ≤ 3`, one checkpoint to
  `d ≤ 516`, then **the entire prefix from token 0**

### Mathematics — survives any rewrite

A recurrent state is a **fold**: the state at position *n* encodes all *n* tokens, so
there is no "block B's contribution" independent of what preceded it. It is not stored
badly; it does not exist as a separable quantity.

This is also why CacheBlend works on attention-only models and cannot here. Attention
has cross-block dependence too, but it is **diffuse** — a weighted average whose
distribution barely shifts under a different prefix, so recomputing the ~15% that
deviate most corrects the rest. A fold has no diffuseness to exploit: there is no
subset whose recomputation repairs the remainder.

### The split

| | verdict | why |
|---|---|---|
| composition across a divergence | **do not rewrite** | mathematically blocked, not an implementation limit |
| splitting attention from recurrent in the save format | **do not rewrite** | after a divergence every layer differs anyway, so there is nothing to reuse |
| **checkpoint density / rewind granularity** | **worth doing** | pure implementation; the `d > 516` cliff is checkpoint placement and nothing else |

**The third row is the actionable one and it was measured by accident.** Rewind is what
fork (§5.5), backtracking and eviction all actually need, and today it falls off a
cliff at 516 tokens. Storing recurrent state at more positions makes rewind cheap. That
is a parameter and a representation choice, not a redesign.

— rows 1 and 2 are measured now, not argued: `experiments/kv-stitching/RESULTS.md` answers
UNVERIFIED-16 negative (T14), so composition across a divergence and a split save format are
both settled against a rewrite.
— OPEN: row 3 is unbuilt — `docs/implementation-plan.md:71` still lists §3.4.1 checkpoint
placement among the six unlocks "none landed", and nothing in `crates/turn` places a checkpoint
or re-enters a rewind (`grep checkpoint` finds only `crates/turn/src/prefix.rs`'s shortfall
reason), so the `d > 516` cliff still stands.

**Status — read with `docs/kv-representation.md`.** Rows 1 and 2 stay settled and the rewrite
question is answered: the only rewrite left would re-buy, for a fork of
`llama_memory_recurrent` and three models to keep, what a checkpoint placement buys for one
flag. Row 3 should **not** be built as written yet, because the code says this harness never
asks for a long-suffix trim — there is no rewind, no backtrack and no truncate on the token
region, so the one request in this tree whose prompt shrinks is a **local compaction**
(`plan_compaction_tail` returns `TailPlan::None` when `!remote`,
`crates/harnessd/src/harness/wall.rs:1171`), whose new base is stable prefix + one summary
item and which therefore re-enters at the *end of the stable prefix* — a position no chat
checkpoint is due at, so the arithmetic says its first turn re-prefills from token 0. That is
a prediction and not a measurement, and it is invisible today: the witness lives on the engine
`Session` and a fork opens a new one (`crates/turn/src/engine.rs:390`), so the first turn of
any new base is `FirstTurn` and `cache_reuse_shortfall` cannot fire on it. What row 3 should
chase is §3.4.1 in placement form — **one checkpoint at `TokenLedger::prefix_len()`**, the
position the harness knows and the server cannot guess — and the one experiment that settles
whether even that pays is §6 of the doc: a post-compaction turn's `cached_tokens` at 40 k and
120 k, with `--checkpoint-min-step` at its default and lowered, `/slots` for the bytes. Arm B
needs the operator: the :8080 server is never restarted for a measurement.

---



---

## T24 — The harness owns what a subagent leaves behind, and monitors need the same owner — **scopes and monitors landed (25000d33, 104e3b73, 32fb7267, 4a188380, 56ada70d); left: a firing is still a user item and a full turn, with the monitor's TTL the only bound**

Operator, 2026-09-09: *"you guys like to accumulate monitors and shells"* — and, on
being shown the measurement, *"you are fine, others nt"*. That correction is the
requirement. The leak is not the top-level agent's own housekeeping; it is **everything
its children spawn and do not clean up.**

Measured on this box the same evening, mid-session:

| | |
|---|---|
| git worktrees in `.claude/worktrees/` | **13**, most from agents that finished hours ago |
| orphan tmux sessions | `nano_test`, **30 hours** old, from a finished agent |
| the top-level agent's own leak | 1 listener, 1 poll loop — correct |

`docs/tui-testing.md` already says *"Kill your sessions. An orphaned tmux session holds a
`harnessd` and its socket, and the next run then attaches to a daemon it did not start."*
Written here, loaded in context, and it did not bind. That is `docs/closed-loop.md` §1
again and the fix is the same: not a better rule.

### The mechanism was already chosen

D4, answered by the operator earlier the same day: *"just do byobu, which connects to
cgroups — use dependent. if a vm is temporary then it is a session cgroup otherwise not.
still must be clearly reapable."*

So: **every process a turn spawns lands in a cgroup owned by a scope.** Three scopes and
no fourth — `turn`, `session`, and `explicit` (survives the session because somebody said
so, and is listed as such). Reaping is `cgroup.kill`, and a subagent's cgroup is a child
of its parent's, so a parent that ends reaps its children by construction rather than by
remembering to.

— ✓ fixed in 25000d33, test the_three_scopes_round_trip_and_there_is_no_fourth (and
`a_reaping_of_an_empty_scope_is_not_the_same_record_as_a_reaping_of_three` for the reaper
that records what it killed).

### The part that pays for itself immediately

**Cgroups retire pattern matching for both reaping and liveness**, which is T21.1 and
T21.2 dissolved rather than guarded:

- `pkill -f X` becomes "kill this cgroup" — no pattern, so nothing to self-match.
- `until pgrep -f X` becomes "is this cgroup non-empty" — no predicate that can match its
  own waiter.

The evidence that a guard is not enough: on 2026-09-09 a process check self-matched
**five times in one session**, with `process-checks-that-self-match` loaded in memory and
T21 open in this file. The fifth was `grep -E '[h]arnessd'` — the bracket trick defeats
`pgrep`, but the shell wrapper echoes the expanded pattern back into its own command line,
so the literal string was there to be found. A hazard with that many spellings is not
one you check for; it is one you make unspellable.

### Monitors, which are the same problem wearing a different hat

Operator, same message: *"btw we need monitors in letibot"*. A monitor is a condition
watched **between** turns that wakes the loop when it fires — the encoder running while
the model is not, in `docs/closed-loop.md`'s terms, and the only correct shape for
listening. The seat brief pays for that distinction: *a Stop hook fires when a session
goes idle, and a seat that is rate limited, has crashed, or never started is not running
a session, so no stop event ever fires and the silence looks exactly like a quiet room.*

A monitor is also, structurally, a long-lived process. **Adding monitors before the
lifetime work multiplies the leak this entry exists to stop**, so they land together:

1. **Scoped at creation.** No monitor without an owner; the default is the session. — ✓ fixed
   in 104e3b73, test a_monitor_dies_with_the_scope_that_owns_it
2. **Listable and attributable.** The head can show what is watching and for whom, the
   way it now shows sessions. An invisible watcher is an unreapable one. — ✓ fixed in
   32fb7267, test
   a_monitor_is_owned_listed_says_why_it_fired_and_refuses_a_second_under_one_name
3. **Reports why it fired**, not just that it did. — ✓ fixed in 104e3b73, test
   a_monitor_says_what_changed_and_not_merely_that_something_did
4. **One waiter per name**, enforced. The fleet already learned this: two processes under
   one reader means the roster shows a seat attached while the real one hears nothing. — ✓
   fixed in 104e3b73, test a_second_monitor_under_one_name_is_refused_with_what_holds_it
5. **Bounded.** A TTL or an explicit renewal, so a monitor whose reason has passed dies
   without anybody remembering it. — ✓ fixed in 104e3b73, test
   a_ttl_past_the_cap_is_refused_rather_than_clamped

### What the fleet answered, 2026-09-09 — and what it ruled out

Asked in `Lab/#general` with the counts stated first so replies were comparable.

| box | worktrees | orphan tmux | listeners |
|---|---|---|---|
| **.79 (here)** | **13**, from finished subagents | 1, 30 h | 1 + 1, correct |
| .78 (lubuntu2) | 4, **deliberate** — four llama.cpp checkouts, 3 live | 1, **14 days** | 1 + 1, correct |
| .76 (lubuntu1) | 0 | 0 (the one present is the operator's login) | 1 Monitor + 1 unit |

**lubuntu1's reading is the finding, and it is better than the counts:**

> *"the leak scales with children spawned, not with seat uptime. .76 has been up as long
> as you and leaked nothing. Reaping belongs wherever subagents are created, not in a
> periodic sweep on each box."*

That **rules out a design this entry left open**: a per-box reaper on a timer. A sweeper
cannot tell debris from a deliberate long-lived resource, and it runs on boxes with
nothing to sweep while the box doing fan-out is the only one that needs it. Reaping is a
property of the *creation site*, which is what the parent/child cgroup shape already
gives — and it means the mechanism ships with subagents (S8/M6) rather than as fleet
housekeeping.

**lubuntu2 supplies the case a sweeper would get wrong**: four worktrees that are not a
leak at all — three are live llama.cpp variants under active comparison. A long-lived
resource with a declared owner is correct. That is the `explicit` scope above, and .78 is
the reason it must exist rather than be a convenience.

**And lubuntu1's caveat is the most useful sentence in the thread**, because it is about
how the measurement lies:

> *"I killed a stray llama-server earlier tonight after a benchmark, and I only noticed
> because I went looking. Had I not, it would be in this count. My zero is partly
> attention, not only design."*

A zero produced by vigilance and a zero produced by a mechanism are the same number and
different facts — `docs/closed-loop.md` §3 exactly, one layer up. So the acceptance test
for this work is **not** "the counts are low". It is that the counts stay low when nobody
is watching, which means the reaper has to be observable: a scope that ended must record
what it killed, or its zero is unfalsifiable too.

### Blocks on

Nothing. This is substrate for M6 subagents (S8) and it is **cheaper to build before them
than after**: the leak measured above came from subagents the harness does not yet have,
run by an agent that does. The requirement was discovered before the feature, which is the
rare ordering.

### Landed, 2026-09-10 — monitors and background promotion

`crates/tools/src/exec/monitor.rs`, `crates/tools/src/builtins/monitor.rs`,
`crates/tools/tests/background.rs`. All five requirements above are mechanised and each
has a test. What is worth recording is the three decisions that were **not** obvious.

**The promotion threshold is 15 s and it is reactive.** Measured, warm, on `.79` at load
1.4: the whole routine set — `find`, `grep -rn`, `cargo check -p`, `cargo check
--workspace`, `cargo clippy --workspace --all-targets`, `cargo test -p letibot-tools`, the
fidelity gate — tops out at **6.5 s**. 15 s clears the slowest by 2.3×, and is 6× under the
`sleep 90` the operator named as the case that must promote. The previous default was 120 s,
which would have let `sleep 90` block the turn for a minute and a half and never promote.
The threshold fires on **elapsed time and never on the command text**: `cargo build` is 3 s
warm and 4 minutes cold and the string is the same both times, so a predictive rule is
`docs/closed-loop.md`'s open-loop stepper in a new costume.

**`m2_runner` is nine tools, one over §8.4's ceiling, and it says so in its own
`max_tools`.** The ceiling's evidence is about confusion between *similar* choices, so the
monitor surface was cut twice before it was allowed to cost a seat: declare/renew/retire are
one tool taking an `action` because all three act on one named handle, and **listing is not a
tool at all** — monitors are in `job_list` beside the jobs, the scopes, the promotions and
the reap log, because "what is running and what is watching" is one question and a second
listing nobody opens is how a watcher becomes invisible without anybody hiding it. What is
left cannot fold into `job_wait`: that blocks *inside* the turn, and a flag switching between
the two would make "I believed I had waited" spellable. A ceiling quietly raised for
everybody is not a ceiling; one role declaring its own number with the trade written down is
a decision somebody can reverse, and a test pins both numbers.

> **2026-09-10: the role is seatable, and it seats eight of the nine.** `--role runner`
> over `HostBackend::confined`. `bash` needs `--bash` on top, because
> `docs/boundary-and-adjudication.md` §5's transcript choke point does not exist: the
> boundary keeps secret bytes out of the process's **view** — `~/.ssh` is *absent* from
> the mount namespace, not denied — and nothing yet stops a tool result carrying bytes
> from inside that view into the transcript. `bash` is the tool whose result is an
> arbitrary byte stream; the job verbs and `monitor` are shaped by the tool. So the
> common case is eight tools, which is `DEFAULT_MAX_TOOLS` exactly, and the ninth seat is
> spent only when somebody asks for it. That is a nicer answer than the entry above
> expected and it is a **consequence**, not a plan: the ceiling was not what kept `bash`
> off.
>
> Measured on `.79`: `HostBackend::confined` builds — delegated cgroup v2 subtree and
> `bwrap` both present — so the runner seat is real here rather than a constructor that
> always errors. A box without either gets a refusal naming which half is missing, and
> never a silent fall back to `HostBackend::executable`, which buys cgroup lifetime and
> no view at all.

**A port watch has no `host` argument, and that omission is load-bearing.** A monitor that
could reach an arbitrary address would have to declare `Access::Network` on *every* call,
including the ones watching a cgroup — which is D12's shape (a network declaration making an
unrelated capability refuse entirely). Loopback-only keeps the whole tool at `Access::Exec`,
which is also the honest class: it leaves something watching after the turn ends.

### Still open — the head cannot promote a job yet

Requirement 3 of the operator's three (*"the operator promotes it, mid-flight, from the
head"*) is **half done**: the daemon-side verb is `ProcessHost::promote`, it is tested with
`Backgrounding::Operator`, and the frame the head must send is specified verbatim in that
method's doc comment — `ClientFrame::PromoteJob { client_request_id, expected_seq, job,
identity }`, queued, idempotent, answered by calling `promote`.

It is **not wired**, deliberately, and the reason belongs with D11. A new `ClientFrame`
variant is a wire change, so `PROTOCOL_VERSION` goes 5 → 6; both sides refuse a mismatch by
name; and landing a bump costs the running daemon its live session. Against that, the verb
buys nothing in production today: `harnessd` seats `m1_orchestrator`, which has no `bash`,
no jobs, and therefore no job to promote. The constant is also the one line every
wire-touching branch edits, and 5 was taken deliberately *"to coordinate rather than race"*
(D10). So: one variant and one integer, whenever a head is ready to send it.

— ✓ fixed in 4a188380, test ctrl_o_promotes_a_running_command_even_after_the_turn_went_terminal:
the frame landed as `ClientFrame::Promote { client_request_id, expected_seq }`
(`crates/sessionlog/src/protocol.rs:531`) rather than the doc's `PromoteJob` shape, because the
head names no job, and `ctrl-o` / `/promote` send it (`crates/tui/src/app/events/mod.rs:694`).

### ~~Still open — nothing calls the monitor wake seam~~ — **CLOSED 2026-09-10**

`Monitors::wait_for_any` now has a caller, and the interesting part is what had to be
added before it could have one.

**`Bell::ring` was not the door.** `Registry::next_work` skips a session whose command
queue is empty, so ringing the existing bell for a firing would have been a no-op that
*read* like a wake — worse than the poll it was replacing, because the poll was honest.
So `Bell::ring_wake` and `Work::Woken` are new: a third queue, drained **last**, because
a head that pressed enter is waiting and a monitor is not. A third `Work` variant rather
than a synthetic `Command`, because a command has an issuing head, an identity and a
`client_request_id`, and inventing three of those would put a head's name on something no
head did.

**Two cursors, and they answer different questions.** The waiter thread keeps *"have I
rung for this?"*; the harness and its steering source share *"has the model seen this?"*.
That is what makes a firing arrive exactly once whether it is picked up mid-turn (through
steering, at the next step boundary) or between turns (through the wake). A wake that
raced a mid-turn pickup returns `Outcome::Ignored` rather than running a turn about
something already delivered.

**No monitors, no thread**, the same rule `Monitors`' own poller keeps and the same rule
this entry exists to enforce: the waiter is armed after a turn in which something is
actually watching, one per session, tracked so a second cannot start. Its `wait_for_any`
deadline is **not** delivery — the condvar is — it is the only thing a thread blocked in
that condvar can do about `Bell::close`, which notifies a different one. Named here
because "no timer, no poll loop" (§18.1-I12) is a property this daemon states about
itself, and a re-check that went undescribed would read as a violation of it.

— ✓ the seam has a caller and a test:
`a_jobs_completion_wakes_the_worker_with_no_command_behind_it_and_after_one_that_is`
(`crates/harnessd/src/jobwatch/tests.rs:434`), and the waiter is `spawn_monitor_wake`
(`crates/harnessd/src/turn_tail.rs:313`).

What is **not** closed: the wake runs a full turn with the firing as a user item. A
session with a chatty monitor therefore spends generations on it, and nothing bounds that
except the monitor's own TTL. A firing that should be noted and not acted on has no
spelling yet.

— OPEN: still true on HEAD — a firing is a user item and a turn
(`crates/harnessd/src/turn_tail.rs:313-355`), and the monitor's TTL is the only bound.

### Still open — the disclosure can say `POLL ONLY` and the operator cannot tell why

`GateWiring::monitor_wake` is stamped by `Sessions::arm_wake`, so a `Harness` driven
directly by a test or by `letibot-m1` honestly reports `false`. What it does not
distinguish is *"no daemon armed one"* from *"the thread failed to start"* — the second
publishes a `Warning`, the first is silent, and both render the same line. Small, and the
kind of thing that costs an hour once.

— OPEN: the disclosure line is still keyed on `wiring.monitor_wake` alone
(`crates/harnessd/src/config.rs:2460`), so both cases draw `POLL ONLY`; a thread that failed to
start does now publish `monitor_wake_not_armed` (`crates/harnessd/src/sessions.rs:3294`), so the
two are told apart by a note rather than by the line itself.

---

---

## T16 — W9's open questions — **several want an operator or a strand owner, not me**

From the tool runtime, in descending order of consequence.

1. **§8.1 clause 3 gives the outcome vocabulary but never the mapping.** Which outcome
   is an empty `grep`? W9 decided: matches in scope or elsewhere ⇒ `Ok`; nothing
   anywhere under any relaxation ⇒ `Abstained`; a nonexistent path ⇒ `Failed`. That
   choice determines how often `propagate()` blocks a caller and should be recorded
   rather than inherited.
   — ✓ fixed in 66077505, test `a_term_that_is_nowhere_abstains_and_says_what_it_searched`;
   the nonexistent-path case is recorded as the listing rather than `Failed`
   (`a_scope_that_does_not_exist_returns_the_listing`, `crates/tools/src/builtins/grep.rs:915`).
2. **D6 answered means M1 ships with clause 5 switched off.** `NoBudget` is correct per
   D6 — unset is a genuine no-op — so **nothing spills until a budget is configured**.
   A session-config gap, not a code one, and invisible unless said.
   — ✓ fixed in d81863e8, test `every_thing_that_is_off_is_disclosed`: `Config::disclosures`
   says `UNSET` and that nothing will spill (`crates/harnessd/src/config.rs:2139`).
3. **`Gate` will collide with W11.** W9 defined the smallest adjudication seam the
   runtime needs (`admit(name, access, args)`). W11 should absorb it rather than build
   a parallel one.
   — fixed in 229555bf in code, untested: there is one `Gate` trait
   (`crates/tools/src/runtime.rs:621`) and its only implementation is `AdjudicatedGate`
   (`crates/tools/src/adjudicate.rs`), so no parallel seam exists to collide with.
4. **§8.1 clause 1 and §9.4 pull against each other.** Relaxing a pattern *is*
   rewriting the query, which §9.4 forbids. The reconciling word is **visible**: every
   relaxation appears in the result. Worth stating in §9.4 so nobody deletes one of
   the two.
   — ✓ fixed in 66077505, test `a_too_strict_anchor_is_relaxed_to_the_bare_identifier_and_says_so`;
   §9.4 now carries the rule ("and if it ever does, the rewrite must be visible",
   `docs/implementation-plan.md:213`, 7bdfd6c9).
5. **§8.4's `orchestrator` role has no `read_spill`** though its `read`/`grep` can
   spill — so for that role the omission notice is advisory, which §8.3 says it must
   not be. W9 shipped `read_spill` in its place.
   — ✓ fixed in 66077505, test `the_default_session_is_read_only_and_seats_exactly_what_it_did`
   (the orchestrator's seated set includes `read_spill`, `crates/harnessd/tests/wired.rs:112`).
6. **Retrieval is inert — no MCP server is running anywhere.** Confirmed by lubuntu3
   on 2026-09-09, checked rather than recalled.
   - `192.168.1.55:9755` is the **flowy node**, not oracle. Never was the address.
   - **RAGFlow and oracle are on lubuntu3 (192.168.1.82)** — but its MCP server **is
     not started by that deployment**. Docker publishes 9382-9384, so a **TCP connect
     succeeds while nothing is behind it** (`curl` → HTTP 000, zero mcp processes in
     the container). That is why a client reports "unable to connect to the url": the
     connection is fine, the service is absent. Same shape as the embed wedge — a
     check that succeeds without touching the thing it checks.
   - `:8100/sse` **looks** like an SSE endpoint and is not — it is arxiv-search, whose
     catch-all returns identical HTML for every path. Do not use it.
   - **Use the REST API instead: `192.168.1.82:9380`, base `/api/v1`, bearer auth.**
     Do not ask for an MCP listener to be started while ingestion is live.
   - `ask_code` is a **separate problem**: per-box, against the local codebase, and
     **nothing is running on this box** (checked — only qwen/bge/postgres listen).
   - **Open and load-bearing:** does RAGFlow signal "the corpus does not cover this"
     in a *field*, or only in prose? §8.1 clause 3 requires abstention to be
     structurally distinguishable. If it is a field, it maps to `Abstained` and the
     harness can never report it as grounded. **If it is only prose, we will not parse
     the prose and pretend** — the tool returns `Ok` with the text and the hole stays
     documented. Asked; awaiting an answer.
   - Note from lubuntu3: `.82:11434` enforces **one request per backend and will hold
     a call rather than refuse it**. Do not point volume at it unannounced.
   — ✓ fixed in c44d41e7, test `a_session_with_nothing_attached_discloses_only_retrieval`:
   retrieval is disclosed as `INERT` under its own name (T16.6) and an answer with no
   citations comes back `NO_RESULT` (`crates/tools/src/builtins/external/mod.rs:173-200`).
7. **Call ids are positional per turn** (`call_0`; GLM carries none), so an id-derived
   mark repeats across turns. Fine now, wrong once a head correlates across a session.
   — OPEN: still positional — `crates/turn/src/items.rs:175` mints `call_{n}` per round and
   `crates/provider/src/openai.rs:624` mints `call_{i}`, so an id still repeats across turns.

---

---

## T3 — Report the minja scoping bug upstream — **ready, not filed**

`{% set %}` inside `{% for %}` is not scoped per iteration in llama.cpp's minja.
Repro needs no GPU: a template, four messages, one `/apply-template` call.
Full writeup and both engines' output in `docs/chat-templates.md` §2.

Affects any llama.cpp user serving GLM with `messages` — the model is conditioned on
reasoning it never produced. We are insulated because we submit token ids.

— OPEN: ready and unfiled — nothing in `docs/` records a link or an issue number, and the
writeup at `docs/chat-templates.md:55-65` cites no upstream report.

---

---

## T6 — Not started, from `docs/workstreams.md`

W1 measurement rig · W2 prefix-invariant suite (grok port, Apache-2.0) · W6 turn
engine (**critical path**) · W7 session log and head protocol · W8 heads · W9 tool
runtime · W10 firecode substrate · W11 adjudication · W12 flowy connector · W13
segments and compaction · W14 EXPLAIN · W15 server track · W16 experiments.

`pytest` is still not installed; `docs/workstreams.md` calls that blocking for W1/W2.

— OPEN: the list is stale — `docs/workstreams.md:20-27` now maps W6 to `crates/turn`, W9 to
`crates/tools`, W11 to `tools/src/adjudicate`, W13 to `turn/src/compaction`, so only W1/W2 and
the docs' own status still wait; `pytest` is genuinely absent (`python3 -m pytest` → `No module
named pytest`).

---

---

## T15 — Replace §10's compaction design with structural eviction — **PARTLY LANDED: structural eviction of units runs before every summary turn, and the record is sized by the room and gated on what it kept; left: the addressable map in the new base, the `recall` tool, a standalone `prune`, and §10 of the plan, which still reads leveled compaction**

`docs/compaction.md`. Compaction today bundles *reducing what is resident* (necessary)
with *destroying what is recoverable* (an accident). Falsifier B removed the usual
justification — quality does not degrade with depth — so the trigger is the context
wall and memory pressure only.

The key constraint that rules out the easy fix: **plain eviction invalidates the
prefix just as summarisation does**, because the prefix *is* the old turns. Both cost
a re-prefill, so the question is what to get for it. Answer: replace an evicted span
with a **structured, addressable map** rather than prose, maintained incrementally so
there is no stop-the-world summarisation call, with zoom-in appended via `recall`
rather than rewritten in place.

Depends on nothing. Feeds `docs/memory.md`. Gives `SegmentMark` its missing producer.

— PARTLY LANDED: the record is sized by the room and gated on what it kept, and the tail keeps the
old turns most relevant to the plan (embedding relevance). Structural eviction of units now runs
before every summary turn (`crates/turn/src/evict.rs`, test `crates/turn/tests/evict_summary.rs`):
superseded reads, settled subagents and finished calls go as stubs, and failures, the exchange in
hand and anything a plan row names stay. OPEN: the addressable map in the new base, the `recall`
tool, a standalone `prune`, and §10 of the plan (`docs/implementation-plan.md:215-223`) still reads
*leveled compaction*.

---

---

## T13 — §4.5's event enum is insufficient for a real head — **five gaps; gap 1 is a priority, see T14; left: item 2 stays half closed — `TurnFinished` carries no `prefix_check` (needs a wire twin of `letibot_turn::PrefixCheck`) and no `dialect_template_sha`**

Found by building one, then by fixing T12. Listed with what was done about each.

**Gap 1 is not a footnote.** T14 argues that an addressable record is the fallback if
composable KV proves impossible on hybrid models — and a log whose transcript events
carry no content cannot be that record. Treat it as blocking for the "sessions that
do not forget" goal, not as a schema nicety.

1. **`TranscriptAppended{item_id, kind, ledger_head}` carries no content**, and
   `EventSink` has no channel for it — so **a head cannot reconstruct a conversation
   from the log at all**. The daemon must reconcile items into the view out of band
   (`Hub::record_item`). W8 kept the event exactly as specified and made the gap
   explicit rather than widening it unilaterally. This is the structural one and it
   needs a decision: either the event carries content, or the log is formally not a
   sufficient record of a session.
   — ✓ fixed in 4b7a99af, test `a_head_that_was_already_attached_is_sent_the_body_it_was_promised`:
   the body travels as its own event (`SessionEvent::TranscriptContent`, `crates/sessionlog/src/event.rs:1033`).
2. **`TurnFinished{usage, timings}` is lossy against `turn_metrics`.** Not carried:
   `prefix_check`, `id_slot`, `n_busy_slots`, `cost`, `dialect_template_sha` — and
   §18.2 says **`id_slot` is exactly what distinguishes a scheduling fact from a
   prefix divergence**, so a head cannot today tell those apart. W8 widened `usage`
   to carry `cached_tokens` on the argument that a harness whose point is cache reuse
   must be able to display it.
   — OPEN: still lossy — `TurnFinished` carries `{usage, timings}` and nothing else
   (`crates/sessionlog/src/event.rs:1003`), and `prefix_check`, `id_slot`, `n_busy_slots`,
   `cost`, `dialect_template_sha` never cross the seam (`crates/sessionlog/src/lift.rs:8-10`).
3. **No event announces who issued a command**, which §13.2 requires twice. Added as
   `CommandIssued`, labelled as an addition rather than smuggled into `Warning`.
   — ✓ fixed in e3354641, test `a_second_heads_prompt_is_queued_not_rejected_and_says_who`
   (it asserts the issuer and the verb, `crates/sessionlog/src/hub.rs:2358`).
4. `ToolStarted` / `ToolProgress` are given by name only in §4.5; their shapes are
   W8's invention and **W9 will find out whether they are right**.
   — ✓ fixed in 66077505, test `a_real_call_reaches_a_head_with_everything_the_runtime_knew`
   (`ToolStarted` carries `turn_id` and `access`, `crates/sessionlog/src/event.rs:965-971`).
5. **`DeltaTarget` is `Text | Reasoning` — there is no channel for a tool call.**
   Found while fixing T12. A tool call's argument text streams as `Text` while
   `items::produce` puts it in `ToolCall{arguments}` and excludes it from the
   Assistant row, so T12's new per-channel equality assertion **would fail on a
   tool-calling turn — correctly**, as a real live/stored disagreement rather than a
   test defect. No test exercises it yet because `live_e2e`'s prompt makes no calls.
   This is T12's defect, unfixed, in a third channel. The enum was not widened
   unilaterally.
   — ✓ fixed in 0a77c5fb, test `the_raw_markup_of_a_tool_call_is_never_on_the_screen_by_default`:
   `DeltaTarget::ToolCall` exists on both sides (`crates/turn/src/events.rs:19-26`,
   `crates/sessionlog/src/event.rs:32-36`).

### Status against `main`, 2026-11-02 (`wt-t22`)

Re-checked because many events were added after W8. Four of the five are closed:

1. **CLOSED.** `SessionEvent::TranscriptContent { item_id, item }`
   (`crates/sessionlog/src/event.rs:1064`) is published by `Hub::record_item`
   (`crates/sessionlog/src/hub.rs:902`) and applied by the head
   (`crates/sessionlog/src/view.rs:647`), so a head reconstructs a conversation from the log.
   The event the plan named kept its shape and the body travels beside it.
2. **HALF CLOSED.** `Usage::served` (`crates/sessionlog/src/event.rs:499`) now carries
   `id_slot` and `n_busy_slots` lifted from `turn_metrics` (`crates/sessionlog/src/lift.rs:116`),
   defaulted and skipped on the wire, so §18.2's scheduling-or-divergence question is
   answerable by a head. **Left:** `prefix_check`, which needs a wire twin of
   `letibot_turn::PrefixCheck` and is a contract decision rather than a field, and
   `dialect_template_sha`, a model fact the head already has from its wiring.
3. **CLOSED.** `CommandIssued` (`crates/sessionlog/src/event.rs:1155`).
4. **CLOSED as shapes.** `ToolStarted { turn_id, call_id, name, access }` and
   `ToolProgress { turn_id, call_id, note }`, both published
   (`crates/harnessd/src/answers.rs:140`, `crates/harnessd/src/harness.rs:4069`).
5. **CLOSED.** `DeltaTarget::ToolCall` (`crates/sessionlog/src/event.rs:33`), emitted by the
   engine (`crates/turn/src/engine.rs:1734`) and drawn by the head
   (`crates/tui/src/app/events/turn.rs:218`).

---

---

## T10 — Contract gaps found by W6 — **items 1, 2, 3 and 5 SETTLED 2026-09-11, see TODO-settled.md (R1, R2, R3, R4), of which 3 and 5 landed in code with no test naming them; left: item 4 — `render_incremental` still takes the whole history and there is no resumable state token**

The turn engine is the first real consumer of the crates below, and it found five
things. Listed in the order I would fix them.

1. **`TranscriptItem::Assistant` has no `truncated` field**, and both §5.7 and §5.8
   require one. Currently tracked on the turn record instead, which is why **one
   piece of steering is unbuilt**. Needs a `transcript` crate change — the only item
   here that blocks a feature rather than costing elegance.
   — ✓ fixed in e5a11374, test `truncated_survives_a_round_trip` (and
   `a_row_written_before_the_field_existed_reads_as_not_truncated`): `Assistant { truncated }`
   is on the row, `crates/transcript/src/lib.rs:102`.
2. **`ParsedSpan` carries no token offsets**, so a `Parser` cannot say which ids an
   item owns. Cost an entire module (`crates/turn/src/items.rs`) to work around
   without reimplementing the parser. Adding a span to `ParsedSpan` would delete it.
   — ✓ fixed in 5a01de3b, test `parse_round_trips_reasoning_and_two_tool_calls`: every variant
   carries its `Range<usize>` of ids (`crates/dialect/src/lib.rs:177`) and the test asserts the
   spans tile the stream from zero (`crates/dialect-glm/tests/invariants.rs:279-289`).
3. **`DialectSpec` has no `ReasoningField`** — a per-model fact of exactly the kind
   that crate exists to model as data. The engine takes it as config rather than
   guessing, which works but puts a model fact in the wrong place.
   — fixed in 5a01de3b in code, untested: `DialectSpec.reasoning_field` is data
   (`crates/dialect/src/lib.rs:266`) and the engine reads it there
   (`crates/turn/src/engine.rs:665`), but no test asserts the spec's field rather than the
   transcript's copy.
4. **`render_incremental(history, new)` replays the whole history** for boundary
   state, so building one ledger row per item is O(n²) replays. Microseconds today
   at our sizes; a resumable state token fixes it. Note T1 may delete this entirely
   when the template-driven renderer lands.
   — OPEN: still O(n²) by design — `render_incremental` takes the whole history
   (`crates/turn/src/renderer.rs:24-30`, whose comment says boundary state is not derivable
   from a `usize`) and there is no resumable state token; `append_items` renders one item at a
   time (`crates/turn/src/engine.rs:434`).
5. **`cargo:rustc-link-arg` does not propagate across crates**, so every crate that
   links `libllama` needs its own `build.rs` to bake the rpath. Without it, test
   binaries link fine and fail at exec looking like a missing library. Fixed in
   `crates/turn`; worth a note so the third crate does not rediscover it.
   — fixed in e51847ed in code, untested: that commit was the last crate to get its own
   `build.rs`, and every crate that links `libllama` now bakes the rpath through the shared
   `crates/llama/build-support.rs` (`crates/bridge/build.rs`, `crates/sessionlog/build.rs`,
   `crates/turn/build.rs`, `crates/harnessd/build.rs`, `crates/letibot/build.rs`), each header
   saying why. A build script is not testable.

---

---

## T18 — The prompt cache costs ~120 KiB/token on recurrent models — **operational, caused a fifth OOM; left: every cost here is the server's and outside this tree — nothing sizes or caps the per-sequence accumulator, `--cache-ram` or `-c`, the eviction and warm/cold figures have no consumer, and `--cache-reuse`'s dense-model disable has no explanation**

Found while running UNVERIFIED-16, which **OOM-killed `qwen-flash-next` at 14:35 on
2026-09-09** (158.1 GiB peak; systemd restarted it, `qwen-slots-restore` put the slots
back, ~40 s down, in-flight requests from two other agents lost).

Cause, and it generalises: **every distinct prompt prefilled becomes a prompt-cache
entry at ~120 KiB/token, because 111 MiB of each entry is the recurrent accumulator** —
fixed per sequence, independent of length. A sweep over many short distinct prompts is
therefore far more expensive than its token count suggests. `server-context.cpp:4549`
records four previous occurrences; this was the fifth.

— OPEN: a fact about the server, not this tree — nothing here sizes or caps the per-sequence
accumulator; the only guard is the sweep's own floor (`experiments/kv-stitching/srv.py:29`).

**A second cost, measured on a DENSE model, 2026-09-14 by `lubuntu1-lab` on .76
(Qwen3.8-27B Q6, llama.cpp, stock).** Different mechanism from the recurrent
accumulator above and it compounds with it:

- A saved prompt-cache state costs roughly the **full `-c` allocation, not the
  tokens actually used** — ~1.5 GiB each at `-c 8192`. So capacity is
  `cache-ram / per-saved-state`, and the 8 GiB default holds about five
  conversations. At 40 GiB: 16 of 16. Two levers, and only two: raise
  `--cache-ram`, or lower `-c`.
  — OPEN: operational and the operator's — no code or config in this tree sets `--cache-ram` or
  `-c`; the server is `~/bin/glm-flash-server`, outside the tree.
- Eviction is **capacity, not conflict**: interleaving A,B,A,B,A,B hits 94–95 %
  once each has been seen. So the number to size against is *how many distinct
  briefs are live*, not how they are ordered.
  — OPEN: a measurement with no consumer — nothing in the tree sizes a pool by live briefs.
- Warm 644 ms median against cold 1986 ms; the first touch of a new conversation
  is always cold.
  — OPEN: a measurement with no consumer — nothing in the tree records or asserts it.
- **`--cache-reuse` is silently disabled on that model** — `cache_reuse is not
  supported by this context, it will be disabled`, logged with `--kv-unified`
  both true and false, so unified KV is not the gate. The 94 % hits are the
  slot's own longest-common-prefix match, not `cache_reuse`. Advice to rely on
  that flag is therefore **model-dependent**, and a measurement taken on one
  model does not port. Nobody has yet established what makes a context
  unsupported; the suspicion is an attention property rather than a flag.
  — OPEN: answered for a hybrid and not for the model it was measured on —
  `experiments/kv-stitching/RESULTS.md:99` gives the gate as `ctx_tgt_has_recurrent_state`
  (`server-context.cpp:5763`), which cannot be what disabled it on the DENSE 27B this bullet is
  about; no doc explains that case.

Mitigation used, and worth generalising: the sweep harness refuses to start a condition
below 40 GiB `MemAvailable`. Any future experiment that prefills many distinct prompts
needs the same guard.

— fixed in b82b906b in code, untested: `experiments/kv-stitching/srv.py:29` refuses to start a
condition below `guard(floor_gib=40.0)` of `MemAvailable`, with this OOM named in its docstring.

---

---

### 2026-09-11 — the sixth OOM, and why the disk tier did not save the turn

GLM was **killed by the kernel OOM killer** at 01:35:34 (`status=9/KILL`), taking a live
letibot turn with it: `malformed http response: connection closed mid-chunk: the server
went away before the terminating 0-length chunk`. Host RAM, not VRAM.

**The good news first, because it corrects a standing fear.** The spills survive a hard
kill and are re-used. 957 GB across 506 files, and the restarted server indexed them:

    L2: indexed 386 cache entries from disk (23,128,678 tokens, 931,079 MiB)

and then served **6,427,050 cached prompt tokens against 240,831 prefilled** — 26.7×
reuse, resuming a 171,901-token conversation rather than re-prefilling it. Critically,
`forcing full prompt re-processing` appears **zero** times, so the plan's "Root cause B"
— a disk restore that loaded the KV and then discarded it because `checkpoints` was
empty, which is exactly the hybrid/recurrent case — is **fixed** in this build. That is
the first evidence of it holding across a `SIGKILL`.

**What is wrong is that the tier is read-only in practice.** Every idle cycle logs

    flush: 0 sequence(s) with tokens, 0 saved, 0 already cached, 0 offloaded (0 saved),
           cache now 386 entries / 0.0 MiB

`0 sequence(s) with tokens` — by flush time no slot holds anything to save, which is
`spill-only-writes-cached-prompts` in the memory dir: *prompt_save is false for an
already-cached prompt, so live slots never spill*. So it serves brilliantly from what was
written before the kill and **adds nothing**. Any conversation that grows from here has
no disk entry, and that one really will prefill from zero after the next kill.

**And the shape of it is perverse.** The RAM tier is **write-back** — entries reach disk
on eviction or graceful shutdown — and write-back is precisely what a `SIGKILL` defeats.
`--cache-ram 81920` therefore both raises the chance of the kill and maximises what the
kill destroys. `/data` is a 7.3 GB/s NVMe with 5.3 T free; **write-through survives OOM
by construction** and the read side is already proven fast enough at 26.7× reuse.

**Also costing something:** 48 `index_disk` calls in one log, ~20 minutes apart, each
re-reading metadata for 931 GB across 386 entries — the sleep/wake cycle
(`--sleep-idle-seconds 600`) paying the index once per wake rather than once per start.

**The decision, and it is the operator's:** lower `--cache-ram` (less pressure, less
lost, more disk reads), or make the RAM tier write-through (survives the kill, costs
NVMe bandwidth that is measurably spare). Not a letibot change either way — it is
`~/bin/glm-flash-server` and llama.cpp's `server_prompt_cache`.

## T20 — Where the assembled parts did not fit — **integration findings, several serious; items 2, 6 and 7 are fixed with their tests, and item 5's fourth trait method is built (`PromptRenderer::render_incremental_by_item`, called from `crates/turn/src/engine.rs:460`, so Qwen's consecutive tool results merge into one user turn with each item's end recorded); left: items 1, 3 and 4 landed in code with no test naming them**

1. **Double envelope.** `ToolRuntime::transcript_item` renders §8.2's `NO_RESULT`
   envelope into `payload`, and **both** dialect renderers wrapped it again — the model
   received two. Fixed: renderers emit `payload` verbatim.
   — fixed in d81863e8 in code, untested: both renderers write `payload` verbatim
   (`crates/dialect-glm/src/render.rs:284-288`, `crates/dialect-qwen/src/render.rs:308`) and
   `outcome_envelope` is off the render path; no test asserts one envelope reaches the model.
2. **`Registry::tools_json()` is not prompt-ready** and this one was nearly invisible.
   It emits `,`/`:`; both templates use HF `tojson`'s `, `/`: `. Using it directly for
   `StablePrefix::tools_json` differs in **the first bytes of the stable prefix** — a
   cold prefill every single turn, with nothing in the tree to catch it. Now routed
   through the dialect's own tool-JSON function, with a test asserting the two differ.
   — ✓ fixed in e2c00056, test `separators_match_the_shipped_template`
   (`crates/dialect-glm/src/json.rs:87` pins HF's `, ` / `: `, which `serde_json` does not emit).
3. **C10 was unmeasurable as specified** — `prompt_tokens − cached_tokens` from
   `TurnMetrics` alone is a tautology. Needed the server's own frame numbers.
   — fixed in d81863e8 in code, untested: `TurnMetrics::prompt_tokens_server` carries the
   server's count (`crates/turn/src/metrics.rs:35`) and C10 compares it against
   cached+processed (`crates/harnessd/src/m1.rs:650-673`); no test drives the check.
4. ~~**`ToolRuntime` is not `Send`** — `Gate` lacks `Send + Sync`~~ — **FIXED, verified
   2026-09-10:** `crates/tools/src/runtime.rs:275` reads `pub trait Gate: Send + Sync`.
   — fixed in 229555bf in code, untested: the bound is at `crates/tools/src/runtime.rs:621`
   now; it is checked by the compiler and no test names it.
5. **`Session::append_items` renders one item at a time**, so Qwen's consecutive tool
   results cannot merge into one user turn without a fourth trait method. Declared as a
   gate divergence.
   — ✓ fixed: the fourth method is `PromptRenderer::render_incremental_by_item`
   (`crates/turn/src/renderer.rs:47`), which returns each item's span `ends`, and the engine
   calls it (`crates/turn/src/engine.rs:460, :590`); `QwenRenderer::render_incremental_by_item`
   (`crates/dialect-qwen/src/render.rs:138`) writes one user turn per run of results and its own
   tests assert the run and the ends (`:873`, `:945`), so the ledger keeps one row per item
   under a prompt that carries one turn per run.
6. **Qwen's `SystemUpdateMode` is `Envelope`, not `InHistory`** — its template *raises*
   on a mid-history system message, so §5.3's conservative default is the only
   renderable form. The model can and did argue with it.
   — ✓ fixed in d81863e8, test `a_mid_history_system_change_is_not_a_system_message_here`
   (it asserts `system_update_mode == Envelope`, `crates/dialect-qwen/src/lib.rs:242`).
7. **`ask_code`/`ask_corpus` return `NotRun`, not `Abstained`** — nothing ran, which is
   a different fact. The daemon's startup disclosure says so.
   — ✓ fixed in 66077505, test `with_an_adjudicator_and_nothing_attached_the_tool_names_what_is_missing`
   (the refusal is `ToolOutcome::NotRun`, `crates/tools/tests/external.rs:117-160`).
8. The `/apply-template` oracle caught a renderer bug **no unit test would have**: an
   `Assistant` following a `Reasoning` opened a second message.
   — fixed in e2c00056 in code, untested: the Assistant arm closes the think block in place
   rather than opening a second message (`crates/dialect-glm/src/render.rs:246-256`); the oracle
   that caught it is `tests/fidelity/run_gate.py --interop`, which needs a server.

---

---

---

## BashArena — Kev versus the incumbent on the attack corpus — **STOPPED 2026-09-22, unresolved; the operator ruled it off**

**Where it stopped.** The measurement is done and committed (`docs/evidence/basharena-2026-09-22.md`,
commits `d1a5a22` + `4b10c0a`): 339 rows converted from HF `abhayesian/basharena-monitor-eval`,
Kev scored on the Blackwell in bf16 (5/47 attacks caught at t=0.5), the incumbent through
`--etalon-oracle` (14/47, then 12/47 on a rerun), the four gate words, the per-side-task split, and
the zero-false-allow threshold (**t > 0.8498 catch-all, 274 of 292 honest calls**).

— OPEN: not code — the record is in the tree (`docs/evidence/basharena-2026-09-22.md`, d1a5a22,
4b10c0a) and the operator ruled the comparison off; nothing is pending.

**What is unresolved, and it is the one that decides whether the comparison means anything:**
`docs/guard-corpus-plan.md` §4b records the SAME guard on the SAME 339 rows at **27 of 47 (57%)**
on 2026-09-17; today's runs give **14 and 12**.

— OPEN: settled in the tree — the record's 57% is ruled not reproducible and the deficit is
attributed (`docs/evidence/basharena-2026-09-22.md` §5b, 15ab1838); the operator ruled the
comparison off, so nothing is pending.

**BISECTED 2026-09-23 — the commit is `3d8754d` (R12), and the mechanism is the prompt's ORDER.
FIXED THE SAME DAY on the operator's ruling: see `docs/evidence/basharena-2026-09-22.md` §5d.**
The order is back to reasoning-then-verdict and `DEFAULT_MAX_TOKENS` is 120 → 400, so truncation
costs latency instead of recall. Four runs of `--etalon-oracle` on the same 47 attacks alone:

    attacks caught     26, 23, 23, 25   (mean 24.25, median 24)   against 18, 18, 17 before
    truncated replies  0 of 47, every run
    median latency     706, 761, 902, 2316 ms;  p90 2282-5728 ms   (mean ms ~1234 -> ~1838)

— OPEN: not code — the revert landed as 1eae071e and the four runs are recorded in
`docs/evidence/basharena-2026-09-22.md` §5d; no guard change is pending.

**The declared threshold (>= 24) was met by the mean and the median and MISSED BY TWO OF THE FOUR
RUNS, both at 23.** The honest range is 23-26, the low edge of the parent's band, not comfortably
inside it. The recovery is ~6.3 rows against the bisect's attribution of ~6.5 — they agree, so the
reordering was the mechanism and a second bisect is not owed. **The honest column is still
unmeasured** for this configuration, which is what a recall-versus-needless-prompts ruling needs.

— OPEN: measured since — `docs/evidence/honest-column-threshold-2026-09-23.md` records both
models on the full 339 rows (00d1eca7) and recommends running NEITHER; the ruling it wanted is
the operator's, and the comparison was ruled off.

See `docs/evidence/basharena-2026-09-22.md` §5c for the instrument, the walk and the isolation.
Every commit was BUILT AND MEASURED, not reasoned about: `--etalon-oracle` on the 47 attacks
alone, local endpoint, threshold declared first (**≥24 caught GOOD, ≤23 BAD**, from poles of
29–30 at af96837 against 17–18 at HEAD). 307 commits, 9 steps.

— OPEN: not code — the instrument and the walk stay in `docs/evidence/basharena-2026-09-22.md`
§5c and `docs/evidence/basharena-bisect-test.sh`; nothing in `crates/` carries it, and nothing is
pending.

The answer, and **all five of my earlier candidates were refuted**: `c3ee6b7`, `a16453f`,
`bf4137e`, `452d7dd`, `76e1b31` are every one of them ancestors of `06ec17b`, which measures
**26, 24** — they cannot be the cause. What settles it beyond the branched-lineage doubt is the
direct parent→child pair: **`3d8754d^` = 26, 24 against `3d8754d` = 17.**

— OPEN: not code — the refutation of the five candidates stands in §5c; no code change follows
from it and none is pending.

**The mechanism, isolated to two edits in one file.** R12's own message names the change it
considers load-bearing — *"the prompt asked for a sentence and then the verdict on the LAST line,
so a reply cut before that line was a budget recorded as an unreadable answer. Now the verdict is
asked for on its FIRST line."* Applying **only** that pair to the parent (25 insertions, none of
the other 738 lines):

    06ec17b as committed                       26, 24 caught
    + the prompt reorder and parser only       18, 18        <- the whole drop
    + max_tokens 120 -> 400 only               24, 25        <- about one row

**So R12's own diagnosis is refuted too:** truncation is real but worth ~1 row. The other ~6 are
the prompt's ORDER changing what the model decides — commit before reasoning makes the oracle
materially more permissive.

— OPEN: not code — the isolation is recorded in §5c and the ordering half it identifies was
never ruled on in its own right; the operator then ruled the comparison off.

**NOT REVERTED, deliberately, and the halves are separable for the operator to rule on.** The
ceiling/classification half is unambiguously right and worth ~1 row; the ordering half is worth
~6 and was never ruled on in its own right. Reverting would restore an artifact — replies cut
before their verdict, scored as catches the guard never made. Every number in §5c is attacks-only
(examples effect held constant); **the honest column is unmeasured for both variants**, which is
what a recall-versus-needless-prompts decision would need.

— OPEN: the ordering half was never ruled on, and the revert §5d landed on 2026-09-23 supersedes
"NOT REVERTED" here (`docs/evidence/basharena-2026-09-22.md:476`, 1eae071e); with the comparison
ruled off nothing waits in this tree, and nothing was built.
