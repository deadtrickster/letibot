<!-- abstract: MEASURED 2026-10-28: compiling one built-in wasm layer costs 1.97 s of CPU in a debug host and every session open paid it, which is what made `no_spin` and the idle-exit test red; and `check-fmt.sh` cannot survive a `git mv` of a .rs file (it feeds `old -> new` to rustfmt). -->

# A built-in wasm layer costs ~2 s of CPU to compile, and the first cut paid it on every session open

MEASURED 2026-10-28 in `agent-fix-tool-mount-landing` (the fix package for `agent/plugin-tool-mount`),
macOS, private `CARGO_TARGET_DIR`, debug profile.

## The numbers

`rano::plugin::Plugin::from_bytes_hooks`, timed on this binary's own table:

| layer | bytes | compile |
|---|---|---|
| `claude-transcript` | 232,869 | **1.97 s** |
| `sessions` | 173,119 | **1.39 s** |

and `PluginHost::production` — which compiled every built-in that declared a tool — took **1.78–1.92 s
per call**, i.e. per session open.

## What it broke, and it looked like two unrelated things

* `letibot-bridge --test no_spin`, phase 1: *"the process spent 3.006 s of CPU over a 3 s window with
  nothing to do"*. The whole window was the daemon opening its first session; `sample` showed every
  thread idle because the sample started after the compile.
* `letibot-harnessd --test session_lifecycle`, the idle exit: *"a daemon that has just started has not
  been idle for its window"*. `idle_exit_at` is armed in the `Sessions` constructor, and
  `Sessions::open_first` now took 1.8 s against an 800 ms window.

Both are the same fact: **opening a session is no longer cheap, and two tests measure that it is.**

## The two halves of the fix

1. **A session installs only the built-in layers it DECLARED a tool from**
   (`PluginHost::production` → `install_builtin_tools(&declared_tools(..))`). A session that seats no
   layer tool can never call one — the `PluginTool` that would is what a declaration seats — so the
   compile was pure waste for it, and that is also the seating rule the tree wanted (a built-in's
   `[[tools]]` is not a tool every session is offered; `crates/harnessd/tests/wired.rs` pins that).
2. **The compile is kept for the process** (`Loader::load_builtin`, overridden by `RanoLoader` with a
   `static OnceLock<Mutex<BTreeMap<String, Arc<dyn Module>>>>`, plus `Stack::register_builtin_shared`),
   so the sessions that DID ask pay it once. `Module` is `Send + Sync`, `Plugin::call` runs in a fresh
   store and fuel is an atomic, which is what makes one compiled module safe for several sessions.

## The rake beside it — `check-fmt.sh` cannot survive a rename

`sh scripts/check-fmt.sh <base>` builds its file list from `git status --porcelain -- '*.rs'` after
`sed 's/^...//'`. A **rename** prints `R  old -> new`, so the list holds `old`, `->` and `new`, and
rustfmt dies with `Unrecognized option: '>'` — reported as *"rustfmt failed and named no file"*, which
reads like a parse error in your code. Commit first (a committed rename goes through
`git diff --name-only --diff-filter=ACMR`, which prints only the new path), or run `rustfmt --check
--edition 2024 <the files>` by hand.
