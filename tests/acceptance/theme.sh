#!/usr/bin/env bash
# **A theme recolours the head, and a line it cannot use is said, not obeyed.**
#
# `head.toml` names a theme in `themes/` beside it and adds one `color.` line of its own; the
# replayed session's operator row is the role the theme overrides (`user_block`), in an exact
# colour, so what reaches the terminal is truecolor. `screen -e` reads the cells' SGR back.
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

mkdir -p "$CONFIG/letibot/themes"
echo 'user_block = "bg:#ff0000"' >"$CONFIG/letibot/themes/loud.toml"
printf 'theme = "loud"\ncolor.wat = "bold"\n' >"$CONFIG/letibot/head.toml"
for i in $(seq 1 30); do echo "line $i"; done >"$WORK/notes.txt"
start --replay "$FIXTURES/edit.jsonl"

spec "the operator's row is drawn in the theme's exact colour"
wait_for "please fix line eleven"
settle
if screen -e | grep -F "please fix line eleven" | grep -qE '48;2;255;0;0'; then
  pass "the row's background is #ff0000, as truecolor"
else
  fail "the row is not the theme's #ff0000: $(screen -e | grep -F 'please fix line eleven' | cat -v)"
fi

spec "a colour line naming no role is reported by name"
expect "head.toml: color.wat: \`wat\` is not a role"

# A theme this build SHIPS: no file in `themes/`, so the name can only be answered by the binary's
# own `dark` palette - and its `user_block` band is the one that reaches the operator's row.
printf 'theme = "dark"\n' >"$CONFIG/letibot/head.toml"
rm -f "$CONFIG/letibot/themes/loud.toml"
restart --replay "$FIXTURES/edit.jsonl"

spec "a built-in theme is drawn without a file beside head.toml"
wait_for "please fix line eleven"
settle
if screen -e | grep -F "please fix line eleven" | grep -qE '48;2;35;43;58'; then
  pass "the shipped dark theme's #232b3a band is on the operator's row"
else
  fail "the shipped dark theme did not reach the screen: $(screen -e | grep -F 'please fix line eleven' | cat -v)"
fi

done_spec
