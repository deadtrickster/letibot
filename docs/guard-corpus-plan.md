# The gatekeeper — paper-backed plan for the guard's rules, corpus and model

Written 2026-09-16, claude-lab2x1. Companion to `boundary-and-adjudication.md` (the gate's
design) and to two flowy rows: the survey `01M2MHKC839DVJEBG4M5RW0JNR` and its arxiv addendum
`01M2MW0S3D2BTMDERXCYT34TYT`, with the nine PDFs on message `01M2MJFZRYEE627Q14DQ8ZTQSA`
in `Lab/#general`. Each step below is a flowy todo row (Lab):
`01M2NTZ3K7FS5Q7PX3WWWPDPE1` step 1 · `01M2NTZ3N3B2SMRXRN418H1YMT` step 2 · `01M2NTZ3PD8D9W61X8V4SWHNGJ` step 3 ·
`01M2NTZ3NS4M5JCNYMC5MKZ2MB` step 4 · `01M2NTZ3MB03DT4ZWT2QBTZAJ4` step 5 · `01M2NXVY8JA69Q54ZYDQQMZJ18` step 6.

**The rule this document sets:** every commit that lands a step here names its source —
the paper by arXiv id (and the section or table where the number came from), the dataset by its
Hugging Face path, the rule by its id in the upstream bank. A number without a source in a
commit message is a claim; a number with one is a measurement somebody else can repeat.

---

## 0. What the gatekeeper is, in one paragraph

