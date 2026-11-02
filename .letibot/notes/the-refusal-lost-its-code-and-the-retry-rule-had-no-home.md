<!-- abstract: `openai.rs` handed the retry classifier `error.message` and dropped `error.code`, so `insufficient_quota` never reached the money check; the retry rule is now `letibot_http::retry`. -->

# A refusal lost its code one layer above the classifier, so the 429 fix was inert

`crates/provider/src/openai.rs`'s non-2xx arm parsed the body, took `error.message` and returned
`BackendError::Refused { status, body: message }`, so the CODE was gone. `insufficient_quota` is
one of the phrases `refused_for_want_of_credit` looks for, and it could never fire on the path a
cloud session takes. The fix appends the code to the body - and not `type`, which says what KIND
of refusal it is and nothing about whether the account can pay.

Rule: a phrase list matched against a projection of a body is a list nobody can keep true. When
a classifier "was fixed twice and is still not firing", ask what the caller handed it.

The rule now lives in `crates/http/src/retry.rs`: `Transport` (no answer reached), `TryAgain`
(`408`, `429`, `500`, `502`, `503`, `504` and nothing else), `Refused` (everything else,
including a `429` whose body names money), one ladder of 5 retries at 1/2/4/8/16 s - 31 s, the
measured llama.cpp cold reload.

`HarnessError::Endpoint { said, cause }` is how a turn's ending carries the count: `Display` is
`said`, so `TurnFailed.error` carries it, and `http_cause()` is the one door for "which 4xx
refused".
