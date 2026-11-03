//! The operator's seat on the host coordinator. A head asks its daemon (`ClientFrame::RoomChat`),
//! the daemon refuses a tool call at `peer::door`, and this speaks to the coordinator as
//! `operator`. The author is the connection's, so a request cannot name another.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_room::client::Client;
use letibot_room::{OPERATOR, Request, Seat, Speaker};
use serde_json::{Value, json};

/// One lazily opened operator connection per daemon, shared by every head attached to it.
pub struct OperatorSeat {
    socket: PathBuf,
    client: Mutex<Option<Client>>,
}

impl OperatorSeat {
    pub fn new(socket: PathBuf) -> Arc<OperatorSeat> {
        Arc::new(OperatorSeat {
            socket,
            client: Mutex::new(None),
        })
    }

    /// The handler a registry seats for `RoomChat`.
    pub fn handler(self: &Arc<Self>) -> letibot_sessionlog::registry::RoomChatHandler {
        let seat = Arc::clone(self);
        Arc::new(move |op| seat.handle(op))
    }

    /// One chat request. `op` is a room `Request`; anything outside the chat set is refused, so
    /// the queue's verbs and `notify` stay out of a head's reach.
    pub fn handle(&self, op: Value) -> Value {
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
        let rooms = seat.handle(json!({"type": "rooms"}));
        assert_eq!(rooms["type"], "rooms");
        assert_eq!(rooms["rows"][0]["id"], "host");
        let said = seat.handle(json!({"type": "say", "id": "1", "room": "host", "text": "hi"}));
        assert_eq!(said["type"], "said", "{said}");
        assert_eq!(said["message"]["author"], "operator");
        assert_eq!(said["message"]["kind"], "operator");
        let read = seat.handle(json!({"type": "read", "room": "host"}));
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
            assert!(seat.handle(op.clone())["error"].is_string(), "{op}");
        }
        server.stop();
    }

    #[test]
    fn no_coordinator_is_a_sentence() {
        let seat = OperatorSeat::new(PathBuf::from("/nonexistent/room.sock"));
        let r = seat.handle(json!({"type": "rooms"}));
        assert!(
            r["error"].as_str().unwrap().contains("not reachable"),
            "{r}"
        );
    }
}
