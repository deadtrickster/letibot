# letibot invariants

Snapshot `9e9312b06` (origin/main, 2026-11-16). Worktree `~/Projects/letibot/wt-review-snap`. The previous edition was at `408353957`. Since then batches 8-12 (about 60 branches) and a comment-only deslop pass landed, so every `file:line` below was re-grepped at `9e9312b06`.

Each rule has its **source** (an operator decision with its date, a memory file, a TODO row, or a doc), **where it is enforced** (`file:line` under `crates/` unless the path says otherwise), and a **verdict**:

- **HOLDS**: a check in code, plus a test.
- **PARTLY**: holds on the main path, but there is a named gap.
- **NOT ENFORCED**: convention only, or contradicted by the code.

A rule whose verdict moved says `Was <verdict> at 408353957; <sha> (R-row).` Gaps are reading findings. Where one was not traced end to end it is marked HYP. Memory files are in `~/.claude/projects/-home-dead-Projects-letibot/memory/`. "Sweep" means `~/Projects/letibot/letibot-session-sweep-2026-11-15.md`. Raw evidence from the first edition is in `/data/review-prep/notes/` (refs there are at `408353957`).

## A. The conversation record

**I1. Never lose a conversation.** PARTLY.
- Source: TODO R276 ("operator's rule is never lose a conversation"), docs/lifecycle.md (c)3 and d.8, AGENTS.md.
- Enforced by:
  - WAL with `synchronous=FULL` (`tokencore/src/store.rs:1608`)
  - triggers that make `transcript_item` append-only (`tokencore/src/store.rs:527-536`)
  - `delete_empty_session` refuses a session that has rows (`tokencore/src/store.rs:3980 delete_empty_session`)
  - archive is a flag, and archive comes before delete (`tools/src/lifecycle/archive.rs`)
  - the prompt journal (`sessionlog/src/hub.rs:458 JournaledPrompt`, thread `:790`)
  - an ended job's delivery is marked when its row is stored, not after the rounds (R313, see I53)
- Gaps:
  - A row is **shown before it is stored**. `TranscriptAppended` is emitted inside `append_items` (`turn/src/engine.rs:487`), and `persist()` runs after it (`harnessd/src/harness/turns.rs:1713`, in `submit_items_then :1697`). A crash in between leaves a row the head drew and the store lacks.
  - The prompt journal writes on its own thread (`sessionlog/src/hub.rs:419 run_journal`), so a prompt accepted just before a crash can be lost.
  - Room JSONL skips lines it cannot parse (`room/src/store.rs:101`) and then compacts from memory (`room/src/store.rs:373 compact`) (HYP).
  - Migration is not transactional (`tokencore/src/store.rs:1848 migrate`, `:1927 migrate_from`).
- Untested: a crash between show and store.

**I2. The conversation is one record: one event is one row, exactly what the model was sent. No operator-only rows. Verbosity only hides.** PARTLY.
- Source: memory `conversation-is-one-record` (operator 2026-11-12), AGENTS.md standing rules, TODO R202.
- Enforced by: every warning code has exactly one destination (`sessionlog/src/warning.rs:1151 TWIN`, `:1269 destination`). Tests `sessionlog/src/warning.rs:1555 every_code_has_exactly_one_destination`, `tui/src/app/tests/onevent.rs:172 no_warning_is_a_row_of_the_conversation`.
- Gaps:
  - R202 left: a folded tool run still draws its own header.
  - Display-only rows were added since: the shadow's grant row on the main session (`OfferKind::Shadow`, `harnessd/src/shadow/permission.rs:359`) and the restart card's `down 25s` line (R312). Each needs checking against this rule.
  - Non-warning rows have no general check.

**I3. An agent's words never count as the operator's (Speaker::Peer).** PARTLY. Still the most load-bearing gap.
- Source: operator decision 2026-11-15 (memory `no-decisions-on-misattributed-data`), TODO R251 and R167, commit 7ab2f03fd.
- Enforced by:
  - `transcript/src/lib.rs:49 Speaker` (`peer("")` gives `unknown`, `transcript/src/lib.rs:100 peer`)
  - `sessionlog/src/hub.rs:87 peer` (`from`, or identity `letibot-mcp`, means a peer)
  - `harnessd/src/harness/turns.rs:255-262` (a peer goes to the trail as `Agent` and is stored as `Peer`)
  - steering: `harnessd/src/harness/hub_io.rs:275`
  - bridge `speaks_as`: `bridge/src/head.rs:1691`
  - compaction summaries are `Agent` (`turn/src/compaction.rs:1124`)
  - only Operator authorises (`tools/src/authorise.rs:24 Speaker`)
  - the trail reseed maps `Peer` and `Agent` to `Agent` (`harnessd/src/harness/trail.rs:138-141`)
- Gaps:
  - **A subagent brief written by the parent model is stored and trailed as the operator.** `harnessd/src/harness/children.rs:4223` calls `h.submit(&brief)`, and `submit` is `Speaker::Operator` (`harnessd/src/harness/turns.rs:201-202`). The oracle and automode read it as an operator utterance, and the reseed (`harnessd/src/harness/trail.rs:107 seed`) carries it across a restart. Compare `submit_a_parents_message` (`harnessd/src/harness/turns.rs:1610`). No test. Unchanged.
  - Command-file bodies, including a cloned repo's `.claude/commands`, still go out as operator prompts (`harnessd/src/sessions.rs:1211`, `run_prompt` with no sender). R283 (3107cc7a6) now applies their `allowed-tools` and `model` to the turn (`harnessd/src/sessions.rs:1185-1208`), which narrows the seat but does not change the speaker. `/skills NAME` is the same (`harnessd/src/harness.rs:1541`).
  - acp still sends as the operator (`acp/src/session.rs:368`, `prompt_with_context` with no sender). R251 left names flowy too.
  - `from` is asserted by the client. A new bridge talking to a pre-75 daemon stores agent lines as operator, silently.
  - The journal keeps the identity and drops the sender (`sessionlog/src/hub.rs:773-779`), so a restored bridge prompt is a peer named `letibot-mcp`, not `cc@x`.
  - Any connection that is neither a descendant nor in a letibot cgroup is the operator (`sessionlog/src/peer.rs:26 of`). That includes `systemd-run --user`, tmux or cron (HYP).

