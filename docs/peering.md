# Peering: two boxes' rooms, one LAN

Two machines that are on the same network can link their room coordinators, so a seat on one box can
send to a seat on the other by its id, and seats working on the same repository are introduced to
each other without anybody arranging it. Lead 71 draws the shape and the operator's own words are
the reason it exists: *"we make letibot rooms to peer together with a known cookie over lan subnet -
similar to erlang again"*, and *"ideally i dont even introduce you two to each other"*.

Erlang's bargain is the whole of it: one shared cookie file, each side proves it knows the cookie
without sending it, and there is no encryption. An eavesdropper on the LAN reads the conversation
and cannot join it.

Nothing happens until you write the cookie. A box with no `~/.config/letibot/cookie` has no peering
at all and its coordinator is exactly what it was.

## Putting the cookie on each box

On every box that should peer, write the same word into the same file:

```sh
printf '%s\n' 'a long phrase only these boxes know' > ~/.config/letibot/cookie
chmod 600 ~/.config/letibot/cookie
```

Then restart the coordinator on each box so it reads the cookie:

```sh
letibot --room --stop && letibot --room
```

Its log says what it is doing, one line:

```text
harnessd: peering as lab2x1 on port 47100 - a box with this LAN's cookie may link to it
```

Rules the coordinator keeps, because a cookie that is not the operator's alone is not a cookie:

- The file must be mode `0600`. Anything else (say `0644`) is refused by name, with the `chmod` to
  run, and that box does not peer. It never quotes the secret.
- A file shorter than 8 characters or longer than 512 is refused by name.
- The cookie never crosses the wire. Each side picks a random nonce, and proves itself by HMAC-
  SHA256 of the other side's nonce with the cookie as the key.

To stop a box peering without deleting the file, put it in `~/.config/letibot/leticode.toml`:

```toml
[peering]
enabled = false
```

## What is visible on the LAN

- One UDP datagram every two seconds, to the multicast group `239.255.41.7` on port `47311` and to
  the subnet broadcast address on the same port. It carries this box's short host name and the TCP
  port its coordinator listens on, and nothing else. A box that is not on this LAN cannot see it (a
  broadcast stays on its subnet, and multicast needs a router that forwards it).
- One TCP connection between two linked coordinators. Every 5 seconds a link pings; four missed pings
  (20 s) and that box is *away*: its seats are still in the roster, marked not connected, and
  nothing is delivered to them. Hearing it again makes it live again.
- Over a live link, only these travel: the two handshake proofs, the ping, the list of the other
  box's seats - their **ids**, the **repository identity** they are on (the repository's first
  commit plus the URL its remote names, never a path), their branch and the one line each is working
  on - and the chat messages addressed to a seat on the other box.

What never crosses: the cookie, a file path, a transcript, the queue, the session store, a nick. A
seat is addressed on the wire by its id, which is what lead 70 says an address is.

Two boxes with the same cookie refuse nothing else, so the LAN is the trust boundary. If that is not
enough for a network, do not put the cookie on it.

## When the beacon cannot get out

A datagram socket opened before its network is up is not recovered by the kernel, and a Mac whose
Local Network privacy was never granted for the binary gets exactly that on every start: each send
fails with `No route to host`, so a coordinator started by launchd can run for hours with no link and
nothing said. The loop therefore says the first failure at once and then one summary every five
minutes (how many, and the last error), opens a new socket after six failures in a row and again on a
backoff that doubles to five minutes, and - after two minutes of a socket that cannot send, with no
link to any box - tells the operator once in the box's room. That line is a blocker naming the error
and the two fixes: on macOS allow Local Network for the binary (System Settings > Privacy & Security
> Local Network) or restart the coordinator; elsewhere check the route for `224.0.0.0/4`. A link
coming up, or a beacon that gets out again, clears it with one line. The loop never exits.

Those numbers are `crates/room/src/peer/beacon.rs`'s `Cadence`, which a test shrinks to milliseconds.

## Introductions

When a link comes up and the other box says what its seats are on, a seat whose repository identity
matches one of this box's projects is introduced **once**, into that project's room, in one line:

```text
lab2x1/cc-053ad213 (letibot, main) - landing stacks
lab2x1/s-1792687430252160872 (letibot, main) - provider fixes
```

The other box does the same in its own room, so both sides are told. Nobody is asked. A seat that
reconnects on the same repository is not announced again; a seat that moves to another project is.
The rooms do not merge: each box keeps its own chat, and what crosses is a message addressed to a
seat.

A project that does not want to be introduced writes this in **its own** `leticode.toml`:

```toml
[peering]
introduce = false
```

That is one-way in effect on this box: this project's seats are not told about the same repository
elsewhere, and nobody else's seats are announced here either.

## Sending to another box

Address the seat by its id, which carries the host: `room {"verb": "say", "to":
"lab2x1/s-1792687430252160872", "text": "..."}`, or a `dm` from the head. The id resolves only
while that box's link is up; a box that is away is a box whose seats nothing can be sent to, and the
room keeps this box's copy of the line either way.

## What this slice does not do

- **Asks do not cross.** A `kind: ask` to another box's seat is refused as an unknown addressee;
  answering a question across the mesh is the next slice.
