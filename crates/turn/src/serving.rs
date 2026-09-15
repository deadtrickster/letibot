//! What the server on the other end is actually serving, asked before a test
//! blames the tokenizer.
//!
//! # The failure this exists to stop being opaque
//!
//! The live tests in this workspace tokenise locally, against a GGUF named in the
//! config, and post token **ids**. Three model services on this box are singletons
//! that evict each other and have shared a port, so the model behind `:8080` is
//! whichever one was started last. Point qwen's vocabulary at GLM and the ids are
//! simply out of range — qwen-3.8-flash-next has 248,320 tokens and GLM-5.3-Flash
//! has 154,880 — and the server answers:
//!
//! ```text
//! 400 {"error":{"message":"Prompt contains invalid tokens", ...}}
//! ```
//!
//! Which is true and names nothing. A reader gets "the tokenizer is broken", and
//! the fact is "this port is serving a different model than the one this test
//! tokenised for". That is the same shape as a 404 that means *not visible to you*
//! rather than *not there*, and this fleet has already paid for the difference
//! once.
//!
//! # Why this is a refusal and not a skip
//!
//! Same discipline as the live tests themselves: a test that quietly passes when
//! its model is absent reports the health of a `TcpStream::connect`. [`expect`]
//! **panics**, and it panics with the served model, the expected model, and the one
//! command that fixes it.

use crate::http::{self, Endpoint};

/// The model alias the server reports, or why the question could not be answered.
///
/// `/props` is a llama.cpp server endpoint and the alias is what `--alias` set,
/// falling back to the GGUF's own name.
pub fn served_model(endpoint: &Endpoint) -> Result<String, String> {
    let body = http::get(endpoint, "/props")
        .map_err(|e| format!("{e}"))?
        .read_to_string()
        .map_err(|e| format!("{e}"))?;
    // Deliberately not a JSON dependency for one field: `model_path` is a string
    // and this is a test-support path, not a parser anybody should rely on.
    for key in ["\"model_path\"", "\"model\""] {
        if let Some(v) = field(&body, key) {
            return Ok(v);
        }
    }
    Err(format!(
        "/props answered but named no model: {}",
        truncate(&body, 200)
    ))
}

/// The server's context window, from `/props`, or `None` when it does not say.
///
/// **The wall, read rather than assumed.** `docs/compaction.md` §1 settled that
/// compaction is driven by `n_ctx` and by memory pressure, and by nothing else:
/// quality does not degrade with depth (160 samples, 1.000 at 60k against 0.829
/// at zero). So the trigger is this number, and a harness that guessed it would
/// either compact a conversation that had room or discover the wall by hitting
/// it — which is what happened on 2026-09-15, as a 500 with nothing recorded.
///
/// `None` is a real answer and not a failure: a metered provider has no `/props`
/// and its window is the operator's to state. The caller decides what to do with
/// not knowing; it must not invent a number.
pub fn served_ctx(endpoint: &Endpoint) -> Option<u64> {
    let body = http::get(endpoint, "/props").ok()?.read_to_string().ok()?;
    // `n_ctx` sits inside `default_generation_settings`, and it is a number rather
    // than a string, so `field` — which reads quoted values — cannot serve.
    let at = body.find("\"n_ctx\"")?;
    let rest = &body[at + "\"n_ctx\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// Refuse, loudly and by name, unless the server is serving `want`.
///
/// `want` is matched as a **substring** of the reported path, because the alias is
/// `qwen-3.8-flash-next` and the path is
/// `…/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf`; comparing them exactly
/// would fail on every correct setup.
pub fn expect(endpoint: &Endpoint, want: &str) {
    let served = match served_model(endpoint) {
        Ok(s) => s,
        // Not reachable is the live tests' own business — they fail on the request
        // and their failure says so. This one is only about serving the WRONG model.
        Err(e) => {
            eprintln!(
                "preflight: could not ask {}/props ({e}); letting the test fail on its own request",
                endpoint.authority()
            );
            return;
        }
    };
    if matches(&served, want) {
        return;
    }
    panic!(
        "PREFLIGHT: {} is serving `{served}`, and this test tokenises for `{want}`.\n\n\
         Nothing was run. The failure you would have got instead is `400 Prompt \
         contains invalid tokens`, which is true and names nothing: the ids are out \
         of range because they came from the other model's vocabulary, not because \
         the tokenizer is wrong.\n\n\
         These services are singletons and evict each other, so one of:\n  \
         - start `{want}` (`~/bin/qwen-flash-server`, `~/bin/qwen-dense-server`, \
         `~/bin/glm-flash-server`), or\n  \
         - point this test elsewhere with LETIBOT_COMPLETION_URL, LETIBOT_VOCAB_GGUF \
         and LETIBOT_MODEL_ALIAS.",
        endpoint.authority()
    );
}

/// Case-insensitive, and blind to the `.`/`-` split that separates an alias from a
/// file name: `qwen-3.8-flash-next` against `Qwen3.8-Flash-Next-UD-Q6_K_XL`.
fn matches(served: &str, want: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>()
    };
    norm(served).contains(&norm(want))
}

fn field(body: &str, key: &str) -> Option<String> {
    let at = body.find(key)? + key.len();
    let rest = body.get(at..)?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let v = &rest[..end];
    (!v.is_empty()).then(|| v.to_string())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alias_matches_the_gguf_file_name_it_was_loaded_from() {
        assert!(matches(
            "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf",
            "qwen-3.8-flash-next"
        ));
        assert!(matches("glm-5.3-flash", "glm-5.3-flash"));
    }

    #[test]
    fn the_other_model_on_the_same_port_does_not_match() {
        // The exact pair that made eight tests fail with a message about tokens.
        assert!(!matches(
            "GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf",
            "qwen-3.8-flash-next"
        ));
    }

    #[test]
    fn the_model_is_read_out_of_a_props_body() {
        let body = r#"{"default_generation_settings":{"n_ctx":262144},"model_path":"/m/GLM-5.3-Flash.gguf"}"#;
        assert_eq!(
            field(body, "\"model_path\"").as_deref(),
            Some("/m/GLM-5.3-Flash.gguf")
        );
    }
}

#[cfg(test)]
mod ctx_tests {
    /// `n_ctx` is a NUMBER nested inside `default_generation_settings`, so the
    /// quoted-string reader used for `model_path` cannot find it — the parse is
    /// separate on purpose and this is what pins that.
    #[test]
    fn the_window_is_read_out_of_a_props_body() {
        let body = r#"{"default_generation_settings":{"n_ctx":262144,"n_batch":2048},"model_path":"/m/x.gguf"}"#;
        let at = body.find("\"n_ctx\"").unwrap();
        let rest = &body[at + "\"n_ctx\"".len()..];
        let rest = rest.trim_start().strip_prefix(':').unwrap().trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        assert_eq!(digits.parse::<u64>().unwrap(), 262144);
    }
}