**I4. Agents never type into a head over tmux.** NOT ENFORCED.
- Source: operator 2026-11-15 (sweep section 4), AGENTS.md standing rules.
- Enforced by: nothing. A head cannot tell. R251, R253, R254.

**I5. Room peer text is a claim, flagged when it claims to be the operator.** HOLDS.
- Source: TODO R167.
- Enforced by: `room/src/chat.rs:627 claims_operator`, `room/src/claim.rs`. Test `room/tests/injection.rs`.

## B. Authority and the gate

**I6. A child's mode only narrows.** PARTLY. Main gap fixed by c2f6bc1ba (R306).
- Source: operator decision 2026-11-15, docs/boundary-and-adjudication.md section 4m, TODO R203, R120, R306.
- Enforced by:
  - at spawn: `tools/src/mode.rs:564 no_wider_than`, `:592 inherited_by`, called at `harnessd/src/harness/open.rs:2875`. Also the perimeter (`tools/src/adjudicate.rs:3019 grant_perimeter`), profiles (`tools/src/profiles.rs:105 narrow`) and skills `allowed-tools` (`harnessd/src/harness/open.rs:2692`).
  - after spawn: `harnessd/src/harness.rs:743 set_mode_consented` calls `:813 within_parent`. `mode_past_parent` (`harnessd/src/harness.rs:826`) runs before any row is written in the worker's `/mode` door (`harnessd/src/sessions.rs:4888`), the in-turn door (`harnessd/src/harness/turns.rs:3294`) and `ask_mode_confirm` (`harnessd/src/harness.rs:924`).
  - a parent that narrows narrows its children: the spawn hands over the live cell (`harnessd/src/harness/children.rs:4114 follow_parent`), and `narrow_to_parent` (`harnessd/src/harness.rs:853`) runs at turn start (`harnessd/src/harness/turns.rs:1826`) and before each call batch (`harnessd/src/harness/turns.rs:2652`).
  - **The guarantee is per batch, and that is what is promised**: a narrowing reaches a child at its next turn start or its next call batch, never in the middle of a call that is already running. A background job a child started keeps the point it was started at until it ends, because nothing re-gates a process that is already running.
  - a narrowing that cannot be applied does not leave the child where it is: `hold_at_floor` (`harnessd/src/harness.rs:940`) holds it at `read-only`, which drops every standing grant and makes every write and command an access the point does not seat, so the next call is refused rather than run at the wider point. Test `harnessd/src/harness/tests/child_mode.rs:290 a_failed_narrowing_refuses_the_next_call`.
  - tests `harnessd/src/harness/tests/child_mode.rs:66 a_child_cannot_widen_past_its_parent_by_mode_or_by_the_bridge`, `:125 a_parent_that_narrows_narrows_its_child`.
- Gaps:
  - **A child reopened after a daemon restart does not follow its parent's narrowing.** Its ceiling is a fresh cell holding the point it reopened at (`harnessd/src/harness/open.rs:2214-2217 parent_ceiling`). Only a spawn hands over the parent's live cell, so a later `/mode` on the parent never reaches it. R306 left. No test.
  - A narrowing reaches a child at its next turn or call batch, not while a call runs: the per-batch guarantee above, documented rather than fixed. A live child is narrowed by the narrowing pass, not by `set_mode_consented` fanning out through the registry, and a background job already started is not re-gated or killed.
  - `GrantScope::Once` is a default, not a ceiling. CLOSED by R339: `once_only_options` is gone, one builder (`tools/src/adjudicate.rs options`) reads the point's `grants`, and the recording site reads the same field, so a once point offers no session rung and records none.

**I7. The always-ask list shrinks only by the operator. A model never admits AlwaysAsk.** HOLDS.
- Was PARTLY at 408353957; 61eecc538 (R305).
- Source: docs/boundary-and-adjudication.md section 4d, R305.
- Enforced by:
  - `tools/src/intent.rs:2409 ALWAYS_ASK`, `tools/src/mode.rs:486 admits_unasked`, and the model path (`tools/src/adjudicate.rs:3558-3559`, which skips `AlwaysAsk`).
  - the permission step falls through on `AlwaysAsk` (`tools/src/adjudicate.rs:3362`), so a `permission.json` allow no longer settles it.
  - "Always allow" writes the exact segment (`tools/src/permission.rs:411 exact_rule_for`, used by `tools/src/adjudicate.rs:3907 derived_pattern`). A segment holding `*` or `?` gets no durable rung.
  - an allow whose wildcard covers a write or exec flag is an ask (`tools/src/permission.rs:423 evaluate_segment`, `:506 write_or_exec_flag`). The seed's `sort*` is `sort *` (`config/permission.json:123`).
  - tests `tools/src/permission.rs:974 an_always_answer_writes_the_exact_call`, `:994 the_seed_admits_no_write_or_exec_flag`, `tools/src/adjudicate/tests/outcomes.rs:725 an_allow_rule_does_not_beat_always_ask`.
- Left (by design): approved shapes admit a second MayApprove call of the same shape after one human answer. Seed rows that run project code (`make test*`, `npm test*`, `pytest*`) are guarded only by I12.

