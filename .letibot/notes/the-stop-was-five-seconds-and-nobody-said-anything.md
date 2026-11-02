<!-- abstract: The orderly stop printed nothing, so `--stop`'s five seconds were compared against bounds it could not see; and a lease is only taken over by somebody who CLAIMS it, which is why dead daemons' rows survived. -->

# The stop was five seconds long, and the daemon said nothing the whole time

The stop is `firecode::down_all()`, `merge_stop` and join the queue workers,
`daemon.shutdown()`, `drain_subagents`. It now prints one `stopping: …` line per step and the
launcher relays the last one it finds. The launcher reads only the bytes written AFTER the
signal (`log_at` + `tail -c +N`), because the per-folder log is append-only across runs.
`STOP_TENTHS` is 30 s for a plain stop; `--force` keeps `FORCE_TENTHS` (5 s) and then SIGKILLs.

`acquire_queue_lease` displaces a holder past `LEASE_TTL_MS`, but only a supervisor asks, and a
supervisor only asks for ITS OWN workspace's repositories - so a lease no daemon serves is stale
for ever. `QueueService::sweep_leases` walks the whole store and releases on the heartbeat it
judged, and `lease::gone` also requires the holder not to be a live process here.

Rake: `bash -c 'sleep 12'` is EXEC'd, so a crafted argv naming a socket vanishes and the
launcher sees no going daemon; `bash -c 'sleep 12; :'` keeps bash as the process.

One edge of serving only the repositories that asked: a repository whose target declares no
merge gate gets no worker, so a review row enqueued there is never claimed and nothing counts
it.