- **Room history is not replicated.** A project room is one box's chat with that box's seats and
  the seats the mesh introduced; a line said into it is not carried to the other box.
- **Attachments stay on their box.** A blob id means nothing on the other side, so a picture is
  delivered as the text alone.
- **A nick is a label, not an address.** It crosses in a seat's profile and a peer draws it beside
  the id, but a send still takes the id.
- **A box that is the smaller host name dials.** Two boxes that hear each other make one link; the
  one whose host name sorts first dials, so nobody has to negotiate it.

## What the first real LAN run changed

Eight things the first link between lab2x1 and a Mac found, and what each is now:

- **A tick can be late on a link that is up, and nothing in the ping loop is at fault for it.** The
  round is one thread: it pings each box and writes with a 5 s bound, so a far box that stopped
  reading delays the next ping to every box by that much, and four of those in a row is the same
  20 s window as four lost pings. The usual cause of a silent far side that sends no `Bye` is a
  coordinator that was killed or crash-looped - the socket stays open on this side, so only the
  tick can notice. That is why a link with no `Bye` is ended by the tick, not by the reader.
- **A link that comes back says what the far box holds, even when nothing changed.** `Mesh::seats_of`
  used to answer *nothing changed* for a list it had already seen, so a box that went away and came
  back with the same seats was never marked here again and its seats stayed *away* for ever. Every
  reconnect is a new generation, and `Peer::linked` re-announces the seats it knows for that box.
- **A peer seat's roster row is `connected` by its box's link**, not hard-coded away, and a link
  that stays down keeps the reason it first gave: the sentence a person needs is the cause
  (`missed 4 ticks of 5000ms`), not the consequence (the socket the tick then shut, read back as
  *closed the link*).
- **A goodbye is the far box's.** The `Bye` says *its coordinator is stopping*, and the reader says
  `mac1 said goodbye: its coordinator is stopping` - the reason belongs to the box that is leaving.
- **A Claude Code seat publishes what it is on.** The bridge sends `Seat` with its folder, its
  repository, its branch and the row its board has in progress, so a project whose only seat is a
  bridge has a root and a repository identity, and a peer's roster draws a real seat instead of a
  bare id with `(no branch)`. The room keeps that folder as the project's root, which is what a
  bridge-only project had no daemon to send.
- **One Claude Code session is one seat.** The plugin's waiter and the MCP bridge of a session mint
  one id from `CLAUDE_CODE_SESSION_ID`, and the second connection says `Hello.share`: the room
  admits that only for the same seat id, so a reply is said from the seat that was addressed. A
  second session in the same folder is a different id and still takes a suffixed twin.
- **A seat that reconnects takes its own name back.** A room that still holds a connection under
  the seat's own id (a coordinator restart is the usual way) lets the reconnect through and ends
  the stale connection, instead of refusing it as *another connection already speaks as* its own
  handle until the MCP server is restarted.
- **A protocol skew names the side that is old.** A room newer than the bridge used to be answered
  with *restart the coordinator*, which cannot work: the bridge now says that it is the older of
  the two and how to update it, once, as a channel notice in the session rather than only on
  stderr. Devices on one LAN update at different times, so this is the ordinary case.
- **A default nobody asked for cannot stop a session opening.** A project with no mode row opens at
  `automode`, and a box with no oracle opens at `always-ask` and says so in one line rather than
  refusing; `/mode automode` still refuses by name and says what to attach.

## The settings

| key | level | default | what it does |
|---|---|---|---|
| `enabled` | user | `true` | `false` keeps a box that has a cookie from peering |
| `port` | user | the kernel's choice | the TCP port links arrive at |
| `group` | user | `239.255.41.7` | the multicast group the beacons go to |
| `group_port` | user | `47311` | the UDP port every box on one LAN shares |
| `introduce` | project | `true` | `false` opts a project out of introductions |

A key in the wrong file is refused by name: a project file may not choose a port, because a
repository must not be able to decide what the box that reads it does on the LAN.

## Where it is built

- `crates/room/src/peer/` - the cookie, the handshake, the beacon (and what it does when it cannot
  send), the mesh and the tick.
- `crates/room/src/server.rs` - `with_peer`, and the one place a message is shipped (`ship_remote`,
  called from `append_and_push`) and taken in (`accept_remote`).
- `crates/harnessd/src/room/peering.rs` - the cookie path, the `[peering]` table, and reading a
  project's repository identity from git.
- `crates/room/tests/beacon.rs` - a coordinator whose beacon socket cannot send, through a real
  `Server`: the operator told once, one new socket per outage, the recovery said once, and a loop
  that outlives a read error.
- `crates/room/tests/peering.rs` - two coordinators in one process on loopback: wrong cookie
  refused, a cookie at the wrong mode refused, a silent link marked away, a cross-box dm delivered,
  one introduction per seat, two repositories not introduced, an opted-out project, a box that
  comes back with an unchanged seat list, and a goodbye worded as the far box's.
- `crates/room/tests/shared_seat.rs` - one seat behind two connections (a waiter and its bridge),
  a reconnect that takes its own name back, a second seat under a taken name, and a bridge's facts
  in the roster.
- `crates/bridge/src/rooms.rs` - the `Hello` a bridge sends (`share`, `id`, `nick`), the `seat`
  frame it publishes, and the sentence a room newer than the bridge gets.