**I8. Gate order is fixed:** breaker, NotRun, Blocked, never-write, permission, boundary, point, standing grants, nobody, decision. An option the card did not offer is NotRun. A timeout is Deny. HOLDS.
- Source: boundary section 4l.
- Enforced by: one function, `tools/src/adjudicate.rs:3179 admit`. Tests in `tools/src/adjudicate/tests/` (13 files).
- New in the ladder since: kept grants from the store are read into the standing-grant step (`tools/src/adjudicate.rs:1630 with_grant_ledger`, `:1654 refresh_grants`, see I51).

**I9. Never show a button the gate would decline.** PARTLY.
- Source: AGENTS.md ("Never show a button the gate would decline... Keep both guards in step").
- Enforced by: `tools/src/adjudicate.rs:533 permission_options`, `:596 once_only_options`, chosen at `:2413-2421`.
- Gap: the comment at `tools/src/adjudicate.rs:2406-2408` says the recording site requires "a point that grants for the session", but the session branch at `tools/src/adjudicate.rs:3638` does not check `mode.grants`. The button is honoured, so nothing is declined, but the two guards are not in step as written (I6).

**I10. NEVER_WRITE is checked before any adjudicator, and nothing overrides it, allow-all included.** HOLDS.
- Source: boundary sections 3 and 4l arm 2.
- Enforced by: `tools/src/adjudicate.rs:1356 never_write`, `:4064 never_hit`, `:4020 ssh_resolution_hit`. The store list is one table (`tools/src/secrets.rs:37 STORES`), held against the gate, the flow rule and both seats by `tools/src/secrets.rs:107 every_store_is_denied_by_the_gate_the_flow_rule_and_the_seat`.

**I11. A compound command the splitter cannot read is asked, never allowed.** HOLDS.
- Source: boundary section 4l arm 3.
- Enforced by: the permission arm (`tools/src/adjudicate.rs:3330`).

**I12. On a raw host, a standing grant never admits a command that runs project code.** HOLDS.
- Source: D16 amended, R98.
- Enforced by: `tools/src/adjudicate.rs:2085 runs_project_code_unconfined`.

**I13. Project files narrow and never widen. A project `[mcp]` is never started.** HOLDS, re-scoped.
- Source: boundary section 4m.
- Enforced by: `tools/src/exec/perimeter.rs:152 widening_refusal`, `harnessd/src/harness/open.rs:2121-2124`.
- Changed by dd76c1bb3: a checkout's `.mcp.json` now starts when the user's `leticode.toml` lists it under `[mcp_projects] trusted` (`harnessd/src/mcp.rs:16 servers`, trust check `:36`). That is an operator act in the user file, so the rule holds, but its text should say "unless the user file trusts the checkout". Test `harnessd/src/mcp.rs:197 an_untrusted_checkouts_servers_are_named_and_not_started`.

## C. Secrets and credentials

**I14. Secrets never land in a stored row. Secret bytes never enter the transcript or leave the box.** PARTLY, with no choke point.
- Source: operator decision 2026-11-15, boundary section 3, D12.
- Enforced by:
  - flow tier `SecretToTranscript` (Blocked) for bash (`tools/src/intent.rs:2277`)
  - masks `tools/src/secrets.rs:37 STORES` plus `tools/src/exec/seatbelt.rs:85 SEAT_UNDER_HOME`
  - `SecretEnv::mask` on job output, `GH_TOKEN` only (`tools/src/exec/seat.rs:23`, `tools/src/exec/jobs.rs:564`)
  - `redact_key_args` for `/models --key` (`sessionlog/src/hub.rs:3040`)
  - secrets now go through the masked card and slash replies are typed (01e577978)
- Gaps:
  - **`read`, `grep` and `glob` skip the gate.** The gate runs only when the effective access is not unattended (`tools/src/runtime.rs:2264`), and unattended means `Read` or `Session` (`tools/src/schema.rs:129 is_unattended`). The file fence denies only the seat's masks (`tools/src/exec/fence.rs:38 of`), which come from `STORES`. It leaves out the rest of `intent::secret_dirs` (`.docker`, `.config/flowy`, `tools/src/intent.rs:1554`) and `SECRET_FILE_NAMES` (`tools/src/intent.rs:1571`, a workspace `id_rsa`). `cat` through bash is blocked, the `read` tool returns the bytes.
  - Nothing checks a row before `append_items`. Output from webfetch, MCP, provider error bodies and typed prompts is stored verbatim.
  - The mask is an exact substring, so any transform defeats it.

**I15. A credential is usable, never readable. No grant binds a private key.** HOLDS.
- Source: D12.
- Enforced by: `tools/src/exec/confine.rs:470 agent_from_env`, `tools/src/exec/mod.rs:94 NoCredentialMechanism`.

**I16. A child session cannot push. Only a top-level seat gets `GH_TOKEN` and the credential helper.** HOLDS.
- Source: AGENTS.md.
- Enforced by: `tools/src/exec/seat.rs:46 github_env`, given on the Wide seat (`tools/src/exec/seat.rs:151`). A child's tight seat carries a secret only when its perimeter names one (`harnessd/src/harness/open.rs:854 default_seat`), and widening a perimeter is an AlwaysAsk card.

**I17. The fleet private key (`~/bin/fleet/headless_server`) and `~/.config/letibot/keys` are never read into a session.** PARTLY.
- Source: memory `ci-runs-locally-with-act` (operator 2026-10-11).
- Enforced by: `.config/letibot` is masked in seats (`tools/src/exec/seatbelt.rs:90`). The shadow guard refuses `.config/letibot/keys` (`harnessd/src/shadow/guard.rs:32`) and its granted reads refuse the daemon's own config (`harnessd/src/shadow/guard.rs:134 OWN_CONFIG`).
- Gaps: `~/bin/fleet` is in no mask list (no match in `tools/src` or `harnessd/src/shadow`), so a Wide seat can read it (HYP). `scripts/gate-at.sh:40-42` still symlinks the real `keys`, `hosts.toml` and `known_hosts` into the gate's test HOME.

## D. Tests and the operator's live state

