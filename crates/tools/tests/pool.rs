//! One round's calls: adjacent reads run in a pool, every other call runs alone, and results
//! come back in call order whatever order the pool finished in.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_tools::events::RecordingToolSink;
use letibot_tools::runtime::{Invocation, InvokeCtx, POOL_WIDTH, Tool, ToolRuntime};
use letibot_tools::schema::{Access, ToolSchema};
use letibot_tools::testing::{Harness, writable_harness};
use letibot_transcript::ToolCall;
use serde_json::Value;

/// Each call's name, start and end, as the tools saw them.
type Spans = Arc<Mutex<Vec<(String, Instant, Instant)>>>;

/// A tool that sleeps for `ms` (or the call's own `ms`) and records when it ran.
struct Nap {
    name: &'static str,
    access: Access,
    ms: u64,
    spans: Spans,
}

impl Tool for Nap {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            self.name,
            "Wait, then answer with the call's tag.",
            serde_json::json!({"type": "object", "properties": {
                "tag": {"type": "string"}, "ms": {"type": "integer"}
            }}),
            self.access,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let start = Instant::now();
        let ms = args.get("ms").and_then(Value::as_u64).unwrap_or(self.ms);
        std::thread::sleep(Duration::from_millis(ms));
        let tag = args.get("tag").and_then(Value::as_str).unwrap_or("");
        self.spans
            .lock()
            .unwrap()
            .push((tag.to_string(), start, Instant::now()));
        Invocation::ok(tag.to_string())
    }
}

fn runtime(spans: &Spans) -> Harness {
    let mut h = writable_harness();
    for (name, access) in [("nap_read", Access::Read), ("nap_write", Access::Write)] {
        h.rt.registry
            .register(Box::new(Nap {
                name,
                access,
                ms: 200,
                spans: spans.clone(),
            }))
            .unwrap();
    }
    h
}

