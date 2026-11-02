<!-- abstract: `shell::normalise` reports an unquoted heredoc body with backticks as resolved, and `<<-` extraction carries the terminator's tab, so a recogniser must refuse `$`, a backtick and a backslash itself and refuse `<<-` - and look up `write` in the registry before rewriting. -->

# A heredoc body is not always the bytes, and `is_resolved()` does not say so

Two ways a recognised `cat > f <<'TAG'` would write bytes the shell would not have. `is_resolved()` is
false for `$` in an unquoted-delimiter body but TRUE for a backtick (`heredoc_redirect` pushes
`ExpandedHeredoc` only when the body node has a child that is not `heredoc_content`, and tree-sitter
does not put a `command_substitution` there), and a backslash is the same class. So the recogniser
checks `literal` itself: unquoted means refuse any of `$`, a backtick or a backslash.

And `<<-` body extraction carries the terminator line's own leading tab inside `heredoc_body`, so
`cat > f <<-'EOF'` with an indented `a` yields `"a\n\t"` where the real body is `"a\n"`. `HereDoc`
does not distinguish `<<` from `<<-`, so the only tell is the redirect's span
(`text.starts_with("<<-")`); the recogniser refuses `<<-` for that reason, because a body off by a tab
is a file rewritten rather than written.

And the seat that has `bash` is not always the seat that has `write`: `runner_tools` and `m2_runner`
register `bash` and no `write`, so a rewrite, or a "use `write` instead" nudge, must look the tool up first.