**I18. Tests never read the operator's `providers.toml`, store, models or live endpoints.** PARTLY, by convention.
- Source: TODO R198, R229, R285, AGENTS.md (`LETIBOT_CHECK_REAL_STORE`), sweep failure mode 7.
- Enforced by:
  - `tokencore/src/apparatus.rs:108 real_store_allowed`
  - `scripts/gate-at.sh:54-55` moves HOME and the XDG dirs
  - per-test temp HOME in `letibot/tests/*` and `tests/launcher.sh`
  - daemon-spawning tests get a private `XDG_RUNTIME_DIR` (f3fddd6b4), and harnessd tests share no HOME, data dir, store or runtime dir (e6cafa02f)
- Gaps:
  - **`letibot/tests/sudo_ask.rs` reads the real providers.toml.** `config` builds `Config::for_this_box` over the repo (`letibot/tests/sudo_ask.rs:86`) and leaves `HOME` and `XDG_CONFIG_HOME` as they are, so the harness resolves keys through `provider/src/keys.rs:194 config_file`. `harnessd/tests/sudo_ask_live.rs:98` moves HOME but not `XDG_CONFIG_HOME`, which `config_file` reads first (`provider/src/keys.rs:195`).
  - **`harnessd/src/config/tests.rs:42 a_section_that_turns_the_download_off_keeps_the_refusal` depends on the box.** It asserts the built-in model refusal, and was seen failing on a box whose `~/.local/share/letibot/models` holds the model. The test passes a scratch data dir (`harnessd/src/config/tests.rs:55`), so the path by which the box's models are reached is not traced (HYP).
  - **`harnessd/src/harness/tests/restart_notice.rs` needs the box's model apparatus.** Every case opens through `opened_by_a_later_daemon_with`, which needs `vocab_gguf()` (`harnessd/src/harness/tests/restart_notice.rs:72`, `harnessd/src/lib.rs:137`), and was reported needing a live model endpoint. It skips without them, so it is a failure only under `LETIBOT_REQUIRE_APPARATUS`.
  - `harnessd/src/config/tests.rs:407` calls `keys::resolve(p, None, None)` and reads the real providers.toml and key env vars.
  - `Config::for_this_box` (`harnessd/src/config.rs:1692`) leaves `providers_file: None`.
  - **The merge-queue gate** (`harnessd/src/mergequeue/gate.rs:474 apply`) sets only target, TMPDIR and jobs. It runs `cargo test --workspace` with the daemon's real HOME.
  - Agent-run `cargo test` inherits the live `LETIBOT_SOCKET` and HOME (`harnessd/src/harness/open.rs:1369`).
  - The launcher reads `XDG_DATA_HOME` only for the room (`scripts/letibot:1948`).

**I19. Model-backed tests use the running production GLM server and never load a model into VRAM. When it is down they skip loudly.** PARTLY.
- Source: memory `tests-use-production-glm` (operator 2026-09-16), AGENTS.md.
- Enforced by: `turn/src/serving.rs:39` (`LETIBOT_COMPLETION_URL`), `:49` (`LETIBOT_REQUIRE_MODEL`).
- Gap: GGUF-loading targets still exist (`grep -li gguf crates/*/tests/*.rs`, per AGENTS.md).

**I20. A missing apparatus is a loud skip ("THIS IS NOT A PASS"), and the gate turns it into a failure.** HOLDS on the queue's gate.
- Source: AGENTS.md, D10.
- Enforced by: `tokencore/src/apparatus.rs:55`, and `LETIBOT_REQUIRE_APPARATUS=1` in the gate block (`AGENTS.md:77`) and `scripts/rrun:111`.
- Gap: `scripts/gate-at.sh:55` does not set it. It now fails on any `FAILED` or `error` line as well as on fewer than 140 ok suites (`scripts/gate-at.sh:58-60`).

**I21. The model server on 127.0.0.1:8080 is never killed or restarted.** NOT ENFORCED (convention).
- Source: AGENTS.md.

## E. Lifecycle

**I22. A reaper never stops a session that has a head, a turn or a job.** PARTLY.
- Source: operator decision 2026-11-15, TODO R225, R241, R266, sweep failure mode 4.
- Enforced by:
  - `tools/src/lifecycle/reap.rs:146 close_due` returns early while the lease is held. A head renews it (`harnessd/src/reap_pass.rs:152 renew_held`), and so does a turn (`harnessd/src/reap_pass.rs:290 LeaseBeat`).
  - busy (`tools/src/lifecycle/reap.rs:433 busy`) and served elsewhere (`tools/src/lifecycle/reap.rs:381`) defer it (`:181`).
- Gaps:
  - The head of a landed package does not renew the lease (`harnessd/src/reap_pass.rs:163`).
  - Parent-closed (`tools/src/lifecycle/reap.rs:150`) and hard TTL (`:159-161`) fire before the busy check.
  - Hard TTL counts from `created_ms` even after a reopen, since a resume copies it (`tools/src/lifecycle.rs:460`). docs/lifecycle.md d.3 says "since creation or last reopen".
  - Its own daemon is a holding marked "never reaps itself" (`tools/src/lifecycle/reap.rs:1177`), so the owner may stay `Closing` (HYP).

**I23. Jobs survive a reaper stop.** PARTLY.
- Source: operator decision 2026-11-15, commit 17fb0d84b.
- Enforced by: the restart marker (`tools/src/lifecycle.rs:814 restart_marker`, written in `tools/src/lifecycle/reap.rs:1151 end_daemon`), read on SIGTERM (`harnessd/src/daemon.rs:291`), detach in `harnessd/src/kept_jobs.rs`. Test `harnessd/src/harness/tests/kept_jobs.rs`.
- Gaps:
  - The closing owner's own jobs end (`tools/src/lifecycle/reap.rs:717`, `keep_jobs = !owns_open_job`).
  - `DAEMON_GRACE` of 5 s then SIGKILL (`tools/src/lifecycle/reap.rs:52`) can interrupt a detach (HYP).

