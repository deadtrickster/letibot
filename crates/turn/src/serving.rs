//! What the server on the other end is actually serving, asked before a test
//! blames the tokenizer.
//!
//! # The failure this exists to stop being opaque
//!
//! The live tests in this workspace tokenise locally, against a GGUF named in the
//! config, and post token ids. Three model services on this box are singletons
//! that evict each other and have shared a port, so the model behind `:8080` is
//! whichever one was started last. Point qwen's vocabulary at GLM and the ids are
//! simply out of range - qwen-3.8-flash-next has 248,320 tokens and GLM-5.3-Flash
//! has 154,880 - and the server answers:
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
//! panics, and it panics with the served model, the expected model, and the one
//! command that fixes it.

use crate::http::{self, Endpoint};
use letibot_transcript::text::clip_bytes;

/// The model alias the server reports, or why the question could not be answered. `/props` is a
/// llama.cpp endpoint and the alias is what `--alias` set, falling back to the GGUF's own name.
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
        clip_bytes(&body, 200)
    ))
}

/// Is a model server answering at this endpoint? - the one question a test asks before it
/// spends a turn, so a live test SKIPS with a sentence rather than failing with a stack trace
/// and aborting every target after it. `SKIPPED ... THIS IS NOT A PASS` is the phrasing.
pub fn reachable(endpoint: &Endpoint) -> bool {
    http::get(endpoint, "/props").is_ok()
}

/// Did somebody SAY where the model is? The distinction is between two states that look
/// identical at the socket: configured and refusing, which is a failure, and never meant to
/// have one, which is absent apparatus and skips.
pub fn endpoint_is_configured() -> bool {
    std::env::var_os("LETIBOT_COMPLETION_URL").is_some()
}

/// Should this live test skip? `true` means nothing answered and the caller returns early; the
/// line is printed here so the wording cannot drift. `LETIBOT_REQUIRE_MODEL` refuses the skip,
/// because a skip that cannot be refused is a suite that quietly stops testing.
pub fn skip_live_test(endpoint: &Endpoint, what: &str) -> bool {
    if reachable(endpoint) {
        return false;
    }
    let required = std::env::var("LETIBOT_REQUIRE_MODEL")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    skip_unreachable(endpoint, what, endpoint_is_configured(), required)
}

