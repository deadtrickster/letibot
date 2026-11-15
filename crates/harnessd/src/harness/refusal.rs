//! A provider's refusal, kept: the status, the provider's code and its message, never the
//! request and never a key. Written to the daemon log and the session's warnings on a limit or
//! a refusal, so a diagnosis reads what the provider said instead of inferring it from timings.

use letibot_turn::HttpError;

/// The longest message kept. A refusal is a sentence; a page of HTML from a proxy is not.
const MESSAGE_CHARS: usize = 400;

/// What the provider said, in the three parts a diagnosis needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub status: u16,
    pub code: Option<String>,
    pub message: String,
}

impl Refusal {
    /// The refusal in `e`, redacted against `secrets` and every key-shaped token, or `None` for a
    /// failure with no answer behind it.
    pub(crate) fn of(e: &HttpError, secrets: &[&str]) -> Option<Self> {
        let HttpError::Status { code, body, .. } = e else {
            return None;
        };
        let (message, provider_code) = split(body);
        let message = redact(&message, secrets);
        Some(Refusal {
            status: *code,
            code: provider_code.map(|c| redact(&c, secrets)),
            message: clip(&one_line(&message), MESSAGE_CHARS),
        })
    }

    /// `status 429 · code 1308 · message ...`, the one shape the log and the warning share.
    pub(crate) fn line(&self) -> String {
        match &self.code {
            Some(c) => format!(
                "status {} · code {c} · message {}",
                self.status, self.message
            ),
            None => format!("status {} · message {}", self.status, self.message),
        }
    }
}

/// The message and the provider's code: a JSON error object's `message` and `code`, or the
/// `message (code)` the provider crate renders.
fn split(body: &str) -> (String, Option<String>) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) {
        let err = v.get("error").unwrap_or(&v);
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| err.to_string());
        let code = err.get("code").or_else(|| err.get("type")).map(|c| {
            c.as_str()
                .map(String::from)
                .unwrap_or_else(|| c.to_string())
        });
        return (message, code);
    }
    let t = body.trim_end();
    if let Some(open) = t.rfind('(')
        && t.ends_with(')')
    {
        let inner = &t[open + 1..t.len() - 1];
        if !inner.is_empty()
            && inner.len() <= 64
            && inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return (t[..open].trim_end().to_string(), Some(inner.to_string()));
        }
    }
    (body.to_string(), None)
}

/// `s` with each of `secrets` and every token that looks like a credential replaced by
/// `[redacted]`. A token is key-shaped when it is long and mixes letters and digits, or carries
/// a known key prefix; a refusal's prose has neither.
pub(crate) fn redact(s: &str, secrets: &[&str]) -> String {
    let mut out = s.to_string();
    for k in secrets.iter().filter(|k| k.len() >= 8) {
        out = out.replace(k, "[redacted]");
    }
    let mut kept = String::with_capacity(out.len());
    let mut token = String::new();
    let flush = |token: &mut String, kept: &mut String| {
        if key_shaped(token) {
            kept.push_str("[redacted]");
        } else {
            kept.push_str(token);
        }
        token.clear();
    };
    for c in out.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=') {
            token.push(c);
        } else {
            flush(&mut token, &mut kept);
            kept.push(c);
        }
    }
    flush(&mut token, &mut kept);
    kept
}

fn key_shaped(t: &str) -> bool {
    let t = t.trim_matches(|c| matches!(c, '.' | '/' | '='));
    if ["sk-", "sk_", "pk-", "key-", "ghp_", "xai-", "AIza"]
        .iter()
        .any(|p| t.starts_with(p) && t.len() >= 16)
    {
        return true;
    }
    t.len() >= 24
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(code: u16, body: &str) -> HttpError {
        HttpError::Status {
            code,
            body: body.into(),
            retry_after: None,
        }
    }

    /// The rendered `message (code)` and a raw JSON error both come apart into the three parts.
    #[test]
    fn a_refusal_is_its_status_code_and_message() {
        let r = Refusal::of(
            &said(
                429,
                "Usage limit reached for 5 hour. Your limit will reset at 2026-11-16 02:11:08 (1308)",
            ),
            &[],
        )
        .unwrap();
        assert_eq!(r.status, 429);
        assert_eq!(r.code.as_deref(), Some("1308"));
        assert!(r.message.starts_with("Usage limit reached for 5 hour."));
        assert_eq!(
            r.line(),
            "status 429 · code 1308 · message Usage limit reached for 5 hour. Your limit will \
             reset at 2026-11-16 02:11:08"
        );
        let r = Refusal::of(
            &said(
                400,
                r#"{"error":{"message":"insufficient balance","code":"insufficient_quota"}}"#,
            ),
            &[],
        )
        .unwrap();
        assert_eq!(r.code.as_deref(), Some("insufficient_quota"));
        assert_eq!(r.message, "insufficient balance");
        assert!(Refusal::of(&HttpError::Malformed("cut".into()), &[]).is_none());
    }

    /// A key echoed back in a refusal never reaches the log: the session's own key by value, and
    /// anything key-shaped by its shape. Prose and the code survive.
    #[test]
    fn a_key_in_a_refusal_is_redacted() {
        let key = "a1b2c3d4e5f6.Zq9XyWvUtSrQpOnM";
        let r = Refusal::of(
            &said(
                401,
                &format!(
                    "Incorrect API key provided: {key}. Also sk-proj-AbCdEf0123456789xyz and \
                     Bearer eyJhbGciOiJIUzI1NiJ9abc123def456 were refused (1001)"
                ),
            ),
            &[key],
        )
        .unwrap();
        let line = r.line();
        assert!(!line.contains("Zq9XyWvUtSrQpOnM"), "{line}");
        assert!(!line.contains("a1b2c3d4e5f6"), "{line}");
        assert!(!line.contains("AbCdEf0123456789"), "{line}");
        assert!(!line.contains("eyJhbGciOiJIUzI1NiJ9"), "{line}");
        assert!(line.contains("Incorrect API key provided"), "{line}");
        assert!(line.contains("code 1001"), "{line}");
        assert_eq!(line.matches("[redacted]").count(), 3, "{line}");
    }

    #[test]
    fn a_long_body_is_clipped_to_one_line() {
        let body = format!("<html>\n{}\n</html>", "x ".repeat(2000));
        let r = Refusal::of(&said(502, &body), &[]).unwrap();
        assert!(!r.message.contains('\n'));
        assert!(r.message.chars().count() <= MESSAGE_CHARS + 1);
    }
}
