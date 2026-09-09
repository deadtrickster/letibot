// Where does grep's time actually go: walking, reading, or matching?
// That is the question "would ripgrep's engine be faster" turns on.
use letibot_tools::backend::{ExecBackend, HostBackend, default_skip, walk};
use letibot_tools::builtins::pattern::Pattern;
use std::time::Instant;
fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| "crates".into());
    let src  = std::env::args().nth(2).unwrap_or_else(|| "TokenLedger".into());
    let b = HostBackend::new(&root).expect("backend");

    let t = Instant::now();
    let (entries, _) = walk(&b, ".", 100_000, &default_skip);
    let walk_ms = t.elapsed().as_secs_f64() * 1000.0;
    let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();

    let t = Instant::now();
    let mut bodies = Vec::new();
    let mut bytes = 0usize;
    for e in &files {
        if let Ok(v) = b.read(&e.path) { bytes += v.len(); bodies.push(v); }
    }
    let read_ms = t.elapsed().as_secs_f64() * 1000.0;

    let pat = Pattern::compile(&src, false);
    let t = Instant::now();
    let mut hits = 0usize;
    let mut lines = 0usize;
    for v in &bodies {
        if let Ok(s) = std::str::from_utf8(v) {
            for l in s.lines() { lines += 1; if pat.is_match(l) { hits += 1; } }
        }
    }
    let match_ms = t.elapsed().as_secs_f64() * 1000.0;

    println!("{} files, {:.1} MB, {lines} lines", files.len(), bytes as f64 / 1e6);
    println!("  walk    {walk_ms:8.1} ms");
    println!("  read    {read_ms:8.1} ms");
    println!("  match   {match_ms:8.1} ms   ({hits} hits)");
    println!("  total   {:8.1} ms", walk_ms + read_ms + match_ms);
    println!("  match is {:.0}% of the total", 100.0*match_ms/(walk_ms+read_ms+match_ms));
}