/// [`skip_live_test`]'s decision once nothing has answered, with the two environment readings
/// handed in rather than read: a `set_var` in one test is seen by every other test in the binary,
/// and a `setenv` beside a C-side `getenv` is undefined behaviour.
fn skip_unreachable(endpoint: &Endpoint, what: &str, configured: bool, required: bool) -> bool {
    // A configured endpoint that refuses is a failure, not a skip: nothing answers either way,
    // so the socket cannot tell the two apart, and what can is whether the variable was SET.
    if configured {
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

/// The server's context window, from `/props`, or `None` when it does not say. The wall is read
/// rather than assumed, and `None` is a real answer: a metered provider has no `/props` and its
/// window is the operator's to state, so a caller must not invent a number.
pub fn served_ctx(endpoint: &Endpoint) -> Option<u64> {
    let body = http::get(endpoint, "/props").ok()?.read_to_string().ok()?;
    // `n_ctx` sits inside `default_generation_settings`, and it is a number rather
    // than a string, so `field` - which reads quoted values - cannot serve.
    let at = body.find("\"n_ctx\"")?;
    let rest = &body[at + "\"n_ctx\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// The token a multimodal prompt must place where its image goes, from `/props`:
///
/// ```text
/// /props → media_marker: "<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>"
/// ```
///
/// It is randomised per server instance and is not any of the model's own tokens, so a hardcoded
/// one works against one process and fails against the next. `None` means no images on this
/// endpoint, which the caller discloses rather than guessing at a marker.
pub fn served_media_marker(endpoint: &Endpoint) -> Option<String> {
    let body = http::get(endpoint, "/props").ok()?.read_to_string().ok()?;
    field(&body, "\"media_marker\"").filter(|m| !m.is_empty())
}

/// How many sequences this server decodes at once, from `/props`. Slots are its batching unit and
/// each has its own cache, so N concurrent requests are cheaper together than serialised - a
/// caller "protecting the cache" has the relationship backwards.
pub fn served_slots(endpoint: &Endpoint) -> Option<usize> {
    let body = http::get(endpoint, "/props").ok()?.read_to_string().ok()?;
    let at = body.find("\"total_slots\"")?;
    let rest = &body[at + "\"total_slots\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// Refuse, loudly and by name, unless the server is serving `want`. `want` is matched as a
/// substring of the reported path, because the alias is `qwen-3.8-flash-next` and the path is
/// the GGUF's file name.
pub fn expect(endpoint: &Endpoint, want: &str) {
    let served = match served_model(endpoint) {
        Ok(s) => s,
        // Not reachable is the live tests' own business - they fail on the request
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

/// What one `/props` answer says about the model a server is serving: the GGUF path, the chat
/// template it renders with, and the media marker, from ONE probe. Every field is `Option`, and
/// each absence is its own fact - the caller refuses naming the missing thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedProps {
    /// The GGUF the server loaded, as it reports it - `…/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf`.
    pub model_path: Option<String>,
    /// The chat template the server reports, verbatim, with the final newline stripped: a
    /// comparison against a shipped template therefore trims trailing whitespace on both sides,
    /// and anything else is a different template.
    pub chat_template: Option<String>,
    /// The per-process media marker, when the server has `mtmd`.
    pub media_marker: Option<String>,
}

/// Ask one endpoint's `/props` for [`ServedProps`], or name why it could not. The error carries
/// the address, which is what an operator with three units needs; the connect timeout keeps a
/// dead port to seconds rather than three minutes.
pub fn served_props(endpoint: &Endpoint) -> Result<ServedProps, String> {
    let body = http::get(endpoint, "/props")
        .map_err(|e| format!("{} did not answer /props ({e})", endpoint.authority()))?
        .read_to_string()
        .map_err(|e| {
            format!(
                "{} answered /props but the body could not be read ({e})",
                endpoint.authority()
            )
        })?;
    Ok(ServedProps {
        model_path: field(&body, "\"model_path\"").or_else(|| field(&body, "\"model\"")),
        // Through a JSON string reader, not `field`: a Jinja template is full of `"` and
        // newlines, which a JSON encoder escapes, and `field` reads to the next `"` byte. A
        // value that is not a string is an absent template rather than a parse error.
        chat_template: json_string(&body, "\"chat_template\""),
        media_marker: field(&body, "\"media_marker\""),
    })
}

/// Does this served chat template match this dialect's shipped one? On the bytes, with trailing
/// whitespace trimmed on both sides - the one divergence measured between `/props` and the
/// GGUF. Anything beyond that trim is a different template, not a match.
pub fn template_matches(spec: &letibot_dialect::DialectSpec, served: &str) -> bool {
    spec.template.trim_end() == served.trim_end()
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

/// One JSON string value, read as its encoder wrote it - escapes decoded - or `None` when the
/// key is absent or its value is not a string. This is [`field`] for values that may CONTAIN
/// quotes, and the escape set is RFC 8259's, surrogate pairs folded.
fn json_string(body: &str, key: &str) -> Option<String> {
    let at = body.find(key)? + key.len();
    let rest = body.get(at..)?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let mut rest = rest.strip_prefix('"')?;
    let mut out = String::with_capacity(rest.len());
    loop {
        let end = rest.find(['\"', '\\'])?;
        out.push_str(&rest[..end]);
        rest = &rest[end..];
        let mut esc = rest.chars();
        let c = esc.next()?;
        rest = esc.as_str();
        match c {
            // The terminator: an unescaped quote. `c` consumed it, so there is
            // nothing to strip - `rest` already points past the value.
            '"' => return (!out.is_empty()).then_some(out),
            '\\' => {
                let e = rest.chars().next()?;
                rest = &rest[e.len_utf8()..];
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex = rest.get(..4)?;
                        let mut n = u32::from_str_radix(hex, 16).ok()?;
                        rest = &rest[4..];
                        // A high surrogate is only half a scalar: the next `\uXXXX` must be its
                        // low half or the pair is malformed, and a broken pair is replaced rather
                        // than failed, so the byte difference is the mismatch it should be.
                        if (0xD800..0xDC00).contains(&n)
                            && let Some(tail) = rest.strip_prefix("\\u")
                            && let Ok(low) = u32::from_str_radix(tail.get(..4)?, 16)
                            && (0xDC00..0xE000).contains(&low)
                        {
                            n = 0x10000 + ((n - 0xD800) << 10) + (low - 0xDC00);
                            rest = &tail[4..];
                        }
                        out.push(char::from_u32(n).unwrap_or('\u{FFFD}'));
                    }
                    // Not an escape RFC 8259 allows: the value is malformed, and a
                    // malformed value is an absent one rather than a guess.
                    _ => return None,
                }
            }
            _ => unreachable!("find stopped on a byte that is neither"),
        }
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

    /// The media marker is read out of a `/props` body, and its absence is an answer: a server
    /// that states one is read correctly, and one that states none is `None` rather than an
    /// empty marker that would place nothing where the picture should be.
    #[test]
    fn the_media_marker_is_read_out_of_a_props_body_or_absent() {
        let live = r#"{"modalities":{"vision":true},"media_marker":"<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>","n_ctx":262144}"#;
        assert_eq!(
            field(live, "\"media_marker\"").as_deref(),
            Some("<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>")
        );
        // An endpoint with no marker at all - a metered provider, or a server without `mtmd`.
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

    /// A served chat template survives the JSON it arrives in, quotes and all. Reading it with
    /// [`field`] would return the 705 bytes before the first escaped quote and call it the
    /// document, so every dialect comparison would fail against a template never served.
    #[test]
    fn a_served_chat_template_survives_the_json_it_arrives_in() {
        // A Jinja fragment with every byte class the reader must survive: escaped
        // quotes, a backslash, newlines, tabs, and one `\\u` scalar.
        let served = "{%- if tools %}\n{{- \"tool calls\" \\ \"here\" -}}\n\t\u{2014} -}\n";
        let body = format!(
            "{{\"model_path\":\"/m/glm.gguf\",\"chat_template\":\"{}\"}}",
            served
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\t', "\\t")
                .replace('\u{2014}', "\\u2014")
        );
        assert_eq!(
            json_string(&body, "\"chat_template\"").as_deref(),
            Some(served)
        );
        // The terminator is the UNescaped quote: an escaped one is content.
        assert_eq!(
            json_string(
                r#"{"chat_template":"a \"quoted\" word"}"#,
                "\"chat_template\""
            )
            .as_deref(),
            Some("a \"quoted\" word")
        );
    }

    /// A `chat_template` that is not a string is an absent one, not a parse error: a newer
    /// llama.cpp can answer the variants as an array, and absent is the honest reading, so the
    /// caller refuses naming the missing template.
    #[test]
    fn a_chat_template_that_is_not_a_string_is_absent() {
        assert_eq!(
            json_string(r#"{"chat_template":[]}"#, "\"chat_template\""),
            None
        );
        assert_eq!(
            json_string(r#"{"chat_template":42}"#, "\"chat_template\""),
            None
        );
        assert_eq!(
            json_string(r#"{"chat_template":""}"#, "\"chat_template\""),
            None
        );
        assert_eq!(json_string(r#"{"n_ctx":8192}"#, "\"chat_template\""), None);
        // A malformed escape is a malformed value: absent rather than half-read.
        assert_eq!(
            json_string(r#"{"chat_template":"a\qb"}"#, "\"chat_template\""),
            None
        );
    }
}

#[cfg(test)]
mod ctx_tests {
    /// `n_ctx` is a NUMBER nested inside `default_generation_settings`, so the
    /// quoted-string reader used for `model_path` cannot find it - the parse is
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

    /// The distinction, tested directly, because neither state can be arranged on a box that HAS
    /// a server: the difference is made by the configuration alone, handed to
    /// [`skip_unreachable`] as a value rather than set in the environment.
    #[test]
    fn a_configured_endpoint_that_refuses_is_a_failure_and_an_unconfigured_one_skips() {
        // Port 9 (discard) is the traditional nothing-listens-here.
        let dead = Endpoint::new("127.0.0.1", 9);
        assert!(!reachable(&dead), "something answers on port 9");

        // Configured: the promise was made, so silence is a failure.
        let configured_blew =
            std::panic::catch_unwind(|| skip_unreachable(&dead, "This test", true, false)).is_err();

        // Not configured: nobody promised anything, so silence is absent apparatus.
        let unconfigured_skipped = skip_unreachable(&dead, "This test", false, false);

        // And a run that requires the model refuses the skip even then.
        let required_blew =
            std::panic::catch_unwind(|| skip_unreachable(&dead, "This test", false, true)).is_err();
        assert!(required_blew, "LETIBOT_REQUIRE_MODEL must refuse the skip");

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
