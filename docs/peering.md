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
- **No nick travels.** A peer seat is drawn by its id until the nick protocol is carried.
- **A box that is the smaller host name dials.** Two boxes that hear each other make one link; the
  one whose host name sorts first dials, so nobody has to negotiate it.

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

- `crates/room/src/peer/` - the cookie, the handshake, the beacon, the mesh and the tick.
- `crates/room/src/server.rs` - `with_peer`, and the one place a message is shipped (`ship_remote`,
  called from `append_and_push`) and taken in (`accept_remote`).
- `crates/harnessd/src/room/peering.rs` - the cookie path, the `[peering]` table, and reading a
  project's repository identity from git.
- `crates/room/tests/peering.rs` - two coordinators in one process on loopback: wrong cookie
  refused, a cookie at the wrong mode refused, a silent link marked away, a cross-box dm delivered,
  one introduction per seat, two repositories not introduced, and an opted-out project.
