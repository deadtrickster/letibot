<!-- abstract: MEASURED 2026-10-31: a five-item brief generated from TODO.md's OPEN entries was four-fifths stale — every one of the four had a fix in main, one dated BEFORE its own entry — and `git log -S '<the symbol the entry names>'` is the one call that settles it. -->

# A TODO entry marked OPEN can have been fixed for weeks

MEASURED 2026-10-31, branch `agent/the-five-small-diagnosed-ones`. The brief was five
TODO.md entries, each with a diagnosis to act on. **Four described a tree that no longer
existed**, and one of those four was filed four days *after* its own fix:

| entry (its first phrase) | fixed by | proves it |
|---|---|---|
| "A stop unlinks the socket without the daemon going" | `f354b051` (2026-10-11) | `sessionlog/tests/one_daemon_one_folder.rs` (7 tests, incl. the entry's own *still open?* check) |
| "A fold that SUCCEEDS is thrown away by the store" | `f0d9c9a1` (2026-10-04) | `harnessd/tests/compact.rs`'s `a_compaction_fork_is_a_store_row_and_a_resume_lands_on_it` |
| "A pane's first resize is a real SIGWINCH" | `401e61fe` (2026-10-10) | `a_same_size_resize_is_not_a_nudge`, alone and 48/48 under sixteen copies of its binary |
| "R78 — Up no longer recalls prompt history" | `08bbf9a7` (2026-10-07, entry filed 10-11) | `up_recalls_the_sessions_earlier_prompts_after_a_restart`, `harnessd/tests/prompt_history_resume.rs` |

Only the fifth ("A 429 that names money") had anything left: `f53a349c` had closed the
classifier, but the phrase list in `refused_for_want_of_credit` did not carry the entry's
own wording — *out of credit* — nor *credit balance is too low*, so a 429 in either shape
was still taken six times. Both are in the list now.

## The one call that settles an entry

```sh
git log --oneline -S '<a symbol only the fix would introduce>' -- <file the entry names>
```

`-S` on the *symbol* (not the prose) found all four in one call each: `FolderClaim`,
`put_fork_unsealed`, `born at the pane's rectangle`, `refresh_prompt_history`. `git log
--grep=R78` does NOT — the fix commit does not carry the entry's number, because the entry
was written afterwards by a session that had not read the log.

**Two rakes under the check itself:**

* **`git log -S` on a 25k-line file is killed by the tool's 64 MiB default.** Give it
  `memory: "2G"`, or it dies with `Killed` and reads as "no such commit".
* **A test that passes is not evidence the entry was fixed** unless it is the entry's own
  check. `a_same_size_resize_is_not_a_nudge` passed alone on the *racy* tree too — that is
  what the entry says. The evidence is the same binary under the load that used to find it
  (sixteen copies, three rounds — `the-load-a-racy-test-needs-is-sixteen-copies-of-itself`).

## What this costs, and what to do about it

A brief built by grepping TODO.md for OPEN hands a session work that is already done, and a
session that trusts the entry will "fix" a fixed thing or, worse, report the tree as broken.
When an entry is closed, close it in place — heading marker plus the commit, the mechanism
and the test — which is what `35442999` does for these five.
