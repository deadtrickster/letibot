<!-- abstract: A protocol number is a statement about two binaries, so two branches that bumped `ROOM_PROTOCOL_VERSION` for different frames resolve as one number and two sections; and a room's route must not return early for an empty room. -->

# Two branches bumped one protocol to 4, and a room's route had an early return that would have eaten the bell

Neither branch ever shipped a client at 4, so no binary speaks 4 without both frames, and each
branch's own `assert_eq!` pin agreed with both and caught neither. One number, two sections -
never a renumber. The number is 5 now and moves at release points
(`crates/room/src/wire.rs:20`).

`Server::route` (`crates/room/src/server.rs:695`) derives and counts the audience whether or not
a socket is open, because an audience with no connection is the case the bell exists for. The
arbiter's own connection is in the BOX's room, so `here` must be `conns ∪ boxed` - built from
`conns` alone, a connected arbiter is counted as a gap.

Rakes: a branch that grows a function's signature is a conflict even where git says nothing,
because a clean rebase says nothing about a caller; `git add` on a half-resolved file is the
trap this tree has paid for twice - check `git grep -n -E '^(<<<<<<< |>>>>>>> |=======$)'`
before `rebase --continue`; and the base moves while you work, so a stale `main` makes your own
diff look like a botched rebase.
