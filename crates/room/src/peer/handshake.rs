//! Erlang's `~/.erlang.cookie` handshake, in the small: each side picks a nonce, each side proves
//! it knows the cookie by HMAC-ing the OTHER side's nonce, and the cookie itself never crosses the
//! wire. A box that cannot prove it is refused, and the refusal names the host it claimed to be.
//!
//! The two proofs are domain-separated (`lbp1-a:` for the dialer's, `lbp1-b:` for the answerer's)
//! so a proof of one nonce cannot be reflected back as the answer to the other. No encryption: an
//! eavesdropper on the LAN reads the conversation and cannot join it, which is Erlang's own bargain
//! and the one lead 71 says to make until it is not enough.

use crate::peer::cookie::Cookie;
use crate::peer::frame::{PEER_PROTOCOL, PeerFrame, protocol_refusal};

/// What the dialer proves: the answerer's nonce, prefixed so a proof of one direction is no proof
/// of the other.
pub const DIALER: &str = "lbp1-a:";
/// What the answerer proves.
pub const ANSWERER: &str = "lbp1-b:";

/// Where a handshake got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing concluded: the next frame decides.
    More,
    /// Both proofs hold. `host` and `port` are the far side's, as its own last frame said them.
    Up { host: String, port: u16 },
    /// The link is not made, and this is the sentence the operator reads.
    Refused(String),
}

impl Outcome {
    /// The sentence a refusal carries, when this is one.
    pub fn why(&self) -> Option<&str> {
        match self {
            Outcome::Refused(why) => Some(why),
            _ => None,
        }
    }

    pub fn is_up(&self) -> bool {
        matches!(self, Outcome::Up { .. })
    }
}

/// One move in a handshake: what to write, and where this side stands after writing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub say: Option<PeerFrame>,
    pub outcome: Outcome,
}

impl Step {
    fn more(say: Option<PeerFrame>) -> Step {
        Step {
            say,
            outcome: Outcome::More,
        }
    }

    fn up(say: Option<PeerFrame>, host: String, port: u16) -> Step {
        Step {
            say,
            outcome: Outcome::Up { host, port },
        }
    }

    fn refused(say: Option<PeerFrame>, why: String) -> Step {
        Step {
            say,
            outcome: Outcome::Refused(why),
        }
    }

    pub fn is_up(&self) -> bool {
        matches!(self.outcome, Outcome::Up { .. })
    }

    pub fn why(&self) -> Option<&str> {
        match &self.outcome {
            Outcome::Refused(why) => Some(why),
            _ => None,
        }
    }
}

/// The side that dials: it speaks first, and it is up when the answerer's `Ready` says both proofs
/// held.
#[derive(Debug, Clone)]
pub struct Dialer {
    host: String,
    port: u16,
    nonce: String,
    up: bool,
}

impl Dialer {
    pub fn new(host: impl Into<String>, port: u16, nonce: impl Into<String>) -> Dialer {
        Dialer {
            host: host.into(),
            port,
            nonce: nonce.into(),
            up: false,
        }
    }

    pub fn hello(&self) -> PeerFrame {
        PeerFrame::Hello {
            proto: PEER_PROTOCOL,
            host: self.host.clone(),
            port: self.port,
            nonce: self.nonce.clone(),
        }
    }

    pub fn on(&mut self, frame: &PeerFrame, cookie: &Cookie) -> Step {
        if self.up {
            return Step::more(None);
        }
        match frame {
            PeerFrame::Challenge {
                proto,
                host,
                port: _,
                nonce,
                proof,
            } => {
                if let Some(why) = protocol_refusal(*proto) {
                    return Step::refused(Some(PeerFrame::Refused { why: why.clone() }), why);
                }
                if !cookie.proves(&format!("{DIALER}{}", self.nonce), proof) {
                    let why = format!(
                        "{host} answered this box's challenge with a proof of another cookie, so \
                         it does not know this LAN's cookie and no link was made. Nothing of this \
                         box's was said to it."
                    );
                    return Step::refused(Some(PeerFrame::Refused { why: why.clone() }), why);
                }
                // Its own nonce is answered now; the link is up when its `Ready` says this side's
                // proof of that was accepted.
                let proof = cookie.proof(&format!("{ANSWERER}{nonce}"));
                Step::more(Some(PeerFrame::Proved { proof }))
            }
            PeerFrame::Ready { proto, host, port } => {
                if let Some(why) = protocol_refusal(*proto) {
                    return Step::refused(None, why);
                }
                self.up = true;
                Step::up(None, host.clone(), *port)
            }
            PeerFrame::Refused { why } => Step::refused(None, why.clone()),
            other => Step::refused(
                Some(PeerFrame::Refused {
                    why: format!("`{}` arrived before the link was up", other.word()),
                }),
                format!(
                    "the far side sent `{}` before the handshake ended",
                    other.word()
                ),
            ),
        }
    }
}

