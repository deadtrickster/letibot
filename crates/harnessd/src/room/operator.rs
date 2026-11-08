//! The operator's seat on the host coordinator. A head asks its daemon (`ClientFrame::RoomChat`),
//! the daemon refuses a tool call at `peer::door`, and this speaks to the coordinator as
//! `operator`. The author is the connection's, so a request cannot name another.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_room::client::Client;
use letibot_room::{OPERATOR, Request, Seat, Speaker};
use letibot_sessionlog::registry::{RoomChatFanout, RoomChatSeat};
use serde_json::{Value, json};

/// One lazily opened operator connection per daemon, shared by every head attached to it.
/// The room keeps one read cursor per principal and every head speaks as `operator`, so
/// the per-head cursors live here and the room's moves only as far as the least-seen head.
pub struct OperatorSeat {
    socket: PathBuf,
    client: Mutex<Option<Client>>,
    /// Each head's inbox cursor, by the id the daemon named it at attach. An entry appears on
    /// the head's first chat request and goes when its connection ends.
    read: Mutex<BTreeMap<String, u64>>,
    /// How far the operator has read each channel, by channel key. It lives with the daemon, so a
    /// head that attaches later does not count what was read as unread.
    marks: Mutex<BTreeMap<String, u64>>,
    /// The daemon's fan-out to the heads that have chatted, handed over by
    /// [`letibot_sessionlog::registry::Registry::set_room_chat`] when it takes this seat. A seat no
    /// daemon installed has none, and then a pushed message goes nowhere.
    fanout: Mutex<Option<RoomChatFanout>>,
}

impl OperatorSeat {
    pub fn new(socket: PathBuf) -> Arc<OperatorSeat> {
        Arc::new(OperatorSeat {
            socket,
            client: Mutex::new(None),
            read: Mutex::new(BTreeMap::new()),
            marks: Mutex::new(BTreeMap::new()),
            fanout: Mutex::new(None),
        })
    }

    /// The seat a registry takes for `RoomChat`: one ask per head's request, one gone per
    /// head that left, and the slot the daemon hands its fan-out to.
    pub fn seat(self: &Arc<Self>) -> RoomChatSeat {
        let ask = {
            let seat = Arc::clone(self);
            Arc::new(move |head: &str, op: Value| seat.ask(head, op))
        };
        let gone = {
            let seat = Arc::clone(self);
            Arc::new(move |head: &str| seat.head_gone(head))
        };
        let attach = {
            let seat = Arc::clone(self);
            Arc::new(move |fanout: RoomChatFanout| {
                *seat.fanout.lock().unwrap_or_else(|e| e.into_inner()) = Some(fanout);
            })
        };
        RoomChatSeat { ask, gone, attach }
    }

    /// Every head that has chatted hears this message but `except`, which wrote it: a head's own
    /// words are already on its screen, and a second copy would draw twice.
    fn fan(&self, except: Option<&str>, message: Value, names_you: bool) {
        let fanout = self
            .fanout
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(fanout) = fanout {
            fanout(except, message, names_you);
        }
    }

    /// Chat the coordinator pushed to this connection: a session's words reach the heads without
    /// waiting for a poll. The room pushes under the author's own name, so this connection never
    /// hears the words it said itself.
    fn pushed(&self, message: &letibot_room::Message, names_you: bool) {
        if let Ok(value) = serde_json::to_value(message) {
            self.fan(None, value, names_you);
        }
    }

    /// A head's connection ended: its cursor stops holding the shared one back.
    fn head_gone(&self, head: &str) {
        self.read
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(head);
    }

