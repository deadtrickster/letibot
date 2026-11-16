---
name: edits
description: Tracked files are changed with `edit` or `write`, never with a bash heredoc that writes one
kind: policy
when_to_use: any change to a file under version control
keywords: edit, write, heredoc, diff, tracked, bash
---

# edits

A heredoc through `bash` - `cat > f <<'EOF'`, `python3 - <<'PY' > f`, `tee f <<EOF` - is the
one edit the gate cannot see coming: it leaves no diff row, no path to judge and no record,
so its change is only detected afterwards. `edit` and `write` are the tools for tracked
files: their changes are one adjudicated diff, recorded and drawn.

Use `bash` heredocs for what is not a tracked file's contents - piping a program its input,
composing text for stdout, seeding a scratch file outside the checkout. When a command
writes a tracked file through a heredoc, the tool re-runs it as the `write` it stands for
and says so on the row; issue the call again with `edit` or `write` rather than through the
shell.
