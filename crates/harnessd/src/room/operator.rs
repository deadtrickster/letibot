//! The operator's seat on the host coordinator. A head asks its daemon (`ClientFrame::RoomChat`),
//! the daemon refuses a tool call at `peer::door`, and this speaks to the coordinator as
//! `operator`. The author is the connection's, so a request cannot name another.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_room::client::Client;
use letibot_room::{OPERATOR, Request, Seat, Speaker};
use letibot_sessionlog::registry::RoomChatSeat;
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
}

impl OperatorSeat {
    pub fn new(socket: PathBuf) -> Arc<OperatorSeat> {
        Arc::new(OperatorSeat {
            socket,
            client: Mutex::new(None),
            read: Mutex::new(BTreeMap::new()),
        })
    }

    /// The seat a registry takes for `RoomChat`: one ask per head's request, one gone per
    /// head that left.
    pub fn seat(self: &Arc<Self>) -> RoomChatSeat {
        let ask = {
            let seat = Arc::clone(self);
            Arc::new(move |head: &str, op: Value| seat.ask(head, op))
        };
        let gone = {
            let seat = Arc::clone(self);
            Arc::new(move |head: &str| seat.head_gone(head))
        };
        RoomChatSeat { ask, gone }
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
    pub fn ask(&self, head: &str, op: Value) -> Value {
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
                Ok(c) => *slot = Some(c),
                Err(e) => {
                    return json!({"error": format!("the host coordinator is not reachable: {e}")});
                }
            }
        }
        let client = slot.as_ref().expect("just seated");
        match client.request(&request) {
            Ok(r) => serde_json::to_value(r).unwrap_or_else(|e| json!({"error": e.to_string()})),
            Err(e) => {
                *slot = None;
                json!({"error": e.to_string()})
            }
        }
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
}
