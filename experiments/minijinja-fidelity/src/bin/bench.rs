//! How long does rendering a real conversation with minijinja actually take?
//! Relevant because the render sits on the per-turn critical path and the
//! provenance technique renders TWICE (clean + sentinel-wrapped) plus a string
//! compare.
use minijinja::value::{Kwargs, ViaDeserialize};
use minijinja::{Environment, Error, ErrorKind, Value};
use std::collections::BTreeMap;
use std::time::Instant;
include!("shared_env.rs");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tpath = &args[1];
    let jpath = &args[2];
    let iters: u32 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(200);
    let src: &'static str = Box::leak(std::fs::read_to_string(tpath).unwrap().into_boxed_str());
    let jobs: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(jpath).unwrap()).unwrap();
    let jobs = jobs.as_array().unwrap();

    let t0 = Instant::now();
    let mut env = build_env();
    env.add_template("t", src).unwrap();
    let compile = t0.elapsed();

    // pick the largest job that renders, to be representative of a long turn
    let mut best: Option<(usize, Value, Value)> = None;
    for j in jobs {
        let a = j.get("args").unwrap();
        let w = j.get("wrapped_args").unwrap();
        let ctx = Value::from_serialize(a);
        if let Ok(s) = env.get_template("t").unwrap().render(ctx.clone()) {
            if best.as_ref().map(|(n, _, _)| s.len() > *n).unwrap_or(true) {
                best = Some((s.len(), ctx, Value::from_serialize(w)));
            }
        }
    }
    let (len, ctx, wctx) = best.unwrap();
    let tmpl = env.get_template("t").unwrap();

    let t1 = Instant::now();
    for _ in 0..iters { std::hint::black_box(tmpl.render(ctx.clone()).unwrap()); }
    let clean = t1.elapsed() / iters;

    let t2 = Instant::now();
    for _ in 0..iters {
        let a = tmpl.render(ctx.clone()).unwrap();
        let b = tmpl.render(wctx.clone()).unwrap();
        std::hint::black_box((a, b));
    }
    let both = t2.elapsed() / iters;

    println!("template {tpath}: compile {compile:?}, prompt {len} bytes");
    println!("  render clean          {clean:?}");
    println!("  render clean+wrapped  {both:?}  (what provenance costs)");
}
