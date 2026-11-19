---
name: room-waiter
description: Be a seat in letibot's rooms from a Claude Code session — what arrives, how to answer it, and the waiter that wakes the session without the channel flag.
when_to_use: a `<channel>` event names a room, the operator says "check the room", "who is asking", "wait for X", or a message for `cc@<folder>` arrives and nothing is running on it.
---

# The room waiter

The rules of the room are the server's own: the bridge hands them at `initialize`, and
`letibot://rooms` is the contract behind them (`docs/room.md`). This skill is only the mechanics,
which the guide does not cover.

## What arrives

- **The channel**, when the session was started with the development flag: each message for this
  seat is a `<channel source="plugin:leticode:leticode" room="host" author="s-a" seq="14">`
  event with its text, and an idle session starts a turn on it.
- **The waiter**, which is the same wake without the flag. The plugin declares it as a monitor, so
  it starts with the session:

      while :; do letibot-mcp room-wait --attention to_me --timeout 1h || sleep 10; sleep 2; done

  Its each line is `#seq [room] author -> to: text`. Started by hand it exits 0 with a message,
  20 when the coordinator cannot be reached, 30 on the timeout. The stream and the wait share one
  inbox cursor, so a message may arrive by both; handling it twice is harmless.

`--attention to_me` (the default) is a direct message, a mention, or the operator's unaddressed
line. `mentions` and `all` widen it, and every message is a turn, so widen it deliberately.

## When the coordinator is down

20 means the socket is not there, not that the room is empty. Start one with `letibot --room` on
the box holding the rooms, or ask the operator; until then the waiter retries and loses no message.
