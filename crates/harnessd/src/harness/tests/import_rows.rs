//! What an import feeds the one writer: a foreign conversation's rows, in the batches a re-render of
//! the same rows draws. A batch boundary inside a run of tool results writes an end-of-turn the rows
//! themselves do not have, and the ledger is the prompt.

use super::*;

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **The same two tool results one round produced are ONE user turn in an imported session.** The
/// importers hand the writer one row per event, and a batch boundary between two results is a
/// `<|im_end|>` the re-render of those rows does not have: the model reads a prompt it was never
/// trained on, and the ledger disagrees with itself.
#[test]
fn a_run_of_tool_results_read_from_a_foreign_conversation_is_one_user_turn() {
    let dir = Scratch::new("import-batch");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = "import-batch".into();
    cfg.vocab_gguf = None;
    cfg.provider = Some(crate::config::ProviderConfig {
        name: "deepseek".into(),
        model: Some("not-called".into()),
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    *parts.mode_store.write().unwrap() = crate::modes::ModeStore::default();
    let vocab = parts.vocab.clone();
    let mut h = Harness::open(&parts, cfg, Hub::new("import-batch")).expect("the session opens");

    // The assistant asks for two reads in one message, and the two results follow.
    h.append_imported(&[TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![
            letibot_transcript::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: r#"{"path":"a"}"#.into(),
            },
            letibot_transcript::ToolCall {
                id: "c2".into(),
                name: "read".into(),
                arguments: r#"{"path":"b"}"#.into(),
            },
        ],
        truncated: false,
    }])
    .expect("the ask lands");
    let before = vocab
        .detokenize(h.session.ledger.tokens(), true)
        .expect("the fixture's own bytes");

    // The read hands them over one at a time, which is how `ReadTree`'s sink sees them.
    let mut held: Vec<TranscriptItem> = Vec::new();
    for (call_id, payload) in [("c1", "A"), ("c2", "B")] {
        h.import_row(
            TranscriptItem::ToolResult {
                call_id: call_id.into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: payload.into(),
                edit: None,
                origin: None,
                media: None,
            },
            &mut held,
        )
        .expect("the result lands");
    }
    h.flush_imported(&mut held).expect("the run ends");

    let after = vocab
        .detokenize(h.session.ledger.tokens(), true)
        .expect("the fixture's own bytes");
    let added = after
        .strip_prefix(&before)
        .expect("appending only ever extends the ledger");
    assert_eq!(
        added.matches("<|im_start|>user").count(),
        1,
        "the two results open a turn each, which is a prompt the model was never trained on: \
         {added}"
    );
    assert_eq!(
        added,
        "<|im_end|>\n<|im_start|>user\n<tool_response>\nA\n</tool_response>\n<tool_response>\nB\n</tool_response><|im_end|>\n",
        "the ledger has to be the template's own bytes for these rows"
    );
}
