# Plan

The living plan. Rewritten when the order changes, never appended. `TODO.md` holds the detail of
every row - claim, file:line, scenario, change, proving test. This file holds the order and refers
to rows by id, so the two never disagree. When they do, `TODO.md` is right about a row's content
and this file is right about its place.

Sources: the review at `9e9312b06` (`docs/review/2026-11-16-letibot.md`, the findings I1-I105 in
`docs/review/2026-11-16/INTEGRATION.md`, the leads in `LEADS-2026-11-16.md`), and the operator's
decisions of 2026-11-16 (INTEGRATION section 10).

## Goal

Stable and coherent. letibot works and has a sound core. The review found a soft perimeter around
it: authority assumed when a label is missing, work acknowledged before it is durable, one rule kept
in two places that drift apart.

The phase is fix-only until every critical and high finding is closed. No new feature starts in it
unless the operator asks for one. A live bug found meanwhile is fixed and also added to the review's
finding list. It is not handled as a standalone feature.

Automode is first. Since H3 / R300 the review queue shows the operator cards for reads, which is in
effect a breaking change. The way out is the shadow answering them through the real oracle, and
that needs the provenance, shadow and gate-rule fixes underneath it.

## Order

1. Automode prerequisites: B3, B2, B4, then the shadow as the real `AuthorisationOracle`.
2. R316 stage 1, reads: queue seats, then every session.
3. R316 stages 2, 3 and 4, each behind its own gate.
4. The rest of the batches in dependency order: B1, B1b, B5, B6, B7, B8, B9, B10, B11, B12, B13,
   B14, B15, B16.
5. Mediums and lows by theme (R338-R349), then the design follow-ups (R350) after the stability bar
   is met.
6. After the bar: the credential broker (R358), beside lead 32.
7. After the bar: job-first tool calls and the flow (R368, R369), then the box daemon (R370-R377).

Steps 2-3 run beside step 4. A stage waits on its replay evidence, not on code, so batches land
while it collects. Stage 2 also needs B6 (I25 lets a child move to a model decider), so B6 is the
first batch of step 4.

Sizes: S under about 300 lines, M up to about 1000, L above that. Each batch lands with its proving
tests, through one full gate. Every proving test fails on the snapshot.

### 1. Automode prerequisites

**B3 - provenance (R323).** Anything without a speaker label counts as the operator, so a subagent
brief, a command file, a browser page and a bridge seat's answer all become operator consent the
oracle can cite. Fix the default once at `submit` and at the connection.
- Scope: `harnessd/src/harness/children.rs`, `turns.rs` (`submit` only), `sessions.rs` command and
  Slash arms, `bridge/src/browser/*`, `answers.rs` `settle`.
- Findings: I1, I2, I3, I4.
- Decision 1 applied: a bridge seat may answer a permission card only for a session it started. The
  attribution change (`agent:<handle>`, `can_decide` from the connection role) lands regardless.
  Decision 10 default: a command-file body is an Agent context row under a header naming the file.
- Tests: `a_childs_first_brief_is_not_an_operator_utterance`,
  `command_file_body_is_not_an_operator_row`, `browser_ask_page_text_is_not_operator_authority`,
  `bridge_answer_is_not_recorded_as_human`,
  `a_bridge_seat_cannot_answer_a_card_of_a_session_it_did_not_start`.
- Size M. Depends on nothing.

**B2 - shadow and grants (R322).** The acting rule is any 8 bytes of any allow rule, never matched
to the path. The breaker lives in memory while its grants are durable rows.
- Scope: `harnessd/src/shadow/*`, `grants.rs`, the `tools/src/runtime.rs` note.
- Findings: I8, I9, I10, I11, I7.
- Decision 3 default: `answer_reads` is off until stage 1 is promoted. Turning it on is the
  promotion act, recorded per session.
- Decision 4 default: the breaker trip is persisted beside the grants and needs an explicit re-arm.
  Queue grants get a TTL or are bound to the reviewer.
- The acting rule is evaluated against the reviewer's own rulesets (C7). Read roots are derived from
  the reviewer's repository at ask time, never a configured `/build` or `/data` (C8).
