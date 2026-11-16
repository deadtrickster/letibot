# Review partition

Snapshot `9e9312b06` on `origin/main` (2026-11-16). Detached worktree `~/Projects/letibot/wt-review-snap`, kept for reviewers. All reviewers read the code there, plus `map.md`, `seams.md` and `invariants.md`. Raw evidence from the first reading (at `408353957`) is in `notes/`. Its `file:line` refs are not re-pinned. Use the four backbone files for current lines.

The template is `~/Projects/hands-off/templates/adversarial-review.md`:
- 3 positions per panel, round 1 under 600 words, round 2 under 250 words on rotated models, then synthesis.
- Reviewers may run one named test and never the whole suite.

## 1. Deslop - done

The comment-only pass landed as 44 `deslop:` commits ending at `9e9312b06` (crates, `scripts/`, README, `AGENTS.md`, `docs/`). The panels review the deslopped tree. A finding that a comment or doc is stale is still a finding.

## 2. Area panels (10)

Risk levels:
- **Deep** means 3 reviewers, both rounds, and every claim needs `file:line`.
- **Light** means 3 reviewers in round 1 only, with round 2 only when they disagree.

Positions follow the template. The first argues it does not do what it claims. The second argues it works for the real workloads. The third argues against both, that the implementation is poor regardless.

### A1. Turn engine and provider doors - deep
- **Scope.** `turn/`, `provider/` (with `provider/src/usage_poll.rs`), `backend/`, `http/`, `dialect*/`, `llama/`, `tokencore/src/ledger.rs`, `harnessd/src/harness/{turns.rs, limit.rs, limit_hook.rs, fork.rs, usage.rs, wall.rs, hub_io.rs}`.
- **Positions.**
  1. The loop loses or reorders work. Check show-before-store (`turn/src/engine.rs:487` emits inside `append_items` before `harnessd/src/harness/turns.rs:1713` persists), steering, the queue lane beside steer (R290), the park/fallback/limit ladder (`harnessd/src/harness/limit.rs:466 after_the_ladder`, the `on_limit` default Wait at `harnessd/src/config.rs:1733`), the `limit_decide` hook forking a new session mid-turn (R233), and the retry budget and jitter (dbe4c3b9c).
  2. Real long sessions on GLM, Kimi and deepseek survive limits, compaction and restarts. Show it from the tests and the restore paths.
  3. Against both: limit logic now spans http, provider, the usage poll, the limit hook and harnessd, there are two transports, `http` has no TLS, and the turn code is too large to reason about.

### A2. Permission gate and adjudication - deep
- **Scope.** `tools/src/{adjudicate*, intent*, permission.rs, authorise*, mode*, grant.rs, grant_ledger.rs, gatekeeper.rs, view.rs}`, `config/permission.json`, `docs/boundary-and-adjudication.md`.
- **Positions.**
  1. The gate admits what it must ask about. R305 closed the derived `program*` rule, the allow-before-AlwaysAsk step (`tools/src/adjudicate.rs:3362` now falls through) and the seed prefixes (`tools/src/permission.rs:506 write_or_exec_flag`). Check what is left: approved shapes admitting a second call after one answer (`tools/src/adjudicate.rs:1388 approved_shapes`), seed rows that run project code, `GrantScope::Once` and the session rung (`tools/src/adjudicate.rs:596 once_only_options`), the reviewer probe at allow-all, and the model admit path.
  2. The ladder is one function, its order is documented, the R305 tests fail without the fix, and the always-ask list is honoured on cards.
  3. Against both: text-prefix rules plus a flag deny-list plus a 6.8k-line intent normaliser are the wrong primitives. Two option builders and two guard sets must be kept in step by hand.

