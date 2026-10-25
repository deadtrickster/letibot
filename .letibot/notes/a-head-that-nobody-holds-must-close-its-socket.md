<!-- abstract: The bridge's end-to-end test needs no GGUF at all (byte vocab + a provider), and a dropped Head that does not shut_down() its socket makes Daemon::shutdown hang forever joining connection threads. -->

# A dropped head must close its socket, or the daemon cannot stop

MEASURED 2026-10-26, `crates/bridge/tests/end_to_end.rs`.

`Daemon::shutdown` joins its connection threads, and a connection thread is blocked
reading a socket whose peer is still open. A bridge `Head` keeps the socket alive through
its own reader/fold threads (`Arc<Mutex<HeadClient>>` + the `FrameReader` clone), so
dropping the *last* `Head` did nothing — the process's own threads held the fds — and the
test that followed (`registry.close()` then `join` the worker) hung for minutes after the
assertion failed. The fix is `impl Drop for Head { client.shut_down() }`. Any second head
written this way (an ACP adapter, leticl's) needs the same line, and a head that only
`detach`es is not enough: the daemon's side of `Detach` does not close the peer's write
half.

# The bridge's end-to-end test needs no GGUF, and that is the finding that makes it possible

A session whose turns go to a provider opens on `Vocab::bytes` — `Parts::load` with
`vocab_gguf: None` and `cfg.provider = Some(ProviderConfig{..})` — and `Harness::rounds`
refuses a turn only when `provider.is_none() && vocab.is_bytes()`. So:
`letibot-harnessd` with `default-features = false` (no llama.cpp, no GGUF, no model file),
a loopback fake answering the OpenAI SSE frames (`crates/provider/tests/fake_provider.rs`'s
shape, not the llama.cpp `tokens[]` one), and `url = "http://127.0.0.1:PORT/v1/chat/completions"`
written into a scratch `$XDG_CONFIG_HOME/letibot/providers.toml` (the URL comes from
`keys::resolve`'s per-provider `url` key). Whole test: ~1.5 s.

Two shapes that cost time and are worth knowing:
- `ServerFrame::Resync` is a STRUCT variant that carries a whole `Snapshot` — a head that
  folds it by reseeding loses nothing; the pre-compaction guess that it was a unit has it
  re-attaching.
- The permission ladder's option IDS are `deny` and `deny_and_tell`, while their KINDS are
  `reject_once`/`reject_always`. Matching an id containing "reject" finds nothing; match
  the kind.

And one that decides the design: `crates/letibot` is a BINARY-ONLY crate (no `src/lib.rs`),
so `launcher.rs`'s key/rundir/store logic is not reachable as a library. `crates/bridge/src/daemon.rs`
re-derives it and asserts the four measured keys against `launcher.rs`'s own test values.
