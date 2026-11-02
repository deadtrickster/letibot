<!-- abstract: A gate step runs in the ssh session's scope, which is not writable, so the apparatus read as absent - `gate_host` wraps each step in `systemd-run --user --scope -p Delegate=yes`, and a subagent on a host cannot answer a question about the machine. -->

# The host had the delegation; the step was not in a scope of its own

A gate step runs in the ssh SESSION's scope (`/user.slice/…/session-NNNNN.scope`, `writable=no`), so
`Cgroup2::probe`'s `mkdir` fails and the apparatus reads as absent. Nothing was there to grant:
`systemd-run --user --scope -p Delegate=yes --quiet -- true` answers 0 on that host. `gate_host` wraps
each step in exactly that (`crates/harnessd/src/mergequeue/gate_host.rs:973`), and the apparatus comes
back with the demand intact - `-p letibot-tools --test confine` went from 2 passed / 23 failed to 25
passed, and `-p letibot-harnessd --lib` from 782/17 to 806/0.

A `task { where: "host:NAME" }` child's `bash` runs INSIDE a bwrap boundary, so it cannot answer a
question about the machine. The channel that is the machine is the tree's own example, which runs steps
over plain ssh in the environment the gate's get and touches no store:

```sh
cargo run --offline -p letibot-harnessd --example gate_on_host -- <host> <tree> -- "<step>" "<step>"
```
