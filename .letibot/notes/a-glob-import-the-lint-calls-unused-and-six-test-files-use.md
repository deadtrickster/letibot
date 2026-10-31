<!-- abstract: MEASURED 2026-11-01 clearing the tree's warnings: rustc's `unused_imports` on a `pub(crate) use x::*` does not see a user that reaches it through another glob, so deleting three of them broke eight test files; `cargo fix` leaves a blank line with trailing spaces where it removed an import; and a crate-level `#![allow(dead_code)]` in a test binary was hiding four dead items. -->

# A glob re-export the lint calls unused, and six test files that use it

**MEASURED 2026-11-01**, branch `agent/clear-the-warnings` (`main` at `ef025a37`),
clearing `cargo check --workspace --all-targets` from 128 warning sites to zero.

## The trap that cost a compile: rustc's lint is blind to a glob-of-a-glob

`cargo check` said, in the lib build:

    warning: unused import: `loading::*`
    warning: unused import: `screen::*`        (crates/tui/src/ui/mod.rs)
    warning: unused import: `composer::*`      (crates/tui/src/app/mod.rs)

I deleted all three. The lib build then compiled clean and the **lib test** build
failed with 21 errors:

    cannot find value `BODY_PATIENCE` / `ATTACH_IMPATIENT` / `CAT_FRAMES` / `CAT_SLOT`
    cannot find value `HOLD_MARKER`
    cannot find function `complete_path_word`, cannot find type `PathCompletion`

The users are test modules that reach the names through a **second** glob:
`app/tests/*.rs` does `use super::*` → `app/tests/mod.rs` does `use super::*` →
`app/mod.rs` does `use crate::ui::*`. rustc counts a glob import as used only when
the name is resolved *through that glob in a module it can see*; a re-export
consumed by another glob in another module reads as unused in the lib build and
as used in the test build, so the warning appears exactly once and lies.

**The fix that is honest:** delete the glob *and* give the test module the import
it actually needs — `use crate::ui::loading::{ATTACH_IMPATIENT, BODY_PATIENCE,
CAT_FRAMES, CAT_SLOT};` and `use crate::ui::screen::HOLD_MARKER;` in
`app/tests/mod.rs`, `use crate::app::composer::{PathCompletion, complete_path_word};`
in `app/tests/composer.rs`. `screen::*` exported exactly one public item.

**Rake:** a `pub(crate) use x::*` whose only readers are `#[cfg(test)]` code warns
in the lib target and works in the test target. Never delete one on the lint's word
alone; `cargo check --workspace --all-targets` (which builds both) is the check, and
even that compiles the lib before the test, so the error arrives only after the
deletion.

## `cargo fix` leaves a blank line with trailing spaces where an import was

Three sites (`harnessd/src/child_todos.rs`, `harnessd/src/harness.rs`,
`sessionlog/src/hub.rs`) came back as `    ` — four spaces on an empty line — where
the import had been. `git diff | grep '^+ *$'` finds them.

## A crate-level `#![allow(dead_code)]` was hiding four dead items

`crates/harnessd/tests/clear_context.rs` opened with `#![allow(dead_code)]`. It was
found by accident: clippy's `duplicated_attributes` fired because that crate-level
allow and `canned.rs`'s own inner allow both applied to `mod canned`. Removing the
crate-level one surfaced `const THINK_OPEN`, `Stub::bodies`, `Stub::prompt_of` and
`TempDir::relative` — all four dead, all four now deleted. A crate-level allow is
how a file's dead code stops being countable.

## The numbers

* `cargo check --workspace --all-targets`: **128 warning sites → 0**, 33 targets.
  Per crate at the start: tui 22 (lib) + 26 (lib test), harnessd 3 (lib) + 7,
  tools 3 (lib) + 51 (lib test), sessionlog 1, turn 1, llama 1, plus 17 test
  targets with one to seven each.
* `cargo clippy --workspace --all-targets`: **376 → 184** after
  `cargo clippy --fix` (which applies only the MachineApplicable ones) and a
  hand pass over the borrows, returns, patterns and sorts. What is left is 69
  design-shaped (`Err`-variant very large 29, complex type 15, large variant 14,
  too many arguments 11) and 75 documentation formatting (a list item's
  continuation line, an over-indented list, a floating paragraph above an item).
