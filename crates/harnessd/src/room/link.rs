//! **One top-level session's connection to the box's room** — the tool's verbs behind it, the bell
//! that becomes a wake, and the beat the daemon runs on the session's behalf.
//!
//! # The three things this file is
//!
//! 1. **[`SessionRoom`] is the `room` tool's half.** `crates/tools` has no socket and no wire; it
//!    has [`letibot_tools::builtins::room::RoomLink`], and this is the implementation that speaks
//!    [`letibot_room::Request`] over the coordinator's socket and answers in sentences.
//! 2. **The bell.** A [`letibot_room::Event`] arrives on the connection's reader thread, becomes a
//!    sentence ([`super::wake::sentence`]) in the session's queue, and rings the worker's bell —
//!    which is firecode's fourth rule satisfied in code: *a bell is something that can inject a
//!    turn.* A session that only ever reads the room when a person prompts it is a mailbox, and the
//!    room would be read when somebody happened to look.
//! 3. **The beat on the session's behalf.** [`SessionRoom::turn_started`] begins beating and
//!    [`SessionRoom::turn_ended`] stops. This is the difference between a design that works and one
//!    that blames the model: a model that must remember to call `beat` is thinking about its work,
//!    and the ninety-second lease would take its item while it works. The beat is the daemon's
//!    because the turn clock is the daemon's.
//!
//! # A subagent has no link
//!
//! The room is for top-level sessions only, and a subagent speaks through its parent. So a session
//! with a `parent_session_id` is given **no link at all** rather than a link that refuses: the tool
//! it would reach is not on its seat either, and two refusals for one rule is one too many to
//! explain. The refusal that matters is the room's own, at the door.

use std::path::Path;
use std::sync::{Arc, Mutex};

use letibot_room::client::{Beat, Client};
use letibot_room::{Decision, Request, Response, Speaker};

use super::wake;

/// **A session's room.** One per top-level session, held by its `Harness` and by its tool.
pub struct SessionRoom {
    client: Client,
    session: String,
    socket: std::path::PathBuf,
    /// The beat, while a turn is running. `None` between turns, which is exactly when the lease is
    /// counting — see [`SessionRoom::turn_ended`].
    beat: Mutex<Option<Beat>>,
}

impl std::fmt::Debug for SessionRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRoom")
            .field("session", &self.session)
            .field("socket", &self.socket)
            .finish()
    }
}

impl SessionRoom {
    /// **Connect as `session`**, with `bell` rung for every event the room sends about it.
    ///
    /// `bell` is a closure rather than the registry so this type needs no daemon: the caller —
    /// `Harness::open` — has the bell, and what it does with it is one `ring_wake`.
    pub fn connect(
        session: &str,
        socket: &Path,
        bell: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Arc<SessionRoom>, String> {
        let me = session.to_string();
        // **The arbiter is not woken by the queue's traffic.** The coordinator's own session is the
        // arbiter, so the server routes every event to it — that is how a question reaches it — but
        // a turn per `post` would be a turn per item somebody else made, on the one session whose
        // job is to answer questions. `Raised`, `Asked` and `Ruled` are its business; the rest it
        // reads when it looks (`room queue`, `room questions`).
        let arbiter = session == super::session_id();
        let wake: letibot_room::client::Wake = {
            let bell = Arc::clone(&bell);
            let me = me.clone();
            Arc::new(move |event: &letibot_room::Event| {
                use letibot_room::Event;
                if arbiter
                    && matches!(
                        event,
                        Event::Posted { .. } | Event::Claimed { .. } | Event::Done { .. }
                    )
                {
                    return;
                }
                // **The push, then the ring** — a ring with nothing behind it is a wake that finds
                // nothing and costs a pass over the store.
                if let Some(line) = wake::sentence(event) {
                    wake::push(&me, line);
                    bell(&me);
                }
            })
        };
        let client = Client::connect(socket, Speaker::top_level(session), wake)
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(SessionRoom {
            client,
            session: session.to_string(),
            socket: socket.to_path_buf(),
            beat: Mutex::new(None),
        }))
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// **A turn is running: say so for as long as it does.** Idempotent — a second call while a beat
    /// is already running is the same beat, because two would double every message.
    pub fn turn_started(&self) {
        let mut slot = self.beat.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(self.client.start_beating());
        }
    }

    /// **The turn is over: stop beating.** The lease now counts from the last beat, which is the
    /// fact the room measures — a session whose turn ended is alive and *not working on it*.
    pub fn turn_ended(&self) {
        let mut slot = self.beat.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(beat) = slot.take() {
            beat.stop();
        }
    }

