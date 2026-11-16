//! The scope probe the launcher left running when the user manager did not answer in time and the
//! daemon started without its own scope. When it ends, every session is told what systemd said: a
//! warning on the log and a notice at its next wake.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use letibot_sessionlog::registry::Registry;

/// Set by `scripts/letibot` to the file the backgrounded probe writes its answer to.
pub const ENV: &str = "LETIBOT_SCOPE_LATE";
pub const CODE: &str = "systemd_answered_late";

static ANSWER: OnceLock<String> = OnceLock::new();

/// The sentence, once the probe has answered.
pub fn answer() -> Option<&'static str> {
    ANSWER.get().map(String::as_str)
}

/// `<rc>\t<seconds>\t<last line>` as the launcher writes it, as the sessions read it.
pub fn sentence(text: &str) -> Option<String> {
    let mut parts = text.trim_end_matches('\n').splitn(3, '\t');
    let rc: i32 = parts.next()?.trim().parse().ok()?;
    let secs = parts.next().unwrap_or("?").trim();
    let line = parts.next().unwrap_or("").trim();
    let said = if rc == 0 {
        "accepted a scope".to_string()
    } else if line.is_empty() {
        format!("refused a scope (exit {rc}, nothing said)")
    } else {
        format!("refused a scope: {line}")
    };
    Some(format!(
        "the systemd user manager answered {secs}s after the launcher asked, and {said}. This \
         daemon did not wait for it: it runs without its own scope, so memory caps and io \
         accounting are off for its tool calls. A scope is not adopted late - the caps are set \
         up at start. `letibot --stop` and start again to run under one."
    ))
}

/// Watch `$LETIBOT_SCOPE_LATE`, when the launcher set it.
pub fn spawn(registry: Arc<Registry>) {
    let Some(path) = std::env::var_os(ENV).map(PathBuf::from) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("late-scope".into())
        .spawn(move || watch(&registry, &path, Duration::from_secs(1)));
}

fn watch(registry: &Registry, path: &Path, every: Duration) {
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(s) = sentence(&text) {
                tell(registry, s);
            }
            // The probe's directory is the launcher's; its answer has been read.
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
            return;
        }
        std::thread::sleep(every);
    }
}

fn tell(registry: &Registry, said: String) {
    eprintln!("harnessd: {said}");
    let _ = ANSWER.set(said.clone());
    for id in registry.ids() {
        if let Some(hub) = registry.get(&id) {
            hub.warn(CODE, said.clone());
        }
        registry.bell().ring_wake(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_says_what_systemd_said_and_what_was_done() {
        let ok = sentence("0\t41\t\n").unwrap();
        assert!(ok.contains("answered 41s"), "{ok}");
        assert!(ok.contains("accepted a scope"), "{ok}");
        assert!(ok.contains("runs without its own scope"), "{ok}");
        let no = sentence("1\t7\tFailed to start transient scope unit: Argument list too long\n")
            .unwrap();
        assert!(no.contains("refused a scope: Failed to start"), "{no}");
        assert_eq!(sentence("garbage"), None);
    }

    #[test]
    fn a_late_answer_is_warned_on_every_live_session() {
        let dir = letibot_sessionlog::testing::scratch_root()
            .join(format!("late-scope-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("answer");
        let registry = Registry::new();
        let wiring = letibot_sessionlog::registry::SessionWiring {
            model: "m".into(),
            dialect: "d".into(),
            endpoint: "http://127.0.0.1:1".into(),
            workspace: "/w".into(),
        };
        let hub = registry.create("s1", "t", wiring).expect("registered");
        let r = registry.clone();
        let p = path.clone();
        let t = std::thread::spawn(move || watch(&r, &p, Duration::from_millis(20)));
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&path, "0\t3\t\n").unwrap();
        t.join().unwrap();
        let warned = hub.retained().iter().any(|e| {
            matches!(&e.event, letibot_sessionlog::SessionEvent::Warning { code, .. } if code == CODE)
        });
        assert!(warned, "the live session got the warning");
        assert!(answer().is_some_and(|a| a.contains("accepted")));
        assert!(!dir.exists(), "the probe's directory is removed once read");
    }
}
