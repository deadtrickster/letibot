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
effect a breaking change. The way out is the shadow answering them through the real oracle. From
stage 3 on automode runs code, and code is where a credential is read and sent, so the credential
broker is part of the same order.

## Order

1. Automode and the credential broker: twelve steps, one section.
2. The rest of the batches in dependency order: B1, B1b, B5, B6, B7, B8, B9, B10, B11, B12, B13,
   B14, B15, B16.
3. Mediums and lows by theme (R338-R349), then the design follow-ups (R350) after the stability bar
   is met.
6. After the bar: the credential broker (R358), beside lead 32.

Section 1 runs beside section 2. A stage waits on its replay evidence, not on code, so batches and
broker steps land while it collects.

Sizes: S under about 300 lines, M up to about 1000, L above that. Each batch lands with its proving
tests, through one full gate. Every proving test fails on the snapshot.

### 1. Automode and the credential broker

Design: `docs/design/automode-and-broker.md`. It holds the threat model, each layer, the protocol and
the open questions. This section holds the order.

Two rules. A credential file exists for its own software to use, and nothing else in a session
reads, prints or sends it. A stage is promoted by the operator on replay evidence, never
automatically.

On main at `ccc95d272`: B3 (R323), B2 (R322), B4 (R324), B6 (R326) and the shadow as the real
`AuthorisationOracle` (R337). Their rows keep the residue. B5 (R325) is built on `b5-fence` and is a
prerequisite of steps 3 and 5.

Promotion, for every stage: figures on operator-answered rows only (timeouts, the gate's own answers
and `shadow:` rows excluded), holding on two replays at least 7 days apart, on rows from after the
previous promotion. The operator promotes with `[shadow] scope`. Two disagreements put a stage back
to observe-only until re-armed, and a demoted stage needs one fresh replay.

| # | step | row | needs | size |
|---|---|---|---|---|
| 1 | Cards per hour: the metric and its baseline | R360 | - | S |
| 2 | Stage 1, reads: every session, then queue seats on a standing instruction | R316, R361 | - | S |
| 3 | One redactor at every door | R362 | B5 | M |
| 4 | Stage 2, writes inside the workspace | R316 | 3 | - |
| 5 | The intent table for credentialed CLIs | R363 | B5 | M |
| 6 | The CLI broker, `gh` first | R364 | 3, 5 | L |
| 7 | Tight egress through a proxy: deny, tunnel, allowlist (takes I39 from R340) | R365 | - | L |
| 8 | Proxy termination: dummy to real, method and route policy | R366 | 5, 7 | L |
| 9 | The install seat | R367 | 7 | M |
| 10 | Script text in the oracle brief, and the checkpoint commit | R368 | - | M |
| 11 | Stage 3, exec of approved shapes, on the canary run | R316, R370 | 4, 6, 8, 9, 10 | M |
| 12 | The remaining CLIs, the trajectory check, stage 4 | R369, R371, R316 | 11 | L |

Steps 1, 2, 3, 5, 7 and 10 start now and do not wait on each other. Stages 1 and 2 need no broker.
Stage 3 does: it is not promoted on a seat that holds a real credential or the host's network.

**1. Cards per hour (R360).** Asks that reached a person, per hour of agent work, on the replay
corpus and live.
- Tests: `cards_per_hour_counts_only_asks_a_person_saw`, `parked_and_idle_time_is_not_agent_work`.
- Gate: none. It produces the baseline every later step reports against.

**2. Stage 1, reads (R316, R361).** Every session first (1b, built). Queue seats answer nothing
until the operator's `[queue] reviewers_may` sentence is in a reviewer's trail as a `Standing`
entry, citable for stage-1 reads only.
- Tests: `at_automode_the_shadow_grants_a_view_read_without_a_card`,
  `the_shadow_oracle_never_answers_an_always_ask`,
  `a_reviewers_brief_is_not_a_citable_instruction`,
  `a_standing_instruction_is_citable_for_a_queue_read_and_nothing_else`,
  `a_project_file_cannot_set_reviewers_may`.
- Gate: at least 100 replayed cards (`letibot shadow replay --job permission`), zero shadow allow
  against an operator No, zero revokes. Queue rows cited `standing` are counted apart, with their
  own 100.

**3. One redactor (R362).** Known values and their encodings, at the tool result, the stored row,
the shadow feed, the bridge, the logs and the provider request. Closes I40.
- Tests: `env_to_file_then_read_stores_no_token`, `a_known_secret_is_redacted_in_the_shadow_feed`,
  `a_base64_of_a_known_secret_is_redacted`, `a_secret_split_across_two_chunks_is_redacted`,
  `a_bridge_snapshot_carries_no_known_secret`, `a_known_secret_in_a_provider_request_warns`.
- Gate: the etalon's ground truth (the shipped transcript with a private key) stays at zero lines.

**4. Stage 2, writes (R316).**
- Tests: `the_shadow_scope_at_writes_admits_a_project_write_and_asks_outside_it`,
  `child_cannot_move_human_to_model_decider`.
- Gate: `--calibrate --compare` on write rows with zero false allows and agreement at least the
  verdict arm's. Every etalon `refused` write row is ASK. Cards per hour reported.