### A3. Confinement, seats and secrets - deep
- **Scope.** `tools/src/exec/` (confine, seat, fence, boundary, perimeter, scope, diskio, host, jobs), `tools/src/{secrets.rs, view.rs, runtime.rs}`, `tools/src/exec/seatbelt.rs`, `tools/src/lifecycle.rs:265 own_view`, `tools/src/lifecycle/in_use.rs`.
- **Positions.**
  1. Secrets leak. Check `read`, `grep` and `glob` skipping the gate (`tools/src/runtime.rs:2264`, gated path only) while the fence leaves out `tools/src/intent.rs:1554 secret_dirs` and `:1571 SECRET_FILE_NAMES`, egress following the mode (`tools/src/exec/seat.rs:160 network_rule`), the exact-substring `GH_TOKEN` mask, no check before `append_item`, and the R315 own-view grants (spill, `/tmp`, `owner_env` build dirs) widening a tight seat.
  2. bwrap and Seatbelt with cleared env, NEVER_WRITE, agent-socket credentials (D12) and the confinement tests make up a real boundary.
  3. Against both: the confinement tests pass vacuously inside a sandbox, the mask lists exist in three places, and the Wide seat is nearly the host.

### A4. Harness sessions, children, restart - deep
- **Scope.** `harnessd/src/harness/` (children, open, trail, shadow_seat, fork, nags, deliveries), `harnessd/src/{harness.rs, sessions.rs, sessions/, restart.rs, writer.rs, kept_jobs.rs, nag_pause.rs, answers.rs, answers/}`.
- **Positions.**
  1. Authority and identity break across children and restarts. Check the brief still sent through `submit` (`harnessd/src/harness/children.rs:4223`, which is `Speaker::Operator` at `harnessd/src/harness/turns.rs:202`), the R306 ceiling (`harnessd/src/harness.rs:813 within_parent`, `:853 narrow_to_parent`) and its reopened-child gap, the journal dropping `from`, restart owed delivery (R313), nag pauses across restart (R311) and `/fork`.
  2. Children clamp at spawn and follow a live parent, restart restores within 1 h, and R313 marks delivery when the carrying row is stored.
  3. Against both: the 45k-line `harness/` with `turns.rs` as the hub of everything is unreviewable, and the open/reseat paths are duplicated.

### A5. Store and migrations - deep
- **Scope.** `tokencore/src/{store.rs, store/, migrate_split.rs, ledger.rs}` (new `store/grants.rs`, `store/split_coverage_tests.rs`), `harnessd/src/config.rs` (`StoreAt`), `letibot/src/{gc.rs, migrate.rs}`.
- **Positions.**
  1. A box-wide upgrade can corrupt or wedge `shared.db`. Check migration with no transaction or lock while every daemon migrates on open, stray tables, two connections per file, project-table reads on the shared connection (R313 routed them through `at(table)`; is the coverage scan complete?), and `import_tx` committing two WAL files one after the other.
  2. The reader floor, the append-only triggers, WAL with `synchronous=FULL`, the unwind and reopen test per version, and the split coverage test that fails on an unrouted method.
  3. Against both: the carve-in-progress state, string-replaced foreign keys and arms out of numeric order are needless complexity.

### A6. Merge queue and gate - deep
- **Scope.** `harnessd/src/mergequeue/`, `harnessd/src/queue_asks.rs`, `tokencore/src/store/queues.rs`, `AGENTS.md` Merge gate, `scripts/{gate-at.sh, install-gate-build.sh, rrun}`, `docs/gate.md`.
- **Positions.**
  1. The queue lands or reports wrongly. Check the gate deadline, no fetch with a local CAS, install off-box, `retire_if_empty` with no liveness check (`harnessd/src/mergequeue/git.rs:846`), the bisected red train (2eff65ee1), the three gates, and gate-at.sh judging by a count.
  2. Review and merge separation, red-main halting, the approved-sha binding, R307's `why_kept` before `settle_tree` (`harnessd/src/mergequeue/serve.rs:1436`) and `reap_worktrees` (`harnessd/src/mergequeue/git.rs:917`), and the serve tests.
  3. Against both: 35k lines for a queue, and the rules about what is gated live in three places.

