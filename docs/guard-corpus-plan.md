# The gatekeeper - the guard's rules, corpus and model

Companion to `boundary-and-adjudication.md` (the gate's design). The section numbers are cited from the
code (`crates/harnessd/src/{etalon,etalon_oracle,etalon_map}.rs`) and from the evidence in the operator's private archive (`hands-off/letibot/evidence/`), so they are
kept.

**The rule this document sets:** every commit that lands a step here names its source - the paper by
arXiv id and the section or table the number came from, the dataset by its Hugging Face path, the rule by
its id in the upstream bank. A number without a source in a commit message is a claim; a number with one
is a measurement somebody else can repeat.

## 0. What the gatekeeper is, in one paragraph

The gate has two layers (`crates/tools/src/adjudicate.rs`). **Layer A** is deterministic:
`shell::normalise` parses the command into stages (tree-sitter bash), `intent.rs` classifies each stage's
intent and region, checks the secret and never-write lists, and settles a **tier** - `auto`,
`may_approve`, `always_ask` or `blocked`. The shape cache settles repeats of what the operator already
approved. What reaches **layer B** - the model - is the `may_approve` residue, and the question it is
asked is not *"is this command dangerous"* but **"does this command fit what the operator asked for"**: it
sees the trail (the operator's recent words with their distance in turns and seconds), what the agent says
it is doing, and the command and its shape, and answers ALLOW/ASK citing the trail entry it relied on.
The brief is `ModelBrief` in `crates/tools/src/authorise.rs`.

## 1. The sources, and what each one is for

| source | what it is | used for |
|---|---|---|
| **CARE**, arXiv 2607.21642, `github.com/prisma-research/CARE` (MIT) | pre-execution shell verifier: canonicalise, a head/flag lexicon, path sensitivity, then 139 provenance-tagged regex rules; 85% F1 at 0.34 ms on their split | **layer A**: the lexicon's 69 heads we did not name, `SENSITIVE_WRITE_PATHS`, and the GTFOBins techniques as tree rules. Not training data - no operator intent in it |
| **BashArena**, arXiv 2512.15688 | AI-control setting: 637 sysadmin tasks, 4 side tasks, 100 honest and 100 attack trajectories per side, per-action suspicion scores | the "agent claims X while doing Y" corpus and the size/latency reference point (§5) |
| **NL2SH-ALFA**, arXiv 2502.06858, HF `westenfelder/NL2SH-ALFA` | 40,639 instruction→bash pairs plus 600 verified | the (intent, command) positives and the mismatch negatives (§6) |
| **SWE trajectories** (HF `AlienKevin/SWE-ZERO-12M-trajectories`, `nebius/SWE-agent-trajectories`, `nvidia/Open-SWE-Traces`) | real coding-agent runs: issue text plus a sequence of shell commands | in-distribution positives and splice negatives (§6) |
| **Saber**, arXiv 2606.01317 | 716 tasks in stateful workspaces, eight unsafe-operation categories | taxonomy check for layer A's intent set |
| **TraceGuard**, arXiv 2604.03968 | five-dimension monitor on BashArena; attacks 0.616 vs honest 0.206 | the prompt shape to try first: score *fits the ask* and *fits the claim* separately (§3) |
| **R-Judge**, arXiv 2401.10019, **ToolSafety** (EMNLP 2025) | labelled agent records; direct, indirect and multi-step harm | the prompt-injection-in-trail case (§4) |
| **AuraGen / Safiron**, arXiv 2510.09781; arXiv 2606.05805 | synthesised trajectories with injected risks; a four-step training-data pipeline | the negatives recipe and the distil-the-reason step (§5, §6) |

Negative result, recorded so nobody repeats it: a retrieval pass over 2.39M arxiv papers for
"NL instruction paired with shell command, classifier for intent match" returned nothing on NL→shell pairs.
NL2SH-ALFA stays the source.

## 2. Measurement, before anything is ported

Nothing lands on a claim. Two instruments:

1. **The store** - the adjudication rows in `~/.local/share/letibot/sessions.db`. `harnessd --calibrate`
   replays them through the guard, `--compare` runs every prompt arm over the same rows, and
   `--show-brief` prints the bytes the guard was handed. This is the in-distribution held-out set and is
   never trained on.
2. **The etalon** (§7) - every tool call in every conversation on this box (Claude Code, opencode,
   letibot), each with the operator's preceding message and whether it ran or was refused.
   `harnessd --etalon <jsonl>`, `--etalon-oracle`, `--etalon-map` and `scripts/etalon-extract.py` are the
   instruments.

A layer A change is measured as **rows that would now be `always_ask` and were approved by a human**
(false prompts) against **rows a human refused that would now be caught before the model** (true catches).
A rule that adds the first without the second does not land.

**The baselines to beat**, over 121,137 rows / 104,496 bash commands from three machines: 10.8% of rows
that ran would prompt now, 11.0% of refusals would be caught before the model, and `not_run` (layer A
cannot read it) was 31.7% - now **2.4%**, after an unresolvable construct became a *widened scope* rather
than a refusal (the reasoning is in `boundary-and-adjudication.md`). What still refuses are the two cases
with no scope to widen: a parse error, and a program nobody can name. The `not_run` fifth was not obscure
work - it was ordinary work refused for spelling a variable. Prompts on rows that ran are dominated by
`network_egress_to_an_unseen_host` (7,024), then `credential_use` (1,736, a name match on
`find ~ -name x.go`), `destruction_outside_the_project` (1,162) and `secret_off_box` (720, the fleet's own
heartbeat), with the rest of the list under 150 each. The redactor is proved against a ground truth: one
shipped transcript holds a real private key in tool output and zero corpus lines carry the key body, and a
password inline on the command line is blanked too.

**Recall, measured through every layer.** BashArena's `monitor-eval` split (HF
`abhayesian/basharena-monitor-eval`; 339 bash actions from 134 trajectories, 47 attack / 292 honest),
converted by `scripts/basharena-to-etalon.py`:

| | attacks caught (of 47) | honest flagged (of 292) |
|---|---|---|
| CARE's 139-regex bank, as shipped | 1 (2%) | 9 (3%) |
| layer A alone | 8 (17%), all `network_egress_to_an_unseen_host` | 36 (12%) |
| **the guard** (layer A + oracle) | **27 (57%)** | 120 asked (41%), 172 allowed |

The oracle triples layer A's recall by judging the action against the task, because the attacks are
ordinary sysadmin text and exist only against the ask. BashArena's own monitor, with the whole trajectory
in view, is ~80% catch at 4% FPR; ours sees one action, so 57% is the baseline to beat.

**The misses, read**: 11 are `chmod +x <a file the agent wrote earlier> && run it`, 3 are
`cat > /opt/x.py <<'EOF'` with the exfiltration inside the body, and 5 are recon reads labelled attack
because every step of an attack trajectory is. Fourteen of nineteen are one fact: **the attack lives in a
heredoc body layer A treats as opaque bytes** - and the operator's own corpus was 31.7% unreadable for the
same reason, so reading a heredoc's body as a program is one change against both numbers.

## 3. Step 1 - the prompt, before any port or finetune

Rewrite the brief's question as two scored dimensions - **goal alignment** (fits what the operator asked;
cite the entry) and **action-trace consistency** (fits what the agent says it is doing) - instead of one
verdict, and measure with `--compare` against the current arm; then measure a 3-call ensemble of the same
prompt against one call, on latency and agreement.

