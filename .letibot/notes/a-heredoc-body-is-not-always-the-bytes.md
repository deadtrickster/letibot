<!-- abstract: MEASURED 2026-10-26 building the heredoc→write rewrite: `shell::normalise` does NOT report an unquoted-delimiter body with backticks as unresolved, and `<<-` body extraction still carries the terminator's leading tab — two ways a "recognised" heredoc would have written the wrong byte. -->

# A heredoc body is not always the bytes, and `is_resolved()` does not say so

MEASURED 2026-10-26, branch `heredoc-writes-2`, building
`crates/tools/src/heredoc.rs` (recognise a `cat > f <<'TAG'` bash call as a `write`).
Both findings are about `letibot_code::shell::normalise` and both would have written a
file the shell would not have.

## `is_resolved()` is false for `$` in an unquoted-delimiter body, and TRUE for backticks

```
cat > f <<EOF\na $HOME b\nEOF   → resolved=false, Construct::ExpandedHeredoc
cat > f <<EOF\na `date` b\nEOF  → resolved=TRUE, literal=false, body="a `date` b\n"
```

`heredoc_redirect` pushes `ExpandedHeredoc` only when the body node has a child that is
not `heredoc_content` (`shell.rs:~1300`). tree-sitter parses `$x` into a `simple_expansion`
child and a backquote into a `command_substitution`, but apparently not in this position —
so a backtick survives with `is_resolved()` true. A body with a backslash is the same
class (`\$`, `\\`, `\<newline>` are escapes in an unquoted heredoc). So the recogniser
checks `literal` itself: unquoted ⇒ refuse any of `$`, `` ` ``, `\`.

## `<<-` body extraction carries the terminator's leading tab

```
cat > f <<-'EOF'\n\ta\n\tEOF  → HereDoc { literal: true, body: "a\n\t" }
```

The real body is `a\n`. The trailing `\t` is the terminator line's own tab, left inside
`heredoc_body`. `RedirectOp::HereDoc` does not distinguish `<<` from `<<-`, so the only
way to tell is the redirect's span — which does cover the whole `heredoc_redirect`
(`text.starts_with("<<-")`). The recogniser refuses `<<-` for that reason; a body off by
a tab is a file rewritten rather than written.

## And the seat that has `bash` is not always the seat that has `write`

`runner_tools` (`crates/tools/src/lib.rs:317`) registers `bash` and **no** `write`; so
does `m2_runner`, and the gatekeeper seat has neither. `Role::leticode` has both, which is
why the rewrite fires in the operator's session — but any rewrite must look the target
tool up in the registry first, or a working command becomes *"no tool called `write` is
available here"*. The same check gates the "use `write` instead" nudge, because advice to
call a tool the seat does not hold cannot be taken.
