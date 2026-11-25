//! Every refusal a model API sent, kept whole in the session's own log: the status, the answer's
//! headers and every byte of its body. A provider's code names no field - GLM's `1210` is "Invalid
//! API parameter" and nothing else - so the body is the only thing that says which parameter it
//! objected to, and a record kept from the projection to a message loses exactly that.

use std::path::Path;

use letibot_provider::refusals::{Refusal, RefusalLog};

/// The record's file, inside the session's logs directory (`Config::daemon_logs`), beside the
/// delivery log. Never the workspace: a `.letibot/` there would make every child's worktree dirty.
pub const LOG: &str = "model-refusals.jsonl";

/// Where one session's refusals are recorded. A closure rather than a type the provider knows,
/// because the provider sits below the session log and who keeps the record is the caller's.
pub fn log_at(dir: &Path, session: &str) -> RefusalLog {
    let path = dir.join(LOG);
    let session = session.to_string();
    std::sync::Arc::new(move |r: &Refusal| {
        let mut row = r.to_json();
        row["session"] = serde_json::Value::String(session.clone());
        let mut line = row.to_string();
        line.push('\n');
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(body: &str) -> Refusal {
        Refusal {
            provider: "glm-coding".into(),
            model: "glm-5.4".into(),
            status: 400,
            headers: vec![("x-request-id".into(), "req_7".into())],
            body: body.into(),
            body_bytes: body.len(),
            request_bytes: 5_900_000,
            shape: None,
            budget: letibot_provider::budget::Budget::NONE,
        }
    }

    /// The record is one JSON line, it names the session, and the answer is in it verbatim.
    #[test]
    fn a_refusal_is_one_line_of_json_in_the_sessions_log() {
        let dir = std::env::temp_dir().join(format!("refusals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = log_at(&dir, "s-1");
        let said = "{\"error\":{\"code\":\"1210\",\"message\":\"Invalid API parameter\"}}";
        log(&refusal(said));
        log(&refusal("{\"error\":{\"code\":\"1213\"}}"));
        let text = std::fs::read_to_string(dir.join(LOG)).expect("the file");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one line per refusal: {text}");
        let row: serde_json::Value = serde_json::from_str(lines[0]).expect("a JSON line");
        assert_eq!(row["session"], "s-1");
        assert_eq!(row["status"], 400);
        assert_eq!(row["body"], said);
        assert_eq!(row["headers"]["x-request-id"], "req_7");
        assert_eq!(row["request_bytes"], 5_900_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
