<!-- abstract: A bridge `Head` dropped without shutting its socket down keeps the connection thread of `Daemon::shutdown` blocked for ever, so `Drop` must `shut_down`; and a bridge end-to-end test needs no GGUF. -->

# A dropped head must close its socket, or the daemon cannot stop

`Daemon::shutdown` joins its connection threads, and a connection thread is blocked reading a socket
whose peer is still open. A bridge `Head` keeps that socket alive through its own reader and fold
threads, so dropping the last `Head` did nothing and a `registry.close()` plus join hung for minutes.
The fix is `impl Drop for Head { client.shut_down() }`, and any second head written this way needs the
same line. A head that only `detach`es is not enough: the daemon's side of `Detach` does not close the
peer's write half.

And the bridge's end-to-end test needs no GGUF at all. A session whose turns go to a provider opens on
`Vocab::bytes` - `Parts::load` with `vocab_gguf: None` and `cfg.provider = Some(..)` - and
`Harness::rounds` refuses a turn only when `provider.is_none() && vocab.is_bytes()`. So:
`letibot-harnessd` with `default-features = false`, a loopback fake answering OpenAI SSE frames, and a
`url` written into a scratch `providers.toml`. The whole test is about 1.5 s.
