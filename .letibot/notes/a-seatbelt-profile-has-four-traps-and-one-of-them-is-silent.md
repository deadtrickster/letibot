<!-- abstract: Four traps in a macOS Seatbelt profile: a filtered `file-read*` allow with no `(literal "/")` aborts every process before `main`, a deny only holds if it comes last, a signal rule does not stop a kill, and the xcrun shims write outside `TMPDIR`. -->

# A Seatbelt profile: the four traps, and the one that is silent

A filtered `file-read*` allow without `(literal "/")` aborts every process. Under `(deny default)` with the subpaths of `/usr` and `/System` alone, `/usr/bin/true` dies with `Abort trap: 6` - something in libsystem reads the root directory during the sandbox's own check-in. `(allow file-read*)` with an EMPTY filter allows everything, and a wide list of every top-level directory still aborts, so the missing piece is `/` itself.

SBPL is last-match-wins, so a `(deny …)` survives a grant only if it comes last: `(allow file-read*) … (deny file-read* (subpath "$HOME/.ssh"))` is what holds when `grant_into_view_impl` widens the view to the whole home.

`(deny signal (target others))` does not stop a command signalling its launcher under `(allow default)`. Under `(deny default)` the same kill is EPERM with no signal rule at all. Do not write the rule; measure it with `kill -0 <the launcher's pid>`.

`git`, `make` and `/usr/bin/python3` are xcrun shims that `dlopen` under `/Library` and write a cache into `confstr(_CS_DARWIN_USER_TEMP_DIR)`, which is not `$TMPDIR`. One allow of `(subpath "<that dir>/xcrun_db")` silences it.
