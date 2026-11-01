<!-- abstract: MEASURED 2026-11-01: the 429-naming-money fix was inert on the cloud path because openai.rs hands the classifier `error.message` and not `error.code` — six calls through a stub; and the retry rule now lives in `letibot_http::retry` with the count in `HarnessError::Endpoint`. -->

# A refusal lost its code one layer above the classifier, so the 429 fix was inert

**MEASURED 2026-11-01**, branch `agent/what-is-retryable` (`main` at `baa2780e`), building the
tree's one answer to *what is retryable, and for how long*.

## 1. The finding: `openai.rs` projected a refusal down to its message

`crates/provider/src/openai.rs`'s non-2xx arm parsed the body, took `error.message`, and returned
`BackendError::Refused { status, body: message }` — so the **code** was dropped. MEASURED through a
loopback stub (`crates/harnessd/tests/retry_policy.rs`, scenario D), first run:

```
{"error":{"code":"insufficient_quota","message":"You exceeded your current quota…"}}  → 6 calls
```

`insufficient_quota` is one of the phrases in `refused_for_want_of_credit`, which two branches had
already extended for exactly this case — and it could never fire on the path a cloud session takes,
because the word that names the case was gone. So `TODO.md`'s 2026-10-04 entry ("THE BILLING HALF IS
FIXED") was true only for a body that spells the money words in its **message** (GLM's `余额不足`),
not for one that spells them in its **code** (OpenAI's `insufficient_quota`). The fix: append the
code — `You exceeded your current quota… (insufficient_quota)` — and do **not** append `type`, which
says what KIND of refusal it is and answers nothing about whether the account can pay.

**Rake**: a phrase list matched against a *projection* of a body is a list nobody can keep true.
When a classifier "was fixed twice and is still not firing", ask what the caller actually handed it.

## 2. The rule now lives in `crates/http/src/retry.rs` (`letibot_http::retry`)

Three classes — `Transport` (no answer reached: DNS/refused/reset/timeout/half an answer),
`TryAgain` (`408`, `429`, `500`, `502`, `503`, `504`, and nothing else), `Refused` (everything else,
including a `429` whose body names money) — one ladder (5 retries, 1/2/4/8/16 s, six calls, 31 s),
and `Ladder::ending` for the sentence. `harnessd`'s `http_retry_after`, `MAX_HTTP_RETRIES` (now a
re-export of `STANDARD_RETRIES`) and `refused_for_want_of_credit` are thin or gone.

**The count was carrying the classification.** The old ladder was 6 retries / 63 s because a rule
that could not tell a permanent refusal from a transient one had to be patient; 31 s is the measured
29–31 s llama.cpp cold reload, which is the longest legitimate silence the ladder exists for.

## 3. Two seams worth knowing

* **`HarnessError::Endpoint { said, cause }`** is how a turn's ending carries the count. `Display`
  is `said` (so `publish_failure` → `TurnFailed.error` → `FAILED — …` carries it), and `cause` is
  the typed `HttpError` because `switch_back_notice` still asks *which 4xx refused* — a question
  that answered only `Turn(TurnFailure::Http(..))` would silently stop advising a switch back.
  `HarnessError::http_cause()` is the one door for that.
* **The summary turn walks the same ladder, inside `run_compaction`** — the instruction is appended
  once before its salvage loop, so a caller that looped would append one per attempt.
  `crates/turn/tests/support/canned.rs`'s `Canned::serve_replies` takes a `Reply::Status` and then
  frames, which is how a compaction test provokes a `503` without a daemon.
