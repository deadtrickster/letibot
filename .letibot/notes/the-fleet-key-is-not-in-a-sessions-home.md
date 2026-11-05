<!-- abstract: A session's own HOME has no `~/.config/letibot/hosts.toml` and no `keys/<host>`, so a merge-queue gate on a declared host cannot be run from a placement; `hosts`, `fanout` and `task where: host:NAME` still work because they run from the daemon's side. -->

# The fleet key is in the operator's home, not the session's

`mergequeue::gate_host` reaches a declared host through `crates/tools/src/remote.rs`'s `Hosts::load`,
which reads `$HOME/.config/letibot/hosts.toml`, and `scripts/rrun` connects with
`$HOME/.config/letibot/keys/<host>`. In a placement, `$HOME` is the session's own
(`/home/dead/letibot-remote/.letibot-home/<id>`): it has `permission.json` and nothing else, so
`Hosts::load` finds no host and the key does not exist. MEASURED 2026-11-05 on xmg: a plain
`ssh dead@lubuntu1` from a session answers `Permission denied (publickey)`, and `accept-new` writes a
host key for a host nothing can use.

So R74 item 39's live half — a branch landing with its gate on lubuntu1/2 — cannot be run from a
placement, however long the seam is built. What is NOT blocked: the `hosts` tool, `fanout`,
`scenario` and `task { where: "host:NAME" }` do reach those machines, because they run on the
daemon's side of the boundary and not through the session's `$HOME`.

Do not read `~/.ssh` to work around it: that read is refused by the harness itself
(`secret_off_box`, boundary:flow) and no grant promotes it. The live run belongs on a box that
already holds `hosts.toml` and the per-host key, with `[queue] gate_host = "lubuntu1"` in the
project's `leticode.toml`, a daemon over that project, and a branch through `/queue`; the merge row
must then say `lubuntu1 gate`.