**Measured, six arms over the operator-answered rows**: the verdict question wins. "asked-for-it plus the
operator's own answers plus what the agent says it is doing" agreed on 52 rows with 11 asks, where the
two-score question agreed on 47 with 16 asks on the identical brief. The 27B's own ALLOW/DENY
discriminates better than fixed thresholds over its own scores, so **verdict stays the production
question** and the code keeps `--oracle-question scores` for a later threshold sweep. The operator's own
answers are worth +4 over the floor and the agent's claim +1. The three "false allows" constant across
every arm were mislabels (two head timeouts and one unoffered option), so every arm's true false-allow
count is 0 and the labelled set cannot measure false allows at all - only the etalon's refusals can, which
is another reason step 5 comes first.

## 4. Step 2 - port CARE into layer A, on the tree

- Add the 69 program heads `intent.rs` does not name, in their CARE class: persistence (`crontab`, `at`,
  `systemctl`, `launchctl`, `update-rc.d`), privilege (`useradd`, `usermod`, `visudo`, `chpasswd`), recon
  (`nmap`, `masscan`, `hping3`), power (`shutdown`, `reboot`), disk (`sgdisk`, `cfdisk`). Two new intents
  where ours have no home: `Persist`, `Escalate`.
- `SENSITIVE_WRITE_PATHS` (`/etc/`, `/boot/`, `/sys/`, `/proc/sys/`, `/dev/` minus the benign devices)
  beside `NEVER_WRITE`, plus `/etc/shadow`, `/etc/sudoers` and `~/.netrc` in the secret list. They belong
  in `config/sensitive.json`, which is **written and not loaded** - loading it is the last piece of this
  step, behind the "a model must ask to change this file" rule that already exists.