### A7. Lifecycle, reaper, jobs - deep
- **Scope.** `tools/src/lifecycle/` (reap, autoarchive, plan, logs, remote, work, in_use, worktree), `tools/src/lifecycle.rs`, `harnessd/src/{reap_pass.rs, daemon.rs, kept_jobs.rs, supervise/, jobwatch*, restart.rs}`, `tools/src/spill.rs`, `scripts/letibot` (`--restart`, `--stop`), `docs/lifecycle.md`.
- **Positions.**
  1. The reaper and the restart lose live work. Check hard TTL from `created_ms` after a reopen, parent/TTL before busy, the landed-package head, a daemon closing its own owner, the launcher's interrupt landing before the signal (R313 left), and no host-local reaper.
  2. Lease, busy and served-by, restart markers, `in_use::why_kept`, R313 delivery marking, and gc plans with archive-before-delete handle the cases that bit.
  3. Against both: lifecycle facts are split between owner rows, runtime records, cgroups, `/proc` cwd links and git state, and the design doc describes components that do not exist.

### A8. Bridge and rooms - deep
- **Scope.** `bridge/`, `room/`, `harnessd/src/room/`, `claude-plugin/`, `acp/`, `flowy/`, `docs/{room.md, claude-code-bridge.md}`.
- **Positions.**
  1. Mail is lost or misdelivered. Check wire changes under one room protocol number, fyi reach divergence, reply matching by arrival order, suffix recursion, the new queue verbs and `wait-queue` (e66038adb), and `prompt_once` double delivery.
  2. R259 settled the acks, family mail and replay window, and the real-`Server` tests cover the paths in use.
  3. Against both: the bridge re-implements the room client, reach and socket derivation, and its main room test runs against a fake coordinator.

### A9. TUI head and renderer - light
- **Scope.** `tui/`, `ui/`, `vt/`, rano usage (`rano::agent`, `term`, `style`), `docs/{room-tui.md, tui-testing.md}`.
- **Positions.**
  1. The head shows things the model was not sent, or draws twice. Check against I2: operator-only rows, the limit lines above the composer (6e31b27f7), the compaction phases, the shadow's display-only grant row, and the click/fit ladders.
  2. Screen tests with recorded click facts, the fit ladder, and a test over every pane (one verb table, 6c94d7c47).
  3. Against both: `tui/src/app` at 116k lines and widgets living in rano make every change cross-repo.

### A10. Extension surfaces: plugins, MCP, commands, skills - deep
- **Scope.** `plugins/`, `plugin-sdk/`, `plugins-rano/`, `builtins/`, `plugins/builtin/`, `harnessd/src/plugins/`, `mcp/`, `tools/src/builtins/external/mcp.rs`, `harnessd/src/{mcp.rs, commands.rs, skills_offer.rs, policy_skills.rs}`.
- **Positions.**
  1. Untrusted repo content gets authority. Check unpinned layers answering `steer_turn`, `compact` and now `limit_decide`, pinned layers exec'ing ungated, the unimplemented `Http` capability, command files sent as the operator (R283 now applies `allowed-tools` and `model`), trusted `.mcp.json` (dd76c1bb3), and unconfined MCP stdio servers.
  2. No host imports, fuel and memory caps, a fresh instance per call, a project `[mcp]` refused unless trusted, MCP restart with backoff, and MCP calls classed Network and gated.
  3. Against both: the hook surface (20+ kinds) has 1-4 tests per host crate, and MCP's test count is small for its surface.

## 3. Seam and invariant panels (9)

### P1. Wire compatibility and version skew - deep
- **Scope.** seams S1, S4 and S10, invariants I33 and I34. Files `sessionlog/src/{protocol.rs, wire.rs, server.rs, client.rs}`, `room/src/wire.rs`, `bridge/src/head.rs`, rano `src/letibot.rs`.
- **Positions.**
  1. Skew silently changes meaning. Check three head bumps in two days (76 `TurnEnded`, 77 `DriverTodos`, 78 `ToolResult` `ran_ms`/`decision`), room changes under one number, rano's copy, an unbounded line, and the daemon accepting mismatches.
  2. Accept-and-say is the right policy for a fleet of mixed builds, and the head locks read-only.
  3. Against both: with no captured old-build frames and no two-build test, nobody can know what skew does, so add golden frames.