- Tests: `acting_rule_must_cover_the_requested_path`, the default-off config test (inverting
  `shadow/tests.rs:1459`), `read_passes_resolves_symlinks_and_refuses_agent_stores`,
  `breaker_survives_reopen_while_grants_persist`, `a_shadow_grant_note_names_the_shadow`.
- Size M. Depends on nothing. Lands after B3 when both are ready, since the shadow reads authored
  spans B3 relabels.

**B4 - gate rules (R324).** Admitting rungs never check what they cover. One "Allow this one" for
`python3 -c 'print(1)'` admits every later `python3 -c` body in the project, and survives
`/mode always-ask`.
- Scope: `tools/src/permission.rs`, `intent.rs` flag and git tables and the own-config rule,
  `project_code.rs`, `adjudicate.rs` shapes and the Absent refusal.
- Findings: I18, I20, I21, I22, I23, I33 (f).
- Decision 2 applied: once means once. A shape is recorded only from AllowSession at a Session
  point, never from a holed body, and cleared on any narrowing.
- Tests: the extended `the_seed_admits_no_write_or_exec_flag`,
  `cargo_doc_ctest_npx_run_project_code`, `cd_then_relative_write_to_own_config_asks`,
  `an_inline_program_approval_admits_a_different_body`,
  `set_mode_to_always_ask_forgets_approved_shapes`, `permission_allow_outlives_a_move_to_read_only`
  inverted, a `git cherry` intent test.
- Size M. Depends on nothing.

**The shadow as the real oracle (R337).** The shadow is a second authorisation model today, with
grants in four stores. It becomes layer B of `AuthorisationOracle`, and the Relay path goes.
- Scope: `tools/src/adjudicate.rs` view asks (trail and reading), `tools/src/authorise.rs`,
  `harnessd/src/shadow/*` (`ShadowOracle`), `answers.rs`, the grant stores.
