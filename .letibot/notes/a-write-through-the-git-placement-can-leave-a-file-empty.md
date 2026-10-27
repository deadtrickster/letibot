<!-- abstract: MEASURED 2026-10-27 in wt-fb on lab2x1: a file being edited through the placement went to 0 bytes twice after the write channel failed; the CAUSE (found by R99) is the chunked write's discarded exit statuses plus an unconditional `mv` over a shared temp name — fixed in R99. -->

# A placed session's `edit` can truncate the file it was told not to touch

MEASURED 2026-10-27, wt-fb (branch `fanout-settles`), a session whose tools run on lab2x1 over the
git placement.

`crates/tools/src/builtins/fanout.rs` went to **0 bytes twice** in one afternoon. Both times the
preceding tool result said the file was fine:

* once, an `edit` that reported `io: writing … lost the connection before it answered
  (mm_send_fd: sendmsg(1): Message too long)` together with *"Nothing on disk changed: the write is a
  temporary file renamed over the target, so a failure leaves the original untouched"* — and a later
  `sed` showed the file still intact, so that one really was untouched;
* the second time the report was `[edit: found no match … Nothing was written]`, and the next `wc -c`
  said `0`.

So the claim above is not reliable across the placement: **a failed write can leave the target
truncated**, and the failure that leaves it is not the one that says so.

## What to do about it

1. **`wc -c` (or `wc -l`) after any write to a file you care about**, and after a *failed* one
   especially. A zero-byte file is silent: the next `grep` answers "no match", which reads like a
   wrong pattern.
2. Recovery is one line, and it is safe because the branch's commit is the base of truth:
   `git checkout -- <path>`, then redo the edits. Nothing else in the worktree is affected.
3. **Keep each write small.** The two failures came at payloads of ~20 KB and ~6 KB, so size is not
   the whole story — the channel was also dropping unrelated calls (`the process did not start …
   .pid was not written within 2 s`) in the same minutes. A big edit is still the one that costs the
   most to redo, and splitting one large insertion into a *new file* (`write`) plus small edits to the
   existing one survived every retry.

## And the shape this pushed me into, which is better anyway

The background fanout's machinery went into `crates/tools/src/builtins/fanout_bg.rs` — one file,
written once — instead of 500 lines inserted into `fanout.rs`. That is a module boundary the code
wanted: `fanout.rs` is the tool, `fanout_bg.rs` is a second *lifetime* over the same plan.

## The cause, found by R99 (2026-10-27) — and what the code does now

`Ssh::write_abs` sent a file as `ceil(len / 48 KiB)` **separate** ssh calls, each splicing a base64
slice into an argv, and then:

* **every chunk's exit status was discarded** (`sh_once` returned it; the caller dropped it), so a
  `printf … | base64 -d > tmp` that failed on the far side was a chunk that "succeeded";
* **the `mv -f` that put the temp file in place was unconditional**, so whatever the temp held —
  nothing, or a prefix — went over the target;
* **the temp name was one per path per PROCESS** (`<path>.letibot-<pid>`), so an attempt whose
  local ssh was killed at its deadline could still `mv` a file a later attempt was writing.

That is exactly the reported pair: a transport hiccup (`mm_send_fd: sendmsg(1): Message too long`,
and the mux's session refusals under R99's load) during a 200 KiB write **returned no error at all**
and left the target at **0 bytes**, and the `edit` that read it afterwards answered *found no match
… Nothing was written* — truthfully, about a file that no longer held anything.

Measured, on the pre-R99 code, with a fake ssh whose far side cannot write:

    write -> None (no error), target is now 0 byte(s), template has 204800

**Fixed in R99** (`crates/tools/src/remote.rs`, branch `ssh-sessions`): a write is now **one** ssh
session with the bytes on its stdin; the far side counts them and hashes them (`sha256sum` /
`shasum` / `openssl dgst`) and runs the `mv` only when both match; the temp name is unique per
attempt and a trap removes it on any exit; and the bound scales with the payload. A failed or
partial write therefore leaves the target byte-identical, and the tool says so.

So the advice below still costs nothing, but it is no longer load-bearing: `wc -c` after a write is
now a check on the *far side's* answer, not a workaround for a write that could not fail honestly.
MD