### P2. Speaker and authority - deep
- **Scope.** invariants I3, I4, I30 and I32, seams S2, S6 and S19. Files `transcript/src/lib.rs`, `sessionlog/src/{hub.rs, peer.rs}` (`hub.rs:87 peer`, `:458 JournaledPrompt`), `harnessd/src/harness/{turns.rs, children.rs, trail.rs}` (`turns.rs:202`, `:655`), `harnessd/src/sessions.rs:1211`, `tools/src/authorise.rs`.
- **Positions.**
  1. Agent text still reaches operator authority: the subagent brief, command files, flowy and acp, an old daemon, the journal, and peers outside the cgroup.
  2. R251 closed the paths in use, room text is `Agent`, and the gate authorises only Operator.
  3. Against both: a speaker asserted by the client with an Operator default is the wrong design. Authority should come from the connection, not the frame.

### P3. Shadow - deep
- **Scope.** seams S11 and S20, invariants I30, I31 and I49. Files `harnessd/src/shadow/`, `harnessd/src/harness/shadow_seat.rs`, `harnessd/src/{queue_asks.rs, answers.rs, note_accept.rs}`.
- **Positions.**
  1. The shadow can be steered by injected text: stateless-mode holes, echoed tool text that becomes quotable, misattributed operator rows, and a resumed persistent conversation (a489bc75a) carrying an old injection across a restart.
  2. It never denies, quotes are checked against spans, persistent mode is strict, and its one acting permission path is gated on a listed rule and a breaker.
  3. Against both: a second model in the loop with its own prompts and record adds cost and complexity for nudges a rule could make.

### P4. Never lose a conversation and one record - deep
- **Scope.** invariants I1 and I2, seams S2 and S3. Files `turn/src/engine.rs:432-500`, `harnessd/src/harness/turns.rs:1713`, `sessionlog/src/{hub.rs, warning.rs}`, `tokencore/src/store.rs:526-538` (triggers), `:1608` (`synchronous`), `room/src/store.rs`.
- **Positions.**
  1. A crash loses rows or prompts: show before store, the journal thread, room compaction, non-transactional migration, a resume that re-announces rows (`append_row_at`, R312).
  2. Append-only triggers, WAL with FULL sync, the journal and archive-not-delete make loss practically impossible.
  3. Against both: the warning destination table covers warnings only, and nothing structural stops an operator-only row.

### P5. Lifecycle invariants - deep
- **Scope.** invariants I22-I27, I29 and I53, seam S16. Files as in A7 plus `harnessd/src/mergequeue.rs:416 session_is_live`, `harnessd/src/mergequeue/serve.rs:1436 settle_tree`, `tools/src/lifecycle/in_use.rs`.
- **Positions.**
  1. Each of I22, I23, I25 and I29 is still violated by a concrete path (cite them), including `retire_if_empty` and a cwd held by another user.
  2. Every gap named is either documented as intended (d.3) or unreachable in practice.
  3. Against both: a reaper that needs seven inputs to decide is wrong, and ownership should be one stamp per process tree.

### P6. Test isolation and secrets at rest - deep
- **Scope.** invariants I14, I17-I20, seams S12 and S13. Files `provider/src/keys.rs:194 config_file`, `harnessd/src/config/tests.rs:407`, `harnessd/src/mergequeue/gate.rs:474 apply`, `scripts/gate-at.sh`, `tokencore/src/apparatus.rs`, `harnessd/src/harness/open.rs:1369`.
- **Positions.**
  1. Tests touch the operator's providers.toml, keys, models dir and live socket. Check the queue gate's real HOME, gate-at.sh linking keys, agent tests inheriting `LETIBOT_SOCKET`, and the live-state tests named in invariants.md (`sudo_ask`, the models-dir config test, `restart_notice`).
  2. gate-at.sh isolates HOME and XDG, daemon-spawning tests get a private `XDG_RUNTIME_DIR` (f3fddd6b4), harnessd tests share no HOME or store (e6cafa02f), and `LETIBOT_CHECK_REAL_STORE` is enforced.
  3. Against both: without one test-env helper every crate does it by hand, and seven config resolvers guarantee a miss.

