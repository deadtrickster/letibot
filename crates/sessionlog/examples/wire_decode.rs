//! An older build's reading of another build's frames. Names only `protocol::{ClientFrame,
//! ServerFrame}` and serde_json, so `scripts/wire-skew.sh` can drop it into an old checkout.
//!     cargo run -q --example wire_decode -p letibot-sessionlog -- <server.jsonl> <client.jsonl>
//! A file named `server*` holds daemon frames, any other head frames. Exit 1 on a frame this
//! build cannot decode, or one whose shared fields it reads with another value. A head frame
//! newer than this build is noted, not failed.

use letibot_sessionlog::protocol::{ClientFrame, ServerFrame};
use serde_json::Value;

/// Re-encodes `line` through this build's type, so what the decoder kept can be compared.
fn read(server: bool, line: &str) -> Result<Value, String> {
    let r = if server {
        serde_json::from_str::<ServerFrame>(line).and_then(|f| serde_json::to_value(&f))
    } else {
        serde_json::from_str::<ClientFrame>(line).and_then(|f| serde_json::to_value(&f))
    };
    r.map_err(|e| e.to_string())
}

fn empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
    }
}

/// Walks what was sent against what was read. A field read with another value is a change of
/// meaning. A key this build drops beside one it fills with a non-empty default is a rename.
fn compare(path: &str, sent: &Value, read: &Value, unknown: &mut Vec<String>) -> Vec<String> {
    let mut bad = Vec::new();
    match (sent, read) {
        (Value::Object(s), Value::Object(r)) => {
            let dropped: Vec<&String> = s.keys().filter(|k| !r.contains_key(*k)).collect();
            let filled: Vec<&String> = r
                .iter()
                .filter(|(k, v)| !s.contains_key(*k) && !empty(v))
                .map(|(k, _)| k)
                .collect();
            if !dropped.is_empty() && !filled.is_empty() {
                bad.push(format!(
                    "{path}: sent {dropped:?}, read {filled:?} filled by default"
                ));
            }
            unknown.extend(dropped.iter().map(|k| format!("{path}.{k}")));
            for (k, v) in s {
                if let Some(rv) = r.get(k) {
                    bad.extend(compare(&format!("{path}.{k}"), v, rv, unknown));
                }
            }
        }
        (Value::Array(s), Value::Array(r)) if s.len() == r.len() => {
            for (i, (a, b)) in s.iter().zip(r).enumerate() {
                bad.extend(compare(&format!("{path}[{i}]"), a, b, unknown));
            }
        }
        _ if sent != read => bad.push(format!("{path}: sent {sent}, read {read}")),
        _ => {}
    }
    bad
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: wire_decode <file.jsonl>...");
        std::process::exit(2);
    }
    let mut failed = 0;
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| {
            eprintln!("wire_decode: {file}: {e}");
            std::process::exit(2);
        });
        let name = std::path::Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let server = name.starts_with("server");
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
            let at = format!("{name}:{}", n + 1);
            let sent: Value = serde_json::from_str(line).expect("a corpus line is JSON");
            let mut unknown = Vec::new();
            let tag = sent["frame"].as_str().unwrap_or("?");
            let bad = match read(server, line) {
                Ok(read) => compare("", &sent, &read, &mut unknown),
                // A daemon that predates a head frame is never sent it: the head goes read-only.
                Err(e) if !server && e.contains(&format!("unknown variant `{tag}`")) => {
                    println!("note {at} {tag} is newer than this build");
                    continue;
                }
                Err(e) => vec![format!("does not decode: {e}")],
            };
            for b in &bad {
                println!("FAIL {at} {b}");
            }
            if !unknown.is_empty() {
                println!("note {at} unknown here, dropped: {}", unknown.join(" "));
            }
            if bad.is_empty() {
                println!("ok   {at} {tag}");
            }
            failed += usize::from(!bad.is_empty());
        }
    }
    if failed > 0 {
        println!("{failed} frame(s) failed");
        std::process::exit(1);
    }
}