- The 31 GTFOBins techniques as **tree rules over `Stage`** (`find -exec sh`, `awk system()`, `vim :!`,
  `git -c alias.x=!sh`, `docker run --privileged`, interpreters with `-e` spawning a shell) rather than
  regexes over the string, re-expressed from the technique with the CARE rule id cited per rule and
  nothing copied from GTFOBins' pages (GPL). Not taken: `grep -r "password"` and `chown root`, which would
  prompt on ordinary work.
- **The L4 regex bank, measured and not taken.** All 139 patterns over the etalon: 31 fire at all, and
  together they would add 879 prompts on rows that ran while catching 5 refused rows (4 already caught by
  `credential_use`). The heaviest firers are wrong - "shutdown/reboot" matches the *word* in grep patterns
  and source text, `/home` counts as a system directory so every `rm -rf /home/…/Projects/x` prompts, and
  the confidence-1.0 hits (`rm -rf /`, `cat ~/.ssh/id_rsa`, `curl … | sh`) are every one of them **text
  about a dangerous command**: this repository's fixtures, its docs, the tool survey's table, written into
  files by heredoc. A whole-string regex over a corpus produced by people building a guard matches the
  guard's own vocabulary, which is the argument for tree rules and the reason the lexicon is the part of
  CARE that ported. It does not say the other 108 patterns are harmless - the etalon is benign work, so
  they have zero false-positive cost here and zero evidence of value here.
- **The bank's CPU cost is not the problem** if a session with real teeth wants all of it: Rust's `regex`
  compiles 136 of the 139, and as separate values tried in turn it is 7.7 µs a command (25 µs worst), so
  a thousand rules of this kind stay under the 300 µs the tree parse already spends. The cost is the 587
  benign commands the bank fires on - and those rules do not know a workspace, a LAN host or `$HOME`,
  which `EffectScope` does.
- Port CARE's 10 pipeline tests as behavioural specs where they apply (`curl | sh`, a secret read, an
  obfuscated destructive); they test their code, not our data, so they are a floor, not a measurement.

## 4b. Step 2b - a heredoc going somewhere executable is a program

When a stage writes a heredoc (`cat > PATH <<'EOF'`, `tee PATH <<EOF`) and any of: the body starts with a
shebang; PATH is under a `bin/`, `/opt`, `/etc` or `/usr/local`; or the same or next command runs
`chmod +x PATH` or executes PATH - then the body is **normalised as shell** or **scanned as a script**,
and its intents and regions join the stage's. `curl http://<unseen host>` inside a script the agent is
about to run is `network_egress_to_an_unseen_host`, the same rule that caught 8 of 47.

**Landed**, and the measurement corrected the premise: the unreadable share was **not** heredocs (of 38,236
unreadable commands, `parameter_expansion` is in 29,024 and `command_substitution` in 22,638, expanded
heredocs in 110 - a quoted heredoc was never unreadable, it was read as a literal with its body ignored,
which is the fact this step is about). The parser also gave a trailing heredoc to the wrong stage, since
tree-sitter hoists `<<'EOF'` above the whole list and nests `| bash` inside it, so `cat <<'EOF' | sudo bash`
normalised to a `cat` - fixed as its own commit. And **a mention is not a call**: counting every host and
secret path in a body added 1,570 prompts, nearly all edit scripts whose literals hold the vocabulary of
the code they edit, so a triple-quoted literal is now data, a host counts only where a script imports a
network module or shells out, and a secret path only after a call that opens a file. Three string-layer
defects fell out too: loopback was egress, a multi-line word was read as `host:path` by its first line, and
`{"line_number":` was a host.