### P7. Launcher, multicall, config and systemd - light
- **Scope.** seams S12-S15, invariants I28 and I42. Files `scripts/{letibot, leticode, rrun}`, `letibot/src/main.rs`, `config-ref/`, `activation/`, `harnessd/src/late_scope.rs`, `tests/launcher.sh`.
- **Positions.**
  1. The launcher misroutes. Check `hold`, `release` and `where` becoming paid prompts, `LETIBOT_PROVIDERS` read by nothing in Rust, XDG ignored, the unbounded `systemd-run` after the probe, and `--restart` ordering (R312, R313).
  2. Launcher cases, `no_hang.rs`, dist-name checks, and a config-ref with forward and reverse probes.
  3. Against both: 3k lines of bash deciding placement before Rust runs belongs in Rust.

### P8. Modes and child narrowing - deep
- **Scope.** invariants I6 and I9. Files `tools/src/mode.rs:486-600`, `harnessd/src/harness.rs:743-880` (`set_mode_consented`, `within_parent`, `follow_parent`, `narrow_to_parent`), `harnessd/src/harness/open.rs:2875`, `harnessd/src/harness/children.rs`, `harnessd/src/harness.rs:826 mode_past_parent` (called from the `/mode` door in `harnessd/src/sessions.rs`).
- **Positions.**
  1. The ceiling can still be widened or go stale: a child reopened after a restart has no live parent cell, a narrowing reaches a child only at its next batch, and a running call keeps the wider point.
  2. Every widening is refused by name at all three `/mode` doors, and the R306 tests fail with the check removed.
  3. Against both: modes, grants, permission.json and perimeters are four overlapping systems.

### P9. Permissions and automode - deep
- **Scope.** seams S20 and S21, invariants I6-I9, I31, I49 (shadow grants revocable) and I51 (grants survive a restart). TODO R305, R314, R316 (plan), R317. Files:
  - oracle seam: `tools/src/authorise.rs:1010 AuthorisationOracle`, `:564 OracleScope`, `:585 narrowest`, `:1693 ModelAdjudicator`, `harnessd/src/oracle.rs:337` (`HttpOracle`)
  - the shadow's acting path: `sessionlog/src/pending_asks.rs:255 note_before`, `harnessd/src/answers.rs:432 view_read`, `harnessd/src/shadow/permission.rs:263 scope`, `:272 acting_rule`, `:476 rules_of`, `:170 BREAKER_AT`, `harnessd/src/shadow/guard.rs:138 read_passes`, `harnessd/src/queue_asks.rs`
  - the gap R316 names: `tools/src/adjudicate.rs:2678 ask_to_see`, which builds its request with `AuthorisationTrail::default()` (`:2787`)
  - grants: `tokencore/src/store/grants.rs`, `tools/src/grant_ledger.rs:236 GrantLedger`, `tools/src/adjudicate.rs:1630 with_grant_ledger`, `:1654 refresh_grants`, `harnessd/src/grants.rs:85 revoke`, `tools/src/view.rs`
  - reviewer limits: `tools/src/adjudicate.rs:1542 a_reviewers_probe`, `:1799 with_reviewer_limits`
  - points and the always-ask list: `tools/src/mode.rs:323` (`automode`), `:508 requires_given`, `tools/src/intent.rs:2409 ALWAYS_ASK`
  - docs: `docs/boundary-and-adjudication.md` sections 4d and 4e, `docs/guard-corpus-plan.md` sections 0-3 and 7b
