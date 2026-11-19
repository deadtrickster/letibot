//! Coordinators peering across a LAN, the way Erlang nodes do: one cookie of the operator's, a
//! link that proves it knows the cookie without sending it, and a tick that says a peer is still
//! there.
//!
//! Nothing here opens without a cookie file. A box with no `~/.config/letibot/cookie` has no peer
//! object at all and its coordinator runs exactly as it did before this module existed - which is
//! every box until an operator writes one, and the reason the rest of `crate::server` needs no
//! switch to say so.
//!
//! What crosses a link is deliberately small: the handshake ([`handshake`]), the tick and the
//! seats a box holds ([`mesh`]), and the chat messages addressed to a seat on the far side. What
//! does not cross is anything a box could derive: a message's `seq`, a room's ring, a nick.

pub mod announce;
pub mod cookie;
pub mod frame;
pub mod handshake;
pub mod mesh;
pub mod profile;
mod run;

pub use cookie::{Cookie, CookieError};
pub use frame::{PEER_PROTOCOL, PeerFrame};
pub use mesh::{Link, Mesh, PeerEvent, Tick};
pub use profile::{Introductions, ProjectProfile, RepoIdentity, SeatProfile};
pub use run::{Peer, PeerConfig};

/// The default group, port and tick, for a caller that wants to say what it changed.
pub use announce::{BEACON, GROUP, PORT as GROUP_PORT};