    /// One verb, and the sentence a model reads back.
    pub fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        let request = request_of(verb, args)?;
        let response = self
            .client
            .request(&request)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        answer_sentence(&response, &self.session)
    }

    /// **Every reminder the room owes, as data rather than as a sentence.**
    ///
    /// The `reminders` VERB answers a model in words (`answer_sentence`), and the daemon's pass
    /// needs the same answer as values: one row per item a session holds, with the wait it declared
    /// and whether its beat has gone quiet. A daemon that parsed the sentence back would be reading
    /// its own formatting, so the request is sent here and the reply read as the struct it is.
    ///
    /// **It is the whole room's list, not this session's** — `reminders` is one read for the room —
    /// so the caller filters by `Reminder::session`. That is deliberate rather than sloppy: the
    /// coordinator answers in one frame, and a per-session read would be a second frame shape for a
    /// question the room already answers.
    pub fn reminders(&self) -> Result<Vec<letibot_room::Reminder>, String> {
        let response = self.client.request(&Request::Reminders).map_err(|e| {
            format!(
                "the room at {} did not answer: {e}",
                self.socket.display()
            )
        })?;
        match response {
            Response::Reminders { rows } => Ok(rows),
            Response::Bye { why } => Err(format!("the room closed the connection: {why}")),
            // **A frame that is not the answer to this read** — said as a fact rather than
            // guessed at, because a daemon that invented a meaning here would put a wrong row on
            // a board. A `Request::Reminders` carries no id, so there is nothing to correlate.
            _ => Err(
                "the room answered a frame that is not a `reminders` read — nothing was written on \
                 the board"
                    .to_string(),
            ),
        }
    }
}

impl letibot_tools::builtins::room::RoomLink for SessionRoom {
    fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        SessionRoom::call(self, verb, args)
    }
}

/// **The verb and its arguments, as a frame.** `Err` is the tool's own refusal, in its words.
fn request_of(verb: &str, args: &serde_json::Value) -> Result<Request, String> {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let n = |k: &str| args.get(k).and_then(|v| v.as_u64());
    // **The id is minted here, per call**, and it is what makes a reconnect safe: a repeat of the
    // same frame after a dropped socket is answered from the room's replay window instead of acting
    // twice. A timestamp plus the verb is enough because one session's calls are serialised.
    let id = format!("{verb}-{}", letibot_room::server::now_ms());
    Ok(match verb {
        "post" => Request::Post {
            id,
            tag: s("tag"),
            body: s("body"),
        },
        "claim" => Request::Claim { id, tag: s("tag") },
        "claim_item" => Request::ClaimItem {
            id,
            item: n("item").ok_or("`claim_item` needs `item`")?,
        },
        "beat" => Request::Beat {
            id,
            item: n("item").ok_or("`beat` needs `item`")?,
        },
        "waiting" => Request::Waiting {
            id,
            item: n("item").ok_or("`waiting` needs `item`")?,
            text: s("text"),
        },
        "done" => Request::Done {
            id,
            item: n("item").ok_or("`done` needs `item`")?,
        },
        "abandon" => Request::Abandon {
            id,
            item: n("item").ok_or("`abandon` needs `item`")?,
            why: s("why"),
        },
        "dispute" => Request::Dispute {
            id,
            item: n("item").ok_or("`dispute` needs `item`")?,
            says: s("says"),
        },
        "rule" => {
            let decision = match s("decision").as_str() {
                "retry" => Decision::Retry,
                "hand" => {
                    let to = s("to");
                    if to.is_empty() {
                        return Err(
                            "`decision: \"hand\"` needs `to`: the session that gets the claim"
                                .to_string(),
                        );
                    }
                    Decision::Hand { to }
                }
                "drop" => Decision::Drop { why: s("why") },
                other => {
                    return Err(format!(
                        "`{other}` is not a decision — say `retry`, `hand` (with `to`) or `drop` \
                         (with `why`)"
                    ));
                }
            };
            Request::Rule {
                id,
                question: n("question").ok_or("`rule` needs `question`")?,
                decision,
            }
        }
        "queue" => Request::Queue { tag: s("tag") },
        "holding" => Request::Holding {
            session: Some(s("session")).filter(|x| !x.is_empty()),
        },
        "reminders" => Request::Reminders,
        "questions" => Request::Questions,
        other => {
            return Err(format!(
                "`{other}` is not a room verb — the verbs are post, claim, claim_item, beat, \
                 waiting, done, abandon, dispute, rule, queue, holding, reminders and questions"
            ));
        }
    })
}

