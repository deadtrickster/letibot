//! A round's tool calls through the harness itself: adjacent reads run at once, rows keep the
//! model's order, a failing call costs only its own row, and `[tools] parallel = false` puts every
//! call back on its own.
//!
//! The concurrency is proved by a rendezvous, not by a clock: a read that does not run beside its
//! neighbours never gets past it, and the wait is bounded so the failure is a sentence rather than
//! a hung test on a loaded box.

use std::sync::Condvar;

use letibot_tools::runtime::{Invocation, InvokeCtx};
use letibot_tools::schema::{Access, ToolSchema};
use serde_json::Value;

use super::*;

/// How long the rendezvous waits before it says the round did not pool. Generous on purpose: the
/// only cost of a generous bound is how long a RED test takes, and the gate runs at no priority.
const PATIENCE: Duration = Duration::from_secs(30);

#[derive(Default)]
struct State {
    arrived: usize,
    /// Whose turn it is to answer, counting down from the number of calls: the LAST call answers
    /// first, so finishing order is the reverse of the order the rows end up in.
    serving: usize,
    /// The calls as they answered.
    turned: Vec<usize>,
}

/// The barrier a pooled round clears and a serial round cannot.
struct Rendezvous {
    calls: usize,
    state: Mutex<State>,
    cv: Condvar,
}

impl Rendezvous {
    fn of(calls: usize) -> Arc<Self> {
        Arc::new(Rendezvous {
            calls,
            state: Mutex::new(State {
                serving: calls,
                ..State::default()
            }),
            cv: Condvar::new(),
        })
    }

    /// Hold the lock until `ready`, or panic with `why`. A panic inside a tool is caught by the
    /// runtime and becomes that call's own failed row, which is how a round that did not pool reads
    /// instead of hanging.
    fn until<F>(
        &self,
        s: std::sync::MutexGuard<'_, State>,
        ready: F,
        why: String,
    ) -> std::sync::MutexGuard<'_, State>
    where
        F: Fn(&State) -> bool,
    {
        let deadline = Instant::now() + PATIENCE;
        let mut s = s;
        while !ready(&s) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                panic!("{why}");
            }
            s = self
                .cv
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        s
    }

    /// One call, by its position in the round (1-based): everybody arrives before anybody answers,
    /// and the calls then answer from the last to the first.
    fn meet(&self, n: usize) {
        let calls = self.calls;
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.arrived += 1;
        self.cv.notify_all();
        let alone = format!(
            "only {} of {calls} reads were running at once, so this round is not pooled",
            s.arrived
        );
        s = self.until(s, |s| s.arrived == calls, alone);
        let stuck = format!(
            "call {n} never got its turn: {} of {calls} had answered",
            s.turned.len()
        );
        s = self.until(s, |s| s.serving == n, stuck);
        s.turned.push(n);
        s.serving -= 1;
        self.cv.notify_all();
    }
}

/// A read-class tool named `name`: it meets the rendezvous, or answers at once, or panics.
struct Call {
    name: &'static str,
    n: usize,
    at: Option<Arc<Rendezvous>>,
    panics: bool,
}

impl Call {
    fn read(name: &'static str, n: usize, at: Option<Arc<Rendezvous>>) -> Box<dyn Tool> {
        Box::new(Call {
            name,
            n,
            at,
            panics: false,
        })
    }

    fn panicking(name: &'static str) -> Box<dyn Tool> {
        Box::new(Call {
            name,
            n: 0,
            at: None,
            panics: true,
        })
    }
}

impl Tool for Call {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            self.name,
            "Read something for a round's test: it meets the others, or fails.",
            serde_json::json!({"type": "object", "properties": {"tag": {"type": "string"}}}),
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        if self.panics {
            panic!("the read this test broke on purpose");
        }
        if let Some(at) = &self.at {
            at.meet(self.n);
        }
        let tag = args.get("tag").and_then(Value::as_str).unwrap_or("");
        Invocation::ok(tag.to_string())
    }
}

