// Where does grep's time actually go: walking, reading, or matching?
//
// It used to be matching, by a factor that made the other two rounding errors:
// 23.7 ms of a 25.0 ms search over `crates/`, for a search ripgrep answers in
// 9-10 ms. That measurement is what got the hand-rolled matcher replaced by
// `regex`, so this example now times BOTH — the old engine is in `legacy.rs`
// beside this file, kept for exactly one purpose, which is to keep the claim
// falsifiable.
//
//   cargo run --release -p letibot-tools --example greptime -- [ROOT] [PATTERN]
//
// Three matchers are timed over the same bytes:
//
//   legacy      the hand-rolled backtracker, one call per line
//   regex/line  the new engine, still one call per line
//   regex/buf   the new engine over the whole file, which is what `grep` does
//
// The middle row is there so the two changes do not get credited to each other:
// legacy -> regex/line is the engine, and regex/line -> regex/buf is the scan.
mod legacy;

use letibot_tools::backend::{ExecBackend, HostBackend, default_skip, walk};
use letibot_tools::builtins::pattern::Pattern;
use std::time::Instant;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| "crates".into());
    let src = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "TokenLedger".into());
    let reps: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let b = HostBackend::new(&root).expect("backend");

    let t = Instant::now();
    let (entries, _) = walk(&b, ".", 100_000, &default_skip);
    let walk_ms = ms(t);
    let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();

    let t = Instant::now();
    let mut bodies = Vec::new();
    let mut bytes = 0usize;
    for e in &files {
        if let Ok(v) = b.read(&e.path) {
            bytes += v.len();
            bodies.push(v);
        }
    }
    let read_ms = ms(t);
    // The matchers see text, not bytes, and utf8 validation is not part of what
    // is being compared. Do it once, outside every timed region.
    let texts: Vec<&str> = bodies
        .iter()
        .filter_map(|v| std::str::from_utf8(v).ok())
        .collect();
    let lines: usize = texts.iter().map(|s| s.lines().count()).sum();

    println!(
        "{} files, {:.1} MB, {lines} lines, pattern `{src}`, best of {reps}",
        files.len(),
        bytes as f64 / 1e6
    );
    println!("  walk    {walk_ms:8.2} ms");
    println!("  read    {read_ms:8.2} ms");

    // Compilation, which the ladder pays several times per call and which the
    // cache exists to stop paying. Timed cold (a source no run has seen) and
    // warm (the second ask for the same one).
    let cold_src = format!("{src}(?:{})", std::process::id());
    let t = Instant::now();
    let cold = Pattern::compile(&cold_src, false).is_ok();
    let compile_cold_us = ms(t) * 1000.0;
    let _ = Pattern::compile(&src, false);
    let t = Instant::now();
    let _ = Pattern::compile(&src, false);
    let compile_warm_us = ms(t) * 1000.0;
    if cold {
        println!("  compile {compile_cold_us:8.1} us cold, {compile_warm_us:.1} us cached");
    }

    let legacy = legacy::Pattern::compile(&src, false);
    let mut legacy_ms = f64::MAX;
    let mut legacy_hits = 0usize;
    for _ in 0..reps {
        let t = Instant::now();
        let mut hits = 0usize;
        for s in &texts {
            for l in s.lines() {
                if legacy.is_match(l) {
                    hits += 1;
                }
            }
        }
        legacy_ms = legacy_ms.min(ms(t));
        legacy_hits = hits;
    }

    let pat = match Pattern::compile(&src, false) {
        Ok(p) => p,
        Err(e) => {
            // The point of phase 1's clause 3, visible from the bench: a pattern
            // that does not compile produces the engine's message, not a search.
            println!(
                "\n  regex refused the pattern:\n{}\n\n{}",
                e.message,
                e.remedy()
            );
            return;
        }
    };

    let mut byline_ms = f64::MAX;
    let mut byline_hits = 0usize;
    for _ in 0..reps {
        let t = Instant::now();
        let mut hits = 0usize;
        for s in &texts {
            for l in s.lines() {
                if pat.is_match(l) {
                    hits += 1;
                }
            }
        }
        byline_ms = byline_ms.min(ms(t));
        byline_hits = hits;
    }

    let mut buf_ms = f64::MAX;
    let mut buf_hits = 0usize;
    for _ in 0..reps {
        let t = Instant::now();
        let mut hits = 0usize;
        for s in &texts {
            pat.line_hits(s, usize::MAX, |_, _| hits += 1);
        }
        buf_ms = buf_ms.min(ms(t));
        buf_hits = hits;
    }

    println!(
        "\n  {:<12}{:>9}  {:>8}  hits",
        "matcher", "match ms", "total ms"
    );
    for (name, m, h) in [
        ("legacy", legacy_ms, legacy_hits),
        ("regex/line", byline_ms, byline_hits),
        ("regex/buf", buf_ms, buf_hits),
    ] {
        println!("  {name:<12}{m:>9.2}  {:>8.2}  {h}", walk_ms + read_ms + m);
    }

    // A speedup claimed over a different answer is not a speedup. The three
    // matchers must agree on the hit count, and if they do not, that is the
    // headline and not a footnote.
    if legacy_hits != buf_hits || byline_hits != buf_hits {
        println!(
            "\n  !! THE MATCHERS DISAGREE: legacy {legacy_hits}, regex/line {byline_hits}, \
             regex/buf {buf_hits}. The timings below mean nothing until this is explained; \
             the old subset did not support everything `regex` does, so a pattern using \
             {{n,m}}, lazy quantifiers, inline flags or unicode classes is expected to \
             differ HERE and only here."
        );
    } else {
        println!(
            "\n  regex/buf is {:.1}x the legacy matcher on match, {:.1}x end to end",
            legacy_ms / buf_ms,
            (walk_ms + read_ms + legacy_ms) / (walk_ms + read_ms + buf_ms)
        );
    }
}