/// **What the model reads back.** Every arm names the item and the state, because a reply a model
/// cannot check is a reply it will ask about again.
fn answer_sentence(response: &Response, me: &str) -> Result<String, String> {
    Ok(match response {
        Response::Posted { item, .. } => format!(
            "posted #{id} under `{tag}` — {}",
            describe(item),
            id = item.id,
            tag = item.tag
        ),
        Response::Claimed { item, .. } => format!(
            "you hold #{id} under `{tag}` — {}; your daemon beats for you while your turn runs",
            describe(item),
            id = item.id,
            tag = item.tag
        ),
        Response::Beaten { item, .. } => format!("#{} — your beat is recorded", item.id),
        Response::Waited { item, .. } => format!(
            "#{} — the room will remind you of this, and whoever is queued behind you can see it",
            item.id
        ),
        Response::Finished { item, .. } => {
            format!("#{} is done and `{}` is free", item.id, item.tag)
        }
        Response::Abandoned { item, .. } => format!(
            "#{id} went back to `{tag}` with {n} claimant(s) lost — {state}",
            id = item.id,
            tag = item.tag,
            n = item.abandonments(),
            state = describe(item)
        ),
        Response::Queue { tag, rows } => {
            if rows.is_empty() {
                format!("`{tag}` is empty")
            } else {
                let mut s = format!("`{tag}`, first come first served:");
                for row in rows {
                    s.push_str(&format!(
                        "\n  #{id} {body} — {state} (posted by `{by}`)",
                        id = row.id,
                        body = row.body,
                        state = describe(row),
                        by = row.posted_by
                    ));
                }
                s
            }
        }
        Response::Holding { session, rows } => {
            if rows.is_empty() {
                format!("`{session}` holds nothing in the room")
            } else {
                let mut s = format!("`{session}` holds:");
                for row in rows {
                    s.push_str(&format!(
                        "\n  #{id} under `{tag}` — {body} — held {held}, last beat {beat}{waiting}",
                        id = row.item,
                        tag = row.tag,
                        body = row.body,
                        held = letibot_room::duration(row.held_ms),
                        beat = row.beat_ms,
                        waiting = match &row.waits_on {
                            Some(w) => format!(", waiting on `{}`", w.text),
                            None => String::new(),
                        }
                    ));
                }
                s
            }
        }
        Response::Reminders { rows } => {
            if rows.is_empty() {
                "nobody holds anything in the room".to_string()
            } else {
                rows.iter()
                    .map(|r| format!("`{}`: {}", r.session, r.sentence()))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        Response::Questions { rows } => {
            if rows.is_empty() {
                "the room is not asking anything".to_string()
            } else {
                rows.iter()
                    .map(|q| {
                        format!(
                            "question #{id}: {text}\n  facts: {facts}",
                            id = q.id,
                            text = q.text,
                            facts = serde_json::to_string(&q.facts).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        }
        Response::Question { question, .. } => format!(
            "question #{id} is open: {text}",
            id = question.id,
            text = question.text
        ),
        Response::Ruled { question, .. } => match &question.answer {
            Some(a) => format!("question #{} is answered: {}", question.id, a.decision),
            None => format!("question #{} is still open", question.id),
        },
        Response::Refused { why, .. } => return Err(why.to_string()),
        Response::Bye { why } => return Err(format!("the room closed the connection: {why}")),
        Response::Welcome { .. } => format!(
            "the room is open to `{me}` (protocol {})",
            letibot_room::ROOM_PROTOCOL_VERSION
        ),
        Response::Event { event } => format!("the room said: {event:?}"),
    })
}

fn describe(item: &letibot_room::Item) -> String {
    match &item.state {
        letibot_room::State::Open => "open".to_string(),
        letibot_room::State::Claimed { claim } => {
            format!("held by `{}` for {}", claim.by, claim.beat_ms)
        }
        letibot_room::State::Done { by, .. } => format!("done by `{by}`"),
        letibot_room::State::Abandoned { last } => format!("abandoned — {}", last.why),
        letibot_room::State::Dropped { by, why, .. } => format!("dropped by `{by}`: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_verb_becomes_a_frame_with_a_request_id() {
        let cases = [
            json!({"verb": "post", "tag": "ram", "body": "8 GB"}),
            json!({"verb": "claim", "tag": "ram"}),
            json!({"verb": "claim_item", "item": 1}),
            json!({"verb": "beat", "item": 1}),
            json!({"verb": "waiting", "item": 1, "text": "the suite"}),
            json!({"verb": "done", "item": 1}),
            json!({"verb": "abandon", "item": 1, "why": "no"}),
            json!({"verb": "dispute", "item": 1, "says": "still on it"}),
            json!({"verb": "rule", "question": 1, "decision": "retry"}),
            json!({"verb": "queue", "tag": "ram"}),
            json!({"verb": "holding"}),
            json!({"verb": "reminders"}),
            json!({"verb": "questions"}),
        ];
        for args in cases {
            let verb = args["verb"].as_str().unwrap();
            let frame = request_of(verb, &args).unwrap_or_else(|e| panic!("{verb}: {e}"));
            // **Every MUTATING frame carries an id** — that is what makes a reconnect's repeat
            // answerable. A read has none by design (`Request::request_id` says so): there is
            // nothing to act on twice, and a re-read is cheap and cannot be wrong.
            let mutates = !matches!(
                frame,
                Request::Queue { .. }
                    | Request::Holding { .. }
                    | Request::Reminders
                    | Request::Questions
            );
            assert!(
                frame.request_id().is_some() == mutates,
                "{verb}: a mutating frame needs a request id and a read must not have one: {frame:?}"
            );
        }
    }

    #[test]
    fn a_hand_without_a_target_is_refused_here_and_never_sent() {
        let e = request_of("rule", &json!({"question": 1, "decision": "hand"})).unwrap_err();
        assert!(e.contains("needs `to`"), "{e}");
        let e = request_of("rule", &json!({"question": 1, "decision": "maybe"})).unwrap_err();
        assert!(e.contains("not a decision"), "{e}");
        let e = request_of("nonsense", &json!({})).unwrap_err();
        assert!(e.contains("not a room verb"), "{e}");
    }

    #[test]
    fn a_refusal_from_the_room_is_a_sentence_and_not_a_panic() {
        let refused = Response::Refused {
            id: Some("c1".into()),
            why: letibot_room::Refused::Busy {
                tag: "ram".into(),
                item: 1,
                by: "s-b".into(),
                held_for: "40s".into(),
            },
        };
        let e = answer_sentence(&refused, "s-a").unwrap_err();
        assert!(e.contains("held by `s-b`"), "{e}");
        assert!(e.contains("a tag is a lock"), "{e}");
    }

    /// **The wake, end to end minus the harness**: a real server, two real connections, and the
    /// bell the daemon rings.
    ///
    /// What this proves is the half that could be wrong: an event about `s-a` reaches `s-a`'s
    /// connection, becomes a sentence in `wake`, and rings the bell — while a session with nothing
    /// under the tag is not rung at all. What it does not prove is `Harness::wake` composing the
    /// sentence into a turn, which is one `take()` (`wake::tests` covers the queue, and the
    /// harness's own wake is where every notice source meets).
    #[test]
    fn an_event_about_a_session_rings_its_bell_and_a_reader_is_not_rung() {
        let dir = std::env::temp_dir().join(format!("letibot-room-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");

        let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bell: Arc<dyn Fn(&str) + Send + Sync> = {
            let rung = Arc::clone(&rung);
            Arc::new(move |session: &str| {
                rung.lock().unwrap().push(session.to_string());
            })
        };
        let me = SessionRoom::connect("s-a", &path, bell.clone()).expect("s-a connects");
        let other = Client::connect(
            &path,
            Speaker::top_level("s-b"),
            Arc::new(|_: &letibot_room::Event| {}),
        )
        .expect("s-b connects");

        // `s-a` posts, so it is a MEMBER of `ram` — a reader is not.
        me.call("post", &serde_json::json!({"tag": "ram", "body": "8 GB"}))
            .expect("a post");
        let _ = other.request(&Request::Post {
            id: "p2".into(),
            tag: "ram".into(),
            body: "2 GB".into(),
        });
        // And somebody else's claim under that tag is news `s-a` must be woken by.
        let claimed = other
            .request(&Request::Claim {
                id: "c1".into(),
                tag: "ram".into(),
            })
            .expect("a claim");
        assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && wake::waiting("s-a") == 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            rung.lock().unwrap().iter().any(|s| s == "s-a"),
            "the bell must ring for the session the event is about: {:?}",
            rung.lock().unwrap()
        );
        let said = wake::take("s-a");
        assert!(
            !said.is_empty(),
            "a ring with nothing behind it is not a wake"
        );
        assert!(
            said.iter()
                .any(|s| s.contains("[room]") && s.contains("s-b")),
            "the sentence names who did it: {said:?}"
        );

        // **The arbiter is not woken by the queue's traffic.** It hears every event on its
        // connection — that is how a question reaches it — but a `post` is not a turn.
        let arbiter = super::super::session_id();
        let rung_arbiter: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let bell_arbiter: Arc<dyn Fn(&str) + Send + Sync> = {
            let n = Arc::clone(&rung_arbiter);
            Arc::new(move |_: &str| *n.lock().unwrap() += 1)
        };
        let host = SessionRoom::connect(&arbiter, &path, bell_arbiter).expect("the arbiter");
        let _ = other.request(&Request::Post {
            id: "p3".into(),
            tag: "ram".into(),
            body: "1 GB".into(),
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            *rung_arbiter.lock().unwrap(),
            0,
            "a post is not a question: the arbiter is woken by a raise, not by the queue moving"
        );
        assert_eq!(wake::waiting(&arbiter), 0);
        drop(host);
        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
