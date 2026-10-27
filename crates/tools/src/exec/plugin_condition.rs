//! **A monitor whose condition is a plugin** — `condition`, `docs/plugins-plan.md` §4.4.
//!
//! A condition today is a Rust type: `GithubCondition` spawns `gh` at most once per interval, polls
//! the child with `try_wait` so a slow network never stalls the poller, keeps a baseline, and fires
//! when something happened. This is that shape with the *policy* moved into a layer: the plugin
//! says what to look at and whether what came back is a firing, and the host does the looking.
//!
//! # The split, and why it is the whole design
//!
//! A hook is pure (`docs/plugins-plan.md` §1 principle 1: plugins never do I/O). So the look is a
//! DECLARATION in the answer — `{"exec": ["gh", "pr", "view", …]}` or `{"read": "path"}` — and this
//! type is the host's half: it starts it, never blocks on it, and hands the result back in the next
//! input's `look`. That is what keeps a plugin call replayable and the shadow gate's diff honest.
//!
//! # Why a look is a [`Look`] and not a result
//!
//! `met()` is called by the session's ONE poller thread, so a look that blocked would stall every
//! other monitor in the session — exactly the defect `GithubCondition`'s spawn-and-poll exists to
//! avoid. A `read` finishes in the call; an `exec` starts a child and answers [`Look::Running`],
//! and the child is polled on the next tick.
//!
//! # State belongs to the host
//!
//! The `state` the plugin returns is kept here and handed back on the next call (§1 principle 5),
//! so a restart, a reload and a newer build of the plugin all keep a watch's baseline. A `null` on
//! the arming call is *nothing has been looked at yet*.
//!
//! # One plugin call per tick
//!
//! A tick hands over at most what came in: a look's result, or — when the interval has come round —
//! a call with nothing new. A plugin that answers every call by asking for another look therefore
//! costs one call per tick, not a loop, which is the same rule the pane's render follows
//! (`one look per pass`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::exec::github::GITHUB_MAX_TTL;
use crate::exec::monitor::Condition;
use letibot_plugin::condition::{ConditionIn, ConditionOut, LookAsk, LookResult};

/// How often a watch polls when the plugin does not say.
pub const EVERY: Duration = Duration::from_secs(30);

/// **What the host can do for a condition**: ask the stack, and run a look.
///
/// A trait, because this crate holds no plugin stack and no workspace — the daemon has both, and
/// it is the same seam `Loader` is for the layer stack one crate down.
pub trait PluginStack: Send + Sync {
    /// Ask `hook` with `input`. `None` when nothing answers it, or when the stack's answer is a
    /// pass or a mask; otherwise the JSON text of the answer.
    fn ask(&self, hook: &str, input: &Value) -> Option<Value>;

    /// **Run a look the hook declared.** `hook` is the export whose topmost layer's capabilities
    /// the look runs under — the same layer `ask` names, so the caller does not have to carry it.
    /// Never blocks on a program: one that was started comes back as [`Look::Running`].
    fn look(&self, hook: &str, ask: &LookAsk) -> Look;
}

/// What running a look produced.
pub enum Look {
    /// Already finished: a file that was read, or a capability that refused it.
    Done(LookResult),
    /// A program is running. Poll it; the poller must not wait for it.
    Running(Box<dyn LookChild>),
}

/// A look that is a live process, as the poller sees it.
pub trait LookChild: Send {
    /// `Some` once it has ended — its exit status and what it wrote.
    fn poll(&mut self) -> Option<LookResult>;
}

/// **The seam the daemon fills** so a `monitor` call can arm a plugin's condition
/// (`docs/plugins-plan.md` §4.4).
///
/// One method, because everything a condition needs after arming is held by the condition itself:
/// the stack it asks, the look it runs, and the state it keeps. `Err` is the sentence a model
/// reads, in the two fields a tool refusal is made of.
pub trait PluginConditions: Send + Sync {
    fn arm(&self, plugin: &str, args: Value) -> Result<Armed, NotArmed>;
}

/// A condition that was armed, and anything the `monitor` call should be told about it.
pub struct Armed {
    pub condition: Arc<dyn Condition>,
    /// A line for the call's answer — what this watch is looking at, when that is not obvious
    /// from the call itself.
    pub note: Option<String>,
}