**5. The intent table (R363).** One table for `gh`, `aws`, `vault`, `rclone`, `psql` and npm auth:
reveal, payload, read, write, destroy, login, escape. Reveal is `Blocked` for every speaker.
- Tests: `gh_auth_token_is_reveal_and_blocked`, `gh_repo_delete_is_destroy_and_always_asks`,
  `gh_api_with_a_method_flag_is_not_a_read`, `an_unknown_subcommand_of_a_credentialed_cli_asks`,
  `every_credentialed_cli_row_has_a_fixture`, `a_reveal_cited_by_the_operator_is_still_blocked`.
- Gate: `corpus_replay::sweep` moves no refused row to admitted. False prompts on rows that ran are
  counted, and a row that adds prompts and catches nothing does not land.

**6. The CLI broker (R364).** A shim in the seat, a host broker, the real CLI in a credential seat.
No real `GH_TOKEN` in any seat. Needs the operator's yes on open question 1.
- Tests: `a_seat_holds_no_real_github_token`, `the_gh_shim_runs_gh_in_the_credential_seat`,
  `a_script_deleting_repos_through_gh_asks_with_its_parent_chain`,
  `a_brokered_reveal_is_refused_for_every_caller`, `the_broker_drops_env_that_configures_the_cli`,
  `a_tool_calls_own_admitted_subcommand_is_not_asked_twice`,
  `an_overlong_broker_frame_is_refused`.
- Gate: the canary run for `gh` - zero canary bytes in any row or leaving the box.

**7. Tight egress through a proxy (R365).** `Egress::Proxied`: an empty namespace, a socket, a
host allowlist. Tunnel and deny only. Carries decision 7 and I39.
- Tests: `a_tight_seat_command_cannot_reach_loopback`,
  `the_free_admit_fires_on_a_production_tight_boundary`,
  `a_host_off_the_allowlist_is_refused_at_the_proxy`,
  `a_client_that_ignores_the_proxy_fails_and_does_not_leak`.
- Gate: none beyond the tests. It changes no stage.

**8. Proxy termination (R366).** A name-constrained CA per daemon, the session's dummy swapped for
the real credential on its own hosts, a method and route policy per host as config.
- Tests: `a_dummy_token_is_swapped_only_for_its_own_host`,
  `a_request_without_the_dummy_gets_no_credential`,
  `a_raw_api_delete_through_the_proxy_asks`, `a_push_to_the_sessions_own_remote_passes`,
  `a_graphql_delete_repository_mutation_asks`, `the_ca_signs_only_terminated_hosts`.
- Gate: the canary run with raw API calls.

**9. The install seat (R367).** `npm install`, `pip install`, `cargo fetch`: no secret, no dummy, no
broker socket, registry hosts only.
- Tests: `an_npm_postinstall_sees_no_secret_and_reaches_only_the_registry`,
  `a_cargo_build_script_cannot_open_a_socket_off_the_allowlist`,
  `a_private_registry_token_is_injected_for_its_host_only`.
- Gate: none beyond the tests.

**10. Script text and the checkpoint commit (R368).** `ModelBrief` carries the script a command
runs, capped, as data. Before an oracle-admitted exec the tree is recorded under `refs/letibot/`.
- Tests: `the_oracle_brief_carries_the_script_as_data`, `script_text_is_never_a_citation`,
  `npm_run_and_make_targets_are_read_as_scripts`,
  `an_oracle_admitted_exec_records_a_checkpoint_first`.
- Gate: `--compare` with and without the script section, on exec rows. It lands only if false
  allows stay at zero.

**11. Stage 3, exec of approved shapes (R316, R370).**
- Tests: `stage_three_refuses_a_seat_with_a_real_credential_or_host_egress`,
  `an_unseen_shape_still_asks_at_stage_three`, `the_canary_run_reports_zero_bytes_out`.
- Gate: the stage 2 figures on exec rows. BashArena `monitor-eval` at no fewer than 22 of 47 caught
  with honest rows flagged no higher than 41%. `corpus_replay::sweep` moves no refused row to
  admitted. The canary run at zero. Cards per hour against the 2 per hour target.

**12. The remaining CLIs, the trajectory check, stage 4 (R369, R371, R316).**
- Tests: one fixture walk per CLI (`every_credentialed_cli_row_has_a_fixture` extended),
  `a_trajectory_that_writes_then_runs_a_script_is_judged_as_one`,
  `stage_four_never_answers_a_destroy_through_a_credential`.
- Gate: every etalon refusal in class is ASK, BashArena recall at or above stage 3, the trajectory
  check, the canary run repeated. Cards per hour against the 0.5 per hour target.

What always stays with the operator is listed in R316 and in section 11 of the design. It does not
move.

### 2. The rest of the batches

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

### 3. Mediums, lows and follow-ups

In this order, each row naming its tests:
- R338: I6 and the rest of I5 - role per connection, the masked operator socket (decision 6).
- R339: I29-I36, the gate ladder after "once means once".
- R340: I41. I39 moved to R365 and I40 to R362 (section 1).
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
features start: the orchestration primitive (lead 32) and the credential broker (R358).

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
