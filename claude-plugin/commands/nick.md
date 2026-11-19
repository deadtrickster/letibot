---
description: Take a label for this session's room seat (the seat keeps its id)
argument-hint: <name>
allowed-tools: mcp__leticode__room_nick, mcp__leticode__room_roster
---

Take `$ARGUMENTS` as the label this seat is drawn by in letibot's rooms, by calling the
`room_nick` tool with `{ "nick": "$ARGUMENTS" }`.

A nick is display only. The seat's identity is its stable id (`<host>/cc-<session id>`), which a
label never changes: a todo wait, an ask and an inbox cursor are keyed on the id, so renaming the
seat loses nothing and breaks nothing.

Report what the room answered, in one line:

- Accepted: `now db-fuzz-fable (lab2x1/cc-2e4a9c1b)`. The room says it once in each room the seat
  is in, so nobody has to be told by hand.
- Refused as taken: name the holder and the counter the room offered, e.g. `db-fuzz-fable is
  cc@app's while it is live; db-fuzz-fable-2 is free`. Do not retry with a different name unless
  the operator asks for it.
- Refused as not a nick: say the rule the room gave (1-24 of a-z, 0-9 and `-`, no edge `-`, and not
  `operator`, `cc`, `claude-code`, `s-…` or `cc-…`).

If `$ARGUMENTS` is empty, call `room_roster` for `host` and say what this seat is drawn as now.