/// A session whose registry holds `tools`, opened with `[tools] parallel` as the file would say it.
fn a_session(tag: &str, parallel: Option<bool>, tools: Vec<Box<dyn Tool>>) -> Harness {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("parallel-reads-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = format!("s-par-{tag}");
    at_always_ask(&mut cfg);
    cfg.vocab_gguf = None;
    cfg.leticode.tools.parallel = parallel;
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let mut h = Harness::open(&parts, cfg.clone(), Hub::new(&cfg.session_id)).expect("opens");
    let rt = Arc::get_mut(&mut h.runtime).expect("the runtime is this session's alone at open");
    for t in tools {
        rt.registry.register(t).expect("registers");
    }
    h
}

fn call(id: usize, name: &str) -> ToolCall {
    ToolCall {
        id: format!("c{id}"),
        name: name.into(),
        arguments: format!(r#"{{"tag":"c{id}"}}"#),
    }
}

/// The rows a round appended, as the call each answers, the payload and whether it succeeded.
fn rows(results: &[TranscriptItem]) -> Vec<(String, String, bool)> {
    results
        .iter()
        .map(|i| {
            let TranscriptItem::ToolResult {
                call_id,
                outcome,
                payload,
                ..
            } = i
            else {
                panic!("a round's call answers with a tool row, not {i:?}");
            };
            (
                call_id.clone(),
                payload.clone(),
                matches!(outcome, letibot_transcript::ToolOutcome::Ok),
            )
        })
        .collect()
}

fn progress() -> crate::progress::ProgressDetector {
    crate::progress::ProgressDetector::new(3)
}

/// Plan item 1.2 through the loop it belongs to: three reads of one round run at once, and the rows
/// come back in the order the model proposed them rather than the order they finished.
#[test]
fn a_rounds_reads_run_at_once_and_their_rows_keep_call_order() {
    let at = Rendezvous::of(3);
    let mut h = a_session(
        "pooled",
        None,
        vec![
            Call::read("par_a", 1, Some(at.clone())),
            Call::read("par_b", 2, Some(at.clone())),
            Call::read("par_c", 3, Some(at.clone())),
        ],
    );
    let calls = vec![call(1, "par_a"), call(2, "par_b"), call(3, "par_c")];
    assert!(
        h.runtime.runs_pooled(&calls[0]),
        "a read runs beside its neighbours unless the config says otherwise"
    );
    assert_eq!(h.runtime.batches(&calls), vec![0..3], "one batch of three");
    let (results, backgrounded) = h.run_the_rounds_calls(&calls, "t1", &mut progress());
    assert!(!backgrounded);
    let rows = rows(&results);
    let ids: Vec<&str> = rows.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2", "c3"], "the rows are in call order");
    for (id, payload, ok) in &rows {
        assert!(ok, "{id} answered {payload:?}");
        assert!(payload.contains(id), "{id}'s row says {payload:?}");
    }
    let s = at.state.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        s.turned,
        [3, 2, 1],
        "the calls answered in the reverse of the call order, so the row order is the loop's"
    );
}

/// A call that panics is answered as its own failure, and the neighbours it ran beside keep their
/// results: the round is one pool, not one attempt.
#[test]
fn a_panicking_read_costs_only_its_own_row() {
    let mut h = a_session(
        "panic",
        None,
        vec![
            Call::read("par_a", 1, None),
            Call::panicking("par_boom"),
            Call::read("par_c", 3, None),
        ],
    );
    let calls = vec![call(1, "par_a"), call(2, "par_boom"), call(3, "par_c")];
    let (results, _) = h.run_the_rounds_calls(&calls, "t1", &mut progress());
    let rows = rows(&results);
    assert_eq!(rows.len(), 3, "{rows:#?}");
    assert_eq!(rows[0].0, "c1");
    assert!(rows[0].2, "c1's row is its answer: {rows:?}");
    assert_eq!(rows[1].0, "c2");
    assert!(!rows[1].2, "the panic is a failure: {rows:?}");
    assert!(
        rows[1].1.contains("panicked"),
        "the failure names what happened: {:?}",
        rows[1].1
    );
    assert_eq!(rows[2].0, "c3");
    assert!(rows[2].2, "the neighbour ran: {rows:?}");
}

/// `[tools] parallel = false` is the whole off switch: nothing is pooled, every call still runs,
/// and the rows are the model's order.
#[test]
fn with_parallel_off_every_call_of_a_round_runs_alone() {
    let mut h = a_session(
        "serial",
        Some(false),
        vec![
            Call::read("par_a", 1, None),
            Call::read("par_b", 2, None),
            Call::read("par_c", 3, None),
        ],
    );
    let calls = vec![call(1, "par_a"), call(2, "par_b"), call(3, "par_c")];
    assert!(
        !h.runtime.runs_pooled(&calls[0]),
        "`parallel = false` leaves nothing for the pool"
    );
    assert_eq!(
        h.runtime.batches(&calls),
        vec![0..1, 1..2, 2..3],
        "one call per batch"
    );
    let (results, _) = h.run_the_rounds_calls(&calls, "t1", &mut progress());
    let rows = rows(&results);
    let ids: Vec<&str> = rows.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2", "c3"]);
    assert!(rows.iter().all(|(_, _, ok)| *ok), "{rows:#?}");
}