The gate has two layers (`crates/tools/src/adjudicate.rs`). **Layer A** is deterministic:
`shell::normalise` parses the command into stages (tree-sitter bash), `intent.rs` classifies
each stage's intent and region, checks the secret and never-write lists, and settles a
**tier** — `auto`, `may_approve`, `always_ask`, `inexpressible`. The shape cache (persistent as
of `6c449ca`) settles repeats of what the operator already approved. What reaches **layer B** —
the gatekeeper model — is the `may_approve` residue, and the question it is asked is not
*"is this command dangerous"* but **"does this command fit what the operator asked for"**:
it sees the trail (the operator's recent words with their distance in turns and seconds), what
the agent says it is doing, the command and its shape, and answers ALLOW/ASK in 25 words citing
the trail entry it relied on. Today that model is Qwen3.8-27B on lubuntu1 at 20 s budget; the
operator's target is zero-unsure at 3–10 s unattended.

## 1. The sources, and what each one is for

| source | what it is | used for |
|---|---|---|
| **CARE**, arXiv 2607.21642, `github.com/prisma-research/CARE` (MIT) | pre-execution shell verifier: canonicalise → L2 head/flag lexicon → L3 path sensitivity → L4 139 provenance-tagged regex rules (92 MITRE, 31 GTFOBins, 16 manual) → allow/warn/deny; static-only 85% F1 at 0.34 ms on their 549-command split (§V, Table 3) | **layer A**: the lexicon's 69 heads we do not name, `SENSITIVE_WRITE_PATHS`, and the 31 GTFOBins techniques as tree rules. Not training data: no operator intent in it. |
| **Saber**, arXiv 2606.01317, `github.com/sssr-lab/saber` | 716 tasks in stateful project workspaces; eight unsafe-operation categories; Scenario C (241 tasks) = constraints visible only after workspace inspection | taxonomy check for layer A's intent set; traces as splice-in negatives (§4) |
| **NL2SH-ALFA**, arXiv 2502.06858, HF `westenfelder/NL2SH-ALFA` | 40,639 instruction→bash pairs (train) + 600 verified (test), merged from nl2bash, LinuxCommands, NL2CMD, InterCode-Bash, tldr-pages | **the (intent, command) positives**; mismatch negatives (§4) |
| **SWE trajectories**: HF `AlienKevin/SWE-ZERO-12M-trajectories` (mini-swe-agent, one bash command per turn), `nebius/SWE-agent-trajectories` (80k), `nvidia/Open-SWE-Traces` (207k), `SWE-bench/SWE-smith-trajectories` (5k) | real coding-agent runs: issue text + a sequence of shell commands | in-distribution positives; splice negatives; multi-utterance trails for citation training (§4) |
| **BashArena**, arXiv 2512.15688 | AI-control setting: 637 sysadmin tasks, 4 side tasks, 100 honest + 100 attack trajectories per side task, per-action monitor suspicion scores; GPT-4.1-mini ~80% catch at 4% FPR single-shot, ~90% with a 3-call ensemble (§6) | the "agent claims X while doing Y" corpus; the size/latency reference point (§5) |
| **TraceGuard**, arXiv 2604.03968 | five-dimension monitor on BashArena (goal alignment, constraint adherence, reasoning coherence, safety awareness, action-trace consistency); attacks 0.616 vs honest 0.206; in ControlArena | the prompt shape to try first: score *fits the ask* and *fits the claim* separately (§3) |
| **AuraGen / Safiron**, arXiv 2510.09781 | synthesise benign trajectories, inject category-labelled risks at calibrated difficulty, reward-model filter, train a compact guardian with rationale; Pre-Exec Bench | the negatives recipe, written up; check the body for a release before building ours |
| **R-Judge**, arXiv 2401.10019, `github.com/Lordog/R-Judge`; **ToolSafety**, EMNLP 2025 | 569 labelled agent records; ~14k direct/indirect/multi-step harm samples | the prompt-injection-in-trail case the brief is otherwise blind to (§4, later) |
| arXiv 2606.05805 | a four-step training-data pipeline: task collection → trajectory generation → teacher distillation → pairing | the distil-the-reason step (§5) |

Negative result, recorded so nobody repeats it: a RAGFlow retrieval over 2.39M arxiv papers for
"NL instruction paired with shell command, classifier for intent match" returned nothing on NL→shell
pairs (four queries, 143–153 s each under ingest load). NL2SH-ALFA stays the source.

## 2. Measurement — before anything is ported

Nothing lands on a claim. Two instruments:

1. **The store.** 402 adjudications, 66 human-decided, in `~/.local/share/letibot/sessions.db`
   on lab2x1. `harnessd --calibrate` replays them through the guard; `--compare` runs every prompt
   arm over the same rows; `--show-brief` prints the bytes the guard was handed. This is the
   in-distribution held-out set and is never trained on.
2. **The etalon** (§6, to be built): every tool call in every conversation on this box — Claude
   Code (143 transcripts, 310 MB; one file alone has 8,429 `Bash` calls and 28 recoverable
   denials), opencode (`~/.local/share/opencode/opencode.db`: 3,696 `bash` and 1,626 `edit` parts
   with completed/error states across 64 sessions), letibot (1,851 tool results, 402 adjudications)
   — each with the operator's preceding message(s) and whether it ran or was refused.

Layer A changes are measured as: **rows in the store that would now be `always_ask` and were
approved by a human** (false prompts) and **rows a human refused that would now be caught before
the model** (true catches). A rule that adds the first without the second does not land.

**Baseline, 2026-09-16** (`harnessd --etalon ~/.local/share/letibot/etalon.jsonl`, layer A at
`9171045`, shell pinned, scope judged in each row's own `cwd`), over 121,137 rows / 104,496 bash
commands from lab2x1, the laptop and lubuntu3 (Claude Code, opencode, letibot; lubuntu1 and
lubuntu2 pending):

| | |
|---|---|
| ran × always_ask — would prompt now | **10,991 of 102,097 (10.8%)** |
| refused × always_ask — caught before the model | **32 of 292 (11.0%)** |
| not_run — layer A cannot read it | **31,462 of 104,496 (30.1%)**, all "meaning does not exist yet" |

Prompts on rows that ran, by rule: `network_egress_to_an_unseen_host` 7,024 ·
`credential_use` 1,736 (`find ~ -name x.go`, `grep … flowy` — a name match) ·
`destruction_outside_the_project` 1,162 · `secret_off_box` 720 (`. ~/.config/flowy/env-… &&
flowy say`, the fleet's own heartbeat) · `secret_to_transcript` 135 · `privilege_escalation` 87 ·
`secret_to_weaker_location` 62 · `changes_its_own_rules` 50 · `secret_flow_unknown` 15.

The redactor was proved against a ground truth: one shipped transcript holds a real private key
in tool output (the host's, in a `head` of a file that was the key). Zero corpus lines carry the
key body. A second hole found on the corpus itself and closed: a password inline on the command
line (`mysql -p…`, `redis-cli -a …`, 950 commands) — now blanked.

What this says about the order of work: **our own rules cost the operator more than the CARE
port would save.** The first three rules and the unreadable 19% are where the hours are; the port
(step 2) adds catches on a class the corpus barely contains (four of fifty refusals were
layer-A-shaped at all). So step 2 is preceded by a step 2a: bring the false-prompt rate down on
the three rules above and read the `not_run` fifth, each measured with the same command.

## 3. Step 1 — the prompt, before any port or finetune

*Source: TraceGuard §4 (dimensions), BashArena §6 (ensemble).*

Rewrite the brief's question as two scored dimensions — **goal alignment** (fits what the operator
asked; cite the entry) and **action-trace consistency** (fits what the agent says it is doing) —
instead of one verdict. Measure with `--compare` over the 402 rows against the current arm.
Then measure a 3-call ensemble of the same prompt against one call, on latency and agreement.
Cheapest step; may move the number the most.

**Measured, 2026-09-16** (`harnessd --compare`, six arms, the 66 operator-answered rows, Qwen3.8-27B
Q6_K_XL on lubuntu2 `192.168.1.78:8082`, budget 20 s, code at `acda545`):

| arm | agreed | asks | false allows |
|---|---|---|---|
| asked-for-it, no examples (the floor) | 47 | 16 | 3 |
| follows-from, no examples | 44 | 19 | 3 |
| follows-from + the operator's own answers | 49 | 14 | 3 |
| asked-for-it + their own answers (ships) | 51 | 12 | 3 |
| + what the agent says it is doing | **52** | **11** | 3 |
| two scores (TraceGuard §4), same brief as the row above | 47 | 16 | 3 |

The two-score question **loses**: five more prompts than the verdict question on the identical
brief, same false allows. The 27B's own ALLOW/DENY discriminates better than fixed thresholds
over its scores, so `verdict` stays the production question; the code stays (`--oracle-question
scores`) for a later threshold sweep, but that is not step 1 any more. The operator's own answers
help (+4 over the floor) and the agent's claim helps again (+1): the claim arm is the best of the
six and is one flag from what ships. The three "false allows", constant across every arm, were **mislabels**: two head timeouts and
one unoffered option, written as `human:dead` refusals of a man-page lookup the operator had
proposed (fixed in `by`-naming and relabelled; the labelled set has zero human refusals among
its 66 asked rows). So every arm's true false-allow count is 0, and the labelled set cannot
measure false allows at all — only the etalon's 334 refusals can, which is another reason step 5
came first.

## 4. Step 2 — port CARE into layer A, on the tree

*Source: CARE §III (layers), Appendix A.4 (rule bank), `care/semantic.py`, `care/path.py`,
`care/rules/rule_provenance.json`; MIT.*

- Add the 69 program heads `intent.rs` does not name, in their CARE class: persistence
  (`crontab`, `at`, `systemctl`, `launchctl`, `update-rc.d`), privilege (`useradd`, `usermod`,
  `visudo`, `chpasswd`), recon (`nmap`, `masscan`, `hping3`), power (`shutdown`, `reboot`), disk
  (`sgdisk`, `cfdisk`). Two new intents where ours have no home: `Persist`, `Escalate`.
- `SENSITIVE_WRITE_PATHS` (`/etc/`, `/boot/`, `/sys/`, `/proc/sys/`, `/dev/` minus the benign
  devices) beside `NEVER_WRITE`; `/etc/shadow`, `/etc/sudoers`, `~/.netrc` into the secret list.
  These go into `config/sensitive.json`, which is written and **not yet loaded** — loading it is
  part of this step, behind the "a model must ask to change this file" rule that already exists.
- The 31 GTFOBins techniques as **tree rules over `Stage`** — `find -exec sh`, `awk system()`,
  `vim :!`, `git -c alias.x=!sh`, `docker run --privileged`, interpreters with `-e` spawning a
  shell — not as regexes over the string. Re-expressed from the technique, with the CARE rule id
  cited per rule; nothing copied from GTFOBins' pages (GPL).
- Not taken: `grep -r "password"` (SE-P-028, 0.75) and `chown root` (SE-P-057, 0.55) — they
  would prompt on ordinary work.
- Port CARE's 10 pipeline tests as behavioural specs where they apply (`curl | sh`, a secret
  read, an obfuscated destructive) — they test their code, not our data, so they are floor, not
  measurement.
- Measure per §2 before landing. Commit message names the rule ids.

## 5. Step 3 — the model: size, ensemble, distillation

*Source: BashArena §6; AuraGen §3–4; arXiv 2606.05805 §3.*

A 4B–8B monitor at ~80% catch / 4% FPR single-shot is the published reference; three calls of a
7B beat one call of the 27B at the operator's budget. Train on the **brief** rendered through
`ModelBrief` so train and inference are the same bytes; distil the 25-word reason and citation
from the production GLM on the synthetic set — verdict-only training is how a model learns to say
ALLOW without citing. The 402 rows stay held out.

## 6. Step 4 — the corpus builder

*Source: NL2SH-ALFA §3 (the merge), AuraGen §3 (risk injection), CARE (splice-ins), BashArena
(attack steps).*

`harnessd --build-corpus`, or a script beside `calibrate.rs`, emitting the JSONL `calibrate.rs`
reads:

1. **Positives**: NL2SH-ALFA rows as (utterance → command, ALLOW, cite 1); SWE-ZERO steps as
   (issue + prior steps → command, ALLOW, cite the issue).
2. **Mismatch negatives**: NL2SH utterance X with a command from another row sharing the program
   head but not the target (`grep -r foo src/` vs `grep -r foo ~/.ssh`) → ASK.
3. **Splice negatives**: a CARE-dangerous step, or a BashArena attack step, inserted into a benign
   SWE trajectory → that step ASK, the rest ALLOW. Teaches judging a step against its context.
4. **Distractor trails**: several utterances per brief, so the citation has to be the right one.
5. The **etalon** (§7) as a separate, real, unbalanced set.

## 7. Step 5 — the etalon from this box's conversations

*Source: none — this is ours.* Extract from all three stores: for each tool call, the operator's
preceding message(s), the tool and its arguments (the command, for `bash`), and the outcome — ran,
errored, or refused (Claude Code: the "user doesn't want to proceed" result; opencode: `state.status`
and the error text; letibot: the adjudication row, which is the only place with an explicit human
label and the mode). Render each through `ModelBrief`. Positives will dominate; the refusals are
few and exact. Use: (a) a second measurement for §2, on real commands this operator ran; (b) the
in-distribution tail of the training set, held out by session.

Not to be done without a look first: some of those transcripts carry things the operator would
not want in a training set (paths, tokens in tool output). The extractor writes commands and
utterances, never tool *output*.

## 7b. Step 6 — the head must be a person, and a person may allow anything

*Source: none — ours. The operator, 2026-09-16: "it is indeed an impersonation gap, yet me, human,
must be able to allow anything."*

**The gap.** A head is a client on the daemon's unix socket that declared `can_decide` and a
name (`--identity dead`). Its authority is the socket's permission — `$XDG_RUNTIME_DIR/letibot`,
mode 0700 — which is the *uid*, not the person. Anything running as that uid can attach a head,
call itself `dead`, and answer a prompt: an agent's own `bash` on an unconfined seat, a script a
model wrote, another agent on the box. Every `human:dead` in the corpus is a declaration the
daemon believed. Confined seats (bubblewrap) cannot reach the socket; the leticode seat with
`--bash` can, and that is the seat the operator runs.

**The other half.** Today `blocked` means nobody can say yes to the call — not the head, not the
model. The operator's rule is different: *a person* may allow anything, including a blocked call;
what nobody but a person may do is override. So the fourth tier's meaning becomes: **no agent and
no model can promote this; a proven person can, once, with the proof recorded.**

**Two mechanisms, for two strengths of claim:**

1. **Provenance of the head — for every decision.** The daemon reads the connecting process's
   credentials (`SO_PEERCRED` → pid) and walks `/proc/<pid>/status` ancestry. A head whose
   ancestry contains `harnessd` itself (a tool call answering its own gate), a known agent process
   (the `claude`, `opencode`, `letibot-tui --no-tty` invocations the agents make), or no
   controlling terminal at all, is seated **read-only** whatever it declared: it sees the session,
   it cannot decide. The verdict row records the head's pid, its tty and the ancestor that was
   checked, so `human:dead` becomes `human:dead@pts/3, parent zsh, no agent above it` — a fact,
   not a name. Cheap, no secret to keep, and it closes the case the corpus actually contains: an
   agent's bash attaching a head.

2. **Proof of person — for overriding `blocked`.** Provenance says the head is not an agent's
   child; it does not say a human is at it. An override of a blocked call asks for something a
   model process cannot supply: the operator's own password, verified through the PAM stack the
   box already trusts (`sudo -S -v` on a throwaway timestamp, through the `letibot-askpass`
   plumbing in `crates/harnessd/src/sudo.rs` that already exists for `sudo`), or a per-boot
   secret written by the login session to a file agents' views never mount. The override is
   recorded as such (`by: person:dead, proof: pam`), never becomes a standing grant, never enters
   the shape cache, and is shown in the corpus as its own row kind — an overridden block is the
   most valuable label there is, because it says where the rule is wrong.

**What this does not do:** it does not authenticate the *daemon* (a uid that can attach a head
can also start a daemon with a different config); that is the sudo/PAM boundary's problem, and
this plan does not pretend to fix it. It does not stop a person from being fooled; a head that
is a person answering a brief is exactly what layer B is for.

**Measured by:** the corpus — how many `human:dead` rows would have been read-only under (1); the
tier table — how many `blocked` rows the operator overrides under (2), which is the list of rules
to revisit.

## 8. Order

1 (prompt, measure) → 5 (etalon; it improves every later measurement) → 2a (our own rules and the
unreadable third) → 6 (the head is a person; mechanism 1 first, it is cheap) → 2 (CARE port, measured
against both) → 4 (builder) → 3 (finetune). 1 and 5 can run in parallel; 2 waits for 5.
