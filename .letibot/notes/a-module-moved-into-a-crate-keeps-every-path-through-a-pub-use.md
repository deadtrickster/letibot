<!-- abstract: `pub use <crate>;` where a module was keeps every `crate::module::…` path compiling across a move into a new crate, and a box scope read from `$HOME` puts the operator's own instructions in every fixture. -->

# Moving a module into a crate: `pub use <crate>;` is the whole trick, and the box's home is not a fixture

`crates/harnessd/src/lib.rs` is `pub use leticode_config;` where it was `pub mod leticode_config;`. A `use` binding is an item in the module namespace, so `crate::leticode_config::…` and `letibot_harnessd::leticode_config::…` resolve through it with no call-site edits - about forty of them, and the diff reads as a rename. The move itself is `git mv`, a manifest, a workspace member line, a dependency line, and that one line. A file that names `crate::` from inside itself cannot move this way.

`~/.claude/CLAUDE.md` exists on this box, so a discovery function that read `$HOME` where it needed it would put the operator's instructions into every test's system prompt - red here and green on CI. `leticode_config::BoxDirs { config, home }` is threaded through `gather` / `section` / `rows` / `notes_offer::compose`, and every fixture points it at scratch dirs.

An empty path is not nothing: `PathBuf::new().join("leticode.md")` is relative and would read `./leticode.md` from the CWD, so every join goes through `in_path`, which answers `None` for an empty directory.
