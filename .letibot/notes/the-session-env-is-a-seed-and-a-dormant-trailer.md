<!-- abstract: There is no persistent shell on the `bash` path - each call is a fresh `/bin/sh -c` - so `ShellSession`'s trailer is dormant and the environment a session holds is the seed put on every spawn. -->

# The session's environment is a seed and a dormant trailer — and four rakes under it

`exec/host.rs::spawn` is `env_clear()`, then the pinned `PATH`, the daemon's `standing_env`,
`Config::build_env` and the request's pairs; a confined call is `--clearenv` + `KEEP_ENV`.
`ShellSession` has no production caller (`exec/shell.rs`'s own tests only), so an `export` never
reaches the next call, and a resolver that spliced a previous call's value would be making a
false claim.

`stage.program_name()` strips the directory on purpose, so writing a resolved file into a
stage's word changes the shape and not the grant key; `name_stage`'s `identity` is the separate
field.

`printf` with more arguments than placeholders REPEATS the format, so a `%s` dropped in an edit
writes the line twice and nothing checks the count.

A value over the cap must be skipped, not refused, or one name takes the session's whole
environment - `PATH` included - with it, and every resolution stops.

An interactive bash EATS a tab sent to it, because readline reads it as completion: a tab cannot
carry a value through that channel.
