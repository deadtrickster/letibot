//! The token core: vocabulary, tokenizer, token ledger and store.
//!
//! This is the crate where a conversation stops being text and becomes the
//! thing the server actually caches. Under §3.1 the harness renders and
//! tokenizes for itself, so the artefact that has to be append-only is not a
//! byte string it *hopes* a remote jinja renderer preserves -- it is one growing
//! `Vec<llama_token>` that both sides address by offset and length.
//!
//! Four pieces, in the order a turn goes through them:
//!
//! * [`vocab::Vocab`] -- a `vocab_only` GGUF load. No weights, no GPU, no
//!   server: measured at 0.6 s against shard 1 of a six-way split, with the
//!   other five shards never opened.
//! * [`control`] -- resolves a dialect's control literals to exact ids **at
//!   startup**, and turns `RenderSpan`s into tokens across the one seam where
//!   text and control tokens must not be able to become each other.
//! * [`ledger::TokenLedger`] -- the rows, the hash chain, and the memfd region
//!   whose shape makes a prefix violation blocked rather than merely
//!   detectable.
//! * [`store::Store`] -- the durable copy, with the append-only rule restated as
//!   SQL triggers.
//!
//! # The one thing to keep in mind while changing this crate
//!
//! Every safety property here is structural, and each one is written where it
//! cannot be argued with:
//!
//! | the property | where it lives |
//! |---|---|
//! | user text cannot become a control token | two functions, one with `parse_special` permanently off ([`vocab`]) |
//! | a missing control token is a startup failure | [`control::resolve`], which reports *all* of them |
//! | the token file cannot shrink | `F_SEAL_SHRINK`, in the kernel ([`region`]) |
//! | a reader cannot write to it | an `O_RDONLY` reopen; `PROT_WRITE` gets `EACCES` ([`region`]) |
//! | a request cannot start anywhere but 0 | [`ledger::PromptSpan`] has no offset field |
//! | history cannot be rewritten in this process | one mutation, pinned by a test that reads its own source |
//! | history cannot be rewritten in the database | `BEFORE UPDATE/DELETE/INSERT` triggers ([`store`]) |
//! | a rewritten store cannot be replayed | the chain is re-verified on restore |
//!
//! If a change makes one of those a matter of calling the right function, it has
//! undone the point of the strand rather than refactored it.

pub mod control;
mod ffi;
pub mod ledger;
pub mod region;
pub mod store;
pub mod vocab;