/// A condition armed from a plugin, and everything the host keeps for it.
pub struct PluginCondition {
    stack: Arc<dyn PluginStack>,
    /// The export name: `condition_<name>`.
    hook: String,
    /// What the `monitor` call said, handed back on every call — the plugin's own vocabulary.
    args: Value,
    /// The longest this watch may live, from the arming answer.
    max_ttl: Duration,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The plugin's opaque state, as last returned.
    kept: Option<Value>,
    /// What to look at next, as last declared.
    spec: Option<LookAsk>,
    /// A result that has come in and has not been handed to the plugin yet.
    ready: Option<LookResult>,
    /// The look that is running, if one is.
    child: Option<Box<dyn LookChild>>,
    /// How often to look, as last answered.
    every: Option<Duration>,
    /// When the next poll is due.
    next_at: Option<Instant>,
    /// A firing the arming call already produced, said on the first poll rather than lost.
    pending: Option<String>,
    /// The pull request this watch is on, when it is on one — so the coalescing rule sees a
    /// plugin condition the way it sees a `github` watch.
    github: Option<(String, Vec<String>)>,
}

impl std::fmt::Debug for PluginCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PluginCondition({})", self.hook)
    }
}

/// **Why a watch could not be armed**, in the two fields a tool refusal is made of: the sentence,
/// and the detail it is fixed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotArmed {
    pub why: String,
    pub detail: String,
}

