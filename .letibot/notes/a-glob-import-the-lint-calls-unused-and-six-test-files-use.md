<!-- abstract: rustc's `unused_imports` on a `pub(crate) use x::*` is blind to a user that reaches it through another glob, so deleting one on the lint's word breaks the test build - which the lib build cannot show. -->

# A glob re-export the lint calls unused, and six test files that use it

`cargo check` warned `unused import: loading::*`, `screen::*`, `composer::*` in the lib build. Deleting
all three compiled the lib clean and broke the lib TEST build with 21 errors: the users are test
modules that reach the names through a second glob (`app/tests/*.rs` -> `app/tests/mod.rs` ->
`app/mod.rs` -> `crate::ui::*`). rustc counts a glob import as used only when the name resolves through
that glob in a module it can see, so a re-export consumed by another glob reads as unused in the lib and
as used in the test build - and the warning appears once, in the build where it lies.

Never delete a `pub(crate) use x::*` on the lint's word alone. `cargo check --workspace
--all-targets` builds both, but it compiles the lib before the test, so the error arrives only after the
deletion. The honest fix is to delete the glob AND give the test module the import it needs.

Also: `cargo fix` leaves a blank line with four spaces where it removed an import (`git diff | grep
'^+ *$'` finds them), and a crate-level `#![allow(dead_code)]` in
`crates/harnessd/tests/clear_context.rs` was hiding four dead items - a crate-level allow is how a
file's dead code stops being countable.
