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

/// **Is a model server answering at this endpoint?** — the one question a test asks before it
/// spends a turn.
///
/// MEASURED, and this is why the function exists: on a machine with no `llama-server`,
/// `cargo test --workspace` spent **373.55 s** failing four tests in one file, and `cargo test`
/// **aborts the remaining targets** after the first failing binary — so most of the suite never ran
/// at all. A CI job built on that is red for a reason nobody can read, six minutes at a time.
///
/// So a live test asks this first and SKIPS with a sentence rather than failing with a stack trace.
/// `SKIPPED … THIS IS NOT A PASS` is the phrasing `live_qwen.rs` already uses for the wrong-model
/// case, and the rule behind it is the house's: a skip must never be mistakable for a green.
///
/// One `GET /props`, which is the cheapest thing an endpoint can answer and needs no tokenizer, no
/// model and no turn. The connect timeout bounds it (10 s), so a blackholed address costs seconds
/// rather than the read timeout's three minutes.
pub fn reachable(endpoint: &Endpoint) -> bool {
    http::get(endpoint, "/props").is_ok()
}

/// **Did somebody SAY where the model is?** — `LETIBOT_COMPLETION_URL` is set.
///
/// The distinction this exists for is between two states that look identical at the
/// socket, because nothing answers in either:
///
///   * **configured and refusing** — somebody promised a server at this address. The
///     run was told where the model is and it is not there. That is a FAILURE.
///   * **never meant to have one** — the address is the default, nobody said
///     anything, and this is a runner, a fresh clone or a laptop. That is absent
///     apparatus, and it skips.
///
/// Without this, an endpoint somebody configured is silently skipped over, and the
/// skip becomes the place a real failure hides — which is the objection that keeps
/// this tree's live tests honest. `LETIBOT_REQUIRE_MODEL` remains as the belt: it
/// refuses the skip even for an unconfigured endpoint.
pub fn endpoint_is_configured() -> bool {
    std::env::var_os("LETIBOT_COMPLETION_URL").is_some()
}

