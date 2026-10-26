<!-- abstract: MEASURED 2026-10-26 building SshBackend (docs/remote-hosts.md RH1): four small shell/parse traps each of which looks like a broken backend — `${v#~/}` tilde-expands its pattern, `{ cmd; }` lets `exit` kill the launcher before it writes the status, `line.trim()` eats the tab that makes an empty first field, and a constant ending in a bare `walk` line turns an appended path into a filesystem walk from `/`. -->

# Four ways a tab-separated shell channel lied — MEASURED 2026-10-26, `crates/tools/src/remote.rs` (branch `remote-placement`)

Each of these presented as "the ssh backend is broken", and each is four lines of shell or one
`trim`. Written down because the next person to build a remote/stub channel will meet all four.

## `${r#~/}` tilde-expands its PATTERN, so the strip never happens

```sh
r='~/letibot-remote'
case "$r" in '~/'*) r="$HOME/${r#~/}" ;; esac   # -> $HOME/~/letibot-remote
```
bash tilde-expands a `~` at the start of the *pattern* of `${v#…}`. `#~/` then matches
`/private/var/…/home/` (the expanded home), not the literal `~/`, and strips nothing. The
symptom is a tree at `$HOME/~/letibot-remote/…`, which the placement then treats as its root.
`${r:2}` is the fix, and the resolved root is also required to start with `/` so a quiet
relative answer is refused rather than used.

## `{ cmd; }` runs in the LAUNCHER's shell, so `exit` ends the launcher

A job launcher that writes `pid`, runs the command, then writes `exit` looks right with a brace
group — until the command is `bash -c 'exit 7'` or a plain `exit 1`. `exit` in `{ …; }` exits the
launcher, `echo $? > exit` never runs, and every reader sees "ended with no exit status".
`( cmd ) > out 2>&1` is a child shell: its `exit` is the status. Measured: pid file present,
output present, exit file missing, and the job settled as `Killed` by a false sentence.

## `line.trim()` eats the tab, so an empty first field collapses the record

The refresh probe prints `<exit>\t<pid-alive>\t<bytes>`; `exit` is EMPTY while the job runs:
`"\t7564\t25\n"`. `.trim()` strips the leading tab, `.split('\t')` then yields two fields, the
three-tuple match falls through, and `refresh` returns early — a live `sleep 300` reported pid 0
fifty times while its pid file said otherwise. `trim_end_matches(['\n','\r'])` is the fix. The
general rule: **never `trim()` a record whose first field may be empty.**

## A constant ending in a bare command line turns an appended argument into a new command

```rust
const WALK: &str = "walk() { … }\nwalk\n";      // ends with the CALL
format!("{} {}", WALK, path)                     // -> "walk\n /path"
```
`walk` ran with no argument and the recursion covered the whole filesystem from `/`: two test
runs sat in it for minutes with 100 % CPU and no output, and one of them was a `cargo test`
that had already been marked as "hanging". Make such a constant the DEFINITION only, have the
caller supply the argument, and put a `[ -n "$1" ] && [ -d "$1" ] || return 0` guard in the
function itself.

## And two that are not shell

* `portable base64`: BSD's `base64` has no `-w0`, GNU's wraps at 76 — `base64 … | tr -d '\n'`
  is the spelling that works on both, and `base64 -d` reads it back on both.
* `pwd -P` resolves `/var` to `/private/var` on this Mac, and `std::fs::canonicalize` does NOT
  (firmlink, not a symlink). A test that builds the expected path itself must take the path
  from the thing under test instead.
