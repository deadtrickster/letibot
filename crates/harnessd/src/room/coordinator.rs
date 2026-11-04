//! The coordinator: a whole `harnessd` whose only job is the room - its own socket, no project,
//! no session store; the queues and the chat live in files beside the socket. Lease, then bind,
//! then refuse if no model can arbitrate: that order is the point, and
//! a room nobody arbitrates is a queue that freezes. `docs/room.md` holds the argument.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use letibot_room::lease::{Acquired, CoordinatorLease};
use letibot_room::{Room, Server};

use crate::config::{Config, Seat};

/// How often the tie-breaker looks for a question it has not prompted yet. The sweep is five
/// seconds and a question is only born from an abandonment, so two seconds is prompt.
pub const TIE_BREAKER_EVERY_MS: u64 = 2_000;

/// Turn `cfg` into the coordinator's own configuration, or refuse with the reason.
/// Called from `cli::run` the moment `--room` is seen, and before anything is bound, opened or
/// loaded: a coordinator with no model must not spend a vocabulary load to find out.
pub fn configure(cfg: &mut Config) -> Result<(), String> {
    cfg.room = true;
    // No session store, and it is the whole point. `sessions.db` is the operator's and shared by
    // every daemon, and this one has no business in it; the room's own queues live in a file
    // beside the socket (`super::items_path`), not in a session store.
    cfg.store = None;
    // One session, derived from the box, so `letibot --session host` always reaches the agent
    // that arbitrates this machine.
    cfg.session_id = super::session_id();
    cfg.title = "the room".to_string();
    // Two sockets, and they are not the same one. `room-<host key>.sock` is the ROOM's, which
    // every session connects to; the daemon needs its own for a head to attach to the arbiter's
    // session. One path for both would have the room's bind take the daemon's socket away.
    let room_socket = super::socket_path().ok_or_else(|| {
        "a room coordinator needs somewhere to put its socket, and this box has neither \
         $XDG_DATA_HOME nor $HOME — set LETIBOT_ROOM_SOCKET to name one"
            .to_string()
    })?;
    let named = room_socket
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "room.sock".to_string());
    cfg.socket = room_socket.with_file_name(format!("daemon-{named}"));
    // A workspace of its own, and a directory that exists. A session whose workspace is
    // missing publishes `workspace_gone` on every wake - the coordinator's first and only sentence.
    let data = cfg
        .socket
        .parent()
        .map(|p| p.join("room-workspace"))
        .ok_or_else(|| "the room's socket has no parent directory".to_string())?;
    std::fs::create_dir_all(&data)
        .map_err(|e| format!("creating the room's workspace {}: {e}", data.display()))?;
    cfg.workspace = data;
    // The seat that opens with nobody attached - `orchestrator` has no gated tool - and the
    // seat that decides whether the arbiter can rule at all: `resolve_role` keeps ONLY the tools a
    // role names, so `room` must be named in `roles::m1_orchestrator`, not merely registered.
    cfg.seat = Seat::Orchestrator;
    // And this daemon's own session joins its own room, so the arbiter does not hold a `NoRoom`
    // tool and hear the refusal one layer too late. Set HERE rather than inherited, so a fixture
    // calling `configure` measures the coordinator and not the constructor.
    cfg.room_link = true;
    // And it is the one daemon that may NOT restart the room: the pane's restart control asks a
    // SUPERVISOR, and this process draws the pane a restart would kill.
    cfg.restart_room = false;
    // No bash, no queue, no project.
    cfg.allow_bash = false;
    cfg.serve_queues = false;
    // And it does not idle out. A folder's daemon can exit because the next `leticode` resumes
    // it from the store; a coordinator has no session store, so an idle exit would be a room that
    // silently emptied overnight and every session's `room` tool would find no socket.
    cfg.idle_exit_after = None;
    // The model: `--model`, else `providers.toml`, else deepseek - the operator's *"a tie
    // breaker agent (model configurable with deepseek as default)"*. The arg loop already applied
    // `--provider`/`--model`, so this only fills an absence.
    if cfg.provider.is_none() && cfg.model.is_empty() {
        crate::cli::apply_main_model(cfg, DEEPSEEK, None);
    }
    // And it must be reachable - a room whose arbiter cannot answer freezes items nobody will
    // ever rule on, which is worse than no room. Asked of `provider/model` and not of `cfg.model`
    // a metered session's `cfg.model` is the local vocabulary alias, not the provider's.
    if let Some(p) = cfg.provider.as_ref() {
        let model = p
            .model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .or_else(|| {
                letibot_provider::Preset::parse(&p.name)
                    .ok()
                    .map(|pr| pr.fallback_model.to_string())
            })
            .unwrap_or_default();
        let named = format!("{}/{model}", p.name);
        if let Some(why) = crate::cli::unreachable_provider(&named) {
            return Err(format!(
                "the room's coordinator has no model to arbitrate with: {why}\n\
                 A room nobody arbitrates is a queue whose items freeze for ever, so this daemon \
                 will not start. Give it one with `--model PROVIDER/MODEL` (the default is \
                 `{DEEPSEEK}`), or put a key for it where `providers.toml` says."
            ));
        }
    }
    Ok(())
}