/// The side that answers: it speaks second, and it is up once the dialer has proved itself.
#[derive(Debug, Clone)]
pub struct Answerer {
    host: String,
    port: u16,
    nonce: String,
    dialer: Option<(String, u16)>,
    up: bool,
}

impl Answerer {
    pub fn new(host: impl Into<String>, port: u16, nonce: impl Into<String>) -> Answerer {
        Answerer {
            host: host.into(),
            port,
            nonce: nonce.into(),
            dialer: None,
            up: false,
        }
    }

    pub fn on(&mut self, frame: &PeerFrame, cookie: &Cookie) -> Step {
        if self.up {
            return Step::more(None);
        }
        match frame {
            PeerFrame::Hello {
                proto,
                host,
                port,
                nonce,
            } => {
                if let Some(why) = protocol_refusal(*proto) {
                    return Step::refused(Some(PeerFrame::Refused { why: why.clone() }), why);
                }
                self.dialer = Some((host.clone(), *port));
                let proof = cookie.proof(&format!("{DIALER}{nonce}"));
                Step::more(Some(PeerFrame::Challenge {
                    proto: PEER_PROTOCOL,
                    host: self.host.clone(),
                    port: self.port,
                    nonce: self.nonce.clone(),
                    proof,
                }))
            }
            PeerFrame::Proved { proof } => {
                let host = self
                    .dialer
                    .as_ref()
                    .map(|(h, _)| h.clone())
                    .unwrap_or_else(|| "a box with no name".to_string());
                if !cookie.proves(&format!("{ANSWERER}{}", self.nonce), proof) {
                    let why = format!(
                        "{host} did not prove this box's cookie, so it is not one of this LAN's \
                         boxes and no link was made. Nothing of this box's was said to it."
                    );
                    return Step::refused(Some(PeerFrame::Refused { why: why.clone() }), why);
                }
                let (host, port) = self.dialer.clone().expect("a hello was read");
                self.up = true;
                Step::up(
                    Some(PeerFrame::Ready {
                        proto: PEER_PROTOCOL,
                        host: self.host.clone(),
                        port: self.port,
                    }),
                    host,
                    port,
                )
            }
            PeerFrame::Refused { why } => Step::refused(None, why.clone()),
            other => Step::refused(
                Some(PeerFrame::Refused {
                    why: format!("`{}` arrived before a `hello`", other.word()),
                }),
                format!(
                    "the far side sent `{}` before the handshake ended",
                    other.word()
                ),
            ),
        }
    }
}