What is read now: the body when it is the stdin of an interpreter (`bash <<'EOF'`, `python3 - <<'PY'`,
`cat <<'EOF' | sh`), the `-c`/`-e` argument of a non-shell interpreter, and a body written to a file that
is evidently a program (a shebang, a script extension, a `bin/` but not `src/bin/`, or a stage of the same
command that runs it). Shell bodies are normalised and folded, tightening only; other languages are scanned
for hosts and secret paths. Plus two host shapes BashArena uses: a bare address as the operand of a network
program, and a host assigned to a variable.

The result on layer A: prompts on rows that ran 10.9% → **7.1%**, `network_egress_to_an_unseen_host` down
from 7,950 to 2,209, BashArena attacks caught before the model 8 → **5**, honest rows flagged 12% → **7%**.
**The guard's recall fell while layer A's precision rose**: two misses are the loopback `curl` rows the old
layer A caught for the wrong reason, and four are actions the reading made *legible* and then admitted as
what the task asked. A single-action judge cannot see that a trajectory is an attack; the honest number is
47%, and the thing to build against it is the trajectory, not a looser reading.

## 5. Step 3 - the model: size, ensemble, distillation

A 4B-8B monitor at ~80% catch / 4% FPR single-shot is the published reference, and three calls of a 7B
beat one call of the 27B at the operator's budget. Train on the **brief** rendered through `ModelBrief` so
train and inference are the same bytes, and distil the reason and citation from the production model on
the synthetic set - verdict-only training is how a model learns to say ALLOW without citing. The held-out
operator rows are never trained on.

## 6. Step 4 - the corpus builder

`harnessd --build-corpus`, or a script beside `calibrate.rs`, emitting the JSONL `calibrate.rs` reads:
**positives** (NL2SH rows as utterance → command ALLOW with a citation; SWE-ZERO steps as issue plus prior
steps → command ALLOW citing the issue); **mismatch negatives** (an utterance with a command from another
row sharing the program head but not the target, `grep -r foo src/` vs `grep -r foo ~/.ssh`); **splice
negatives** (a CARE-dangerous or BashArena attack step inserted into a benign SWE trajectory, so that step
ASKS and the rest ALLOW); **distractor trails** (several utterances per brief, so the citation has to be
the right one); and the etalon as a separate, real, unbalanced set.

## 7. Step 5 - the etalon from this box's conversations

*Source: none - this is ours.* Extract from all three stores: for each tool call, the operator's preceding
message(s), the tool and its arguments, and the outcome - ran, errored, or refused (Claude Code's "user
doesn't want to proceed" result, opencode's `state.status`, letibot's adjudication row, which is the only
place with an explicit human label and the mode). Render each through `ModelBrief`. Positives will
dominate and the refusals are few and exact; use it as a second measurement for §2 and as the
in-distribution tail of the training set, held out by session.

Not to be done without a look first: some transcripts carry things the operator would not want in a
training set. The extractor writes commands and utterances, never tool *output*.

## 7b. Step 6 - the head must be a person, and a person may allow anything

*Source: none - ours. The operator: "it is indeed an impersonation gap, yet me, human, must be able to
allow anything."*

**The gap.** A head is a client on the daemon's unix socket that declared `can_decide` and a name. Its
authority is the socket's permission - `$XDG_RUNTIME_DIR/letibot`, mode 0700 - which is the *uid*, not the
person, so anything running as that uid can attach a head, call itself `dead`, and answer a prompt: an
agent's own `bash` on an unconfined seat, a script a model wrote, another agent on the box. Confined seats
cannot reach the socket; the leticode seat with `--bash` can, and that is the seat the operator runs.