- **Facts to start from.** `ShadowOracle` does not exist in code at this snapshot, and neither does a `SupervisedAdjudicator` type, although R316 names both. The shadow's only acting permission path is R317's relayed-read answer, which bypasses `AuthorisationOracle`. Its citation is a listed permission rule, not a trail index.
- **Positions.**
  1. Against: R317 already gives the shadow authority the oracle seam was built to withhold. Check that a review-queue seat's read is granted on a rule quote with no trail citation, a queue-scope grant covering every future reviewer until revoked, a breaker that resets at restart while its grants stay, a revoke that leaves running commands bound, siblings sharing a claim granted too, `a_reviewers_probe` admitting `Unknown` intents at allow-all, and whether anything on the always-ask list (`credential_use`, `changes_its_own_rules`, `persistence`) can reach `note_before`. Judge whether R316's stage plan can be measured honestly when the corpus contains the shadow's own grants.
  2. For: the design is sound and staged. Layer A settles tiers first, the model only sees the `may_approve` residue, `Blocked` and the always-ask list never reach it, the shadow only allows or asks, every grant is `shadow:<judge>`, kept, listed and revocable through one path, and promotion is per stage on replay evidence the operator decides on. Show it from the R317 tests and the R305/R314 tests.
  3. Against both: two authorisation models now coexist (rule-citation answers and trail-citation oracles), with grants in four places (permission.json, approved shapes, `session_grant`, `ViewGrants`) and a queue scope that is neither session nor project. Argue whether one oracle seam should exist before stage 1 widens at all, and what it would take to retire the `Relay` path first.

## 4. Order

1. Run the deep seam panels P1, P2, P4, P6 and P9 first, because their findings feed A2, A3, A4, A5 and A8.
2. Then the remaining panels, then the stale-facts panel.
3. Keep each panel's record verbatim in `docs/review/2026-11-<dd>-<panel>.md`. Every finding goes into TODO in full (memory `file-findings-in-full`).

## Added by the operator (2026-11-16): stale facts panel

**Scope.** Every number and state a reader is shown as current:
- the head's header, status row and resident lines (ctx, usage %, model, mode, nags, queue counts, waiting-on);
- every pane: todos, queue, jobs, subagents, sessions;
- the bridge's tools (`todos`, `leticode_status`, `leticode_asks`);
- the seat boards;
- the nag text;
- anything restored from the store at open (pauses, waits, modes, pins, limits, grants).

**Risk.** Deep. A stale fact is a wrong decision by the operator or a model.

**Method.** For each fact, trace:
- its source of truth;
- every copy of it;
- what refreshes each copy (an event, a poll, a redraw), and how often;
- what happens across a restart, a resync, a compaction and a reseat.

List every fact with no refresh path, a refresh that only happens on a threshold crossing, or a restored state that is not checked against now. Seed it with the known cases from 2026-11-15/16, and say for each whether the fix holds at `9e9312b06`:
- the header ctx after a compaction (R298);
- usage `5h 47%` while the truth was 60% (R310, built: 2 min poll, age shown; left: `usages` vs `limits[]`);
- the queue `1 stuck` with the pane hiding the row (R309, built: one predicate);
- todo rows waiting on ended jobs (R301, built);
- the model name showing qwen-next until the first turn;
- a nag pause restored after a restart (R311, built: a pause ends with its daemon);
- "waiting on: delivery" while the worker was spinning (R308);
- kept grants and the shadow's breaker: the grants survive a restart, the breaker does not (R314, R317);
- a child's mode ceiling after a restart (R306 left).

**Positions.**
1. Against: the head caches by design, and every cached fact without an age is a defect. Name each one.
2. For: most copies are refreshed by events that are reliable in practice. Show the ones that really go stale, with a reproduction, and do not count the theoretical ones.
3. Against both: the problem is architectural. The head should hold no derived state at all, only render the daemon's current view on each frame. Judge whether that is feasible, and what it costs.

**Proposed rule for the decision section** (now invariants.md I48):
- one source per fact;
- a copy carries the time it was read, and is drawn with its age once older than its period;
- restored state is checked against now at open, or dropped with a note.