fn call(id: usize, name: &str, extra: &str) -> ToolCall {
    ToolCall {
        id: format!("c{id}"),
        name: name.into(),
        arguments: format!(r#"{{"tag":"c{id}"{extra}}}"#),
    }
}

/// The round as the harness runs it: batch by batch, pooled when a batch holds more than one call.
fn run_round(rt: &ToolRuntime, calls: &[ToolCall]) -> Vec<String> {
    let mut sink = RecordingToolSink::default();
    let mut ids = Vec::new();
    for b in rt.batches(calls) {
        let batch = &calls[b];
        let ran = if batch.len() > 1 {
            rt.invoke_pooled("t1", batch, &mut sink)
        } else {
            vec![(rt.invoke("t1", &batch[0], &mut sink), Instant::now())]
        };
        ids.extend(ran.into_iter().map(|(r, _)| r.call_id));
    }
    ids
}

#[test]
fn three_reads_of_200_ms_finish_under_400_ms_in_call_order() {
    let spans = Spans::default();
    let h = runtime(&spans);
    let rt = &h.rt;
    // The first sleeps longest, so finishing order differs from call order.
    let calls = vec![
        call(1, "nap_read", r#","ms":300"#),
        call(2, "nap_read", ""),
        call(3, "nap_read", r#","ms":100"#),
    ];
    assert_eq!(rt.batches(&calls), vec![0..3]);
    let t = Instant::now();
    let ids = run_round(rt, &calls);
    let took = t.elapsed();
    assert!(took < Duration::from_millis(400), "{took:?}");
    assert_eq!(ids, ["c1", "c2", "c3"]);
}

#[test]
fn a_write_between_two_reads_runs_alone() {
    let spans = Spans::default();
    let h = runtime(&spans);
    let rt = &h.rt;
    let calls = vec![
        call(1, "nap_read", ""),
        call(2, "nap_read", ""),
        call(3, "nap_write", ""),
        call(4, "nap_read", ""),
    ];
    assert_eq!(rt.batches(&calls), vec![0..2, 2..3, 3..4]);
    let ids = run_round(rt, &calls);
    assert_eq!(ids, ["c1", "c2", "c3", "c4"]);
    let spans = spans.lock().unwrap();
    let span = |tag: &str| spans.iter().find(|s| s.0 == tag).cloned().unwrap();
    let (_, ws, we) = span("c3");
    for tag in ["c1", "c2", "c4"] {
        let (_, s, e) = span(tag);
        assert!(e <= ws || s >= we, "{tag} overlapped the write");
    }
}

#[test]
fn the_pool_holds_at_most_its_width_at_once() {
    let spans = Spans::default();
    let h = runtime(&spans);
    let rt = &h.rt;
    let calls: Vec<ToolCall> = (0..POOL_WIDTH + 5)
        .map(|i| call(i, "nap_read", r#","ms":50"#))
        .collect();
    let ids = run_round(rt, &calls);
    let want: Vec<String> = (0..calls.len()).map(|i| format!("c{i}")).collect();
    assert_eq!(ids, want);
    let spans = spans.lock().unwrap();
    let most = spans
        .iter()
        .map(|(_, s, _)| spans.iter().filter(|(_, a, b)| a <= s && s < b).count())
        .max()
        .unwrap();
    assert!(most <= POOL_WIDTH, "{most} ran at once");
}

#[test]
fn an_unknown_tool_is_a_barrier() {
    let spans = Spans::default();
    let h = runtime(&spans);
    let rt = &h.rt;
    let calls = vec![
        call(1, "nap_read", ""),
        call(2, "no_such_tool", ""),
        call(3, "nap_read", ""),
    ];
    assert_eq!(rt.batches(&calls), vec![0..1, 1..2, 2..3]);
}

/// `[tools] parallel = false` is read off the runtime, so a round is one call per batch and the
/// pool is never entered.
#[test]
fn with_parallel_off_a_round_is_one_call_per_batch() {
    let spans = Spans::default();
    let mut h = runtime(&spans);
    h.rt.parallel_reads = false;
    let rt = &h.rt;
    let calls = vec![
        call(1, "nap_read", ""),
        call(2, "nap_read", ""),
        call(3, "nap_write", ""),
        call(4, "nap_read", ""),
    ];
    assert!(!rt.runs_pooled(&calls[0]));
    assert_eq!(rt.batches(&calls), vec![0..1, 1..2, 2..3, 3..4]);
    let ids = run_round(rt, &calls);
    assert_eq!(ids, ["c1", "c2", "c3", "c4"]);
    let spans = spans.lock().unwrap();
    // Nothing of the round overlapped anything: the four calls are one after another.
    let mut ordered: Vec<&(String, Instant, Instant)> = spans.iter().collect();
    ordered.sort_by_key(|(_, s, _)| *s);
    for pair in ordered.windows(2) {
        assert!(
            pair[0].2 <= pair[1].1,
            "{:?} ran beside {:?}",
            pair[0].0,
            pair[1].0
        );
    }
}

/// The ten calls already in flight when a stop arrives are joined, and the two the pool had not
/// started are answered as not run: a turn the operator ended does not begin new work, and every
/// call the model made still gets its row.
#[test]
fn a_stop_mid_group_runs_only_what_had_started() {
    let calls_n = POOL_WIDTH + 2;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ran: Arc<Mutex<Vec<String>>> = Arc::default();
    let at = Arc::new(Meet {
        width: POOL_WIDTH,
        stop: stop.clone(),
        ran: ran.clone(),
        state: Mutex::new(0),
        cv: std::sync::Condvar::new(),
    });
    let dir = std::env::temp_dir().join(format!("pool-stop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let backend = letibot_tools::backend::HostBackend::writable(&dir).expect("fixture root");
    let backend = backend.with_interrupt_probe({
        let stop = stop.clone();
        move || stop.load(std::sync::atomic::Ordering::SeqCst)
    });
    let mut registry = letibot_tools::runtime::Registry::new();
    registry
        .register(Box::new(Held { at: at.clone() }))
        .expect("registers");
    let rt = ToolRuntime::new(registry, Box::new(backend))
        .with_gate(letibot_tools::testing::allow_all());
    let calls: Vec<ToolCall> = (0..calls_n)
        .map(|i| ToolCall {
            id: format!("c{i}"),
            name: "held_read".into(),
            arguments: format!(r#"{{"tag":"c{i}"}}"#),
        })
        .collect();
    let mut sink = RecordingToolSink::default();
    let ran_round = rt.invoke_pooled("t1", &calls, &mut sink);
    let ids: Vec<String> = ran_round.iter().map(|(r, _)| r.call_id.clone()).collect();
    let want: Vec<String> = (0..calls_n).map(|i| format!("c{i}")).collect();
    assert_eq!(
        ids, want,
        "every call the model made has its answer, in its order"
    );
    for (i, (r, _)) in ran_round.iter().enumerate() {
        if i < POOL_WIDTH {
            assert!(
                matches!(&r.outcome, letibot_transcript::ToolOutcome::Ok),
                "c{i} had started, so it ran: {:?}",
                r.outcome
            );
        } else {
            assert!(
                matches!(&r.outcome, letibot_transcript::ToolOutcome::NotRun { .. }),
                "c{i} had not started, so the stop took it: {:?}",
                r.outcome
            );
        }
    }
    let ran = ran.lock().unwrap();
    assert_eq!(
        ran.len(),
        POOL_WIDTH,
        "nothing past the stop started: {ran:?}"
    );
}

/// The group a stop arrives in the middle of: the first `width` calls arrive, the last of them
/// raises the stop, and no call answers before it does.
struct Meet {
    width: usize,
    stop: Arc<std::sync::atomic::AtomicBool>,
    ran: Arc<Mutex<Vec<String>>>,
    state: Mutex<usize>,
    cv: std::sync::Condvar,
}

struct Held {
    at: Arc<Meet>,
}

impl Tool for Held {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "held_read",
            "Wait for the rest of the group, raise the stop, then answer.",
            serde_json::json!({"type": "object", "properties": {"tag": {"type": "string"}}}),
            Access::Read,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let tag = args
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let at = &self.at;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut n = at.state.lock().unwrap();
        *n += 1;
        if *n == at.width {
            // The last of the group raises the stop, so no worker can finish before it is set.
            at.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            at.cv.notify_all();
        } else {
            while !at.stop.load(std::sync::atomic::Ordering::SeqCst) {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    panic!("only {} of {} calls were running at once", *n, at.width);
                }
                n = at.cv.wait_timeout(n, left).unwrap().0;
            }
        }
        drop(n);
        at.ran.lock().unwrap().push(tag.clone());
        Invocation::ok(tag)
    }
}
