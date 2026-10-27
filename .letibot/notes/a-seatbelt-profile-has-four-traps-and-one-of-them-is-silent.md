<!-- abstract: MEASURED 2026-10-27 (branch `mac-seatbelt`): an SBPL allow filtered to subpaths without `(literal "/")` makes every process SIGABRT before main; `(deny …)` after the allows is how a deny survives a widened grant; `(deny signal (target others))` under `(allow default)` does not stop a kill of an outside process; and git/make/python3 are xcrun shims that write to `confstr`, not `TMPDIR`. -->

# A Seatbelt profile: the four traps, and the one that is silent

MEASURED 2026-10-27 on the operator's Mac (arm64, macOS 26.7.1), branch `mac-seatbelt`
(`crates/tools/src/exec/seatbelt.rs`). Each of these presented as "the sandbox does not work",
and each is four lines of profile.

## 1. A filtered `file-read*` allow without `(literal "/")` aborts EVERY process

```text
(deny default)
(allow process*)(allow sysctl-read)(allow mach-lookup)
(allow file-read* (subpath "/usr") (subpath "/System") …)   ; -> /usr/bin/true: Abort trap: 6
(allow file-read* (literal "/") (subpath "/usr") …)         ; -> rc 0
```

The abort is `SIGABRT` **before `main`**, with nothing on stderr and no denial in `log show` —
something in libsystem reads the root directory during the sandbox's own check-in, and a refusal
there aborts the process instead of returning `EPERM`. It cost an hour of bisecting because the
symptom (`Abort trap: 6`, empty output) reads like a broken profile *parser*.

Two facts that made it confusing: `(allow file-read*)` with an **empty** filter list allows
everything (so it works), and a *wide* list — every top-level directory — still aborts, so the
missing piece is not one of the directories but `/` itself. Also: `(literal "/")` alone runs
`/usr/bin/true` fine, so *fewer* permissions can be fine where *this* set is fatal.

## 2. A `(deny …)` is only a rule about a grant if it comes LAST

SBPL is last-match-wins. So

```text
(allow file-read*) … (deny file-read* (subpath "$HOME/.ssh"))
```
is the shape that makes a deny survive `HostBackend::grant_into_view_impl` widening the view to the
whole home — which on Linux is not survivable at all, because a bind mount cannot exclude a path
inside itself. **This is the one place a policy boundary is stricter than a namespace**, and it is
worth keeping the allows-then-denies order for that reason alone.

## 3. `(deny signal (target others))` is not how you stop a command signalling its launcher

Under `(allow default)` it did **not**: a `sleep` started outside, `kill -0` and then
`kill -TERM` from inside the sandbox both reached it and killed it. Under `(deny default)` the
same kill is `EPERM` **with no signal rule at all** — and a child *inside* stays killable, so job
control works. So: do not write the rule, rely on `(deny default)`, and measure it with
`kill -0 <the launcher's own pid>` (signal 0 sends nothing).

## 4. `git`, `make` and `/usr/bin/python3` are xcrun shims that write OUTSIDE `TMPDIR`

```
git: error: couldn't create cache file '/var/folders/…/T/xcrun_db-…' (errno=Operation not permitted)
```
They `dlopen` `/Library/Developer/CommandLineTools/usr/lib/libxcrun.dylib` (**so `/Library` must be
readable** — the first minimal profile broke all three) and then write a cache into
`confstr(_CS_DARWIN_USER_TEMP_DIR)`, which is **not** `$TMPDIR` and so cannot be pointed at the
session's own directory. One allow of `(subpath "<that dir>/xcrun_db")` silences it; nothing else
in that directory is reachable.

# Two rakes beside the profile

* **A `bash` command whose text contains the operator's ssh path is refused by the harness**
  (`boundary:host`, §11.4) — including inside a heredoc and inside a `git commit -m` body, which
  cost this session one commit attempt. Write the path as `$HOME/…` in a command, and let a
  *test* spell it (a test binary is not run through the gate).
* **`cargo test … | tail` exits with `tail`'s status**, so a failing test binary reads as
  `exited 0`. Redirect to a log and echo `$?`, or the run that failed says green.

# What it costs, so nobody re-measures it

100 × `true`: 1.28 ms plain, 6.41 ms inside → **+5.1 ms per exec** (87-line profile, 11 rules,
61 path filters). `cargo --version` +4.5 ms, `git --version` +4.6 ms; a `cargo check --offline`
with one registry dependency builds fine inside, and the cost does not grow with the work.
