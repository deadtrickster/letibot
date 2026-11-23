//! End to end, on the byte vocabulary: with the shadow's during job on, its verified pick is
//! the round's skill offer and similarity offers nothing; a child session is offered no skills.

use super::*;
use crate::shadow::{self, Job, runner};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROMPT: &str = "make latex beamer slides for the offsite talk presentation";

/// Two rounds that read a file, the second slow enough for the during verdict to land, then an
/// answer. Every request's tail is kept.
struct Reads {
    tails: Arc<Mutex<Vec<String>>>,
}

impl letibot_backend::MessagesBackend for Reads {
    fn caps(&self) -> letibot_backend::BackendCaps {
        letibot_backend::BackendCaps::METERED_API
    }
    fn name(&self) -> &str {
        "deepseek"
    }
    fn model(&self) -> &str {
        "fake"
    }
    fn authority(&self) -> String {
        "a fake provider".into()
    }
    fn complete(
        &self,
        req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        let round = {
            let mut t = self.tails.lock().unwrap();
            t.push(format!("{:?}", req.tail));
            t.len()
        };
        if round == 2 {
            // The stub's job here is to be slow: the shadow's during job runs on its own thread and
            // its verdict has to land before this round returns. Nothing in the stub can see it.
            std::thread::sleep(Duration::from_millis(500));
        }
        let tool_calls = match round {
            1 | 2 => vec![ToolCall {
                id: format!("call_{round}"),
                name: "read".into(),
                arguments: r#"{"path":"talk.md"}"#.into(),
            }],
            _ => Vec::new(),
        };
        Ok(letibot_backend::Completion {
            // Round one states the plan, as a real turn does: during checks work against a plan.
            text: match round {
                1 => "I will read talk.md and then make latex beamer slides for the talk.".into(),
                _ if tool_calls.is_empty() => "done".into(),
                _ => String::new(),
            },
            reasoning: String::new(),
            tool_calls,
            finish: letibot_backend::Finish::Stop,
            cost: letibot_backend::TurnCost {
                meter: letibot_backend::Meter::Metered,
                prompt_tokens: 10,
                cached_tokens: 0,
                generated_tokens: 1,
                wall_ms: 1,
                micros_usd: None,
            },
            raw_usage: None,
            thoughts: Vec::new(),
        })
    }
}

struct Judges(&'static str);

impl shadow::judge::Judge for Judges {
    fn label(&self) -> String {
        "stub/judge".into()
    }
    fn judge(&self, _: &str, _: &str) -> Result<String, String> {
        Ok(self.0.into())
    }
}

#[derive(Default)]
struct Rows(Mutex<Vec<String>>);

impl shadow::record::Recorder for Rows {
    fn row(&self, title: &str, _body: &str) {
        self.0.lock().unwrap().push(title.to_string());
    }
}

/// A session whose workspace holds the slides skill, with the during job answering `reply`
/// when `reply` is given, and the tails its requests carried.
fn a_session(
    tag: &str,
    reply: Option<&'static str>,
    child: bool,
) -> (Harness, Arc<Mutex<Vec<String>>>, Arc<Rows>) {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("shadow-during-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let skill = dir.join(".letibot/skills/latex-presentation");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: latex-presentation\ndescription: beamer slides for a talk\n\
         keywords: latex, beamer, slides, talk\n---\none idea to a frame\n",
    )
    .unwrap();
    std::fs::write(dir.join("talk.md"), "the offsite talk outline\n").unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = format!("s-during-{tag}");
    at_always_ask(&mut cfg);
    cfg.vocab_gguf = None;
    if child {
        cfg.parent_session_id = Some("s-parent".into());
    }
    if reply.is_some() {
        cfg.shadow = shadow::config::ShadowConfig::from_lines(
            letibot_provider::keys::ShadowLines {
                enabled: vec!["during".into()],
                ..Default::default()
            },
            "local".into(),
        )
        .unwrap();
    }
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let mut h = Harness::open(&parts, cfg.clone(), Hub::new(&cfg.session_id)).expect("opens");
    let tails = Arc::new(Mutex::new(Vec::new()));
    h.provider = Some(Box::new(Reads {
        tails: tails.clone(),
    }));
    let rows = Arc::new(Rows::default());
    if let Some(reply) = reply {
        let events: shadow::Events = Default::default();
        let ev = events.clone();
        let on = move |job: Job| -> runner::OnOutcome {
            let ev = ev.clone();
            Box::new(move |position, outcome| {
                if job == Job::During && matches!(outcome, runner::Outcome::Verdict(_)) {
                    ev.lock().unwrap().push_back(shadow::Event {
                        job,
                        position,
                        outcome: outcome.clone(),
                    });
                }
            })
        };
        let s = runner::Shadow::start(
            &cfg.shadow,
            "deepseek/fake",
            None,
            rows.clone(),
            &|_, _| Arc::new(Judges(reply)),
            &runner::Timing::of,
            &on,
        )
        .expect("during is on");
        h.shadow = shadow::Slot::On {
            shadow: Arc::new(s),
            events,
            recorder: rows.clone(),
            session_id: shadow::session_id(&cfg.session_id),
        };
    }
    (h, tails, rows)
}

fn offered(tails: &[String]) -> Vec<&String> {
    tails
        .iter()
        .filter(|t| t.contains("[skill offer"))
        .collect()
}

#[test]
fn the_shadows_pick_is_the_offer_and_similarity_says_nothing() {
    let (mut h, tails, rows) = a_session(
        "pick",
        Some(
            r#"{"verdict":"on_plan","offers":[{"name":"latex-presentation","quote":"make latex beamer slides"}]}"#,
        ),
        false,
    );
    h.submit_operator(PROMPT).expect("a turn runs");
    let tails = tails.lock().unwrap();
    assert!(
        !tails[0].contains("[skill offer"),
        "round one: {}",
        tails[0]
    );
    let made = offered(&tails);
    assert_eq!(made.len(), 1, "{tails:?}");
    assert!(made[0].contains("shadow reviewer picked it"), "{}", made[0]);
    assert!(made[0].contains("make latex beamer slides"), "{}", made[0]);
    let titles = rows.0.lock().unwrap();
    assert!(
        titles
            .iter()
            .any(|t| t.starts_with("during · offer · latex-presentation")),
        "{titles:?}"
    );
}

#[test]
fn an_unsure_pick_is_no_offer_while_the_shadow_decides() {
    let (mut h, tails, _) = a_session(
        "unsure",
        Some(
            r#"{"verdict":"unsure","offers":[{"name":"latex-presentation","quote":"make latex beamer slides"}]}"#,
        ),
        false,
    );
    h.submit_operator(PROMPT).expect("a turn runs");
    let tails = tails.lock().unwrap();
    assert!(offered(&tails).is_empty(), "{tails:?}");
}

#[test]
fn with_no_shadow_similarity_offers_and_a_child_is_offered_nothing() {
    let (mut h, tails, _) = a_session("plain", None, false);
    h.submit_operator(PROMPT).expect("a turn runs");
    let made = offered(&tails.lock().unwrap()).len();
    assert_eq!(made, 1, "{:?}", tails.lock().unwrap());
    let (mut h, tails, _) = a_session("child", None, true);
    h.submit_operator(PROMPT).expect("a turn runs");
    assert!(offered(&tails.lock().unwrap()).is_empty());
}
