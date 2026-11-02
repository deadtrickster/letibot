<!-- abstract: Four shell/parse traps behind a tab-separated ssh channel, each of which reads as "the remote backend is broken": a tilde in a `${v#...}` pattern, `{ cmd; }` letting `exit` end the launcher, `trim()` eating a leading tab, and a constant ending in a bare command line. -->

# Four ways a tab-separated shell channel lied — MEASURED 2026-10-26, `crates/tools/src/remote.rs` (branch `remote-placement`)

`${r#~/}` tilde-expands its PATTERN, so the strip never happens and the tree lands at
`$HOME/~/letibot-remote`. Use `${r:2}`, and require the result to start with `/`.

`{ cmd; }` runs in the LAUNCHER's shell, so a command's `exit` ends the launcher and `echo $? > exit`
never runs - every reader then sees "ended with no exit status". `( cmd )` is a child shell.

`line.trim()` eats a leading tab, and the refresh probe's first field is EMPTY while a job runs
(`"\t7564\t25\n"`), so the tuple match falls through and a live job reports pid 0.
`trim_end_matches(['\n','\r'])` is the fix (`remote.rs:4022`). Never `trim()` a record whose first field
may be empty.

A constant ending in a bare command line turns an appended argument into a new command:
`format!("{} {}", WALK, path)` on `"walk() { … }\nwalk\n"` ran `walk` with no argument and the recursion
covered the filesystem from `/`. Keep it a DEFINITION, have the caller pass the argument, and guard with
`[ -n "$1" ] && [ -d "$1" ] || return 0`.
