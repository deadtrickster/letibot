<!-- abstract: MEASURED 2026-10-31 (branch agent/one-agent-per-tag): a tag's agent is `s-room-<host key>-<escaped tag>`; the escape (not sanitisation) is what keeps `8 gb` and `8-gb` two agents; the server's route never included an agent (it is neither poster nor holder) so a bell per tag needed a routing line as well as a filter; and the cost rule is two mechanisms — `wake::push` for a note, `bell()` for a turn — so claim/beat/done cost nothing and only Raised/Asked ring. -->

# A tag's agent: the name is an escape, and the bell has two mechanisms

MEASURED/DECIDED 2026-10-31, branch `agent/one-agent-per-tag` (stacked on
`agent/the-room-asks-for-a-restart`), building the room's slice 5.

## The derivation, and why it must be an ESCAPE rather than a sanitisation

`letibot_room::tag_session(host_key, tag)` = `s-room-<host key>-<slug>`, and
`tag_of_session` inverts it. The suffix keeps `a-z 0-9 -`, writes `_` as `__`, and writes every
other character as `_` + two hex digits **per UTF-8 byte**. A sanitiser that mapped every odd
character to `-` would make `8 gb` and `8-gb` one session — one agent for two queues — and the
inverse could not tell them apart. The unescape accumulates **bytes** and ends in
`String::from_utf8`: a `char` accumulator writes each byte of `д` as a character of its own and the
round-trip check (below) then refuses a tag that was spelled correctly.

`tag_of_session` ends with `tag_session(k, &tag) == session` — a hand-written id that merely starts
with the prefix is not an agent of anything, and that comparison is the only thing that tells the
two apart.

## The bell needed a ROUTING line, not only a filter

`Server::route` addressed an event to the sessions **queued under the tag** (posters, holders) and
to whoever the event named. A tag's agent is neither — it is not a poster and not a holder — so
without a line in `route` it hears nothing at all, and the connection-side filter would have been
green in a unit test while a real tag's agent sat deaf. The audience now includes
`tag_session(host_key(), tag)` for every tagged event.

## The cost rule is TWO mechanisms, and they are already there

`wake::push` writes a note into a per-session queue; `bell()` rings `ring_wake`, which is what opens
a turn. So "a claim is a comparison, a dispute is a judgement" is expressible without a new
mechanism: push for `Posted`/`Claimed`/`Done`/`Abandoned`, push **and** bell for `Raised`/`Asked`.
The test asserts the ring count is 0 after a claim and an abandonment and 1 after a dispute — which
is the cheapest honest proof, because a turn is a model call and cannot be asserted directly.

`Event::Ruled` carries no tag (`Event::tag()` answers `""`; the server looks the item up), so the
tag-side filter accepts a `Ruled` unconditionally: its arrival IS the addressing.

## The upkeep's signature is the cost argument

`ensure_tag_agents(ledger, registry, host_key, owner, tags, now)` takes **no bell**. A function with
nothing to ring cannot open a turn, which is a stronger statement than a test that the model stub
was not called. The re-seat itself is `Registry::create_under(tag_session, …, Some(owner))` after
`Registry::holds` says no — the same liveness fact the review queue asks before it re-seats a
reviewer — and `AgentLedger::seat` vs `reseat` are two calls so a caller that did not check
liveness cannot inflate the count.

## Rakes

- **`json!` builds in order.** `{"tag": tag, …, "agent": agents.get(tag.as_str())}` moves `tag`
  before the lookup and does not compile; compute the block first.
- **`Response::Room` grew a field**, so every destructuring of it in the tree had to be visited —
  `link.rs`'s `answer_sentence` used `..` and was fine, `server.rs`'s own test listed all five.
- **The protocol is now 3**, and `wire.rs`'s version doc is the place the reason is written: a pane
  drawn from a protocol-2 room would say *no agent* for every tag, which is a lie of omission.