/// **Should this live test skip?** — the one question a live test asks before it spends a turn.
///
/// `true` means nothing answered at `endpoint` and the caller should return early. The line is
/// printed HERE rather than at each call site so the wording cannot drift, and `THIS IS NOT A
/// PASS` is deliberate: a skip that reads like a green is how a suite stops testing without
/// anybody noticing.
///
/// # `LETIBOT_REQUIRE_MODEL` turns the skip into a failure
///
/// A skip that cannot be refused is a suite that quietly stops testing: with the variable unset,
/// a machine with no server runs zero live tests and reports success. So this is how a run that
/// means it says so — a nightly with a server, or an operator checking the live tests still pass.
/// Set it to anything but `0` or empty and a missing server panics, naming the endpoint.
///
/// # Why the skip exists
///
/// MEASURED with the endpoint pointed at a dead port: four tests in `loop_closes.rs` spent
/// **373.55 s** failing, and `cargo test` **aborts the remaining targets** after the first failing
/// binary — so most of the suite never ran at all. A CI job built on that is red for a reason
/// nobody can read, six minutes at a time. With this guard the same four finish in **0.00 s**.
pub fn skip_live_test(endpoint: &Endpoint, what: &str) -> bool {
    if reachable(endpoint) {
        return false;
    }
    // **A configured endpoint that refuses is a failure, not a skip.**
    //
    // MEASURED, and it is why this is here rather than assumed: nothing answers,
    // either way, so the socket cannot tell the two apart. What can is whether
    // `LETIBOT_COMPLETION_URL` was SET — a promise about where the model is.
    if endpoint_is_configured() {
        panic!(
            "LETIBOT_COMPLETION_URL points at {}, and nothing answers there, so {} \
             cannot run.\n\n\
             A server that was CONFIGURED and is refusing is a failure rather than a \
             skip: this run was told which endpoint to use, and it is not up. Unset \
             LETIBOT_COMPLETION_URL to let a machine that was never meant to have a \
             server skip instead, or set LETIBOT_REQUIRE_MODEL=1 to refuse both.",
            endpoint.authority(),
            what
        );
    }
    let required = std::env::var("LETIBOT_REQUIRE_MODEL")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    if required {
        panic!(
            "LETIBOT_REQUIRE_MODEL is set, and no model server answers at {}.\n\
             {what} cannot run, and this run does not accept a skip.",
            endpoint.authority()
        );
    }
    eprintln!(
        "SKIPPED: no model server at {}.\n\n\
         {what} needs one: a real turn cannot be run without it — a llama.cpp `llama-server` on \
         127.0.0.1:8080 by default, or wherever LETIBOT_COMPLETION_URL points.\n\
         Nothing was run, and THIS IS NOT A PASS.",
        endpoint.authority()
    );
    true
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

/// **The token a multimodal prompt must place where its image goes**, from `/props`.
///
/// # Why this is fetched and not known
///
/// llama.cpp's `mtmd` splits a prompt on a **media marker** and substitutes the image's embeddings
/// at each occurrence. MEASURED on this box, 2026-09-27, and the measurement is the whole reason this
/// function exists: the marker is **randomised per server instance** —
///
/// ```text
/// /props → media_marker: "<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>"
/// ```
//
/// — unless `LLAMA_MEDIA_MARKER` is set, and it is **not** any of the model's own tokens. Passing
/// the qwen template's `<|vision_start|><|image_pad|><|vision_end|>` — which is what the Jinja
/// template emits, and what this tree's dialect renderers already write — is answered
/// `HTTP 400 · Failed to tokenize prompt`. So the marker is a fact about the running server, exactly
/// like `n_ctx` and `total_slots`, and a head that hardcoded one would work against one process and
/// fail against the next.
///
/// `None` when the endpoint does not say: every metered provider, and any local server old enough
/// not to have `mtmd`. `None` means **no images on this endpoint**, which the caller discloses
/// rather than guessing at a marker.
pub fn served_media_marker(endpoint: &Endpoint) -> Option<String> {
    let body = http::get(endpoint, "/props").ok()?.read_to_string().ok()?;
    field(&body, "\"media_marker\"").filter(|m| !m.is_empty())
}

/// **How many sequences this server decodes at once**, from `/props`.
///
/// llama.cpp's slots are its batching unit: N slots means N sequences are
/// decoded in the same batch, each with its own KV cache, so N concurrent
/// requests are cheaper together than one after another. A caller that
/// serialises to "protect the cache" has the relationship backwards — the caches
/// are per slot and do not evict one another.
///
/// `None` when the endpoint does not say, which is every metered provider and
/// any server too old to report it. The caller picks its own bound then, rather
/// than assuming a number off a server that never claimed one.
pub fn served_slots(endpoint: &Endpoint) -> Option<usize> {
    let body = http::get(endpoint, "/props").ok()?.read_to_string().ok()?;
    let at = body.find("\"total_slots\"")?;
    let rest = &body[at + "\"total_slots\"".len()..];
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
pub fn matches(served: &str, want: &str) -> bool {
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

    /// **The media marker is read out of a `/props` body, and its absence is an answer.**
    ///
    /// The value is the server's own and it is randomised per process — MEASURED 2026-09-27:
    /// `<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>`. A test cannot pin the live string, so it
    /// pins the two things that decide behaviour: a server that states one is read correctly (the
    /// brackets and underscores survive, and the key is not confused with a longer one), and a
    /// server that states none is `None` rather than an empty marker that would silently place
    /// nothing where the picture should be.
    #[test]
    fn the_media_marker_is_read_out_of_a_props_body_or_absent() {
        let live = r#"{"modalities":{"vision":true},"media_marker":"<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>","n_ctx":262144}"#;
        assert_eq!(
            field(live, "\"media_marker\"").as_deref(),
            Some("<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>")
        );
        // An endpoint with no marker at all — a metered provider, or a server without `mtmd`.
        let visionless = r#"{"modalities":{"vision":false},"n_ctx":8192}"#;
        assert_eq!(field(visionless, "\"media_marker\""), None);
        // An empty marker is no marker: mtmd refuses it, so a caller must not treat it as one.
        assert_eq!(field(r#"{"media_marker":""}"#, "\"media_marker\""), None);
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

#[cfg(test)]
mod skip_tests {
    use super::*;

    /// **The distinction, tested directly**, because neither state can be arranged
    /// on a box that HAS a server: it is set up with an endpoint on a port nothing
    /// listens on, and the difference is made by the environment alone.
    #[test]
    fn a_configured_endpoint_that_refuses_is_a_failure_and_an_unconfigured_one_skips() {
        // Port 9 (discard) is the traditional nothing-listens-here.
        let dead = Endpoint::new("127.0.0.1", 9);
        let saved = std::env::var_os("LETIBOT_COMPLETION_URL");

        // Configured: the promise was made, so silence is a failure.
        unsafe { std::env::set_var("LETIBOT_COMPLETION_URL", "http://127.0.0.1:9") };
        let configured_blew =
            std::panic::catch_unwind(|| skip_live_test(&dead, "This test")).is_err();

        // Not configured: nobody promised anything, so silence is absent apparatus.
        unsafe { std::env::remove_var("LETIBOT_COMPLETION_URL") };
        let unconfigured_skipped = skip_live_test(&dead, "This test");

        unsafe {
            match saved {
                Some(v) => std::env::set_var("LETIBOT_COMPLETION_URL", v),
                None => std::env::remove_var("LETIBOT_COMPLETION_URL"),
            }
        }

        assert!(
            configured_blew,
            "a CONFIGURED endpoint that refuses must fail — otherwise the skip becomes \
             the place a real failure hides"
        );
        assert!(
            unconfigured_skipped,
            "an UNCONFIGURED endpoint that refuses must skip — that is a runner, a fresh \
             clone, or a laptop"
        );
    }
}
