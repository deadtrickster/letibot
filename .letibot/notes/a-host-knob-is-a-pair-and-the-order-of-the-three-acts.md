<!-- abstract: MEASURED 2026-11-01 building `host_config`: X11's auto-repeat is ONE knob with two numbers (`xset r rate <delay> <rate>`), so a set of either half must carry the other's current value; macOS's `defaults read` exits 1 with `does not exist` for a key never set (a fact, not a failure); and a value check that runs after the `before` read has already run a command, which is not "refused before anything runs". -->

# A host knob can be a pair, and "refused before anything runs" is an order, not an adjective

MEASURED 2026-11-01, branch `agent/host-config-the-first-slice` (worktree of the same name,
`main` at `976d9863`), building `host_config` on top of `host_changelog`.

## 1. X11's auto-repeat is ONE knob with two numbers

`xset q` prints `auto repeat delay:  660    repeat rate:  25`, and the write is
`xset r rate <delay> <rate>` — **both at once**. So `key-repeat-delay` and `key-repeat-rate` are
two names for two halves of one command, and a `set` of either has to read the other half first
and pass both:

```rust
"key-repeat-rate" => { let (delay, _, _) = self.repeat()?; self.set_repeat(delay, n) }
```

A platform whose write needs a *sibling read* is a shape the `read`/`apply` pair does not imply.
macOS's two keys are genuinely independent (`defaults write -g KeyRepeat` / `InitialKeyRepeat`),
which is what makes the difference easy to miss: the same two setting names on two platforms, and
on one of them a write is a two-number command.

## 2. `defaults read` says *does not exist* on stderr and exits 1 for a key never set

That is a **fact** (`before: ""` — *there was no such setting*, the changelog's own word for it),
not a failure, and a read that treats exit != 0 as unreadable will refuse to change a setting that
macOS is happily running on its own default. The distinction is the error text:

```rust
if ran.exit == 0 { ... } else if ran.said().contains("does not exist") { Ok(None) }
```

The consequence is left for the next slice: putting *not set* back is `defaults delete -g <key>`,
a third act the two-value `before`/`after` model has no verb for.

## 3. A value check placed after the `before` read has already run a command

The first cut validated the value inside `apply`, which runs after `set` has read the current
value. A test asserting *nothing was applied* failed with `["defaults read -g KeyRepeat"]` — true
that nothing was *applied*, false that nothing *ran*. The check is now `Platform::check`, called
by `set` before the read and again by `apply` (which is public, so a caller that came around
`set` is still checked).

**The rake**: "refused before anything runs" is a claim about the ORDER of the acts, and the only
way to hold it is a test that asserts the runner recorded no command — a test that asserts the
setting did not change passes either way.

## 4. `sudo -A` with the helper in the COMMAND's environment

`Privilege::as_root` returns `(["sudo","-A", …argv], [("SUDO_ASKPASS", helper)])`, and the runner
sets that env on the child only. A daemon has no terminal, so the alternative is not "ask later" —
it is a hang, and the refusal names `letibot-askpass` rather than letting sudo wait.

## 5. Two gates above a tool, in a fixed order, and a fixture needs both harnesses

`testing::harness()` (no adjudicator) refuses an `Access::Exec` call as **not run** before the tool
is reached; `testing::read_only_harness_with_gate(Some(allow_all()))` is the shape that reaches
the tool so its own backend check is what refuses. A test of a tool's second gate written against
`harness()` measures the first gate and reads as a pass on the wrong sentence.