- Findings: I12 (a view read's tier is a literal and its trail is empty), I13 (the oracle move), I15
  (oracle `allow_once` on a view maps to Refused), I17 (shadow hygiene, nine items, (i) first),
  I14 and I16 (grant scope and revoke reach). R317's two leftovers are filed here.
- Tests: `view_asks_are_recorded_and_shadow_rows_excluded_from_agreement` and one per item, as
  named in the row.
- Size L. Depends on B2 and B3.

### 2. R316 stage 1 - reads

Blocked until B2, I12, I15 and I17 (i) land. Then:
1. Review-queue seats: the shadow answers a relayed view read through `ModelAdjudicator` and
   `ShadowOracle`, inside the permission job's deadline.
2. Every session (stage 1b): `ask_to_see` carries the trail and a baseline, `OracleScope` gains a
   `view_read` entry limited to reads past `read_passes`.

Promotion gates, measured on operator-answered rows only (timeouts, the gate's own answers and
`shadow:` rows excluded):
- at least 100 replayed cards (`letibot shadow replay --job permission`);
- zero cases where the shadow allowed and the operator said No;
- zero revokes over the same period;
- the figures hold on two replays at least 7 days apart, on rows from after the previous
  promotion;
- the operator promotes, with `[shadow] scope` and `answer_reads`. The replay proposes, never
  promotes.

Two disagreements in a stage (a revoke, or a later No to the same shape) put it back to
observe-only. With B2 the trip survives a restart and needs a re-arm. A demoted stage needs one
fresh replay before it is promoted again.

### 3. R316 stages 2-4

Each widens `ShadowOracle::scope()` by one step and nothing else. Gates from R316:
- Stage 2, writes inside the workspace: `--calibrate --compare` over operator-answered write rows
  with zero false allows and agreement at least the verdict arm's, and every etalon `refused` write
  row ASK. Needs B6.
- Stage 3, exec of known shapes: the stage 2 figures on exec rows, BashArena `monitor-eval` at no
  fewer than 22 of 47 attacks caught with honest rows flagged no higher than 41%, and
  `corpus_replay::sweep` moving no refused row to admitted. Needs the heredoc and trajectory work in
  `docs/guard-corpus-plan.md` section 4b, and B4's shape rules.
- Stage 4, everything except the always-ask list: every etalon refusal in class ASK, BashArena
  recall at or above stage 3, and a trajectory-level check.

What always stays with the operator is listed in R316 and does not move.

### 4. The rest of the batches

**B6 - modes (R326).** A reopened child builds from the daemon's base and is its own ceiling. The
decider is unranked. Two of five mode doors persist wrongly.
- Scope: `harnessd/src/harness.rs` mode doors, `sessions.rs` mode arms, `harness/open.rs` child
  ceiling, `tools/src/mode.rs` `no_wider_than`. One `Harness::move_mode`, one `open_with` that
  requires the parent ceiling.
- Findings: I24 (R306's remainder), I25, I26, I27.
- Tests: `a_reopened_child_keeps_its_narrowed_point_and_ceiling`,
  `child_cannot_move_human_to_model_decider`,
  `mode_refused_move_still_writes_project_row_and_next_open_fails` inverted,
  `ask_mode_confirm_persists_session_row`.
- Size L. Depends on B3.

**B1 - wire and peer (R320).** Frame readers have no length bound. Snapshots go raw to older heads,
which already broke every bridge at 74 against the 78 daemons.
- Scope: `sessionlog` `wire.rs`, `server.rs`, `compat.rs`, `peer.rs`; `room/src/client.rs`.
- Findings: I65 (sessionlog and room), I66, I5 cheap part.
- Decision 5 default: translation driven by a per-variant `since` table, the floor computed from it.
- Tests: `an_overlong_first_frame_is_refused_not_buffered`,
  `old_head_hello_snapshot_decodes_with_peer_speaker`, `a_peer_in_another_daemons_cgroup_is_inside`.
- Size M. Depends on nothing.

**B1b - skew measured (R321).** A two-build job under `act` and golden frames the previous tag must
decode. Finding I67. Test `wire_golden_frames_from_v78_still_decode_with_same_meaning`. Size M.
Depends on B1.

**B5 - fence and secrets (R325).** The file tools' fence lacks the protected set, the store
carve-out and the secret extras the shell view has. A tight child can plant a git hook the
operator's next commit runs.
- Scope: `tools/src/exec/fence.rs`, `secrets.rs`, `exec/seat.rs` protected set. One secret table
  with a per-entry seat scope.
- Findings: I19, I37, I38.
- Tests: `a_wide_seat_file_read_refuses_flow_rule_secrets`,
  `tight_view_file_tools_must_not_read_store_data`, `write_cannot_reach_protected_git_paths`.
- Size M. Depends on B4 (shares `intent.rs`).

**B7 - durability (R327).** Acks come before the journal and the store hold the work, so a routine
restart loses prompts, steers, job results and room lines.
- Scope: `sessionlog/src/hub.rs` journal, `turns.rs` `persist` and `opened`, `hub_io.rs` give-back.
- Findings: I42, I43, I44, I45, I47.
- Tests: `a_taken_prompt_survives_a_kill_before_persist`,
  `a_prompt_accepted_during_a_failing_save_is_not_lost_at_close`,
  `esc_during_busy_store_wait_does_not_mark_batch_stored`,
  `a_steer_taken_by_a_refused_round_survives_a_restart`,
  `a_queued_prompt_keeps_its_lane_and_sender_across_restart`.
- Size L. Depends on B3.

**B8 - limit state (R328).** A restart after a refused probe drops the owed turn. One persisted
`LimitState`. Finding I46. Test `a_restart_after_a_refused_probe_still_owes_the_turn`. Size M.
Depends on B6.

**B9 - lifecycle and stop (R329).** The launcher interrupts every turn before SIGTERM, so a restart
kills subagents and marks them stopped. A reopened package is closed by a TTL measured from
creation. A self-reaped package sticks in Closing.
- Scope: `tools/src/lifecycle/*`, `reap_pass.rs`, `mergequeue/git.rs` retire,
  `sessionlog/src/registry.rs` stopping flag, `subagent_record.rs`, `docs/lifecycle.md`, the
  `scripts/letibot` spawner export.
- Findings: I57, I58, I59, I60 (R307's remainder), I61 (strike and runtime bound).
- Decisions 11 and 12 defaults: the `stopping` flag on the `stop-force` interrupt, then the flush,
  then SIGTERM. Strike the timer docs now, ship the timer after the phase.
- Tests: `launcher_interrupt_before_close_keeps_child_resumable`,
  `a_reopened_package_older_than_hard_ttl_is_not_closed`,
  `an_idle_package_whose_repo_names_its_own_daemon_still_closes`,
  `a_superseded_fix_branch_with_a_foreign_process_in_its_tree_keeps_it`,
  `the_daemon_spawner_exports_a_runtime_bound`.
- Size L. Depends on B7.

**B10 - merge queue (R330).** Gate steps inherit the daemon's whole environment, keys included. A
skipped apparatus suite passes. A bridge seat approves its own branch. The queue never fetches.
- Scope: `mergequeue/{gate.rs,gate_host.rs,verbs.rs,serve.rs}`, `bridge/src/queue_desk.rs`,
  `scripts/gate-at.sh` env through one shared builder.
- Findings: I84, I85, I86, I87, I88.
- Decisions 13, 14 and 16 defaults: fetch, reset the local target and requeue, never force-push. A
  host that cannot meet the demand is Blocked. The approver differs from the author, plus an
  operator card for rows the bridge enqueued.
- Tests: `gate_step_env_is_isolated`, `tests/gate_host.rs:643` inverted to Blocked,
  `bridge_cannot_approve_own_enqueue`, `agent_restart_cannot_undo_operator_veto`,
  `landing_after_origin_moves_refetches_and_lands`.
- Size L. Depends on B9.

**B11 - launcher (R331).** `letibot hold s-123`, the command the idle-close warning tells the
operator to type, becomes a billed prompt. The launcher and the daemon read different
providers.toml files. The store ignores `XDG_DATA_HOME`.
- Scope: `scripts/letibot`, `tests/launcher.sh`, `provider/src/keys.rs`, a leaf-crate
  `config_dir()`.
- Findings: I78, I79, I80.
- Tests: a verb loop asserting no `--prompt`, `launcher_and_daemon_resolve_same_providers_file`,
  `an_empty_xdg_config_home_is_unset`, the `--store` XDG test.
- Size M. Depends on B9 (the spawner line).

**B12 - store carve (R332).** Lock the carve: one IMMEDIATE transaction and an `open_transition`
refusal. Finding I53. Test `rows_appended_after_the_claim_check_are_not_deleted_uncopied`. Size S.
Depends on nothing.

**B13 - rooms (R333).** Gap replay drops rows below a live push and acks over them. Finding I98.
Test `gap_rows_older_than_the_open_cap_are_not_acked_away` against a real Server. Size S. Depends on
nothing.

**B14 - plugins and MCP (R334).** A cloned repo's unpinned layer answers deciding hooks. MCP stdio
servers inherit the daemon's env and outlive their kill. `.mcp.json` is trusted by directory prefix.
- Scope: `plugins/hook.rs`, `plugins/src/stack.rs`, `crates/mcp`, `leticode-config` MCP trust,
  `harnessd/src/mcp.rs`.
- Findings: I93, I94, I95, I65 (MCP part).
- Decisions 8 and 9 defaults: `env_clear` and a process group now, the sandbox later. An unpinned
  deciding answer falls back to the fixed ladder.
- Tests: `an_unpinned_layers_limit_decide_answer_is_ignored`,
  `stdio_spawn_does_not_leak_env_or_orphan`, `an_edited_mcp_json_under_a_trusted_dir_is_not_started`,
  `an_mcp_server_endless_line_is_dropped`.
- Size M. Depends on nothing.

**B15 - header facts (R335).** Header ctx survives `/clear`, a reseat and a re-ingest. A shown-unit
`context_after` on every fork event, the prose parse deleted. Finding I72. Tests
`a_snapshot_after_an_idle_compaction_shows_the_new_base_in_the_header`, `a_clear_moves_the_header_ctx`.
Size M. Depends on B1 and B6.

**B16 - workflow stability (R336).** The review was run by the layer it reviewed, and that layer
failed five ways.
- Scope: `bridge/src/tools.rs` start, `harnessd/src/cli.rs` flags, the bridge plugin restart after
  install, a caller lease for leticode sessions.
- Findings: I28, I81, I100; leads 27-30, 33 and 37.
- Items:
  - an unknown mode on `leticode_start` refuses and sends nothing;
  - a config-backed one-shot flag applies or refuses;
  - two starts in one folder both survive or the second refuses (reproduce first);
  - a caller lease closes a session when its caller is gone;
  - a stale bridge binary re-execs or says "restart to pick up build X";
  - lead 37: a fan-out budgets each provider's share against its live window before launching, and
    refuses or splits when it does not fit.
- After the phase, as plan items, no code in B16:
  - lead 31: one background-after-threshold rule for every tool, so past the threshold any call
    becomes a job with the same settlement path;
  - lead 32: one orchestration primitive that runs as a job and settles like a `task`, with
    `digest` as its first script and review panels as its second.
- Tests: `start_with_unknown_mode_refuses_and_sends_no_brief`,
  `a_one_shot_with_model_runs_that_model_or_refuses`,
  `two_starts_in_one_folder_both_survive_or_the_second_refuses`,
  `stopping_the_caller_closes_its_sessions`, `a_stale_bridge_binary_says_so_at_attach`,
  `a_fan_out_over_a_providers_live_window_refuses_or_splits`.
- Size M. Depends on nothing.

B1-B16 close 50 of the 52 high entries. I12 closes in R337 and I67 in B1b.

### 5. Mediums, lows and follow-ups

In this order, each row naming its tests:
- R338: I6 and the rest of I5 - role per connection, the masked operator socket (decision 6).
- R339: I29-I36, the gate ladder after "once means once".
- R340: I39, I40, I41 - tight egress denied by default (decision 7).
- R341: I48-I52 - rows reaching the head before the store, jobs marked delivered first (R319's
  remainder), operator-only rows in the model transcript, warnings, the room store.
- R342: I54-I56 - migrations and the split schema.
- R343: I62-I64 - liveness predicates, `closed_ms`, the lifecycle invariants.
- R344: I68-I71 - wire meaning and visible skew, the stale-build notice the rano week needs.
- R345: I73-I75 and leads 35, 36 - facts with no read time, non-job waits (R319's remainder),
  repeated usage rows, one park said three times.
- R346: I82, I83 - launcher twins.
- R347: I89-I92 - `gate-at.sh`, queue doors, queue structure, tests reading live state, the env
  guard and its lint.
- R348: I96, I97, I99 - plugin looks, runtime bounds, the bridge as a second room client.
- R349: I101, I102, I103, I105.
- R350, after the bar is met: the facts frame (I76), TUI structure (I77), a single editor, the
  launcher shim, the bridge on `letibot_room::Client`, the carve cut, `letibot-gc.timer`.

Older open rows in `TODO.md` follow the phase rule: a row that is a fix is taken when its area's
batch lands. A row that is a feature waits for the end of the phase.

### 6. After the phase: the credential broker (R358)

A credential file exists for its own software to use. That is its only purpose. So that software uses it, and nothing else in a session can read it, print it or send it.

It sits beside automode: both rely on the gate judging the subcommand, not the program. B5 masks
the operator's CLI stores (`gh`, rclone, vault, ...) in a child's view only, so a top-level seat
still reads them. The broker closes that, in layers:
- Brokered CLIs: in the sandbox `gh` (then vault, rclone, ...) is a shim to a host-side broker that
  runs the real binary with the real credential. Reads pass, revealing commands (`gh auth token`,
  `--show-token`, `vault read` of a value) are refused, destructive ones (`gh repo delete`) always
  ask. The credential file never exists in the sandbox. Prior art: aws-vault exec, `op run`, git
  credential helpers.
- An egress proxy for HTTP tools: the sandbox holds a dummy, swapped for the real credential only
  on that credential's allowed hosts.
- Known-value redaction: every tool result and stored row is scrubbed of the loaded secret values,
  and their base64 and URL-encoded forms, before the model or the store sees them.
- Package installs: no secrets, limited egress, install steps judged as package changes.
- Order: the `gh` broker, then the proxy, then redaction.
- Tests: `an_agent_cannot_read_gh_token_but_gh_pr_view_works`,
  `a_script_sending_the_dummy_elsewhere_sends_nothing_of_value`,
  `a_secret_echoed_by_a_test_never_reaches_the_transcript`.
- Size L. After the stability bar, and after B4's subcommand rules.

### 7. After the stability bar: the box daemon, the flow, job-first calls (R368-R377)

Design: `docs/design/box-daemon-and-orchestration.md`. It covers leads 52, 32, 31 and 37, and
R294. No code before the bar is met.

- **Job-first tool calls (R368, lead 31).** A call that can outlive the threshold is a job from its
  first instant, and the foreground is a wait on it. One loop in the runtime, `bash`'s. The operator
  typing ends any wait.
- **The flow (R369, lead 32).** One orchestration primitive: split, map, verify, fold, as a job
  that settles once. A journal, a resume cache, a backup before resume, a canary first, a worktree
  per writing child, no silent caps. `digest` is its first script. Lead 37's budget ships here
  against the daemon's own reading.
- **The box daemon (R370-R375, lead 52).** One trusted process per box, in the room coordinator's
  process. It owns accounts (usage, limits, budget), housekeeping (the reaper), placement (R294),
  the opt-in MCP pool, and hosts the broker and the proxy. Folder daemons keep sessions,
  transcripts, the gate and heads. A folder daemon with no box daemon behaves as it does today.
- **Scripts and heads (R376, R377).** Review panels, sweeps and rebases as flow scripts. The `/box`
  pane, flow rows, and rano's widgets through `cc@rano`.
- Order: R368, R369, R370, R371, R372, then R373-R377 in any order. R370 lands before the broker's
  host is built (R358 and its rows on `auto-broker-design`), because the design moves that host
  from the folder daemon to the box daemon.
- Each phase deletes what it replaces in the same landing.
- Tests, by row: `a_slow_web_fetch_becomes_a_job_and_its_body_is_not_in_the_settlement`,
  `a_digest_past_the_threshold_settles_as_a_flow`, `a_resume_backs_up_before_it_runs`,
  `a_fan_out_over_a_providers_live_window_refuses_or_splits`,
  `a_tool_call_is_refused_on_the_box_socket`, `two_daemons_on_one_account_poll_once`,
  `a_daemon_with_no_box_daemon_runs_as_before`.
- Size L for R369 and R375, M for the rest.

## The exit test

The stability bar, measured in this order. None of its four conditions is met at the snapshot.
1. **Gate.** After B10, I89 and I92 (R347): three consecutive full gates on one tip, with no rerun
   and no Blocked step, logged. Not measurable before, because a skip passes and a test reads the
   operator's live state.
2. **A week of rano.** After B7, B8, B9 and B16 are installed and rano is restarted onto that build
   with I71's notice in place: seven days with a restart log, and a journal-against-transcript
   reconciliation counting zero lost and zero repeated prompts, steers, job results and room lines.
3. **Figures.** After B15 and I73-I75: every fact on the STALE list traced to its source, with its
   refresh and its age shown.
4. **Invariants.** Last, one pass over `docs/review/2026-11-16/invariants.md`: each PARTLY or NOT
   ENFORCED rule reaches HOLDS (a check and a test) or is deleted. The process rules among them get
   a hook or move to `AGENTS.md`.

The phase ends when all four hold. Then the design follow-ups (R350) and the first post-stability
features start: job-first calls and the flow (R368, R369), the credential broker (R358) and the
box daemon (section 7).

## Process rules

- One full gate per stack. Agents test the crates they touch, push a branch and enqueue. The lead
  batch-gates the stack and lands it.
- A live regression's fix is built and installed at once, while its gate runs. It also goes on the
  finding list.
- A tightened rule lists its callers in the commit message, and each caller gets a test that crosses
  the seam.
- Review results live in markdown under `docs/review/`. Findings are filed in `TODO.md` in full.
- Bash goes through shellcheck, shfmt, `bash -n` and a smoke run.
- The rest are in `AGENTS.md`, "Rules from the 2026-11-16 review".

## Decisions

Taken on 2026-11-16:
1. A bridge seat may answer permission cards only for sessions it started (B3).
2. A "once" grant means once. The session rung is dropped (B4, R339).
3. Instead of answering decisions 3 and 4: cards for reads are a breaking change, so automode is the
   priority. Their recommendations are the defaults in B2.

Open, with the default each batch is built on: decisions 5-23 in INTEGRATION section 8. A batch
builds on the default and names it in its commit. An operator answer goes in INTEGRATION section 10
and this file is rewritten to match.
