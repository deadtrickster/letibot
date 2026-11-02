<!-- abstract: Four things a remote command line does differently: a tilde expands only when unquoted and first, BSD `base64` takes no file argument, a unix socket path is capped by `SUN_LEN`, and a Mac's `pwd -P` resolves `/var` to `/private/var`. -->

# A remote command line, and the four things a Mac does differently

`~` expands only when it is unquoted and first, which is exactly the case quoting discipline removes. `cd "~/letibot-remote"` created a directory literally named `~` under the process's cwd, and `git status` then carried it. Resolve the tilde once, in Rust, so every later path is absolute. The same failure returns one layer along when a config value goes through a quoter: a `~/.cargo/bin` entry on the remote `PATH` became `'~/.cargo/bin'`, a directory named `~` and nothing on `PATH`. Decide which side expands a path, then quote everything except that.

BSD `base64` takes no file argument - `base64 FILE` is GNU's spelling and exits 2 on a Mac - so `base64 < FILE` works on both; `base64 -d` is fine on both.

A unix socket path is capped by `SUN_LEN` (104 here), and a `$TMPDIR` under `/var/folders` plus a session id gets there, so point the fixture's `XDG_RUNTIME_DIR` somewhere short. And `pwd -P` resolves `/var/folders/...` to `/private/var/...`, so a path a fixture built and a path the host answered compare unequal while naming one directory - canonicalize before writing the workspace into the store.
