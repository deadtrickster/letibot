//! What a session's provider learned about pictures: held by the session, and still there when the
//! session reopens. The two halves are the marks' own scope and the row they are written to.

use super::*;

/// A provider that refuses one picture, the way `openai.rs` does: the refusal is recorded against
/// the bytes and the call answers 400. Nothing here is a real image - the mark is about a refusal.
struct RefusesABitmap(String);

impl letibot_backend::MessagesBackend for RefusesABitmap {
    fn caps(&self) -> letibot_backend::BackendCaps {
        letibot_backend::BackendCaps::METERED_API
    }

    fn name(&self) -> &str {
        "glm"
    }

    fn model(&self) -> &str {
        "glm-5.3"
    }

    fn authority(&self) -> String {
        "a fake provider".into()
    }

    fn complete(
        &self,
        _req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        letibot_provider::messages::mark_bad_picture(
            &self.0,
            "You have uploaded an unsupported image",
        );
        Err(letibot_backend::BackendError::Refused {
            status: 400,
            body: "You have uploaded an unsupported image".into(),
            retry_after: None,
        })
    }
}

/// A harness over a store that outlives it, and the path that store is at.
fn open(
    dir: &std::path::Path,
    id: &str,
    gguf: std::path::PathBuf,
) -> (Harness, std::path::PathBuf) {
    let path = dir.join("sessions.db");
    let mut cfg = Config::for_this_box(dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path.clone());
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let h =
        Harness::open(&parts, cfg, letibot_sessionlog::Hub::new(id)).expect("the session opens");
    (h, path)
}

/// The mark goes on the session's own row and comes back with the session: a refusal earned before a
/// restart must not be earned again after it, because the picture stays in the history.
#[test]
fn a_picture_mark_learned_in_a_session_is_there_when_the_session_reopens() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "r123-marks-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    const ID: &str = "r123-session-marks";
    let reference =
        letibot_transcript::testing::data_ref("image/png", &letibot_transcript::testing::png(8, 4));

    let (h, path) = open(&dir, ID, gguf.clone());
    // The backend the daemon gives a session, with one that refuses this picture: the request runs
    // under the session's marks and a mark it learns is written to the session's row.
    let provider = scoped_provider(
        Box::new(RefusesABitmap(reference.clone())),
        h.picture_marks.clone(),
        Some(persist_marks(Some(path.clone()), ID.to_string())),
    );
    let req = letibot_backend::TurnRequest {
        system: "",
        tools_json: &[],
        items: &[],
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    assert!(
        provider
            .complete(&req, &mut |_| letibot_backend::StreamFlow::Continue)
            .is_err(),
        "the fake refuses every call"
    );
    drop(h);
    let row = Store::open(&path)
        .expect("the store")
        .session_marks(ID)
        .expect("the marks row");
    let row = row.expect("the mark was not written to the session's row");
    assert!(
        row.contains(&letibot_provider::messages::picture_key(&reference)),
        "the row does not name this picture: {row}"
    );

    // A restart, as the daemon makes one: the same store, a new harness.
    let (h, _) = open(&dir, ID, gguf);
    let _held = letibot_provider::marks::enter(h.picture_marks.clone());
    assert!(
        letibot_provider::messages::known_bad_picture(&reference).is_some(),
        "the reopened session forgot a refusal its own row carries"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