/// The default model of the tie-breaker - deepseek, as the operator asked.
pub const DEEPSEEK: &str = "deepseek/deepseek-flash";

/// A running coordinator, as `cli::run` holds it. Dropping it stops the socket and gives the
/// lease back, so an orderly stop leaves nothing for the next coordinator to take over.
pub struct Coordinator {
    server: Server,
    lease: CoordinatorLease,
    socket: PathBuf,
    stop: Arc<std::sync::atomic::AtomicBool>,
    tie_breaker: Option<std::thread::JoinHandle<()>>,
}

impl Coordinator {
    /// Take the lease, bind the socket, and start the tie-breaker's watch. `bell` is the
    /// daemon's - `registry.bell.ring_wake` - and it is what turns *there is a question* into a
    /// turn of the coordinator's own session.
    pub fn start(
        session: &str,
        bell: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Coordinator, String> {
        let socket = super::socket_path().ok_or_else(|| "no room socket path".to_string())?;
        let lease_path = super::lease_path().ok_or_else(|| "no room lease path".to_string())?;
        let lease = CoordinatorLease::new(lease_path);
        // The lease first, so a second coordinator is refused before it binds anything.
        let now = letibot_room::server::now_ms();
        match lease.acquire(now) {
            Ok(Acquired::Fresh) => {}
            Ok(Acquired::TookOver { from, why }) => {
                eprintln!("harnessd: took the room's lease from {} — {why}", from)
            }
            Err(held) => return Err(held.sentence()),
        }
        let server = Server::serve(&socket, Room::new()).map_err(|e| {
            let _ = lease.release();
            format!("binding the room's socket at {}: {e}", socket.display())
        })?;
        if let Some(chat) = super::chat_path() {
            server.with_chat_store(&chat);
        }
        if let Some(items) = super::items_path() {
            server.with_item_store(&items);
        }
        server.with_arbiter(session).with_lease(lease.clone());
        eprintln!("harnessd: {}", server.identity());

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tie_breaker = spawn_tie_breaker(&server, session, &bell, &stop);
        Ok(Coordinator {
            server,
            lease,
            socket,
            stop,
            tie_breaker,
        })
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    /// The flag that ends this coordinator's own threads - the tie-breaker's, and the tag
    /// agents' upkeep beside it. Handed out rather than duplicated, so a daemon that started a
    /// thread on the coordinator's behalf stops it with the coordinator.
    pub fn stop_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.stop)
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.server.stop();
        let _ = self.lease.release();
        if let Some(h) = self.tie_breaker.take() {
            // Not joined: the thread's next act is to look at the flag and leave, and a daemon's
            // shutdown must not wait on a two-second sleep.
            let _ = h;
        }
    }
}