pub use control::{
    ControlMap, ControlResolveError, VocabDecoder, resolve, resolve_stops, tokenize_spans,
};
pub use ledger::{LedgerRow, PromptSpan, TokenLedger, chain, hash_tokens};
pub use region::TokenRegion;
pub use store::{SessionRecord, StablePrefixRecord, Store};
/// **The SQL driver, re-exported.** `Store::connection` already hands out a
/// `&rusqlite::Connection`, so the type is public API here whether or not the
/// name is. A caller that needs to write a query against it — the daemon's
/// corpus reader does — would otherwise take its own `rusqlite` dependency, and
/// two versions of it in one tree makes `&Connection` and `&Connection` two
/// unrelated types with one spelling.
pub use rusqlite;
pub use vocab::{ResolveCause, TokenId, Vocab};

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_dialect::{
        ControlRole, ControlToken, ControlTokens, RenderSpan, StablePrefix, StopToken,
        TokenDecoder, spans_to_string,
    };
    use letibot_transcript::{TranscriptItem, UserPart};
    use std::path::{Path, PathBuf};

    /// The GGUF the FFI tests load a vocabulary out of.
    ///
    /// Deliberately **not** skipped when it is missing. A tokenizer test that
    /// quietly passes on a box with no model file is a test that reports the
    /// health of `std::fs::exists`, and this fleet has already paid for the
    /// difference between a liveness indicator and the fact it stands in for. If
    /// the default is wrong for your box, set `LETIBOT_VOCAB_GGUF`.
    fn vocab_path() -> PathBuf {
        let p = std::env::var("LETIBOT_VOCAB_GGUF").unwrap_or_else(|_| {
            "/home/dead/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf"
                .to_string()
        });
        let p = PathBuf::from(p);
        assert!(
            p.is_file(),
            "no vocabulary GGUF at {}. Set LETIBOT_VOCAB_GGUF to one; for a split \
             model pass the first shard.",
            p.display()
        );
        p
    }

    fn vocab() -> Vocab {
        Vocab::load(&vocab_path()).unwrap()
    }

    // --- the vocabulary loads at all -------------------------------------

    #[test]
    fn vocab_only_load_needs_no_weights_and_no_server() {
        let v = vocab();
        assert!(v.n_tokens() > 1000, "loaded {} tokens", v.n_tokens());
        assert!(v.bos().is_some());
        assert!(v.eos().is_some());
    }

    /// **Does a space survive tokenise → detokenise?**
    ///
    /// Asked 2026-09-15 because the operator reports the same model editing files
    /// fine in opencode and failing here on dropped spaces — `self.x!= y` for a
    /// file holding `self.x != y`. letibot is token-native where opencode sends
    /// text, so this round trip is the one step opencode does not have, which
    /// makes it the first place to look rather than the last.
    ///
    /// NOT YET RUN: every test in this module loads a vocabulary, and a
    /// vocab-only load still initialises the CUDA backend, which aborts while the
    /// model servers hold the cards (97 of 98 GiB on both, 2026-09-15). So this
    /// is written and unverified, and saying so is the point.
    #[test]
    fn spaces_survive_the_token_round_trip() {
        let v = vocab();
        for text in [
            "if self.session_id != s.session_id {",
            "if !text.is_empty() && x != y {",
            "v.filter(|l| !l.trim().is_empty())",
        ] {
            let ids = v.tokenize_text(text).expect("tokenize");
            let back = v.detokenize(&ids, false).expect("detokenize");
            assert_eq!(back, text, "the round trip changed the text");
        }
    }

    /// Which side of the round trip loses the space — and it decides whether the
    /// model is affected at all. If `tokenize` maps `x != y` and `x!= y` to the
    /// SAME ids, the information is gone before the prompt is built and the model
    /// is genuinely shown the wrong text. If the ids differ, tokenize is faithful
    /// and only `detokenize` is lossy, which is a display bug and reaches nothing.
    /// **A character whose bytes straddle two tokens.** The case the first
    /// version of the piece-based `detokenize` did not survive: it decoded each
    /// token on its own, so both halves of a multi-byte character were invalid
    /// UTF-8 and any message containing one became `<undecodable N token(s)>`.
    /// Six transcript rows on the operator's box before it was caught.
    ///
    /// The lesson is narrow and worth keeping: the fidelity fix needed `piece`
    /// instead of `llama_detokenize`; it never needed per-token DECODING, and
    /// per-token decoding is what broke it.
    #[test]
    fn multibyte_characters_survive_detokenizing() {
        let v = vocab();
        for text in [
            "héllo wörld",
            "日本語のテキスト",
            "emoji: 🙂 and ✅ and 🚀",
            "Кириллица и ещё немного текста",
            "math: ∀x ∈ ℝ, x² ≥ 0",
            "mixed: if x != y { \"señor\" } // ✓",
        ] {
            let ids = v.tokenize_text(text).expect("tokenize");
            let back = v.detokenize(&ids, false).unwrap_or_else(|e| {
                panic!("{text:?} came back undecodable ({e:?}) — a character split across tokens")
            });
            assert_eq!(back, text, "the round trip changed {text:?}");
        }
    }

    #[test]
    fn which_half_of_the_round_trip_eats_the_space() {
        let v = vocab();
        let with = v.tokenize_text("if self.session_id != s.session_id {").unwrap();
        let without = v.tokenize_text("if self.session_id!= s.session_id {").unwrap();
        eprintln!("with space:    {with:?}");
        eprintln!("without space: {without:?}");
        eprintln!("detok(with)    = {:?}", v.detokenize(&with, false).unwrap());
        eprintln!("detok(without) = {:?}", v.detokenize(&without, false).unwrap());
        assert_ne!(
            with, without,
            "TOKENIZE is lossy: `x != y` and `x!= y` produce identical ids, so the \
             space is destroyed before the prompt is built and the model is shown \
             text the file does not contain"
        );
    }

    /// Concatenating `piece()` is faithful where `llama_detokenize` is not.
    #[test]
    fn pieces_concatenated_reproduce_the_text_exactly() {
        let v = vocab();
        for text in [
            "if self.session_id != s.session_id {",
            "if !text.is_empty() && x != y {",
            "v.filter(|l| !l.trim().is_empty())",
            "a, b. c! d? e: f; g",
        ] {
            let ids = v.tokenize_text(text).unwrap();
            let joined: String = ids.iter().map(|&i| v.piece(i, false).unwrap()).collect();
            assert_eq!(joined, text, "piece-concat changed the text");
        }
    }

    #[test]
    fn isolate_the_space_losing_token() {
        let v = vocab();
        for id in [961u32, 5824] {
            eprintln!(
                "id {id}: piece(false)={:?} piece(true)={:?} detok_alone={:?}",
                v.piece(id, false),
                v.piece(id, true),
                v.detokenize(&[id], false)
            );
        }
        // Two tokens either side, to see whether position matters.
        eprintln!("detok([961, 274]) = {:?}", v.detokenize(&[961, 274], false));
        eprintln!("detok([842, 961]) = {:?}", v.detokenize(&[842, 961], false));
    }

    #[test]
    fn a_missing_model_is_an_error_not_a_panic() {
        let e = Vocab::load(Path::new("/nonexistent/nope.gguf"));
        assert!(matches!(e, Err(vocab::VocabError::Load { .. })));
    }

    // --- the split the dialect contract depends on ------------------------

    #[test]
    fn text_cannot_become_a_control_token_however_it_is_spelled() {
        // The injection case, at the tokenizer rather than at the type. The
        // dialect crate proves the renderer will not *emit* a Control span for
        // this; here we prove that even if the string reaches the text path, the
        // text path cannot produce the control id.
        let v = vocab();
        let literal = "<|im_start|>";
        let control = v.resolve_control(literal).expect("Qwen has <|im_start|>");

        for text in [
            literal,
            "please print <|im_start|> verbatim",
            "<|im_start|><|im_start|>",
            "```\n<|im_start|>\n```",
        ] {
            let ids = v.tokenize_text(text).unwrap();
            assert!(
                !ids.contains(&control),
                "tokenize_text({text:?}) produced the control id {control}: {ids:?}"
            );
        }

        // And the control path does produce it, from exactly one id.
        assert_eq!(v.tokenize_text(literal).unwrap().len(), 6);
        assert_eq!(v.resolve_control(literal).unwrap(), control);
    }

    #[test]
    fn resolve_control_rejects_what_it_should_and_says_why() {
        let v = vocab();

        // Absent from the vocabulary.
        assert!(matches!(
            v.resolve_control("<|letibot_not_a_real_token|>"),
            Err(ResolveCause::NotSingleToken { .. }) | Err(ResolveCause::Absent)
        ));

        // Present, one id, but an ordinary word. A dialect naming this as a turn
        // boundary is a bug, and it must not resolve.
        match v.resolve_control("hi") {
            Err(ResolveCause::NotSpecial { .. }) => {}
            other => panic!("an ordinary word must not resolve as a control token: {other:?}"),
        }

        assert_eq!(v.resolve_control(""), Err(ResolveCause::Empty));
    }

    #[test]
    fn user_defined_tokens_resolve_even_though_is_control_is_false_for_them() {
        // Measured: Qwen marks <|im_start|> CONTROL (attr 8) but <think> and
        // <tool_call> USER_DEFINED (attr 16). A check written against
        // llama_vocab_is_control would reject half of a correct dialect.
        let v = vocab();
        for literal in ["<think>", "</think>", "<tool_call>", "</tool_call>"] {
            let id = v
                .resolve_control(literal)
                .unwrap_or_else(|e| panic!("{literal} did not resolve: {e}"));
            assert_eq!(v.token_text(id).unwrap(), literal);
        }
    }

    const QWEN_CONTROLS: &[ControlToken] = &[
        ControlToken::borrowed(ControlRole::TurnStartUser, "<|im_start|>"),
        ControlToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
        ControlToken::borrowed(ControlRole::ThinkOpen, "<think>"),
        ControlToken::borrowed(ControlRole::ThinkClose, "</think>"),
        ControlToken::borrowed(ControlRole::ToolCallOpen, "<tool_call>"),
        ControlToken::borrowed(ControlRole::ToolCallClose, "</tool_call>"),
    ];

    fn qwen() -> ControlTokens {
        ControlTokens::borrowed(QWEN_CONTROLS)
    }

    /// The span for a role, for the stand-in renderer below.
    ///
    /// It asserts the assumption it depends on instead of relying on it: ChatML
    /// spells each of these exactly one way, so "the token for this role" is a
    /// well-formed question here. `ControlTokens` no longer answers it in
    /// general, because for GLM it was not one -- one role owned eight literals
    /// and `get(role)` returned whichever the table happened to list first.
    fn ctl(role: ControlRole) -> RenderSpan {
        let tokens = qwen();
        let mut all = tokens.all_with_role(role);
        let token = all
            .next()
            .unwrap_or_else(|| panic!("ChatML has no token for {role:?}"))
            .clone();
        assert!(
            all.next().is_none(),
            "{role:?} has several literals; this renderer must name the one it means"
        );
        RenderSpan::Control(token)
    }

    #[test]
    fn resolving_a_whole_dialect_reports_every_failure_at_once() {
        let v = vocab();
        let map = resolve(&v, &qwen()).expect("all six exist");
        assert_eq!(map.len(), 6);
        assert_eq!(map.ids_for_role(ControlRole::ThinkOpen).len(), 1);
        assert!(map.ids_for_role(ControlRole::TurnStartTool).is_empty());
        let im_end = map.ids_for_role(ControlRole::TurnEnd)[0];
        assert!(map.is_control_id(im_end));
        // The reverse direction, which is what a parser reads.
        assert_eq!(map.role_of(im_end), Some(ControlRole::TurnEnd));
        assert_eq!(map.id("<|im_end|>"), Some(im_end));

        const BROKEN: &[ControlToken] = &[
            ControlToken::borrowed(ControlRole::TurnStartUser, "<|im_start|>"),
            ControlToken::borrowed(ControlRole::ThinkOpen, "<|no_such_thing|>"),
            ControlToken::borrowed(ControlRole::ThinkClose, "<|also_missing|>"),
            ControlToken::borrowed(ControlRole::TurnEnd, "hi"),
        ];
        let err =
            resolve(&v, &ControlTokens::borrowed(BROKEN)).expect_err("three of four are broken");
        assert_eq!(
            err.failures.len(),
            3,
            "a startup failure must name every broken literal, not the first: {err}"
        );
        let text = err.to_string();
        assert!(text.contains("<|no_such_thing|>") && text.contains("hi"), "{text}");
    }

    #[test]
    fn a_failure_listing_is_ordered_by_role_not_by_hashing() {
        // `ControlRole: Ord` exists for exactly this: two runs against the same
        // broken vocabulary must print the same report, so a diff of two startup
        // logs is about the vocabulary. Declared here in a deliberately unsorted
        // order.
        let v = vocab();
        const BROKEN: &[ControlToken] = &[
            ControlToken::borrowed(ControlRole::ToolCallClose, "<|missing_c|>"),
            ControlToken::borrowed(ControlRole::TurnStartUser, "<|missing_a|>"),
            ControlToken::borrowed(ControlRole::ThinkOpen, "<|missing_b|>"),
        ];
        let err = resolve(&v, &ControlTokens::borrowed(BROKEN)).expect_err("all three are broken");
        let roles: Vec<ControlRole> = err.failures.iter().map(|f| f.role).collect();
        assert_eq!(
            roles,
            [
                ControlRole::TurnStartUser,
                ControlRole::ThinkOpen,
                ControlRole::ToolCallClose
            ]
        );
    }

    #[test]
    fn a_stop_token_that_is_really_a_sequence_fails_at_startup_and_names_the_boundary() {
        // The whole argument for `StopToken` carrying a role. A stop literal that
        // is not one vocabulary entry never fires, and the turn then runs to
        // n_ctx -- a failure that costs a whole context window and says nothing.
        // Resolving it here costs one startup and names what was lost.
        let v = vocab();
        let good = [
            StopToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
            StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
        ];
        let ids = resolve_stops(&v, &good).expect("Qwen has both");
        assert_eq!(ids.len(), 2);

        let bad = [StopToken::borrowed(
            ControlRole::TurnStartTool,
            "<|observation|>",
        )];
        let err = resolve_stops(&v, &bad).expect_err("Qwen has no <|observation|>");
        assert_eq!(err.failures[0].role, ControlRole::TurnStartTool);
        let text = err.to_string();
        assert!(
            text.contains("TurnStartTool") && text.contains("<|observation|>"),
            "a stop-token failure must say which boundary was lost: {text}"
        );
    }

    #[test]
    fn the_decoder_a_parser_needs_is_the_one_thing_with_a_vocabulary() {
        // `parse(&[u32])` was unimplementable because the dialect crate has no
        // vocab and must return Content(String). `TokenDecoder` is the seam, and
        // this is its only real implementation: ids in, the exact rendered bytes
        // back out, plus the role of every id that is a boundary.
        let v = vocab();
        let map = resolve(&v, &qwen()).unwrap();
        let decoder = VocabDecoder::new(&v, &map);

        let spans = vec![
            ctl(ControlRole::TurnStartUser),
            RenderSpan::Text("user\nis <|im_end|> a boundary?\n".into()),
            ctl(ControlRole::TurnEnd),
        ];
        let toks = tokenize_spans(&v, &map, &spans).unwrap();

        // Ids the decoder calls boundaries are exactly the ids the Control spans
        // produced -- the pasted literal in the text is several ordinary tokens
        // and none of them answers a role.
        let boundaries: Vec<ControlRole> = toks
            .iter()
            .filter_map(|&t| decoder.control_role(t))
            .collect();
        assert_eq!(
            boundaries,
            [ControlRole::TurnStartUser, ControlRole::TurnEnd],
            "user text spelling <|im_end|> must not decode as a boundary"
        );

        // And the text between them comes back byte for byte.
        let first = toks
            .iter()
            .position(|&t| decoder.control_role(t) == Some(ControlRole::TurnEnd))
            .unwrap();
        assert_eq!(
            decoder.decode(&toks[1..first]),
            "user\nis <|im_end|> a boundary?\n"
        );
        assert_eq!(decoder.decode(&toks), spans_to_string(&spans));
    }

    // --- a stand-in dialect, so the seam can be exercised ------------------

    /// A ChatML renderer, in this test module only.
    ///
    /// W4 owns the real ones. This exists so the *seam* -- render to spans,
    /// tokenize the spans, append to the ledger -- can be tested end to end
    /// before a real dialect lands, and so the round trip below is over
    /// something with control tokens in it rather than over synthetic ids.
    fn render(prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        let mut spans = Vec::new();
        spans.push(ctl(ControlRole::TurnStartUser));
        spans.push(RenderSpan::Text(format!("system\n{}\n", prefix.system)));
        for tool in &prefix.tools_json {
            spans.push(RenderSpan::Text(format!("{tool}\n")));
        }
        spans.push(ctl(ControlRole::TurnEnd));
        for item in items {
            spans.extend(render_item(item));
        }
        spans
    }

    fn render_item(item: &TranscriptItem) -> Vec<RenderSpan> {
        match item {
            // §4.2: renders to nothing.
            TranscriptItem::SegmentMark { .. } => vec![],
            TranscriptItem::System { text, .. } => vec![
                ctl(ControlRole::TurnStartUser),
                RenderSpan::Text(format!("system\n{text}\n")),
                ctl(ControlRole::TurnEnd),
            ],
            TranscriptItem::User { parts, .. } => {
                let mut body = String::from("user\n");
                for part in parts {
                    if let UserPart::Text { text } = part {
                        body.push_str(text);
                    }
                }
                body.push('\n');
                vec![
                    ctl(ControlRole::TurnStartUser),
                    RenderSpan::Text(body),
                    ctl(ControlRole::TurnEnd),
                ]
            }
            TranscriptItem::Reasoning { text, .. } => vec![
                ctl(ControlRole::ThinkOpen),
                RenderSpan::Text(text.clone()),
                ctl(ControlRole::ThinkClose),
            ],
            TranscriptItem::Assistant { text, tool_calls, .. } => {
                let mut spans = vec![
                    ctl(ControlRole::TurnStartUser),
                    RenderSpan::Text(format!("assistant\n{text}")),
                ];
                for call in tool_calls {
                    spans.push(ctl(ControlRole::ToolCallOpen));
                    spans.push(RenderSpan::Text(format!(
                        "{{\"name\": \"{}\", \"arguments\": {}}}",
                        call.name, call.arguments
                    )));
                    spans.push(ctl(ControlRole::ToolCallClose));
                }
                spans.push(ctl(ControlRole::TurnEnd));
                spans
            }
            TranscriptItem::ToolResult { name, payload, .. } => vec![
                ctl(ControlRole::TurnStartUser),
                RenderSpan::Text(format!("tool\n{name}: {payload}\n")),
                ctl(ControlRole::TurnEnd),
            ],
        }
    }

    fn conversation(turns: usize) -> Vec<TranscriptItem> {
        let mut items = Vec::new();
        for k in 0..turns {
            items.push(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    // Adversarial content in half the turns: a user who pastes a
                    // control literal must not move a turn boundary.
                    text: if k % 2 == 0 {
                        format!("turn {k}: what does <|im_end|> mean?")
                    } else {
                        format!("turn {k}: ordinary text")
                    },
                }],
            });
            items.push(TranscriptItem::Reasoning {
                text: format!("thinking about turn {k}"),
                field: letibot_transcript::ReasoningField::Inline,
                truncated: false,
            });
            if k % 3 == 0 {
                items.push(TranscriptItem::SegmentMark {
                    segment_id: format!("s{k}"),
                    label: "l".into(),
                    kind: "k".into(),
                    edge: letibot_transcript::SegmentEdge::Open,
                });
            }
            items.push(TranscriptItem::Assistant {
                text: format!("answer {k}"),
                tool_calls: vec![],
                truncated: false,
            });
        }
        items
    }

    // --- the round trip the strand exists to guarantee --------------------

    #[test]
    fn tokenize_ledger_append_tokenize_is_always_a_strict_prefix_extension() {
        let v = vocab();
        let map = resolve(&v, &qwen()).unwrap();

        let prefix = StablePrefix {
            system: "You answer in the language of the question.".into(),
            tools_json: vec![r#"{"name":"read","access":"read"}"#.into()],
        };
        let prefix_tokens = tokenize_spans(&v, &map, &render(&prefix, &[])).unwrap();

        let mut ledger = TokenLedger::new("round-trip", &prefix_tokens).unwrap();
        let items = conversation(40);

        let mut spans_so_far: Vec<PromptSpan> = vec![ledger.span()];
        let mut tokens_so_far: Vec<Vec<TokenId>> = vec![ledger.tokens().to_vec()];

        for (k, item) in items.iter().enumerate() {
            let spans = render_item(item);
            let toks = tokenize_spans(&v, &map, &spans).unwrap();
            ledger.append(&format!("item-{k}"), &toks).unwrap();

            let now = ledger.span();
            let live = ledger.tokens();

            for (earlier, earlier_tokens) in spans_so_far.iter().zip(&tokens_so_far) {
                assert!(now.extends(earlier), "request {k} did not extend an earlier one");
                assert_eq!(earlier.offset(), 0);
                assert_eq!(
                    &live[..earlier_tokens.len()],
                    earlier_tokens.as_slice(),
                    "request {k} changed tokens an earlier request had already read"
                );
            }
            spans_so_far.push(now);
            tokens_so_far.push(live.to_vec());
        }

        ledger.verify_chain().unwrap();

        // Incremental and from-scratch must be the same token vector. This is the
        // fact the whole append-only design rests on, and it is why the harness
        // never re-renders: if these ever differ, the ledger is internally
        // consistent and wrong, which is the 612 GB failure with better
        // instrumentation (S1 in the work breakdown).
        let from_scratch = tokenize_spans(&v, &map, &render(&prefix, &items)).unwrap();
        assert_eq!(
            ledger.tokens(),
            from_scratch.as_slice(),
            "incremental tokenization diverged from a full render"
        );

        // A control literal pasted by the user did not become a boundary: the
        // number of <|im_end|> ids is exactly what the renderer emitted.
        let im_end = map.ids_for_role(ControlRole::TurnEnd)[0];
        let emitted = render(&prefix, &items)
            .iter()
            .filter(|s| matches!(s, RenderSpan::Control(c) if c.role == ControlRole::TurnEnd))
            .count();
        assert_eq!(
            ledger.tokens().iter().filter(|&&t| t == im_end).count(),
            emitted,
            "user text added a turn boundary"
        );
    }

    #[test]
    fn a_segment_mark_costs_no_tokens_and_still_changes_the_head() {
        let v = vocab();
        let map = resolve(&v, &qwen()).unwrap();
        let mark = TranscriptItem::SegmentMark {
            segment_id: "s".into(),
            label: "l".into(),
            kind: "k".into(),
            edge: letibot_transcript::SegmentEdge::Open,
        };
        assert!(render_item(&mark).is_empty());

        let mut ledger = TokenLedger::new("seg", &[1, 2, 3]).unwrap();
        let before = ledger.head();
        let toks = tokenize_spans(&v, &map, &render_item(&mark)).unwrap();
        assert!(toks.is_empty());
        ledger.append("mark", &toks).unwrap();
        assert_eq!(ledger.len(), 3);
        assert_ne!(ledger.head(), before);
    }

    #[test]
    fn detokenizing_our_spans_reproduces_the_rendered_string() {
        // The "ours" half of the /apply-template fidelity diff (§7.2, I3). W3
        // compares this string against the server's; here we only prove the
        // tokenizer is not the thing that would make them differ.
        let v = vocab();
        let map = resolve(&v, &qwen()).unwrap();
        let prefix = StablePrefix { system: "sys".into(), tools_json: vec!["{}".into()] };
        let items = conversation(6);
        let spans = render(&prefix, &items);

        let toks = tokenize_spans(&v, &map, &spans).unwrap();
        assert_eq!(v.detokenize(&toks, true).unwrap(), spans_to_string(&spans));
    }

    #[test]
    fn a_control_span_from_a_foreign_dialect_is_a_wiring_error() {
        let v = vocab();
        let map = resolve(&v, &qwen()).unwrap();
        const FOREIGN: ControlToken =
            ControlToken::borrowed(ControlRole::TurnStartTool, "<|observation|>");
        let err = tokenize_spans(&v, &map, &[RenderSpan::Control(FOREIGN)])
            .expect_err("a literal this map never resolved must not silently vanish");
        assert!(matches!(
            err,
            control::SpanTokenizeError::UnmappedControl { .. }
        ));
    }

    // --- the store, over real tokens --------------------------------------

    #[test]
    fn a_restart_replays_real_tokens_rather_than_re_rendering() {
        let v = vocab();
        let map = resolve(&v, &qwen()).unwrap();
        let prefix = StablePrefix {
            system: "sys".into(),
            tools_json: vec![r#"{"name":"read"}"#.into()],
        };
        let prefix_tokens = tokenize_spans(&v, &map, &render(&prefix, &[])).unwrap();

        let store = Store::open_in_memory().unwrap();
        let prefix_id = store
            .put_stable_prefix(&StablePrefixRecord {
                dialect_sha: "cc".repeat(32),
                system: prefix.system.clone(),
                tools_json: prefix.tools_json.clone(),
                h_init: hash_tokens(&prefix_tokens),
                tokens: prefix_tokens.clone(),
                vocab_source: v.source().to_string(),
            })
            .unwrap();
        store
            .put_session(&SessionRecord {
                id: "s".into(),
                title: None,
                model_id: "qwen".into(),
                dialect_sha: "cc".repeat(32),
                workspace_root: "/w".into(),
                owner: "deadtrickster".into(),
                role: None,
            approvers: vec![],
            parent_session_id: None,
            })
            .unwrap();
        store.put_transcript("t", "s", &prefix_id).unwrap();

        let mut ledger = TokenLedger::new("t", &prefix_tokens).unwrap();
        for (seq, item) in conversation(12).iter().enumerate() {
            let toks = tokenize_spans(&v, &map, &render_item(item)).unwrap();
            let row = ledger.append(&format!("i{seq}"), &toks).unwrap().clone();
            store.append_item("t", seq as u32, item, &row, &toks).unwrap();
        }

        // The daemon comes back up. Nothing is re-rendered and no vocabulary is
        // consulted -- only the persisted ids.
        let loaded = store.load_transcript("t").unwrap();
        let restored = TokenLedger::restore(
            "t-again",
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .unwrap();

        assert_eq!(restored.tokens(), ledger.tokens());
        assert_eq!(restored.span(), ledger.span());
        assert!(restored.span().extends(&ledger.span()) && ledger.span().extends(&restored.span()));
    }

    // --- the independent oracle -------------------------------------------

    /// Cross-check against the running server's `/tokenize`.
    ///
    /// This is a *check*, not a dependency: the FFI path above never touches the
    /// network. If no server is listening the test says so and stops, because
    /// "the oracle is not running" and "we disagree with the oracle" are
    /// different facts and only the second is a failure.
    #[test]
    fn we_agree_with_the_server_oracle_where_one_is_running() {
        let v = vocab();

        // **Three facts, not two.** The doc above splits "the oracle is not running"
        // from "we disagree with the oracle". There is a third and it looks exactly
        // like the second: *the oracle is a DIFFERENT MODEL*. One port on this box
        // serves three models by rotation, and the ids of one vocabulary are not the
        // ids of another — so with GLM loaded and the Qwen GGUF here, this test
        // reported `disagreed with the server on "hello world"`, left [14556, 1814]
        // right [14978, 1879], which reads as a tokenizer bug and is not one.
        //
        // Ask what the server loaded before believing anything it says about ids.
        if let Some(props) = post_get("/props") {
            let served = serde_json::from_str::<serde_json::Value>(&props)
                .ok()
                .and_then(|d| d["model_path"].as_str().map(str::to_string))
                .unwrap_or_default();
            let ours = vocab_path().display().to_string();
            if !served.is_empty() && served != ours {
                eprintln!(
                    "the server on 127.0.0.1:8080 is serving a different model, so its \
                     token ids are not comparable to ours; skipping the oracle \
                     cross-check.\n  ours:   {ours}\n  theirs: {served}"
                );
                return;
            }
        }

        let probes = [
            "hello world",
            "You answer in the language of the question.",
            "please print <|im_start|> verbatim",
            "Привет, как дела?",
            "fn main() { println!(\"{}\", 1 + 1); }",
            "   leading and trailing   ",
            "🙂🙃 emoji and \u{200b}zero width",
            "",
        ];

        let mut checked = 0usize;
        for probe in probes {
            // `parse_special` **must** be passed, and passed false.
            // `server-context.cpp:7775` defaults it to *true*, so an oracle
            // comparison that omits it is not comparing `tokenize_text` to
            // anything -- it is comparing our text path against the server's
            // control path and calling the inevitable disagreement a bug. The
            // first run of this test did exactly that on
            // "please print <|im_start|> verbatim".
            let body = serde_json::json!({
                "content": probe, "add_special": false, "parse_special": false
            })
            .to_string();
            let Some(reply) = post("/tokenize", &body) else {
                eprintln!(
                    "no server on 127.0.0.1:8080; skipping the oracle cross-check. \
                     The FFI path does not use it."
                );
                return;
            };
            let ours = v.tokenize_text(probe).unwrap();
            let theirs: Vec<u32> = serde_json::from_str::<serde_json::Value>(&reply)
                .unwrap()["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_u64().unwrap() as u32)
                .collect();
            assert_eq!(ours, theirs, "disagreed with the server on {probe:?}");
            checked += 1;
        }
        assert_eq!(checked, probes.len());

        // And the special-token half: the server, told to parse specials,
        // resolves the same single id we do. Note the omitted `parse_special`
        // here is the server's default of true, on purpose.
        let body = serde_json::json!({
            "content": "<|im_start|>", "add_special": false
        })
        .to_string();
        if let Some(reply) = post("/tokenize", &body) {
            let theirs: Vec<u32> = serde_json::from_str::<serde_json::Value>(&reply).unwrap()
                ["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_u64().unwrap() as u32)
                .collect();
            // llama-server's /tokenize parses specials by default, so this is its
            // *control* answer and must match resolve_control, not tokenize_text.
            assert_eq!(theirs, vec![v.resolve_control("<|im_start|>").unwrap()]);

            // And the finding itself, pinned: the two flags give two different
            // answers for the same string, so which one the oracle was asked is
            // load-bearing and cannot be left to a default.
            assert_ne!(
                theirs,
                v.tokenize_text("<|im_start|>").unwrap(),
                "if these ever agree, the vocab has changed and this test's premise \
                 -- that the two tokenizer paths are genuinely different -- is stale"
            );
        }
    }

    /// A four-line HTTP/1.1 POST. Enough for one JSON round trip against
    /// localhost, and it keeps a network client out of this crate's dependency
    /// list for a test the crate does not depend on.
    /// `GET`, for `/props`. Same framing tolerance as [`post`]; a `None` means
    /// "could not ask", which the caller must not read as "the answer was no".
    fn post_get(path: &str) -> Option<String> {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        let addr = std::env::var("LETIBOT_ORACLE").unwrap_or_else(|_| "127.0.0.1:8080".into());
        let mut s = TcpStream::connect(&addr).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let mut raw = String::new();
        s.read_to_string(&mut raw).ok()?;
        let (head, tail) = raw.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.1 200") {
            return None;
        }
        let start = tail.find('{')?;
        let end = tail.rfind('}')?;
        Some(tail[start..=end].to_string())
    }

    fn post(path: &str, body: &str) -> Option<String> {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        let addr = std::env::var("LETIBOT_ORACLE").unwrap_or_else(|_| "127.0.0.1:8080".into());
        let mut s = TcpStream::connect(&addr).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
        write!(
            s,
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .ok()?;
        let mut raw = String::new();
        s.read_to_string(&mut raw).ok()?;
        let (head, tail) = raw.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.1 200") {
            return None;
        }
        // The server may answer chunked; take the JSON object out of whatever
        // framing it used.
        let start = tail.find('{')?;
        let end = tail.rfind('}')?;
        Some(tail[start..=end].to_string())
    }
}
