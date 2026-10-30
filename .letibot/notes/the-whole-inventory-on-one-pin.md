<!-- abstract: MEASURED 2026-10-30: 25 branches with unlanded work, 19 rebased onto the pinned a8df34f5 and verified, 6 already landed or superseded; the table of tips, the four semantic conflicts (a rano pin with no commit carrying both fixes, main deleting the SETTLE model this branch adds, an empty cwd read as a gone tree, and the reviewers' Stop/CouldNotStart merge), and the three rakes under the verification itself. -->

# Twenty-five branches on one pin: nineteen rebased and verified, six already there

MEASURED 2026-10-30, worktree `agent-rebase-everything-unlaid`, `main` pinned at `a8df34f5`
throughout and never written to. **The work is nineteen branch refs**, moved with
`git update-ref refs/heads/<b> <new> <old>`; every rebase was done in a throwaway worktree under
`/tmp`, now gone. Nothing was pushed and no daemon was touched.

`main` moved while this ran — `a8df34f5` → `0d71e1e0` — and the two commits it gained are one of
the six rows below (`the-queue-replays-the-old-main` landed). The pin is kept so the table is
reproducible.

The operation was `git rebase --onto a8df34f5 <the branch's fork>` (see
`the-queues-rebase-replays-the-old-main.md`). Every one of the twenty-five forked from a commit
that is **still on the current main's first-parent spine**, so the plain form would have been
equivalent for all of them — checked, not assumed: the fork is the newest commit in
`git rev-list --first-parent $(git rev-list -g refs/heads/main)` that is also in
`git rev-list --first-parent <branch>`.

## The nineteen, with what verified each

| branch | old tip | **rebased tip** | conflicts | how resolved | verification |
|---|---|---|---|---|---|
| `agent/the-room-runs-the-coordinator` | `9a0e7426` | **`8d9665d4883bba584e76cabf95cd8aaea71c47ef`** | `cli.rs` ×2: both sides appended at one anchor | both kept; main's new 4-arg `spawn_for(.., reviews)` | `cargo test -p letibot-room --lib` → **9 passed; 0 failed** |
| `agent/answer-the-room-refusal` (onto ①) | `25a520c0` | **`b0ce9dc2b6043039ec74b21fb97e400d1f33319b`** | none | — | **9 passed; 0 failed** |
| `agent/queue-pane-as-a-plugin` | `28bd41c4` | **`7aa4ba932f0dde84f02ac53a11014a980c117927`** | `app/tests/queue.rs`: main renamed a test, the branch changed its fixture | main's name, the branch's `builtin()`; **plus a follow-up commit** for three tests main added after the cut | `cargo test -p letibot-tui --lib` → **1094 passed; 0 failed; 6 ignored** |
| `agent/answer-the-queue-plugin-refusal` (onto ③) | `78663df2` | **`4c264775c5e32bdd7417adbf45a9b1ab52dc1a00`** | none | — | **1102 passed; 0 failed; 6 ignored** |
| `agent/plugin-tool-mount` | `0f2d9550` | **`20c64ead166eebe36c9ab35d43c31d2556530060`** | none | — | `cargo test -p letibot-plugins --lib` → **46 passed; 0 failed** |
| `agent/fix-tool-mount-landing` (onto ⑤) | `c967a07c` | **`bca00d8b60274f6039946e9a61e447d80300b7e8`** | `builtin_layers.rs` ×2, `sessions_override.rs` ×2 | main's table-driven assertion where it already had one; the branch's list + its new layer where it did not | **46 passed; 0 failed** |
| `agent/answer-the-tool-mount-rejection` (onto ⑥) | `5f35ace5` | **`45f35e9076de6f08294291c46558f3a9d7e9b231`** | none | — | **47 passed; 0 failed** |
| `bridge-no-queue` | `180ada6a` | **`3c7f1bf9c9cbbf09c609e6883f527f05e3e462cb`** | none | — | notes only (one `.md`); nothing to build |
| `agent/reviewers-fan-out-to-the-hosts` | `942bac42` | **`fdbdff0ca0a681d6ee27c5ccd6ea4485dd061228`** | 7 files, 17 hunks — both sides rewrote `start_reviewer` | one `Stop` enum of three arms; main's tree proof + the branch's pool; **a behaviour fix** (below) | `cargo test -p letibot-harnessd --lib` → **828 passed; 0 failed; 1 ignored** |
| `agent/grep-does-not-read-a-file-at-a-time` | `c90657bd` | **`4c9516ba985b492ba17ac17f4a6261eebc235132`** | `firecode.rs`: both sides appended tests at one anchor | both kept | `cargo test -p letibot-tools --lib` → **1236 passed; 0 failed** |
| `agent/an-irc-session-list-pane` | `4d025f63` | **`fb2b64fea7db450a1b26d015e1a6ba218c61d1c1`** | none | — | `cargo test -p letibot-tui --lib` → **1080 passed; 0 failed; 5 ignored** |
| `agent/a-shell-escape-runs-on-bash-3-2` | `54c324eb` | **`adf3daa794ba1af7a747520fa94f685e479b83ee`** | none | — | `cargo test -p letibot-tools --lib` → **1236 passed; 0 failed** |
| `agent/markdown-fences-render` | `511bb062` | **`dcfdd7fe68cc16eeebf5b25ec902b55ff71873d6`** | 5 Cargo files: the rano pin | **main's `b2bd7d22`** — see the semantic list | `cargo test -p letibot-ui --lib` → **47 passed; 0 failed** |
| `agent/every-queue-event-reaches-a-session` | `5ee28ef2` | **`87f0256aa79d66f6ce1628fa68923b327ff32877`** | `serve.rs`: two restructures of one landing block | main's `proved` + the branch's `live`; **plus a follow-up commit** for `Pool::live` and the notice's address | `cargo test -p letibot-harnessd --lib` → **823 passed; 0 failed; 1 ignored** |
| `agent/a-one-line-citation-is-filled` | `0200fe9a` | **`c14a6bef74f535ba05273806bba936c960f5ce4c`** | none | — | `cargo test -p letibot-transcript --lib` → **33 passed; 0 failed** |
| `agent/a-background-job-gets-a-file-too` | `5621c250` | **`c9cb700abd824be64514994469f47124e476a3cb`** | none | — | `cargo test -p letibot-tools --lib` → **1236 passed; 0 failed** |
| `agent/a-refusal-reaches-a-live-session` | `e93fdb80` | **`e2c7b0cb0966076277aef6c14977c7304e3114de`** | none | — | `cargo test -p letibot-tui --lib` → **1078 passed; 0 failed; 5 ignored** |
| `agent/the-launcher-raises-the-file-limit` | `6f66ca31` | **`e1b574484ea444bf226d4600e144193e3b2d3f08`** | none | — | shellcheck: the same 1+2 findings main has, none new |
| `agent/one-store-per-session-and-nobody-closes-it` | `802aa0c9` | **`28bc751e68f7352f9d9d67e94a85cd1cd84d0131`** | none | — | `cargo test -p letibot-tokencore --lib` → **138 passed; 0 failed** |

The four stacks were rebased parent-first and each child `--onto <the parent's rebased tip>
<the parent's old tip>`; `git merge-base --is-ancestor` holds for all four
(`the-room-runs-the-coordinator`→`answer-the-room-refusal`,
`queue-pane-as-a-plugin`→`answer-the-queue-plugin-refusal`,
`plugin-tool-mount`→`fix-tool-mount-landing`→`answer-the-tool-mount-rejection`).

`sh scripts/check-fmt.sh a8df34f5` in each of the nineteen worktrees: all formatted. Two needed a
`rustfmt: the files this change touches` commit (`reviewers`, `grep`) — both were hunks this
session wrote.

## The six that are NOT to be landed

**`git cherry`'s `+` is not a measurement of "unlanded".** For four of these the branch's content
is on `main` under a rewritten sha, and the patch-id differs because the landed version was
rebased over a base this branch never had.

| branch | why not |
|---|---|
| `mac-seatbelt` | landed: `5442b7c8` (the commit) and `6a4322dd` (the note). `crates/tools/src/exec/seatbelt.rs`, `tests/seatbelt.rs` and `tests/seatbelt_no_helper.rs` are on main byte-for-byte; main's file is 18 lines LONGER (the D16 test from the branch below). |
| `boundary-admits` | landed: `fb89728f`, `407a94ff`, `0456cdb6`, `401de44b`. Every file it touches is on main; `crates/harnessd/src/leticode_config.rs` reads as "1412 lines missing" only because that module MOVED to `crates/leticode-config` (`comm` against the moved file: 0 absent). |
| `llama-home` | **superseded.** `crates/llama/build-support.rs` on main is the same fix (a `$HOME` candidate, `build-glm/bin` then `build/bin`, the three library names) shared by all five build scripts, which is the shape `the-gate-could-not-find-llama-cpp.md` argues for. The branch's five per-crate copies are the thing that file replaced. |
| `plugin-test-order` | landed. Rebasing its one commit onto `a8df34f5` produced an EMPTY commit — main's `plugins.rs` already creates `a-below` before `z-above` in all three tests. |
| `bridge-settle` | **superseded, and deliberately.** Main's `head.rs` says it in its own words (lines 1688-1697 and 2036-2046): the `in_flight` set, the `round_done` flag and the ten-second `SETTLE` window are what this branch ADDS, and main DELETED them for the daemon's `Activity`. Rebasing it would put back a model main removed on purpose; the test that names the replacement is `the_daemons_activity_says_what_the_rounds_events_cannot`. |
| `agent/the-queue-replays-the-old-main` | landed **while this ran**: `597d6dfd` on main, and the queue retired the ref and removed its worktree. Its first commit was `8d37d5da`; main's is the same patch with a different base. Nothing to rebase, and the ref no longer exists to move. |

## The semantic conflicts — for the operator, not decided here

1. **`agent/markdown-fences-render`: two rano pins, and no commit carries both.**
   Main pins `b2bd7d22` ("keep the super bit, so ⌘C is a chord"); the branch pins `32ed5246` ("a
   fence glued to a word's punctuation is a fence too"). Measured in
   `~/.cargo/git/db/rano-*`: `32ed524`'s parents are `0d3e682` → `bf3fa48`, and `b2bd7d2`'s parent
   is `bf3fa48` too — **siblings, neither an ancestor of the other**, and no ref contains both.
   The rebase took main's pin (dropping it would break `Mods::SUPER`, which main's own tests use),
   so **this branch does not actually get its rano fix** until rano has one commit with both.
   That is a rano-side rebase, not a letibot decision.

2. **`agent/reviewers-fan-out-to-the-hosts`: an empty `cwd` is not a tree that went away.**
   Main's `review::verdict` refuses a verdict about a tree that is gone (the rule the 2026-10-29
   night turned on). A reviewer placed on a declared host works in a checkout on THAT machine, so
   its `ReviewRequest.cwd` is `""` by construction — and `Path::new("")` is not a directory, so the
   rule refused every verdict a host-placed reviewer ever handed in and parked the row on
   `waiting` for a tree that can never appear here. Measured on the branch's own
   `a_verdict_frees_the_place_the_reviewer_held`: `left: Waiting, right: Approved`.
   I treated an empty path as *nothing here to check* (`bee4b1c0`). **This is a change to main's
   rule** — the alternative is to record the child's real cwd on the request, which the remote
   placement does not have.

