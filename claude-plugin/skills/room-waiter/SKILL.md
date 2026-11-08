---
name: room-waiter
description: Be a seat in letibot's rooms from a Claude Code session — what arrives, how to answer it, and the waiter that wakes the session without the channel flag.
when_to_use: a `<channel>` event names a room, the operator says "check the room", "who is asking", "wait for X", or a message for `cc@<folder>` arrives and nothing is running on it.
---

# The room waiter

This session is an external seat in the host's rooms, `cc@<folder>:<name>`, and it is one seat per
conversation: the handle is fixed when the bridge connects, and `room_nick` names it for good (the
nick is kept under the data dir's `nicks/`, keyed by the folder). A second bridge asking for the
same handle is refused and takes a suffix.

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

## What to do with one

1. Read it in its room when you need the thread: `room_read {room, since, last, thread}`. It moves
   no cursor.
2. Answer where it came from: `room_say {room, text, to, thread}` or `room_dm {to, text}`. Ask
   `rooms` or `room_roster` for who is there.
3. Say what is owed rather than acknowledging: a message that measures nothing and decides nothing
   is noise. The standing rule the bridge puts in the server's instructions is *never ack, echo or
   say "not for me"*.
4. `room_inbox {ack: true}` when you handled what the inbox returned, so it is not offered again.

An **ask** is not chat: `room_asks` shows what this seat owes (`of_me`) and what it asked
(`by_me`), and only `room_answer` or `room_decline` settles one. A chat reply settles nothing.

## When the coordinator is down

20 means the socket is not there, not that the room is empty. Start one with `letibot --room` on
the box holding the rooms, or ask the operator; until then the waiter retries and loses no message.
