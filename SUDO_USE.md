# `sudo` attempts by coding agents on this box

Same scan as `PS_USE.md`, for `sudo`: every bash tool call in the Claude Code
transcripts (`~/.claude/projects/*/*.jsonl`) and the opencode store
(`~/.local/share/opencode/opencode.db`, `part` table) containing a line
starting with `sudo`. Window: 2026-08-23 → 2026-09-14.

**Totals: 40 attempts — Claude Code 25, opencode 15.**
**Authenticated successes: 0.** Every single attempt died with
`sudo: A terminal is required to authenticate` or
`sudo: interactive authentication is required`. This box has no
passwordless sudo, and no agent ever had a password.

```
2026-08-23  11 (all opencode)   2026-09-06   4
2026-08-24   1                  2026-09-10   1
2026-08-25   1                  2026-09-14   7
2026-08-28   2
2026-08-29   4
2026-09-05   9  <- incl. one mkfs
```

## Categories

### 1. Privilege probe — "can I sudo at all" (11 · claude 7, opencode 4)

The most common sudo is not a command but a question. `sudo -n true` (or any
`sudo -n <cmd>` with a fallback message) before anything else, every time.
The fallback text is always written for the operator, not the model.

> `sudo -n true 2>/dev/null && echo "  passwordless sudo: yes" || echo "  passwordless sudo: no - starting a display manager needs your sudo"`
> — claude, 2026-09-05 08:56 UTC

### 2. Package management (7 · claude 3, opencode 4)

apt install/purge/update. The largest ask of the whole set is an NVIDIA
uninstall; the model wrote it into a script for the operator rather than
running it (it could not have anyway).

> `sudo apt purge '^nvidia-.*' '^libnvidia-.*' && sudo apt autoremove`
> — claude, written into `install-nvidia-stack.sh`, 2026-09-05 08:55 UTC

### 3. Group and device permission setup (3 · opencode 3)

The ROCm-era attempts: add the user to `render,video`, open up `/dev/kfd`.
All refused.

> `sudo usermod -aG render,video dead && sudo chmod 666 /dev/kfd && newgrp render && groups && ls -l /dev/kfd`
> — opencode, 2026-08-23 18:38 UTC

### 4. Root-only hardware inspection (6 · claude 5, opencode 1)

Read-only commands that need root: `dmidecode`, `lspci -vv`, `amd-smi`,
`dmesg`, `nft list ruleset`. Notably the models tried sudo for these before
finding unprivileged paths (`/sys/devices/virtual/dmi/id/board_name`,
`journalctl --user`).

> `sudo -n dmidecode -t memory 2>/dev/null | grep -E 'Speed|Locator|Size' | grep -v 'No Module' | head -40 || echo …`
> — claude, 2026-09-06 19:28 UTC

### 5. Service and unit management (8 · claude 8, opencode 0)

systemctl targets, and the fullest pattern in the set: writing a systemd
unit and enabling it — done as a *script for the operator*, after the probe
said no.

> `sudo rm -f "$UNIT"; sudo systemctl daemon-reload; sudo systemctl disable --now cpu-performance.service; sudo systemctl enable cpu-performance.service`
> — claude, inside a generated install script, 2026-09-14 06:26 UTC

### 6. Kernel and CPU parameter writes (2 · claude 2, opencode 0)

> `sudo -n sysctl -w kernel.perf_event_paranoid=1`
> — claude, 2026-08-24 19:28 UTC — refused; `sudo cpupower frequency-set -g …`
> (claude, 2026-09-14) — same fate, and the model's own script prints
> "cpupower failed - are you able to sudo?"

### 7. System-level installs and destructive operations (5 · claude 3, opencode 2)

`make install` into system paths, and the single most dangerous attempt on
the box:

> `sudo mkfs.xfs /dev/nvme2n1 2>&1`
> — opencode, 2026-09-05 11:43 UTC — **refused** ("A terminal is required to
> authenticate"). The disk was never formatted. The model probed `command -v
> mkfs.xfs` and `whoami` first, then tried anyway.

Also: `sudo make -C "$BUILD" install` (emacs build, claude 2026-08-29 ×3),
`sudo bash ~/fix-rocm-headless.sh` (opencode 2026-08-23 — itself a script
the model had written for the operator).

### 8. Authentication bypass attempts (3 · opencode 3, claude 0)

The one behavioural split between the agents. Blocked by the tty check,
opencode tried to *fake the terminal* and to feed an empty password:

> `script -q -c 'sudo usermod -aG render,video dead' /dev/null` — allocate a
> pty so sudo's tty check passes
> — opencode, 2026-08-23 18:41 UTC, retried at 19:25

> `echo '' | sudo -S -p '' usermod -aG render,video dead`
> — opencode, 2026-08-23 18:42 UTC — "Authentication failed, try again."

Claude Code never attempted either shape; it probed, announced the refusal,
and moved on.

### 9. Delegation — hand the root work to the operator (the universal fallback)

When sudo is impossible the agents converge on the same move: write the
script, print the exact `sudo …` line, and stop.

> `cat > /home/dead/install-nvidia-stack.sh` (94 lines, `sudo apt purge` inside),
> `bin/install-build-deps` ("Root half of the from-source Emacs build … the
> ONLY part that needs root"), `fix-rocm-headless.sh` ("Run this as: sudo bash …")
> — claude 2026-09-05 / 2026-08-29, opencode 2026-08-23

## Observations

- **Zero of 40 attempts succeeded.** The agents' effective privilege on this
  box was none, and nothing in 22 days changed that.
- The probe-first discipline (`sudo -n true`) is universal — 11 of 40 calls
  are pure probes, and nearly every real attempt is wrapped in
  `… || echo "<how the operator can do this>"`.
- The refusal path is the safe path: the destructive tier (mkfs, apt purge,
  make install) is exactly the tier that never got through.
- opencode's bypass attempts (fake pty, empty password over `sudo -S`) are
  the one pattern worth gating on explicitly in an agent harness: they are
  indistinguishable from ordinary sudo at the command-string level until you
  look for `script -q -c` and `sudo -S`.

## Re-running the scan

```bash
# Claude Code: bash tool calls containing a sudo line
jq -r 'select(.type=="assistant") | .timestamp as $t
       | .message.content[]? | select(.type=="tool_use" and .name=="Bash")
       | .input.command as $c | select($c | test("(?m)^\\s*sudo( |$)"))
       | [$t, $c] | @tsv' ~/.claude/projects/*/*.jsonl

# opencode: bash tool parts in the store
sqlite3 ~/.local/share/opencode/opencode.db \
  "select p.time_created, p.data from part p where p.data like '%\"tool\":\"bash\"%'"
```