fn spawn_tie_breaker(
    server: &Server,
    session: &str,
    bell: &Arc<dyn Fn(&str) + Send + Sync>,
    stop: &Arc<std::sync::atomic::AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let server = server.clone();
    let session = session.to_string();
    let bell = Arc::clone(bell);
    let stop = Arc::clone(stop);
    std::thread::Builder::new()
        .name("room-tie-breaker".into())
        .spawn(move || {
            let mut asked: BTreeSet<u64> = BTreeSet::new();
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(TIE_BREAKER_EVERY_MS));
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let open = server.open_questions();
                // A question that is answered (or gone) is forgotten, so the set cannot grow for
                // ever and a question re-asked under a new id is prompted again.
                let live: BTreeSet<u64> = open.iter().map(|q| q.id).collect();
                asked.retain(|id| live.contains(id));
                for q in open {
                    if !asked.insert(q.id) {
                        continue;
                    }
                    super::wake::push(&session, tie_breaker_prompt(&q));
                    bell(&session);
                }
            }
        })
        .ok()
}

/// The question, in the arbiter's own shape - a sentence with the decision in it, and the typed
/// facts beside it. Never a dashboard: a model handed a table answers with a paragraph about it.
pub fn tie_breaker_prompt(q: &letibot_room::Question) -> String {
    format!(
        "[room] question #{id} is yours to rule on.\n\n{text}\n\nfacts: {facts}\n\n\
         Answer with the `room` tool: verb `rule`, `question` {id}, and one of `decision: \
         \"retry\"` (offer it again, the count kept), `decision: \"hand\"` with `to` (a top-level \
         session the room has met) or `decision: \"drop\"` with `why`. Nothing moves until you \
         answer, and every claimant behind it waits.",
        id = q.id,
        text = q.text,
        facts = serde_json::to_string_pretty(&q.facts).unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_room::client::Client;
    use letibot_room::{Request, Response, Speaker};

    /// One test at a time for anything that sets `LETIBOT_ROOM_SOCKET` - it is process-wide and
    /// the harness runs a module's tests in parallel, so two tests would read each other's answer.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn the_default_model_is_deepseek_and_it_is_spelled_provider_slash_model() {
        assert_eq!(DEEPSEEK, "deepseek/deepseek-flash");
        assert!(
            DEEPSEEK.split_once('/').is_some(),
            "a bare alias would mean a local model, and the default is a provider"
        );
    }

    #[test]
    fn a_coordinator_with_no_store_and_no_project_is_configured_by_name() {
        let _env = env_lock();
        let mut cfg = Config::for_this_box(std::path::PathBuf::from("/tmp"));
        // A socket somewhere this test owns, so nothing touches the box's data directory.
        let dir = std::env::temp_dir().join(format!("letibot-room-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("room.sock");
        // SAFETY: single-threaded within this test's own process region, and the variable is read
        // once by `socket_path` before the test's first assertion.
        unsafe { std::env::set_var(super::super::SOCKET_ENV, &socket) };

        let outcome = configure(&mut cfg);
        unsafe { std::env::remove_var(super::super::SOCKET_ENV) };
        // The model check is the box's own - this test asserts the SHAPE, not whether a key exists.
        if let Err(why) = outcome {
            assert!(
                why.contains("no model to arbitrate with"),
                "a refusal must be the model's, said by name: {why}"
            );
            return;
        }
        assert!(cfg.room);
        assert_eq!(cfg.store, None, "the room keeps no session store");
        assert_eq!(cfg.session_id, super::super::session_id());
        assert_ne!(
            cfg.socket, socket,
            "the daemon's own socket is not the room's: the room's bind would take it away"
        );
        assert_eq!(
            cfg.socket
                .file_name()
                .map(|n| n.to_string_lossy().to_string()),
            Some("daemon-room.sock".to_string()),
            "beside it, named so a reader can tell which is which"
        );
        assert!(!cfg.allow_bash);
        assert!(!cfg.serve_queues);
        assert_eq!(cfg.seat, Seat::Orchestrator);
        // And the arbiter's own session can reach the room it arbitrates. Without this the seat
        // names `room` and the tool behind it is a `NoRoom`: the tie-breaker is woken, told to call
        // a tool it cannot use, and a raised item stays frozen.
        assert!(
            cfg.room_link,
            "a coordinator's own session must join the room: the arbiter rules through it"
        );
        assert!(cfg.workspace.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tie_breaker_is_asked_a_question_with_the_decision_in_it() {
        let q = letibot_room::Question {
            id: 7,
            asked_ms: 1,
            about: letibot_room::About::ItemRaised {
                item: 3,
                tag: "ram".to_string(),
            },
            text: "Item #3 under `ram` has abandoned 3 claimants: … Decide: Retry, Hand{to} or \
                   Drop{why}."
                .to_string(),
            project: String::new(),
            facts: letibot_room::Facts {
                item: 3,
                tag: "ram".to_string(),
                body: "8 GB".to_string(),
                posted_by: "s-a".to_string(),
                posted_ms: 1,
                claimed_by: None,
                claim_ms: None,
                beat_ms: None,
                abandonments: Vec::new(),
                waiting: Vec::new(),
            },
            answer: None,
        };
        let prompt = tie_breaker_prompt(&q);
        for wanted in [
            "question #7",
            "abandoned 3 claimants",
            "facts:",
            "\"item\": 3",
            "`decision: \"retry\"`",
            "Nothing moves until you answer",
        ] {
            assert!(
                prompt.contains(wanted),
                "the prompt must say {wanted}: {prompt}"
            );
        }
    }

    /// The coordinator's whole runtime, on a socket of its own: the lease, the server, the
    /// arbiter, and a real client ruling through it. No daemon, no store, no model.
    #[test]
    fn a_coordinator_serves_a_question_to_its_arbiter_and_takes_the_ruling() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("letibot-room-coord-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("room.sock");
        unsafe { std::env::set_var(super::super::SOCKET_ENV, &socket) };
        let arbiter = super::super::session_id();
        let rung = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bell = {
            let rung = Arc::clone(&rung);
            Arc::new(move |_: &str| {
                rung.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        let coord = Coordinator::start(&arbiter, bell).expect("a coordinator");
        assert!(socket.exists(), "the socket is bound");

        // Two sessions: one posts and abandons three times, which raises the item.
        let wake = Arc::new(|_: &letibot_room::Event| {});
        let a = Client::connect(&socket, Speaker::top_level("s-a"), wake.clone()).unwrap();
        let b = Client::connect(&socket, Speaker::top_level("s-b"), wake).unwrap();
        let _ = b;
        let posted = a
            .request(&Request::Post {
                id: "p1".into(),
                tag: "ram".into(),
                body: "8 GB".into(),
            })
            .unwrap();
        assert!(matches!(posted, Response::Posted { .. }));
        for k in 1..=3 {
            let _ = a.request(&Request::Claim {
                id: format!("c{k}"),
                tag: "ram".into(),
            });
            let _ = a.request(&Request::Abandon {
                id: format!("a{k}"),
                item: 1,
                why: "the suite blew up".into(),
            });
        }
        let questions = coord.server().open_questions();
        assert_eq!(questions.len(), 1, "three abandonments raise one question");

        // The tie-breaker's own door: the prompt it is given, and the ruling it makes.
        let prompt = tie_breaker_prompt(&questions[0]);
        assert!(prompt.contains("question #1"), "{prompt}");
        let ruling = a
            .request(&Request::Rule {
                project: None,
                id: "r1".into(),
                question: questions[0].id,
                decision: letibot_room::Decision::Drop {
                    why: "three claimants died on it".into(),
                },
            })
            .unwrap();
        assert!(matches!(ruling, Response::Ruled { .. }), "{ruling:?}");
        assert!(coord.server().open_questions().is_empty());
        drop(coord);
        // An orderly stop gives the lease back, so the next coordinator is a FRESH start and
        // not a takeover of a dead one.
        let lease = CoordinatorLease::new(super::super::lease_path().unwrap());
        assert_eq!(lease.read(), None);
        unsafe { std::env::remove_var(super::super::SOCKET_ENV) };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
