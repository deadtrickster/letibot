//! What two coordinators say to each other: one line of JSON at a time, on a TCP connection that
//! is one link between two boxes.
//!
//! The shape follows [`crate::wire`]'s for the same reason - a frame is a value, a line is a frame,
//! and a reader that does not know a word says so rather than failing the link. What travels here
//! is only what a peer cannot derive: the handshake's two proofs, the tick, the seats a box holds,
//! and the messages addressed to a seat that lives on the other side.

use serde::{Deserialize, Serialize};

use crate::chat::Message;
use crate::peer::profile::SeatProfile;

/// Which conversation these frames are. A beacon and a `Hello` both carry it, and a box that does
/// not speak it is refused by number rather than obeyed wrongly.
pub const PEER_PROTOCOL: u16 = 1;

/// The oldest peer this build still speaks to.
pub const OLDEST_PEER: u16 = 1;

/// How long one frame may be. A `Message` carries a chat line whose own cap is [`crate::MAX_TEXT`],
/// and a `Profiles` list is a few hundred bytes a seat; past this a peer is holding this box's
/// memory, not saying anything.
pub const MAX_LINE: usize = 128 * 1024;

/// One frame of a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerFrame {
    /// The dialer's first word: who it is, where to dial it back, and the nonce it will be proved
    /// against. A nonce and not the cookie: the cookie never crosses the wire.
    Hello {
        proto: u16,
        host: String,
        port: u16,
        nonce: String,
    },
    /// The answerer's: its own nonce, and its proof of the dialer's.
    Challenge {
        proto: u16,
        host: String,
        port: u16,
        nonce: String,
        proof: String,
    },
    /// The dialer's proof of the answerer's nonce - the second half of the handshake.
    Proved { proof: String },
    /// The answerer's word that the link is up, and its own facts.
    Ready { proto: u16, host: String, port: u16 },
    /// The link is not made, by name: a wrong cookie, or a protocol neither side can read.
    Refused { why: String },
    /// The one-direction tick: a box pokes the far side, and a link that stops hearing anything is
    /// down. `at` is the sender's clock, for a reader that wants to say how late it was.
    Ping { at: u64 },
    /// What this box's seats are on. Sent when a link comes up and whenever the answer changes.
    Profiles { seats: Vec<SeatProfile> },
    /// A chat message for a seat on the far side: the only thing a link carries for delivery.
    Message { message: Message },
    /// Who may send whom direct messages across projects, as this box holds it: the whole table
    /// when a link comes up, and one row when a consent is given or taken back.
    Consents { rows: Vec<ConsentRow> },
    /// An orderly goodbye, so the far side reads a down link as a decision rather than a fault.
    Bye { why: String },
}

/// One row of [`PeerFrame::Consents`]: the pair, and whether it holds for one message or for as
/// long as both seats are here. `on` false is the pair being taken back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRow {
    pub from: String,
    pub to: String,
    pub session: bool,
    pub on: bool,
}

impl PeerFrame {
    /// The frame's own word on the wire, for a log line and for [`PeerFrame::of`].
    pub fn word(&self) -> &'static str {
        match self {
            PeerFrame::Hello { .. } => "hello",
            PeerFrame::Challenge { .. } => "challenge",
            PeerFrame::Proved { .. } => "proved",
            PeerFrame::Ready { .. } => "ready",
            PeerFrame::Refused { .. } => "refused",
            PeerFrame::Ping { .. } => "ping",
            PeerFrame::Profiles { .. } => "profiles",
            PeerFrame::Message { .. } => "message",
            PeerFrame::Consents { .. } => "consents",
            PeerFrame::Bye { .. } => "bye",
        }
    }

    /// The frame as the one line a link writes.
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{\"type\":\"bye\"}"))
    }

    /// A line as a frame, or `None` when it is not one this build knows: an unknown word is a
    /// newer peer's, and a link that failed to parse one line is a link that should keep reading.
    pub fn of(line: &str) -> Option<PeerFrame> {
        serde_json::from_str(line).ok()
    }
}

/// Why a peer speaking `theirs` cannot be linked, or `None` when it can. One sentence because two
/// doors say it - the answerer's refusal and the dialer's - and two sentences would drift.
pub fn protocol_refusal(theirs: u16) -> Option<String> {
    if (OLDEST_PEER..=PEER_PROTOCOL).contains(&theirs) {
        return None;
    }
    Some(format!(
        "this box's room speaks peering protocol {PEER_PROTOCOL} and the other box speaks \
         {theirs}: a frame one of them does not know would be obeyed wrongly, so nothing was \
         linked. Upgrade the older box's letibot."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SayKind;
    use crate::chat::Seat;

    fn a_message() -> Message {
        Message {
            seq: 7,
            ts: 1_700_000_000_000,
            room: "dm".to_string(),
            author: "lab2x1/s-1792".to_string(),
            kind: Seat::Session,
            to: Some("mac/s-1".to_string()),
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            text: "the cache key moves with the branch".to_string(),
        }
    }

    #[test]
    fn every_frame_is_one_line_that_reads_back() {
        let frames = [
            PeerFrame::Hello {
                proto: PEER_PROTOCOL,
                host: "lab2x1".to_string(),
                port: 47100,
                nonce: "ab".repeat(16),
            },
            PeerFrame::Challenge {
                proto: PEER_PROTOCOL,
                host: "mac".to_string(),
                port: 47101,
                nonce: "cd".repeat(16),
                proof: "ef".repeat(32),
            },
            PeerFrame::Proved {
                proof: "12".repeat(32),
            },
            PeerFrame::Ready {
                proto: PEER_PROTOCOL,
                host: "mac".to_string(),
                port: 47101,
            },
            PeerFrame::Refused {
                why: "no cookie".to_string(),
            },
            PeerFrame::Ping { at: 12 },
            PeerFrame::Profiles { seats: Vec::new() },
            PeerFrame::Message {
                message: a_message(),
            },
            PeerFrame::Consents {
                rows: vec![ConsentRow {
                    from: "lab1/s-1".to_string(),
                    to: "lab2/s-2".to_string(),
                    session: true,
                    on: true,
                }],
            },
            PeerFrame::Bye {
                why: "stopping".to_string(),
            },
        ];
        for frame in frames {
            let line = frame.line();
            assert!(!line.contains('\n'), "{} is one line", frame.word());
            assert_eq!(PeerFrame::of(&line), Some(frame));
        }
        assert_eq!(a_message().say_kind(), SayKind::Post);
    }

    #[test]
    fn a_word_this_build_does_not_know_is_not_a_link_rather_than_an_error() {
        // A newer peer's frame reads as nothing, so the reader goes on to the next line.
        assert_eq!(PeerFrame::of("{\"type\":\"shiny\"}"), None);
        assert_eq!(PeerFrame::of("not json"), None);
        assert_eq!(PeerFrame::of(""), None);
        // And a protocol is a range this build reads, refused by name outside it.
        assert_eq!(protocol_refusal(PEER_PROTOCOL), None);
        let why = protocol_refusal(PEER_PROTOCOL + 7).expect("a newer box is refused");
        assert!(why.contains("peering protocol"), "{why}");
        assert!(protocol_refusal(0).is_some());
    }
}