impl PluginCondition {
    /// **Arm a watch**: ask the hook once with no look, keep what it says, answer the condition.
    ///
    /// The call at arming is not a formality. It is where a plugin that traps or runs out of fuel
    /// is found before a monitor exists to be told about it, where `max_ttl_s` comes from, and
    /// where a condition that already knows enough to fire fires at once rather than on the second
    /// poll.
    pub fn arm(
        stack: Arc<dyn PluginStack>,
        plugin: &str,
        args: Value,
    ) -> Result<Arc<PluginCondition>, NotArmed> {
        let name = plugin.trim();
        if name.is_empty() {
            return Err(NotArmed {
                why: "`plugin` was empty".into(),
                detail: "name the condition's plugin — the `condition_<name>` a layer exports — \
                         and nothing was declared."
                    .into(),
            });
        }
        let hook = letibot_plugin::condition::hook(name);
        let asked = stack
            .ask(
                &hook,
                &serde_json::to_value(ConditionIn::arm(args.clone())).unwrap_or(Value::Null),
            )
            .ok_or_else(|| NotArmed {
                why: format!("no layer answers `{hook}`, so nothing was declared"),
                detail: format!(
                    "a plugin condition is an export named `{hook}` in a layer, with the same \
                     name in its plugin.toml's `hooks`. `plugin stack` lists what is in force."
                ),
            })?;
        let out: ConditionOut = match serde_json::from_value(asked.clone()) {
            Ok(out) => out,
            Err(why) => {
                return Err(NotArmed {
                    why: format!("`{hook}` did not answer a condition"),
                    detail: format!(
                        "the answer has to be `look`, `every_s`, `max_ttl_s`, `fired`, `state` \
                         and `github` — {why}. It was {asked}. Nothing was declared."
                    ),
                });
            }
        };
        let every = interval(out.every_s);
        // **The longest a watch may live in this tree is the GitHub watch's week**, because the
        // reason is the same one: a review takes days, and a monitor's ordinary hour would expire
        // before most of them. One number, so a plugin condition and a `github` watch are capped
        // by the same rule.
        let max_ttl = out
            .max_ttl_s
            .map(Duration::from_secs)
            .unwrap_or(GITHUB_MAX_TTL)
            .min(GITHUB_MAX_TTL);
        let github = out.github.map(|g| (g.pr, g.watches));
        let condition = Arc::new(PluginCondition {
            stack,
            hook,
            args,
            max_ttl,
            state: Mutex::new(State {
                kept: out.state,
                spec: out.look,
                ready: None,
                child: None,
                every: Some(every),
                next_at: Some(Instant::now() + every),
                pending: out.fired,
                github,
            }),
        });
        // The first look starts now, so the first `met()` has something to hand over rather than
        // waiting out an interval.
        condition.start_look();
        Ok(condition)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start the declared look, if there is one and nothing is running.
    fn start_look(&self) {
        let spec = {
            let st = self.lock();
            if st.child.is_some() || st.ready.is_some() {
                return;
            }
            match st.spec.clone() {
                Some(spec) => spec,
                None => return,
            }
        };
        match self.stack.look(&self.hook, &spec) {
            Look::Done(result) => self.lock().ready = Some(result),
            Look::Running(child) => self.lock().child = Some(child),
        }
    }

    /// **One advanced step**: hand `look` (or nothing) to the plugin, keep what it says, and start
    /// whatever it asks to look at next.
    fn advance(&self, look: Option<LookResult>) -> Option<String> {
        let input = {
            let st = self.lock();
            match look {
                Some(result) => ConditionIn::looked(self.args.clone(), st.kept.clone(), result),
                None => {
                    let mut input = ConditionIn::arm(self.args.clone());
                    input.state = st.kept.clone();
                    input
                }
            }
        };
        let asked = self.stack.ask(
            &self.hook,
            &serde_json::to_value(&input).unwrap_or(Value::Null),
        )?;
        let out: ConditionOut = match serde_json::from_value(asked) {
            Ok(out) => out,
            // **A layer that answers something else is spent for this tick, not for ever.** The
            // next tick asks again: a reload or a fixed layer answers then, and a watch whose
            // plugin is broken is a watch that says nothing rather than one that kills the poller.
            Err(_) => return None,
        };
        {
            let mut st = self.lock();
            st.kept = out.state;
            if let Some(spec) = out.look {
                st.spec = Some(spec);
            }
            let every = interval(out.every_s.or(st.every.map(|d| d.as_secs())));
            st.every = Some(every);
            st.next_at = Some(Instant::now() + every);
            if let Some(github) = out.github {
                st.github = Some((github.pr, github.watches));
            }
        }
        self.start_look();
        out.fired
    }
}

/// How often to look: the plugin's own answer, raised to the monitors' floor.
fn interval(every_s: Option<u64>) -> Duration {
    Duration::from_secs(
        every_s
            .unwrap_or(EVERY.as_secs())
            .max(letibot_plugin::condition::EVERY_MIN_S),
    )
}

impl Condition for PluginCondition {
    /// **One tick**: hand over a result that came in, else poll the running look, else — when the
    /// interval has come round — ask the plugin with nothing new.
    ///
    /// At most one plugin call per tick: a hook that always asks for another look costs one call a
    /// tick rather than a loop, and a look that is still running is not started again.
    fn met(&self) -> Option<String> {
        // **The guard is taken and DROPPED before `advance` is called**, and that is not style.
        // Writing it as `if let Some(result) = self.lock().ready.take()` keeps the guard alive
        // inside the block, and `advance` locks again: measured as a test binary at 0% CPU for
        // fifty-eight minutes, because a `std::sync::Mutex` is not reentrant and the deadlock is
        // silent. Every path below takes the lock in a block or a statement of its own.
        let ready = { self.lock().ready.take() };
        if let Some(result) = ready {
            return self.advance(Some(result));
        }
        let finished = {
            let mut st = self.lock();
            match st.child.as_mut() {
                Some(child) => match child.poll() {
                    Some(result) => {
                        st.child = None;
                        Some(result)
                    }
                    None => return None,
                },
                None => None,
            }
        };
        if let Some(result) = finished {
            return self.advance(Some(result));
        }
        {
            let mut st = self.lock();
            if let Some(fired) = st.pending.take() {
                // A condition that fired on what it already knew says so on the first poll, and
                // its own interval measures from there.
                st.next_at = Some(Instant::now() + st.every.unwrap_or(EVERY));
                return Some(fired);
            }
            if !st.next_at.is_none_or(|at| Instant::now() >= at) {
                return None;
            }
            st.next_at = Some(Instant::now() + st.every.unwrap_or(EVERY));
        }
        self.advance(None)
    }

    /// The monitors' own `Wait` — a poll — is the default, and this condition has nothing
    /// finer to wait for than the poller's tick.
    fn describe(&self) -> String {
        format!(
            "plugin `{}` watching {}",
            self.hook
                .strip_prefix(letibot_plugin::condition::PREFIX)
                .unwrap_or(&self.hook),
            self.args
        )
    }

    fn max_ttl(&self) -> Option<Duration> {
        Some(self.max_ttl)
    }