/// A nonce from the kernel's pool: 16 bytes, hex. A guessable nonce would let one box answer
/// another's challenge from a proof it saw on the wire.
pub fn nonce() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::frame::PEER_PROTOCOL;

    /// Run a whole handshake between two sides, answering every frame the other one asked for,
    /// and return what each concluded.
    fn shake(cookie_a: &Cookie, cookie_b: &Cookie) -> (Outcome, Outcome) {
        let mut a = Dialer::new("lab2x1", 47100, "aa".repeat(16));
        let mut b = Answerer::new("mac", 47101, "bb".repeat(16));
        let mut to_b = Some(a.hello());
        let mut to_a: Option<PeerFrame> = None;
        let (mut out_a, mut out_b) = (Outcome::More, Outcome::More);
        for _ in 0..6 {
            if let Some(frame) = to_b.take() {
                let step = b.on(&frame, cookie_b);
                to_a = step.say;
                out_b = step.outcome;
                if matches!(out_b, Outcome::Refused(_)) {
                    break;
                }
            } else if let Some(frame) = to_a.take() {
                let step = a.on(&frame, cookie_a);
                to_b = step.say;
                out_a = step.outcome;
                if matches!(out_a, Outcome::Refused(_)) {
                    // A refusal is said to the other side, and the far side reads it.
                    if let Some(PeerFrame::Refused { why }) = to_b {
                        out_b = Outcome::Refused(why);
                    }
                    break;
                }
            } else {
                break;
            }
        }
        (out_a, out_b)
    }

    #[test]
    fn one_cookie_is_two_proofs_and_no_secret_on_the_wire() {
        let cookie = Cookie::of("a word for this lan");
        let (a, b) = shake(&cookie, &cookie);
        assert_eq!(
            a,
            Outcome::Up {
                host: "mac".to_string(),
                port: 47101
            }
        );
        assert_eq!(
            b,
            Outcome::Up {
                host: "lab2x1".to_string(),
                port: 47100
            }
        );
        // Nothing anybody sent carries the cookie: both proofs are of nonces.
        let dialer = Dialer::new("lab2x1", 47100, "aa".repeat(16));
        let hello = dialer.hello().line();
        assert!(!hello.contains("a word for this lan"));
        let mut answerer = Answerer::new("mac", 47101, "bb".repeat(16));
        let step = answerer.on(&dialer.hello(), &cookie);
        let challenge = step.say.expect("a challenge").line();
        assert!(!challenge.contains("a word for this lan"));
        assert!(
            challenge.contains(&"bb".repeat(16)),
            "the answerer's own nonce"
        );
    }

    #[test]
    fn a_box_that_does_not_know_the_cookie_is_refused_by_its_name() {
        // The answerer has another cookie: the dialer is the one that sees the bad proof, and it
        // names the host that claimed to be a peer.
        let (a, b) = shake(
            &Cookie::of("a word for this lan"),
            &Cookie::of("another lan's word"),
        );
        let why = a.why().expect("the dialer refuses");
        assert!(
            why.contains("mac") && why.contains("no link was made"),
            "{why}"
        );
        assert!(b.why().is_some(), "the answerer never goes up");
        // The dialer checks first, so a mismatched cookie is always refused by it - and the
        // answerer is never up, which is the whole of what a refusal has to achieve.
        let (a, b) = shake(
            &Cookie::of("another lan's word"),
            &Cookie::of("a word for this lan"),
        );
        assert!(a.why().is_some() && !a.is_up());
        assert!(
            !b.is_up(),
            "the answerer never goes up on a cookie it did not hear proved"
        );
    }

    #[test]
    fn a_proof_of_the_wrong_nonce_is_no_proof_at_all() {
        let cookie = Cookie::of("a word for this lan");
        // A box that replays the challenge it was sent back cannot answer the answerer's nonce.
        let mut answerer = Answerer::new("mac", 47101, "bb".repeat(16));
        let dialer = Dialer::new("lab2x1", 47100, "aa".repeat(16));
        let first = answerer.on(&dialer.hello(), &cookie);
        let PeerFrame::Challenge { nonce, .. } = first.say.expect("a challenge") else {
            panic!("a challenge");
        };
        // The dialer's proof is of the DIALER domain; the answerer wants the ANSWERER one.
        let reflected = Step::more(None);
        assert_eq!(reflected.outcome, Outcome::More);
        let wrong = PeerFrame::Proved {
            proof: cookie.proof(&format!("{DIALER}{nonce}")),
        };
        let step = answerer.on(&wrong, &cookie);
        assert!(step.why().is_some(), "a reflected proof is refused");
        // And the honest proof of that same nonce is accepted.
        let right = PeerFrame::Proved {
            proof: cookie.proof(&format!("{ANSWERER}{nonce}")),
        };
        let step = answerer.on(&right, &cookie);
        assert!(step.is_up(), "{:?}", step.outcome);
    }

    #[test]
    fn an_unreadable_protocol_is_refused_before_anything_is_proved() {
        let cookie = Cookie::of("a word for this lan");
        let mut answerer = Answerer::new("mac", 47101, "bb".repeat(16));
        let step = answerer.on(
            &PeerFrame::Hello {
                proto: PEER_PROTOCOL + 5,
                host: "lab2x1".to_string(),
                port: 47100,
                nonce: "aa".repeat(16),
            },
            &cookie,
        );
        let why = step.why().expect("a protocol skew is refused");
        assert!(why.contains("peering protocol"), "{why}");
        assert!(matches!(step.say, Some(PeerFrame::Refused { .. })));
        // Out of order is refused too, and says which frame it was.
        let mut dialer = Dialer::new("lab2x1", 47100, "aa".repeat(16));
        let step = dialer.on(
            &PeerFrame::Message {
                message: crate::chat::Message {
                    seq: 1,
                    ts: 0,
                    room: "dm".to_string(),
                    author: "mac/s-1".to_string(),
                    kind: crate::chat::Seat::Session,
                    to: None,
                    thread: None,
                    refs: Vec::new(),
                    attachments: Vec::new(),
                    text: "too early".to_string(),
                },
            },
            &cookie,
        );
        assert!(
            step.why()
                .unwrap_or_default()
                .contains("before the handshake")
        );
    }

    #[test]
    fn a_nonce_is_kernel_random_and_never_the_same_twice() {
        let a = nonce().expect("a nonce");
        let b = nonce().expect("a nonce");
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
