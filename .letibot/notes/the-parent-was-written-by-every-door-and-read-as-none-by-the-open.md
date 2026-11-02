<!-- abstract: Every door that creates a child writes `parent_session_id`, but a session reopened by id opened as a root; `Registry::parent_for` is the one rule and `put_session` writes the row once, so an old NULL stays NULL. -->

# The parent was written by every door, and read as none by the open

The row was never the hole. A `task` spawn, the daemon's startup session, a head's
`ResumeSession`, a parked child's reopen and the room's tag agents all write
`parent_session_id`. What a session REOPENED BY ID got was the daemon's config verbatim, so a
child reopened with `--session` opened as a root: a room link, the ROOT paragraph in its prompt,
the flowy door registered, and no parent to address.

`Registry::parent_for` (`crates/sessionlog/src/registry.rs:1169`) is the one rule;
`Harness::open_with_registry` applies it once, AFTER the mode block, because that block asks
`cfg.parent_session_id` a different question (does this session inherit a LIVE parent's point)
and would otherwise hand a resumed child the daemon's `--mode`.

`sub_cfg`'s `Some(parent)` is load-bearing: at a `task` child's open it is not in the registry
yet, so that argument is the only record of the relation.

`put_session` is called once, at the open, guarded by `if stored.is_none()`
(`crates/harnessd/src/harness/open.rs:2421`), so a NULL written by an older build is never
repaired.