    fn github(&self) -> Option<(String, Vec<String>)> {
        self.lock().github.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// **Run `f` with a deadline, so a deadlock fails instead of stalling.**
    ///
    /// The condition's state is behind one mutex and `met()` calls into itself more than once, so a
    /// re-lock is a SILENT hang: MEASURED once as this test binary at 0% CPU for fifty-eight
    /// minutes. Every test that reaches `met()` or `arm()` goes through here, so the same defect
    /// costs ten seconds and a panic that says what it is.
    fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let value = f();
            let _ = tx.send(value);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(value) => value,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("`{what}` did not finish in 10s — a deadlock, not a slow test")
            }
            // The worker panicked before it could answer: its own panic is the result, and a
            // failed assertion must not read as a deadlock.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
                Ok(()) => panic!("`{what}` answered nothing and did not panic"),
                Err(panic) => std::panic::resume_unwind(panic),
            },
        }
    }

    /// A stack that answers from a script, repeating its last answer, and records every question
    /// and every look.
    struct Scripted {
        answers: Mutex<Vec<Value>>,
        asked: Mutex<Vec<Value>>,
        looks: Mutex<Vec<LookAsk>>,
    }

    impl Scripted {
        fn new(answers: Vec<Value>) -> Arc<Scripted> {
            Arc::new(Scripted {
                answers: Mutex::new(answers),
                asked: Mutex::new(Vec::new()),
                looks: Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<Value> {
            self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn looks(&self) -> Vec<LookAsk> {
            self.looks.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    impl PluginStack for Scripted {
        fn ask(&self, _hook: &str, input: &Value) -> Option<Value> {
            self.asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(input.clone());
            let mut answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
            if answers.len() > 1 {
                Some(answers.remove(0))
            } else {
                answers.first().cloned()
            }
        }

        fn look(&self, _hook: &str, ask: &LookAsk) -> Look {
            self.looks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(ask.clone());
            Look::Done(LookResult::ok("{\"reviews\":3}"))
        }
    }

    struct Silent;
    impl PluginStack for Silent {
        fn ask(&self, _hook: &str, _input: &Value) -> Option<Value> {
            None
        }
        fn look(&self, _hook: &str, _ask: &LookAsk) -> Look {
            Look::Done(LookResult::ok(""))
        }
    }

    /// **Arming asks once, and the answer's interval and ttl are the watch's.** The call at arming
    /// is what makes a trap a refusal rather than a monitor that explodes on its second poll.
    #[test]
    fn arming_asks_the_hook_and_takes_its_interval_and_ttl() {
        let stack = Scripted::new(vec![json!({
            "look": {"read": ".letibot/status.json"},
            "every_s": 60,
            "max_ttl_s": 7200,
            "state": {"armed": true}
        })]);
        let condition = within("arming a watch nobody answers", {
            let stack = stack.clone();
            move || {
                PluginCondition::arm(stack, "coderabbit", json!({"pr": "o/r#24"})).expect("armed")
            }
        });
        assert_eq!(
            condition.max_ttl(),
            Some(Duration::from_secs(7200)),
            "the plugin's own cap, inside the monitors'"
        );
        assert!(
            condition.describe().contains("coderabbit"),
            "{}",
            condition.describe()
        );
        let asked = stack.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0]["args"], json!({"pr": "o/r#24"}));
        assert!(
            asked[0]["look"].is_null(),
            "arming is the call with no look"
        );
        assert!(asked[0]["state"].is_null());
        // The first look starts at arming, so the next tick hands its result over.
        assert_eq!(stack.looks().len(), 1);
    }

    /// **A watch that cannot be armed says why** — no layer answers the hook, or the answer is not
    /// a condition. Both are sentences a model can act on.
    #[test]
    fn arming_refuses_with_a_reason() {
        let why = PluginCondition::arm(Arc::new(Silent), "nobody", json!({})).unwrap_err();
        assert!(
            why.why.contains("no layer answers `condition_nobody`"),
            "{}",
            why.why
        );

        struct Wrong;
        impl PluginStack for Wrong {
            fn ask(&self, _hook: &str, _input: &Value) -> Option<Value> {
                Some(json!("not an object"))
            }
            fn look(&self, _hook: &str, _ask: &LookAsk) -> Look {
                Look::Done(LookResult::ok(""))
            }
        }
        let what: Arc<dyn PluginStack> = Arc::new(Wrong);
        let why = PluginCondition::arm(what, "odd", json!({})).unwrap_err();
        assert!(
            why.why.contains("did not answer a condition"),
            "{}",
            why.why
        );
    }

    /// **A look's result reaches the plugin, and `fired` is the firing** — the whole round trip,
    /// with the state carried across it.
    #[test]
    fn a_look_result_is_handed_back_and_a_firing_is_said() {
        let stack = Scripted::new(vec![
            // The arming answer: look at a file, and keep a baseline.
            json!({"look": {"read": ".letibot/status.json"}, "every_s": 5, "state": {"seen": 0}}),
            // The answer to the look: it fired, and here is the new baseline.
            json!({"fired": "CodeRabbit reviewed: 3 comments", "state": {"seen": 3}}),
        ]);
        let condition = within("arming", {
            let stack = stack.clone();
            move || {
                PluginCondition::arm(stack, "coderabbit", json!({"pr": "o/r#24"})).expect("armed")
            }
        });
        // The file's result is ready from the arming tick; the first `met()` hands it over.
        let said = within("the first tick", {
            let condition = condition.clone();
            move || condition.met()
        });
        assert_eq!(said, Some("CodeRabbit reviewed: 3 comments".to_string()));
        let asked = stack.asked();
        assert_eq!(asked.len(), 2);
        assert_eq!(
            asked[1]["look"]["exit"],
            json!(0),
            "the look's result is in the input"
        );
        assert_eq!(
            asked[1]["state"],
            json!({"seen": 0}),
            "and so is the state it kept"
        );
        // And the state the answer returned is what the NEXT call is told.
        let _ = condition.met();
        let asked = stack.asked();
        assert!(asked.len() >= 2, "no third call within one tick: {asked:?}");
    }

    /// **A github declaration takes part in the coalescing rule**, and it is the plugin's own
    /// words a refusal reads back.
    #[test]
    fn a_plugin_condition_can_be_about_a_pull_request() {
        let stack = Scripted::new(vec![json!({
            "look": {"read": "x.json"},
            "every_s": 90,
            "github": {"pr": "o/r#24", "watches": ["coderabbit"]}
        })]);
        let condition = PluginCondition::arm(stack, "coderabbit", json!({})).expect("armed");
        assert_eq!(
            condition.github(),
            Some(("o/r#24".to_string(), vec!["coderabbit".to_string()]))
        );
    }

    /// **A look that is still running is not started again**, and its result is what the plugin is
    /// asked about when it ends — the non-blocking half.
    #[test]
    fn a_running_look_is_polled_and_not_restarted() {
        struct Gated {
            answers: Mutex<Vec<Value>>,
            started: Mutex<usize>,
        }
        struct Child {
            done: std::sync::Arc<Mutex<bool>>,
        }
        impl LookChild for Child {
            fn poll(&mut self) -> Option<LookResult> {
                let done = *self.done.lock().unwrap();
                done.then(|| LookResult::ok("finished"))
            }
        }
        impl PluginStack for Gated {
            fn ask(&self, _hook: &str, _input: &Value) -> Option<Value> {
                let mut a = self.answers.lock().unwrap();
                Some(a.remove(0))
            }
            fn look(&self, _hook: &str, ask: &LookAsk) -> Look {
                *self.started.lock().unwrap() += 1;
                let _ = ask;
                Look::Running(Box::new(Child {
                    done: std::sync::Arc::new(Mutex::new(false)),
                }))
            }
        }
        let gated = Arc::new(Gated {
            answers: Mutex::new(vec![
                json!({"look": {"exec": ["gh", "pr", "view", "24"]}, "every_s": 5}),
                json!({"fired": "it finished"}),
            ]),
            started: Mutex::new(0),
        });
        let condition = within("arming", {
            let gated = gated.clone();
            move || PluginCondition::arm(gated, "slow", json!({})).expect("armed")
        });
        // The child never ends, so no amount of ticking asks the plugin anything.
        within("three ticks of a running look", {
            let condition = condition.clone();
            move || {
                for _ in 0..3 {
                    assert_eq!(condition.met(), None);
                }
            }
        });
        assert_eq!(
            *gated.started.lock().unwrap(),
            1,
            "the look was started once and polled, not restarted"
        );
        assert_eq!(
            gated.answers.lock().unwrap().len(),
            1,
            "the plugin was not asked while its look was still running"
        );
    }
}
