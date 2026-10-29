<!-- abstract: MEASURED 2026-10-29 (branch agent/the-seat-decides-what-it-reads): `pub use leticode_config;` in harnessd's lib.rs kept ~40 `crate::leticode_config::…` paths compiling across a `git mv` of the module into a new crate — zero call-site edits; and `~/.claude/CLAUDE.md` EXISTS on the operator's box, so any reader of the seat's instruction files must take its box dirs as a VALUE or every fixture reads the operator's home. -->

# Moving a module into a crate: `pub use <crate>;` is the whole trick, and the box's home is not a fixture

MEASURED 2026-10-29, branch `agent/the-seat-decides-what-it-reads` (`65b20c72`,
`133fb48a`), while moving `crates/harnessd/src/leticode_config.rs` into a new
crate `crates/leticode-config` so the two instruction-file names could live
outside the runtime.

## `pub use leticode_config;` in the old crate's `lib.rs` keeps every path

`crates/harnessd/src/lib.rs` had `pub mod leticode_config;`. Replacing it with

```rust
pub use leticode_config;   // the external crate, re-exported as a root item
```

keeps `crate::leticode_config::LeticodeConfig`, `crate::leticode_config::Level`
and `letibot_harnessd::leticode_config::…` in the integration tests compiling
**with no call-site edits at all** — a `use` binding is an item in the module
namespace, so paths resolve *through* it. The whole move was: `git mv` the file
to `crates/leticode-config/src/lib.rs`, a manifest for the new crate, one
workspace member line, one dependency line, and that one `pub use`. ~40 call
sites untouched, and the diff reads as a rename (98% similarity).

The moved file needed **two** new lines of its own: it referenced
`letibot_provider::keys::config_file` and `letibot_tools::exec::memory::*`, so
both are dependencies of the new crate. Nothing referenced `crate::` from
inside it, which is what made it movable at all.

## `~/.claude/CLAUDE.md` exists on this box — so a box scope must be a value

`ls -la ~/.claude/CLAUDE.md` on the operator's Mac: a symlink to
`~/Projects/hands-off/claude/CLAUDE.md` (the 155-line global-instructions file
that is in this very prompt). `~/.config/letibot/leticode.md` did **not** exist.

So a discovery function that read `$HOME` where it needed it would inject the
operator's own instructions into every test's system prompt — and
`standing_notes`' unit tests assert `section(..).is_none()` for an empty
workspace, which would have gone red on this box and stayed green on CI. The
fix is `leticode_config::BoxDirs { config, home }` threaded through `gather` /
`section` / `rows` / `notes_offer::compose` (one new parameter each), with
every fixture pointing it at scratch dirs. `standing_notes::section` already
took its box-wide notes dir this way, which is why the shape was familiar.

**And an empty path is not "nothing":** `BoxDirs::default()` with
`PathBuf::new().join("leticode.md")` is a RELATIVE path — it would have read
`./leticode.md` out of the process's CWD. Every join goes through `in_path`,
which answers `None` for an empty directory.

## The rake that cost a tool call

`sh scripts/check-fmt.sh main` died with `Killed` under the tool's default
**64 MiB** memory cap: it runs `rustfmt --check` over every touched file, and
`crates/harnessd/src/harness.rs` is 24,865 lines. Pass `memory: "4G"` (or more)
to any call that formats or compiles this tree.

## What the rule became, in one line

`leticode_config::instructions(workspace, &BoxDirs)` → at most two paths:
the nearest `leticode.md` (or `CLAUDE.md` in that same directory) at or above
the workspace, then `<config>/leticode.md` else `~/.claude/CLAUDE.md`. Neither
present is an empty list, not an error. `standing_notes::gather` reads them as
part of its FIRST source (with `AGENTS.md`), not a fourth one, because they are
a rule and a rule has to arrive whole ahead of every note.