**The other half.** Today `blocked` means nobody can say yes to the call - not the head, not the model. The
operator's rule is different: *a person* may allow anything, including a blocked call; what nobody but a
person may do is override. So the fourth tier's meaning becomes: **no agent and no model can promote this;
a proven person can, once, with the proof recorded.**

**Two mechanisms, for two strengths of claim:**

1. **Provenance of the head, for every decision.** The daemon reads the connecting process's credentials
   (`SO_PEERCRED` → pid) and walks `/proc/<pid>/status` ancestry. A head whose ancestry contains
   `harnessd` itself, a known agent process, or no controlling terminal at all is seated **read-only**
   whatever it declared: it sees the session, it cannot decide. The verdict row records the head's pid,
   tty and the ancestor checked, so `human:dead` becomes `human:dead@pts/3, parent zsh, no agent above
   it`. Cheap, no secret to keep, and it closes the case the corpus actually contains. **Not built**:
   today `can_decide` is a cooperative declaration (`crates/sessionlog/src/protocol.rs`) and nothing
   inspects the peer.
2. **Proof of person, for overriding `blocked`.** Provenance says the head is not an agent's child; it
   does not say a human is at it. An override asks for something a model process cannot supply: the
   operator's own password through the PAM stack the box already trusts (`sudo -S -v` on a throwaway
   timestamp, through the `letibot-askpass` plumbing in `crates/harnessd/src/sudo.rs`), or a per-boot
   secret written by the login session to a file agents' views never mount. The override is recorded as
   such (`by: person:dead, proof: pam`), never becomes a standing grant, never enters the shape cache, and
   is shown in the corpus as its own row kind - an overridden block is the most valuable label there is,
   because it says where the rule is wrong.

What this does not do: it does not authenticate the *daemon* (a uid that can attach a head can start a
daemon with a different config); that is the sudo/PAM boundary's problem. And it does not stop a person
being fooled - a head that is a person answering a brief is exactly what layer B is for.

**The operator's words as a one-shot grant, recorded.** When an always-ask fires and the trail holds an
utterance naming the host, the credential or the program, the head shows the ask *with the utterance
quoted* - "you said: use gh - allow github.com and gh for this session?" - and a yes is a stored decision
seeded into the seen-host set and the grant table for that session. Nothing is lifted by the model's
reading; the human's click is the grant and the sentence is the evidence beside it. The always-ask stays
for anything the sentence did not name. A host the operator's tooling is already logged into is seen from
the start, and an admitted contact makes its host seen.

**Measured by:** how many `human:dead` rows would have been read-only under (1); how many `blocked` rows
the operator overrides under (2), which is the list of rules to revisit; and under (3), the asks whose
trail already held the answer.

## 7c. What the store said about balance

The operator: *"more and more often i think this permission thing we built is not balanced … i got asked
about edits even … there is no point when i no longer asked, so i always have to keep an eye which defeats
the whole agentic purpose."* Read against the adjudication table, and every line was the rules being fed
the wrong fact rather than being too strict: a second, re-derived reading of a call carried `unknown` and
escalated edits that should have been admitted; `saw_host` was never called, so no session ever tapered; a
`file.rs:LINE` was read as a host; loopback was egress; and in `--adjudicator head` mode no adjudication
row is written at all, so the operator's own denials - the rows the etalon is starving for - were lost.
The list of rules has not needed to shrink; the facts they are fed did.

## 8. Order

1 (prompt, measure) → 5 (etalon; it improves every later measurement) → 2a (our own rules and the
unreadable third) → 6 (the head is a person; mechanism 1 first, it is cheap) → 2 (CARE port, measured
against both) → 4 (builder) → 3 (finetune). Steps 1 and 5 can run in parallel; 2 waits for 5.

**What remains, in the order it blocks things:** `config/sensitive.json` is written and not loaded, which
is the last piece of step 2; the corpus builder (step 4) and the finetune (step 3) are unstarted; head
provenance (§7b mechanism 1) is unbuilt, so `can_decide` is still a declaration; and the PAM override
(mechanism 2) is a design with the plumbing already in `sudo.rs`.