**I24. A stopped session reopens by id.** HOLDS through the bridge.
- Source: operator decision 2026-11-15, TODO R241 item 4, f561bed66.
- Enforced by: `bridge/src/tools.rs:908 reopen_gone`, `:723 attach_placed`, `bridge/src/daemon.rs:580 ensure_with`. Reviewers reopen on restart (`harnessd/src/mergequeue/review.rs:777`). Test `bridge/tests/reopen.rs`.
- Gaps: hard TTL closes a reopened old package (I22). The bridge passes `--no-queue` (`bridge/src/daemon.rs:324`), so a main-checkout daemon it reopens serves no queue (HYP). No end-to-end test through a real reaper stop.

**I25. Cleanup never touches live packages. Merged and clean does not mean finished.** PARTLY.
- Was NOT ENFORCED at 408353957; a88adbb82 (R307).
- Source: memory `cleanup-never-touches-live-packages` (2026-11-12), docs/lifecycle.md d.4 rule 7, R201, R307.
- Enforced by: `tools/src/lifecycle/in_use.rs:13 why_kept` keeps a tree that a live `session_writer` under it holds (`tokencore/src/store/writers.rs:145 writers_under`, another host's row keeps it) or that any readable process has its cwd in. Asked in the queue's settle (`harnessd/src/mergequeue/serve.rs:1456`), the release after a landing (`:3481`), the worktree sweep (`harnessd/src/mergequeue/serve.rs:1301`, `harnessd/src/mergequeue/git.rs:917 reap_worktrees`) and lifecycle's work settle (`tools/src/lifecycle/work.rs:205`). Dirty trees are kept (`tools/src/git.rs:322 remove_worktree`). Tests in `tools/src/lifecycle/in_use.rs:106-156`.
- Gaps:
  - **`retire_if_empty` has no liveness check.** `harnessd/src/mergequeue/git.rs:846 retire_if_empty` removes a superseded branch's tree when it has no commits of its own. The queue's caller asks only its own registry first (`harnessd/src/mergequeue/serve.rs:1627`, then `:1635`). The child cleanup calls it with no check at all (`harnessd/src/harness/children.rs:3555`). A package served by another daemon whose tree is clean and empty can still go. R307 left.
  - Another user's process with its cwd inside a tree is not seen (its `cwd` link cannot be read).

**I26. The reaper never touches its own owner or ancestors. The never-touch list comes first. It never signals a bare pid.** PARTLY.
- Source: docs/lifecycle.md d.4 items 5-6, d.10.
- Enforced by: `harnessd/src/supervise.rs:197 may_touch`, `tools/src/lifecycle/reap.rs:1177`.
- Gap: see I22 (self-owner).

**I27. The operator's own top-level session is never auto-archived.** HOLDS.
- Source: docs/lifecycle.md line 641.
- Enforced by: `tools/src/lifecycle/autoarchive.rs:226 operators_own`.

**I28. Nothing hangs on systemd.** PARTLY.
- Source: operator decision 2026-11-15, TODO R274 and R192, a50e92338, sweep failure mode 11.
- Enforced by: `activation/src/bounded.rs` (2 s query, 30 s step), the launcher's backgrounded probe (`scripts/letibot:230 scope_probe_start`), `harnessd/src/late_scope.rs`. Tests `letibot/tests/no_hang.rs`, `tests/launcher.sh`.
- Gaps:
  - After the probe, the real `systemd-run --scope` (`scripts/letibot:317 scope_run`, run by `detach :337`) has no bound.
  - On success, `bounded.rs` joins its pipe drains (`activation/src/bounded.rs:55-56`).
  - The job-scope sites (R65) and the gate's `SiblingScope` (`harnessd/src/mergequeue/gate.rs:1210`) were not checked.

**I29. Remote hosts are reaped by lease, even when the home box is gone.** NOT ENFORCED.
- Source: docs/lifecycle.md d.3 ("host-local reaper").
- Enforced by: nothing on the host. `tools/src/lifecycle/remote.rs:173 host_pass` runs from the home box over ssh (it now also removes settled owners' homes, e30635163).

## F. Shadow

**I30. Shadow acting verdicts quote only authorised rows: the operator's or the model's, never tool output or an agent.** PARTLY.
- Source: operator decision 2026-11-15, R251, e039da80a.
- Enforced by: `harnessd/src/shadow/verdict.rs:241 verify`, `:72 authored_span`, strict-only rules at `:345`, `:415` and `:481`, feed authors (`harnessd/src/shadow/feed.rs:25-29`), tests `harnessd/src/shadow/persistent_tests.rs`.
- Gaps:
  - With `[shadow] mode = "stateless"`, the during-offer author rule, the `none`-evidence rule and the listed-rule rule for permission are off (`harnessd/src/shadow/verdict.rs:345,415,481`), yet R261 skill picks still act. R317's own path re-checks the listed rule (`harnessd/src/shadow/permission.rs:272 acting_rule`). No test of the stateless skill pick.
  - A quote passes if any copy of it falls in an authorised span, so tool text the model echoes becomes quotable.
  - Command files and acp count as `Author::Operator` (I3).
  - The persistent conversation now resumes from its record after a restart (a489bc75a, `harnessd/src/shadow/resume.rs`), so a misattributed row outlives the daemon there too.

**I31. The shadow never denies.** HOLDS. See I49 for its grants.
- Source: operator decision 2026-11-15, R243 stage 3, R316.
- Enforced by: the parser accepts only `allow` or `ask` (`harnessd/src/shadow/verdict.rs:458-464`), the denylist on allow (`harnessd/src/shadow/guard.rs:32`), observe-only after the card for every ask outside R317's scope (`harnessd/src/shadow/permission.rs:263 scope`), tests `harnessd/src/shadow/tests.rs`.

**I32. The shadow does not drive decisions from misattributed data.** PARTLY. Same gaps as I3 and I30.
- Source: memory `no-decisions-on-misattributed-data`.

## G. Wire and store compatibility

**I33. A protocol mismatch is refused by name at attach.** NOT ENFORCED, and contradicted.
- Source: `sessionlog/src/protocol.rs:12-14`, workspace `Cargo.toml:110-122`.
- The code accepts and logs (`sessionlog/src/server.rs:731-739`). The TUI seats read-only. The bridge reports and prompts anyway. The doc comment still says "refuse a mismatch by name".

**I34. A new variant bumps `PROTOCOL_VERSION`. A room frame added mid-series rides the next number.** PARTLY.
- Source: the policy string in `sessionlog/src/protocol/tests.rs`, `room/src/wire.rs:10-18`.
- Enforced by: `sessionlog/src/protocol/tests.rs:7 every_frame_is_accounted_for_at_this_version`. The head bumped properly in the window: 76 (8d4943a7a, `TurnEnded`), 77 (`DriverTodos`, R260), 78 (`ToolResult` `ran_ms`/`decision`, R287, `sessionlog/src/protocol.rs:15`).
- Gaps: the room is still 12 (`room/src/wire.rs:15`, `OLDEST_CLIENT` 11 at `:19`) with `MayPrompt` and Hello `share`/`family` added under it. `Prompt.from` (7ab2f03fd) changed meaning with no bump.

**I35. An older daemon on a newer store opens only above the reader floor.** HOLDS.
- Source: store design, R229.
- Enforced by: `tokencore/src/store/floor.rs:39 READER_ARMS`, tests in `tokencore/src/store/tests.rs`. `SCHEMA_VERSION` is 51 (`tokencore/src/store.rs:207`). v50 (`session_grant`) and v51 (`nags_paused_at`, `nags_paused_by`, `granted_by`, `tokencore/src/store.rs:2634-2652`) are additive.
- Gap: concurrent and interrupted migration (seams S3).

**I36. A second copy of a list drifts. One source per closed set.** NOT ENFORCED (pattern). See I48 for facts.
- Source: AGENTS.md (`SettingRow::choices`).
- Fixed since: the secret store list is one table held by a test (`tools/src/secrets.rs:107`). The queue's stuck count and pane read one predicate (`tui/src/ui/panes/queue.rs:418 review_stuck`, `:426 merge_stuck`, e9c9eafc5, R309). List panes share one verb table (6c94d7c47).
- Live copies:
  - bridge `reach` maps every fyi to passing (`bridge/src/rooms.rs:409`) vs room `Reach` (`room/src/chat.rs:388 reach`)
  - three gate definitions (AGENTS.md, `scripts/gate-at.sh`, `scripts/rrun:111`)
  - seven config-dir resolvers
  - rano's protocol client
  - rrun's ssh flags vs `tools/src/remote.rs`
  - `HOOK_PREFIXES` in the SDK test
  - the rano rev in 6 manifests

## H. Merge queue and operations

**I37. Review and merge are separate queues. A red main halts merging, main goes to review first, and reviews continue.** HOLDS.
- Source: memory `review-and-merge-are-separate-queues` (operator 2026-10-11).
- Enforced by: `harnessd/src/mergequeue/serve.rs:1958 target_went_red`, `:55 halts_reviews`.

**I38. The gate is read from the target's `main`, so a branch cannot change its own gate.** HOLDS.
- Source: AGENTS.md.
- Enforced by: `harnessd/src/mergequeue/gate.rs:2056 declared_gate`, called with the target tip (`harnessd/src/mergequeue/serve.rs:2450`, `:2658`).

**I39. One thirty-minute deadline covers the whole gate run; the step in flight is killed (`timed_out`).** HOLDS.
- Was NOT ENFORCED at 408353957 (the docs said per step); 5e602864e brought `AGENTS.md` and docs/gate.md to the code.
- Source: `AGENTS.md:65`, `docs/gate.md:17,146`.
- Enforced by: `harnessd/src/mergequeue/gate.rs:21 GATE_DEADLINE`, one deadline per run (`harnessd/src/mergequeue/serve.rs:785`).

**I40. Fetch and rebase onto `origin/main` before every landing, because the Mac Claude commits to main.** NOT ENFORCED in the queue.
- Source: memory `mac-claude-commits-main`.
- The queue never fetches (no `fetch` in `harnessd/src/mergequeue/`). The CAS is on the local ref (`harnessd/src/mergequeue/git.rs:515 update_ref_cas`). A moved origin gives `landed_local` with no install.

**I41. One full gate at a time per box. Agents test only the crates they touch.** PARTLY.
- Source: memory `one-full-gate-at-a-time`, AGENTS.md, R258.
- Enforced by: the flock in `scripts/gate-at.sh:47-50`. Reviewer seats refuse whole-suite runs (`tools/src/exec/suite.rs`).
- Gap: the queue's own gate does not take the lock (no `gate.lock` in `harnessd/src/mergequeue/`, R258 not built).

**I42. Never install a binary under a launcher's name (`letibot`, `leticode`).** HOLDS at dist.
- Source: AGENTS.md, memory `letibot-is-the-shell-launcher`, R255.
- Enforced by: `scripts/check-dist-names.sh --built`. The multicall is `letibot-multi`.

**I43. A red stacked landing gets its failing commit fixed, never dropped.** NOT ENFORCED (process).
- Source: memory `fix-dont-drop`.
- The queue now bisects a red train (2eff65ee1, `harnessd/src/mergequeue/serve.rs:61-64`, `land_train :2529`), which finds the commit but says nothing of fixing it.

**I44. lubuntu3 is never used. The runners are lubuntu1 and lubuntu2.** HOLDS for reviewers.
- Source: AGENTS.md, memory `fleet-lan-addresses`.
- Enforced by: `harnessd/src/mergequeue/review/hosts.rs:30 FORBIDDEN`. `harnessd/src/mergequeue/gate_host.rs` does not read it.

**I45. Never edit live checkouts. Edits go in worktrees.** NOT ENFORCED (process).
- Source: memory `never-edit-live-checkouts`.
- `harnessd/src/mergequeue/git.rs:574 follow_checkouts` does `read-tree -m -u` on checked-out trees, by design.

**I46. rano changes go as a branch plus a DM to `cc@rano`. No push to rano master and no rev bump.** NOT ENFORCED (process).
- Source: memory `rano-changes-go-through-cc-rano`, AGENTS.md.

**I47. After any firecode storage move, run one VM round trip before starting packages.** NOT ENFORCED (process).
- Source: memory `firecode-canary-after-storage-change`.
- Partial in code: `down` waits for the copy-out record (`tools/src/firecode.rs:1059 down`, record at `:985-997`), and failure keeps the copy.

## I. Rules decided 2026-11-16

**I48. One source per fact. A copy carries the time it was read and is drawn with its age once older than its period. Restored state is checked against now at open, or dropped with a note.** PARTLY.
- Source: operator 2026-11-16 (partition.md stale-facts panel), R298, R301, R308, R309, R310, R311.
- Enforced by, per fact:
  - usage: polled every 120 s (`provider/src/usage_poll.rs:15 EVERY`), a failed round re-told with its age (`provider/src/usage_poll.rs:174 age`, R310)
  - the queue's stuck count: one predicate for edge and pane (`tui/src/ui/panes/queue.rs:418`, `:426`, R309)
  - todo rows waiting on ended jobs are freed with a note (`harnessd/src/harness/nags.rs:198 resolve_ended_job_waits`, R301)
  - a restored nag pause is checked against this daemon's life (I50)
  - kept grants are re-read at open and on every ledger generation (I51)
- Gaps:
  - No general mechanism. Each fact above was fixed one at a time, and the panel's list (header, status row, panes, bridge tools, seat boards) has no audit.
  - R310 left: Kimi's two 5 h windows can disagree, and `windows()` reads `limits[]` only when `usages` is absent.
  - R312 left: the restart row does not say which launcher or seat restarted the daemon.
  - The model name showing the previous model until the first turn was not traced.

**I49. The shadow never denies, and every grant it gives is revocable.** HOLDS.
- Source: operator 2026-11-16, R316, R317.
- Enforced by:
  - only `allow` or `ask` parse (`harnessd/src/shadow/verdict.rs:458-464`), and an `allow` needs a rule (`:459`)
  - it answers only a relayed review-queue read past the guard (`harnessd/src/shadow/permission.rs:263 scope`, `harnessd/src/shadow/guard.rs:138 read_passes`), asked before the card inside 2.5 s (`sessionlog/src/pending_asks.rs:255 note_before`), and acts only on a listed rule (`harnessd/src/shadow/permission.rs:272 acting_rule`)
  - its grant is written `by: shadow:<judge>`, never `human:` (`harnessd/src/shadow/permission.rs:389`), and kept in `session_grant` with `granted_by` (store v51)
  - `/shadow revoke` and `/permissions revoke` are one path (`harnessd/src/grants.rs:85 revoke`, ledger bump `:96`), and two disagreements turn answering off (`harnessd/src/shadow/permission.rs:170 BREAKER_AT`)
  - tests `harnessd` `answers::escalate_tests` (the R317 list), `tools` `view::tests::a_revoke_leaves_the_record_and_every_cursor_reads_it_once`
- Gaps:
  - The breaker lives in the process and resets at restart, while the grants are kept and re-applied at open (R317 left).
  - A revoke unbinds at each session's next call. Commands already running keep their namespace.
  - R317 does not go through `AuthorisationOracle`. No `ShadowOracle` exists. The only `impl AuthorisationOracle` is `harnessd/src/oracle.rs:337 HttpOracle` (R316 is a plan).
  - The reviewer's tool note still reads "by your answer" for a shadow grant.

**I50. A pause never survives a restart.** HOLDS.
- Was not a rule at 408353957. Before acb4c65cb (R311) a stored until-resumed pause came back after every restart.
- Source: operator 2026-11-16, R311.
- Enforced by: `harnessd/src/harness/nags.rs:626 restore_nag_pause` keeps a pause made at or after `daemon_life_started_ms` (`harnessd/src/nag_pause.rs:102`) and drops an older one with one `nags_resumed` row. A pause with no `nags_paused_at` (pre-v51) is dropped. The todos pane has no pause key. Test `harnessd/tests/nag_pause.rs:325 a_pause_holds_across_a_reopen_and_never_across_a_restart`.
- Gaps: `daemon_life_started_ms` is a lazy `OnceLock`, correct only if it is first read before any pause is written (HYP). Other pauses (limit parks, waits) are not covered by this rule.

**I51. Operator grants survive a restart and are revoked only by name.** PARTLY.
- Was not a rule at 408353957 (all grants lived in memory); 47894496a (R314).
- Source: operator 2026-11-16 on rano ("i allowed them for session and expect it to be preserved between restarts"), R314.
- Enforced by: `tokencore/src/store/grants.rs` (`session_grant`, scope `session` or `queue`), `tools/src/grant_ledger.rs`, the gate reading and re-reading them (`tools/src/adjudicate.rs:1630 with_grant_ledger`, `:1654 refresh_grants`, `:1731 keep_grant`), kept views bound before the first command (`harnessd/src/harness/open.rs:2027`, `:931`). Tests `harnessd/src/grants/tests.rs:105 a_view_grant_survives_a_daemon_restart`, `:174 a_queue_grant_does_not_widen_the_main_session`, `:195 revoking_a_grant_removes_it_from_the_store_and_a_running_session`.
- Gaps:
  - A child's view grants are kept under the child's id, so after a restart the parent does not re-read them.
  - A queue-scope grant covers every current and future reviewer until revoked. That is the intent, but it is a standing widening with no expiry.
  - The `/config` pane does not list grants. `/permissions` does.

**I52. A read goes to the file that holds its table on a split store.** HOLDS.
- Was not a rule at 408353957 (the old I1 did not cover it); 9489dbac3, 313efe571 (R313).
- Source: R313 (113 "no such table" lines on rano).
- Enforced by: project-table reads route through `tokencore/src/store.rs:1768 at` (for example `tokencore/src/store/subagents.rs:511 job_context`). `import_session` runs on the project file with `shared.db` attached (`tokencore/src/store/transfer.rs:312 import_tx`). `tokencore/src/store/split_coverage_tests.rs:253 every_public_method_on_a_project_table_is_exercised_on_a_split_set` scans the store source for project-table methods and fails on one the run does not call, and `:309 every_project_method_runs_on_a_split_set` runs them.
- Gap: the scan is lexical. A query built at run time from a table name it cannot see is not found (HYP).

**I53. An ended job is told exactly once, and never lost across a restart.** PARTLY.
- Source: R313 (operator: "a job was lost it seems with `leticode --restart`").
- Enforced by: every door marks its batch delivered once the carrying row is stored, before the rounds (`harnessd/src/harness.rs:1405 the_batch_stored`, called at `harnessd/src/harness/turns.rs:354`, `harnessd/src/harness/wall.rs:360`, `harnessd/src/harness/limit.rs:378`, `harnessd/src/harness/nags.rs:446`; `harnessd/src/jobwatch.rs:815 stored`, `harnessd/src/harness/deliveries.rs:1051 stored`). `Harness::wake` opens no turn once the registry is stopping (`harnessd/src/harness/turns.rs:440`).
- Gaps:
  - **The launcher interrupts before it signals.** `--stop` (and so `--restart`) runs `interrupt` over the socket (`scripts/letibot:2668`), then `stop_one`, which sends SIGTERM (`scripts/letibot:2672`). `is_stopping` is set only when the daemon's close begins (`sessionlog/src/registry.rs:2635`), which is after the SIGTERM. In the window between the two, a wake passes the check at `harnessd/src/harness/turns.rs:440` and opens a turn, as on rano (turn #530 interrupted at 01:33:57, wake opened #572 at 01:33:58). That turn is then killed by the SIGTERM. Its batch row was stored, so the job is marked delivered, and the next daemon does not tell it again although the turn that carried it never finished (HYP on the last step). R313 closed the after-SIGTERM half only. A fix needs the interrupt to set stopping, or the daemon to refuse wakes from the interrupt on.
  - The restart notice's jobs wait in `restart_owed` (`harnessd/src/harness.rs:422`) until a turn stores its row. A daemon that dies before that leaves them owed, by design.

**I54. A session's own spills, `/tmp` and build dirs are in its view.** HOLDS.
- Was not a rule at 408353957; ea766b0f7, eff5ff752 (R315).
- Source: R315 (a rano reviewer asked to read its own job output).
- Enforced by: `tools/src/lifecycle.rs:265 own_view` (session dirs, `owner_env` build dirs, children's spills), `tools/src/exec/confine.rs:367 ChildSpills` (read-only rule). Tests `tools/tests/confine.rs:1379 a_sessions_own_spill_is_in_its_view_and_another_sessions_is_not`, `:1489 a_sessions_build_dirs_from_the_gc_root_are_writable_with_no_ask`.
- Gap: the seatbelt regex rule for children is not exercised on Linux.

## Verdict changes since 408353957

- I7 always-ask and always-allow: PARTLY -> HOLDS (61eecc538, R305).
- I25 cleanup vs live packages: NOT ENFORCED -> PARTLY (a88adbb82, R307). Left: `retire_if_empty`.
- I39 gate deadline: NOT ENFORCED -> HOLDS (5e602864e, the docs now state the code's whole-run rule).
- I6 child mode only narrows: stays PARTLY, but the main gap closed (c2f6bc1ba, R306). Left: a child reopened after a restart.
- I13 project config: stays HOLDS, re-scoped to trusted `.mcp.json` (dd76c1bb3).
- New: I48 PARTLY, I49 HOLDS, I50 HOLDS (acb4c65cb, R311), I51 PARTLY (47894496a, R314), I52 HOLDS (9489dbac3, R313), I53 PARTLY (313efe571, R313), I54 HOLDS (R315).

## Summary

Holds:
- the gate order and its hard floors (I8, I10-I13, I15, I16), and always-ask now beats any allow (I7)
- the shadow never denies, and its grants are revocable (I31, I49)
- a pause never survives a restart (I50)
- split-store reads (I52) and own spills in view (I54)
- red-main handling (I37), the gate read from main (I38), the whole-run gate deadline (I39)
- the store reader floor (I35), operator sessions never auto-archived (I27), reopen by id (I24)

Not enforced, or contradicted by the code:
- tmux typing (I4)
- the host-local reaper (I29)
- protocol refusal (I33)
- list drift (I36)
- fetch before landing (I40)
- the process rules I43, I45-I47

Highest-risk partial rules:
- I3: the subagent brief and command-file bodies are still stored and trailed as the operator.
- I14: `read`, `grep` and `glob` skip the gate, and the fence misses `.docker`, `.config/flowy` and `id_rsa`.
- I18: tests that read the box (`letibot/tests/sudo_ask.rs`, the embeddings refusal test, `restart_notice`), plus the queue gate's real HOME.
- I53: the launcher's interrupt-before-signal window, where a wake opens a turn the SIGTERM then kills.
- I25 and I6: `retire_if_empty` with no liveness check, and a reopened child that does not follow its parent's narrowing.
