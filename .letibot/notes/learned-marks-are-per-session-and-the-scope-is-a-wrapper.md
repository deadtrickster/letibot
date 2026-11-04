<!-- abstract: session_marks (schema 38) keeps a session's learned provider marks on its own row; openai.rs cannot be told which session it serves, so the set is entered as a scope by a wrapping backend, and a backend that is not wrapped learns nothing and says so to nobody. -->

# Learned marks are per session, and the scope holder is a wrapper backend

`crates/provider/src/openai.rs` decides `Eyes` and the retry itself and carries no session
identity: `known_text_only(preset, model)` and `mark_bad_picture(data_ref, why)` are the whole
seam, and that file belongs to another package. So a session's learned set cannot be handed in as
a parameter; it is reached through a `thread_local` holder (`crates/provider/src/marks.rs`,
`enter`), which the daemon enters by wrapping the backend: `Scoped` enters for the length of
`complete`, so `eyes()`, the retry and the mark calls inside it all read one session's set.

Two consequences worth not rediscovering:

- The wrapper is the ONLY thing that scopes anything. A backend built and stored without
  `scoped_provider` still answers and still retries, and silently forgets: nothing is recorded,
  `mark_*` answers false, so the operator's once-only notice is not said either. There are exactly
  two construction sites, `Harness::open` and the model switch in
  `crates/harnessd/src/harness/models.rs`; a third added later must be wrapped too.
- A request assembled with no scope in force is not an error, it is a caller that learns nothing
  (`update`/`read` answer `None`). That is what keeps the provider crate's own tests honest, and
  what makes an unwrapped backend a quiet defect rather than a crash.

Persistence is `session_marks` (`crates/tokencore/src/store/marks.rs`, schema 38, `Additive` in
`READER_ARMS`): one row per session holding the provider crate's JSON verbatim. The write happens
in the wrapper's `learned` callback, which opens the store BY PATH - `Store::open` hands back the
process's one connection for that file, so no descriptor is spent, and the callback runs after
`complete` returns, where the harness holds no store lock.

The bound is `MARKS_MAX = 64` per list, oldest out: evicting re-teaches the provider one old mark,
which is the cheaper loss than an unbounded list.