    /// One chat request from one head. `op` is a room `Request`; anything outside the chat set
    /// is refused, so the queue's verbs and `notify` stay out of a head's reach.
    pub fn ask(self: &Arc<Self>, head: &str, op: Value) -> Value {
        // The marks are this seat's own state, not the coordinator's.
        let mut marks = self.marks.lock().unwrap_or_else(|e| e.into_inner());
        match op.get("type").and_then(Value::as_str) {
            Some("marks") => return json!({"type": "marks", "marks": *marks}),
            Some("mark") => {
                if let (Some(key), Some(seq)) = (
                    op.get("key").and_then(Value::as_str),
                    op.get("seq").and_then(Value::as_u64),
                ) {
                    let at = marks.entry(key.to_string()).or_insert(0);
                    *at = (*at).max(seq);
                }
                return json!({"type": "marked"});
            }
            _ => {}
        }
        drop(marks);
        let request: Request = match serde_json::from_value(op) {
            Ok(r) => r,
            Err(e) => return json!({"error": format!("not a room request: {e}")}),
        };
        if !matches!(
            request,
            Request::Rooms
                | Request::Say { .. }
                | Request::Dm { .. }
                | Request::Read { .. }
                | Request::Roster { .. }
                | Request::Inbox { .. }
                | Request::Ack { .. }
                | Request::Signals
                | Request::PutBlob { .. }
                | Request::GetBlob { .. }
        ) {
            return json!({"error": "the operator seat takes chat requests only"});
        }
        // A head that chats has a cursor from its first request, at nothing seen: until it
        // draws and acks, it holds the shared cursor where it was.
        let request = {
            let mut read = self.read.lock().unwrap_or_else(|e| e.into_inner());
            match request {
                Request::Ack { seq } => {
                    read.insert(head.to_string(), seq);
                    Request::Ack {
                        seq: read.values().copied().min().unwrap_or(seq),
                    }
                }
                other => {
                    read.entry(head.to_string()).or_insert(0);
                    other
                }
            }
        };
        let mut slot = self.client.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let wake = Arc::new(|_: &letibot_room::Event| {});
            match Client::connect_as(
                &self.socket,
                Speaker::top_level(OPERATOR),
                letibot_room::host_key(),
                Seat::Operator,
                wake,
            ) {
                Ok(c) => {
                    // The hook is the live half of this seat: what the coordinator pushes to the
                    // operator lands here and goes on to the heads. Installed at the connect, so a
                    // message pushed before a head's first ask is not lost on the floor.
                    let seat = Arc::clone(self);
                    c.on_chat(Arc::new(move |m, names_you| seat.pushed(m, names_you)));
                    *slot = Some(c);
                }
                Err(e) => {
                    return json!({"error": format!("the host coordinator is not reachable: {e}")});
                }
            }
        }
        let talk = matches!(request, Request::Say { .. } | Request::Dm { .. });
        let answered = slot.as_ref().expect("just seated").request(&request);
        // A head's own words are fanned out here and not by the coordinator: the room skips the
        // author's connection, and every head shares this one, so the other heads would wait for a
        // poll to hear them.
        let said = match &answered {
            Ok(letibot_room::Response::Said { message, .. }) if talk => {
                serde_json::to_value(message)
                    .ok()
                    .map(|v| (message.names(OPERATOR), v))
            }
            _ => None,
        };
        let reply = match answered {
            Ok(r) => serde_json::to_value(r).unwrap_or_else(|e| json!({"error": e.to_string()})),
            Err(e) => {
                *slot = None;
                json!({"error": e.to_string()})
            }
        };
        // The fan-out writes to other heads' sockets, so it is done with this seat's client lock
        // released: one slow head must not hold the next head's request.
        drop(slot);
        if let Some((names_you, message)) = said {
            self.fan(Some(head), message, names_you);
        }
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve(name: &str) -> (letibot_room::server::Server, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("letibot-opseat-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("room.sock");
        let server = letibot_room::server::Server::serve(&sock, letibot_room::Room::new()).unwrap();
        (server, sock)
    }

    /// A seat installed on a registry, with the heads `head` given a sink each: what a head's
    /// connection does in `sessionlog`, done here so the fan-out can be read by the test.
    /// The four-part answer is a listing, not a type: naming it would name the test's own shape.
    #[allow(clippy::type_complexity)]
    fn seated(
        name: &str,
        heads: &[&str],
    ) -> (
        letibot_room::server::Server,
        PathBuf,
        Arc<OperatorSeat>,
        Arc<Mutex<Vec<(String, Value)>>>,
    ) {
        let (server, sock) = serve(name);
        let seat = OperatorSeat::new(sock.clone());
        let heard: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let registry = letibot_sessionlog::registry::Registry::new();
        registry.set_room_chat(seat.seat());
        for head in heads {
            let heard = Arc::clone(&heard);
            let name = head.to_string();
            registry.chat_head(
                head,
                Arc::new(move |message, _names_you| {
                    heard.lock().unwrap().push((name.clone(), message));
                }),
            );
        }
        // Both heads have chatted, so the seat's connection to the coordinator exists.
        for head in heads {
            assert_eq!(seat.ask(head, json!({"type": "rooms"}))["type"], "rooms");
        }
        (server, sock, seat, heard)
    }

    /// A message the coordinator pushes to the operator's seat reaches every head that has chatted,
    /// with no head asking for anything: this is the whole of live delivery on the daemon's side.
    #[test]
    fn a_pushed_message_reaches_every_chatting_head() {
        let (server, sock, _seat, heard) = seated("fanout", &["h1", "h2"]);
        let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
        let other = crate::room::link::SessionRoom::connect(
            "s-op-live",
            &sock,
            "aaaaaaaaaaaa",
            None,
            bell,
            None,
        )
        .expect("a session link");
        other
            .call(
                "say",
                &json!({"room": "host", "text": "pushed, not polled"}),
            )
            .expect("the session's word lands");
        let reached = |head: &str| -> bool {
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|(h, m)| h == head && m["text"] == "pushed, not polled")
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !(reached("h1") && reached("h2")) {
            assert!(
                std::time::Instant::now() < deadline,
                "the push never reached both heads: {:?}",
                heard.lock().unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(other);
        server.stop();
    }

    /// A session's daemon reports how a wake ended and the room's scorecard shows it: the tokens
    /// of a silent wake are charged to the message that rang it.
    #[test]
    fn a_silent_wake_reported_by_the_daemon_reaches_the_scorecard() {
        let (server, sock) = serve("silent-wake");
        let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
        let me = crate::room::link::SessionRoom::connect(
            "s-quiet",
            &sock,
            "aaaaaaaaaaaa",
            None,
            bell,
            None,
        )
        .expect("a session link");
        let said = me
            .call(
                "say",
                &json!({"room": "host", "text": "passing by: build is red"}),
            )
            .expect("the word lands");
        let seq: u64 = said
            .split('#')
            .nth(1)
            .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|n| n.parse().ok())
            .expect("the answer names the seq");
        crate::room::link::wake_ended("s-quiet", vec![seq], true, true, 250);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let card = server.scorecard();
            if let Some(r) = card.senders.iter().find(|r| r.silent_tokens == 250) {
                assert_eq!((r.wakes, r.silent), (1, 1));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never recorded: {card:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(me);
        server.stop();
    }

    /// The head that wrote a message already has it on its screen: the daemon pushes it to the
    /// other heads only, because a second copy would draw the line twice.
    #[test]
    fn the_authors_own_message_is_not_pushed_back() {
        let (server, _sock, seat, heard) = seated("author", &["h1", "h2"]);
        let said = seat.ask(
            "h1",
            json!({"type": "say", "id": "1", "room": "host", "text": "mine alone"}),
        );
        assert_eq!(said["type"], "said", "{said}");
        let reached = |head: &str| -> bool {
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|(h, m)| h == head && m["text"] == "mine alone")
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reached("h2") {
            assert!(
                std::time::Instant::now() < deadline,
                "the other head never heard it: {:?}",
                heard.lock().unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !reached("h1"),
            "the author was pushed its own message: {:?}",
            heard.lock().unwrap()
        );
        server.stop();
    }

    #[test]
    fn the_operator_lists_and_posts_through_the_seat() {
        let (server, sock) = serve("post");
        let seat = OperatorSeat::new(sock);
        let rooms = seat.ask("h1", json!({"type": "rooms"}));
        assert_eq!(rooms["type"], "rooms");
        assert_eq!(rooms["rows"][0]["id"], "host");
        let said = seat.ask(
            "h1",
            json!({"type": "say", "id": "1", "room": "host", "text": "hi"}),
        );
        assert_eq!(said["type"], "said", "{said}");
        assert_eq!(said["message"]["author"], "operator");
        assert_eq!(said["message"]["kind"], "operator");
        let read = seat.ask("h1", json!({"type": "read", "room": "host"}));
        assert_eq!(read["rows"][0]["text"], "hi");
        server.stop();
    }

    /// A head's pictures go through the same seat as its words: an upload and a fetch are chat
    /// requests, and a seat that took `say` and refused the blob under it could never send one.
    #[test]
    fn the_seat_passes_blob_requests() {
        let (server, sock) = serve("blobs");
        let seat = OperatorSeat::new(sock);
        // One part, last: the store hashes what it holds and answers the attachment.
        let said = seat.ask(
            "h1",
            json!({"type": "put_blob", "upload": "u1", "name": "a.png", "mime": "image/png",
                   "part": 0, "last": true, "data": "iVBORw0KGgo="}),
        );
        assert_eq!(said["type"], "blob", "{said}");
        let id = said["attachment"]["id"]
            .as_str()
            .expect("an id")
            .to_string();
        assert_eq!(id.len(), 64, "the sha256 hex of the bytes: {said}");
        assert_eq!(said["attachment"]["mime"], "image/png");
        // And it can be fetched back by id, which is what a head draws a picture from.
        let got = seat.ask("h1", json!({"type": "get_blob", "id": id}));
        assert_eq!(got["type"], "blob_part", "{got}");
        assert_eq!(got["data"], "iVBORw0KGgo=", "{got}");
        assert_eq!(got["last"], true, "{got}");
        // And a say names it: the picture travels with the words it was sent with.
        let line = seat.ask(
            "h1",
            json!({"type": "say", "id": "1", "room": "host", "text": "![a](blob:B)",
                   "attachments": [id]}),
        );
        assert_eq!(line["type"], "said", "{line}");
        assert_eq!(line["message"]["attachments"][0]["mime"], "image/png");
        server.stop();
    }

    #[test]
    fn the_seat_takes_chat_and_nothing_else() {
        let (server, sock) = serve("only");
        let seat = OperatorSeat::new(sock);
        for op in [
            json!({"type": "post", "id": "1", "tag": "ram", "body": "x"}),
            json!({"type": "notify", "id": "1", "source": "todo", "text": "x"}),
            json!({"type": "room"}),
            json!({"nonsense": true}),
        ] {
            assert!(seat.ask("h1", op.clone())["error"].is_string(), "{op}");
        }
        server.stop();
    }

    #[test]
    fn no_coordinator_is_a_sentence() {
        let seat = OperatorSeat::new(PathBuf::from("/nonexistent/room.sock"));
        let r = seat.ask("h1", json!({"type": "rooms"}));
        assert!(
            r["error"].as_str().unwrap().contains("not reachable"),
            "{r}"
        );
    }

    /// One head's ack must not mark messages read for another: the room's one `operator`
    /// cursor moves only as far as the head that has seen the least, and a head that leaves
    /// stops holding it back.
    #[test]
    fn one_heads_ack_does_not_mark_read_for_another() {
        let (server, sock) = serve("cursors");
        let seat = OperatorSeat::new(sock.clone());
        // Somebody else's words, three of them: the operator's own posts are never counted as
        // the operator's unread, so the cursor only shows in what another session said.
        let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
        let a1 = crate::room::link::SessionRoom::connect(
            "s-op-a1",
            &sock,
            "aaaaaaaaaaaa",
            None,
            bell,
            None,
        )
        .expect("a session link");
        // Both heads have chatted before anything was said: the operator connection arms at the
        // head of the log, so the notes have to come after it or they were never in any head's
        // inbox to begin with.
        seat.ask("h2", json!({"type": "rooms"}));
        for i in 0..3 {
            let said = a1.call("say", &json!({"room": "host", "text": format!("note {i}")}));
            said.expect("the session's word lands");
        }
        // h1 has read to the head of the log.
        seat.ask("h1", json!({"type": "ack", "seq": 3}));

        // h2 has seen nothing, so the shared cursor has not moved: its own rooms answer still
        // counts everything h1 read.
        let seen = seat.ask("h2", json!({"type": "rooms"}));
        let unread = seen["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "host")
            .and_then(|r| r["unread"].as_u64())
            .unwrap_or(0);
        assert_eq!(unread, 3, "h1's ack marked h2's messages read: {seen}");

        // h2 catches up and the shared cursor moves.
        let acked = seat.ask("h2", json!({"type": "ack", "seq": 3}));
        assert_eq!(acked["cursor"], 3, "{acked}");
        let after = seat.ask("h2", json!({"type": "rooms"}));
        let unread = after["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "host")
            .and_then(|r| r["unread"].as_u64())
            .unwrap_or(0);
        assert_eq!(unread, 0, "the shared cursor never moved: {after}");
        server.stop();
    }

    /// What one head read stays read for the next head of the same daemon.
    #[test]
    fn a_mark_outlives_the_head_that_set_it() {
        let (server, sock) = serve("marks");
        let seat = OperatorSeat::new(sock);
        seat.ask("h1", json!({"type": "mark", "key": "host", "seq": 9}));
        seat.ask("h1", json!({"type": "mark", "key": "host", "seq": 4}));
        seat.head_gone("h1");
        let marks = seat.ask("h2", json!({"type": "marks"}));
        assert_eq!(marks["marks"]["host"], 9, "{marks}");
        server.stop();
    }
}