3. **`agent/every-queue-event-reaches-a-session`: `Pool::live` and the notice's address.** The
   branch adds `live` to `Pool`; main added `served` (a fixture) and made the landing's
   liveness an `Option<String>`. Both are the branch's own subject arriving twice, so the merge is
   mechanical — but `raise_to_the_branch` now addresses `from.as_deref().unwrap_or("")`, i.e.
   **nobody when the row's child has ended**, which is a decision about who hears a kept tree.

4. **`agent/queue-pane-as-a-plugin`: three tests main added read the LAYER's pane.** They were
   written against `app()` when `app()` drew `ui/panes/queue.rs`; on this branch `app()` draws the
   `queue` layer. `builtin()` (`/queue builtin`) is the file's own fixture for the head's pane, so
   the change is a fixture, not a behaviour — but it is a judgement about which pane those three
   assertions are about.

## Three rakes under the verification itself

* **`cargo clean` takes no `--offline`.** The first cut of the script passed one, the clean was a
  silent no-op, and every worktree then compiled ANOTHER tree's rlibs: `letibot-plugin` carried
  `Act::Verb` from the queue-pane tree into a tree whose `tui` had no arm for it, and the error
  named a file the branch never opened. `clean` also has to run **in the worktree** — it is about
  the manifest it is run under, so a `cd`-less loop cleans nothing this tree will use.
* **A shared `CARGO_TARGET_DIR` across worktrees is not survivable even with the touched packages
  cleaned**, because the stale package is often one the branch does NOT touch (a dependency of the
  crate that fails). Every FIRST-PARTY package has to be cleaned; the third-party deps are what
  stay warm. With that, one worktree is ~50 s.
* **`cargo test … | grep` reports grep's status.** The first script wrote `check rc=0` for a check
  that had failed, because the pipeline's status was `tail`'s.
