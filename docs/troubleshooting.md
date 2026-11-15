# Troubleshooting

Each entry is the symptom, the check, and the fix. The check names the `letibot doctor` row that
catches it, where one does. `letibot doctor` exits 0 when every row is ok, 1 on warnings and 2 on a
FAIL, and prints the fix under each row that is not ok. The one-time host setup behind most of these
is in [install.md](install.md), "On Linux".

## Two bridges under one name

**Symptom.** A DM or mention for `cc@<folder>` never wakes the session, and the roster shows the
seat twice, as `cc@letibot` and `cc@letibot-8e0`. The bridge that held the base name pushed the
message, its cursor moved, and Claude Code dropped the push (R259).

**Check.** Doctor rows `no user-scope leticode MCP server`, `one bridge per Claude session` and
`claude channel flag`. By hand: `claude mcp get leticode`.

**Fix.** `claude mcp remove leticode -s user`. Start Claude Code with
`--dangerously-load-development-channels plugin:leticode@leticode`, not `server:leticode`, and
restart each running session. [claude-code-bridge.md](claude-code-bridge.md) has why.

## The user manager wedged by a coredump loop

**Symptom.** A daemon starts with "memory caps OFF", the launcher says "no systemd user manager
here", or `systemd-run --user --scope` hangs. A gate test killed for file size dumped core,
drkonqi's coredump launcher crashed on the dump and looped, and the user manager collected about
128k failed units (R191, R192).

**Check.** Doctor rows `drkonqi coredump socket masked`, `user manager failed units` and `daemons in
their own scope`. Doctor does not time `systemd-run --user --scope` itself; it lists that in its
"not checked" footer.

**Fix.** `systemctl --user mask --now drkonqi-coredump-launcher.socket`, then log out of every
session or reboot to clear the units. Restart the daemons through the launcher so each gets its
scope.

## Bridges spinning on old binaries

**Symptom.** `letibot-mcp` processes take whole cores and the load climbs. They run a binary that a
rebuild replaced (`/proc/<pid>/exe` ends in `(deleted)`), or one run straight from
`target/release`, from before the push-thread backoff landed.

**Check.** Doctor rows `bridges run an installed binary` and `no bridge spinning` (above 50% CPU over
a 1 s sample). By hand: `ps -o pid,pcpu,etime,args -p "$(pgrep -d, -x letibot-mcp)" --sort=-pcpu`.

**Fix.** Install the current build (install.md, "Installing from a checkout") and restart the Claude
session that owns the bridge. Never point Claude Code at `target/release/letibot-mcp`: the next
build replaces it under the live session.

## No room waiter

**Symptom.** Messages to a seat are not delivered: a DM or mention sits in the inbox and the
session never wakes. Without the channel flag, or with a lost push, nothing else wakes it.

**Check.** Doctor row `room waiter per Claude folder`, which names the folders that have a Claude
session and no `letibot-mcp room-wait`.

**Fix.** Install the `leticode` plugin, whose monitor runs the waiter, or arm it by hand as a
persistent Monitor:
`while :; do letibot-mcp room-wait --attention to_me --timeout 1h; sleep 2; done`
([room.md](room.md), § *Claude Code*).

## Usage-limit park

**Symptom.** A session on a provider with a usage limit stops with "Responding", or a turn fails
with a 429 and a reset time. The turn is parked until the reset.

**Check.** No doctor row: `[fallback] covers every model in use` is in its "not checked" footer. The
status row names the reset. Read `on_limit`, `wait_if_within` and `[fallback]` in
`~/.config/letibot/leticode.toml` ([config/leticode.toml.md](config/leticode.toml.md)).

**Fix.** Add a `[fallback]` entry for the model, for example
`"kimi-code-global/*" = ["deepseek/deepseek-v4-flash"]`, and keep `on_limit = "switch"` (the
default when a fallback exists). A reset within `wait_if_within` (45m) is waited for. To move a
parked turn now, press Esc to end it, or switch the model with `/models`, which retries it there
(R265).

## Stale prompt card on shadow sessions

Fixed. **Symptom.** A "PROMPT IS STALE" card on an `s-...-shadow` session, or on the gatekeeper and
reviewers, that nobody answers.

**Check.** Doctor row `daemons run the installed binary` lists daemons on a build older than the
fix.

**Fix.** Install a build with the fix (shadows get no card, daemon-run sessions reseat before a
turn, `[sessions] auto_reseat`, R262) and restart those daemons.

## Reaper kills

**Symptom.** A worktree daemon, its attached seat or a running job is ended by the idle reaper, with
nothing reopening the session. A tool call running past 30 minutes lost its session (R241).

**Check.** No doctor row: `reap_event kills of live daemons` is in its "not checked" footer. Read
the journal with `letibot gc --log`, and what holds each owner with `letibot holds`.

**Fix.** Install a build with R241 and R266: only the owner's live daemon judges idleness, an
attached head or an open job renews the lease, and a stopped session reopens by id through
`leticode_attach` or `leticode_send`. Read `letibot gc` (the plan) before `letibot gc --now`.

## The launcher overwritten by an ELF

**Symptom.** `leticode --continue` answers `letibot: no such role: --continue`. A build product was
installed as `letibot` over the launcher script.

**Check.** Doctor row `letibot is the launcher script` (FAIL on an ELF). By hand:
`head -c2 "$(command -v letibot)"` prints `#!`.

**Fix.** `ln -sfn ~/Projects/letibot/letibot/scripts/letibot ~/.local/bin/letibot`, then `hash -r`.
The multicall is `letibot-multi`; never install anything as `letibot`.
