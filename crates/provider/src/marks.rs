//! What one session's requests have learned about pictures: the bytes a provider refused, and the
//! models that take text only. They belong to the session that earned them, because one conversation's
//! refusal must not re-render another's pictures as placeholders, and the row keeps them across a restart.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

/// What a backend calls with the marks after it learned one, to keep them.
pub type Learned = Arc<dyn Fn(&Marks) + Send + Sync>;

use letibot_backend::{
    BackendCaps, BackendError, Completion, Delta, MessagesBackend, StreamFlow, TurnRequest,
};

/// The most marks one session keeps. A mark is a picture's key with the provider's words, or a
/// model's name, so this is tens of kilobytes at worst and more refusals than a conversation has;
/// the oldest goes first, because the newest refusal is the one a live request is about to meet.
pub const MARKS_MAX: usize = 64;

/// One session's marks, in the order they were learned.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Marks {
    /// `(picture_key, the provider's words)` for the bytes a provider refused.
    pictures: Vec<(String, String)>,
    /// `provider/model` for the models that refused a picture outright.
    models: Vec<String>,
}

impl Marks {
    /// Nothing learned yet.
    pub fn new() -> Self {
        Marks::default()
    }

    /// Every picture a provider refused, oldest first.
    pub fn pictures(&self) -> &[(String, String)] {
        &self.pictures
    }

    /// Every model that refused a picture, oldest first.
    pub fn models(&self) -> &[String] {
        &self.models
    }

    /// Nothing is known, so this session's requests are the ones a fresh session sends.
    pub fn is_empty(&self) -> bool {
        self.pictures.is_empty() && self.models.is_empty()
    }

    /// Record that a provider refused these bytes, in the words it refused them in, and `true`
    /// when it is new - which is what makes the notice to the operator once.
    pub fn mark_picture(&mut self, key: &str, said: &str) -> bool {
        if self.pictures.iter().any(|(k, _)| k == key) {
            return false;
        }
        if self.pictures.len() >= MARKS_MAX {
            self.pictures.remove(0);
        }
        self.pictures.push((key.to_string(), said.to_string()));
        true
    }

    /// Why this picture is not sent, when this session has learned that it is not.
    pub fn picture_why(&self, key: &str) -> Option<&str> {
        self.pictures
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, why)| why.as_str())
    }

    /// Record that this model refused a picture, and `true` when it is new.
    pub fn mark_model(&mut self, key: &str) -> bool {
        if self.models.iter().any(|m| m == key) {
            return false;
        }
        if self.models.len() >= MARKS_MAX {
            self.models.remove(0);
        }
        self.models.push(key.to_string());
        true
    }

    /// Whether this session has been told this model takes text only.
    pub fn has_model(&self, key: &str) -> bool {
        self.models.iter().any(|m| m == key)
    }

    /// The marks as the text a session's row carries. JSON because the store keeps the session's
    /// mutable metadata that way and knows nothing about this type.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Marks read back from a session's row, or `None` for a row this build cannot read - an empty
    /// set is the honest reading of marks that cannot be parsed, and it costs one 400.
    pub fn from_json(json: &str) -> Option<Marks> {
        serde_json::from_str(json).ok()
    }
}

thread_local! {
    /// The marks of the request being assembled on THIS thread: what a session enters around a
    /// provider call, and nothing else.
    static CURRENT: RefCell<Option<Arc<Mutex<Marks>>>> = const { RefCell::new(None) };
}

/// Assemble this thread's requests under one session's marks, until the guard drops. The previous
/// holder is restored, so a nested call reads its own session's marks and no other's.
#[must_use = "the marks are in force only while the guard is held"]
pub fn enter(marks: Arc<Mutex<Marks>>) -> Entered {
    let previous = CURRENT.with(|c| c.borrow_mut().replace(marks));
    Entered { previous }
}

/// Holds one session's marks in force on this thread; see [`enter`].
pub struct Entered {
    previous: Option<Arc<Mutex<Marks>>>,
}

impl Drop for Entered {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// Change the marks this thread's request belongs to. `None` when no session entered any: nothing
/// is remembered, and a refusal is answered for this call alone rather than for every session.
pub fn update<T>(f: impl FnOnce(&mut Marks) -> T) -> Option<T> {
    let held = CURRENT.with(|c| c.borrow().clone())?;
    let mut marks = held.lock().unwrap_or_else(|e| e.into_inner());
    Some(f(&mut marks))
}

/// Read the marks this thread's request belongs to; `None` when no session entered any.
pub fn read<T>(f: impl FnOnce(&Marks) -> T) -> Option<T> {
    let held = CURRENT.with(|c| c.borrow().clone())?;
    let marks = held.lock().unwrap_or_else(|e| e.into_inner());
    Some(f(&marks))
}

/// A backend whose request runs under one session's marks, so that what a refusal teaches is
/// learned by that session. `learned` is called with the marks once a call has added one, which is
/// where the session's row is written.
pub struct Scoped {
    inner: Box<dyn MessagesBackend>,
    marks: Arc<Mutex<Marks>>,
    learned: Option<Learned>,
}

/// `inner` as one session's backend: every call it assembles runs under `marks`, and `learned` is
/// called at most once per call that adds a mark, with the whole set, so a writer needs no diff.
pub fn scoped(
    inner: Box<dyn MessagesBackend>,
    marks: Arc<Mutex<Marks>>,
    learned: Option<Learned>,
) -> Box<dyn MessagesBackend> {
    Box::new(Scoped {
        inner,
        marks,
        learned,
    })
}

impl Scoped {
    fn snapshot(&self) -> Marks {
        self.marks.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl MessagesBackend for Scoped {
    fn caps(&self) -> BackendCaps {
        self.inner.caps()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn authority(&self) -> String {
        self.inner.authority()
    }

    fn complete(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
    ) -> Result<Completion, BackendError> {
        let before = self.snapshot();
        let outcome = {
            let _entered = enter(self.marks.clone());
            self.inner.complete(req, on_delta)
        };
        let after = self.snapshot();
        if after != before
            && let Some(said) = &self.learned
        {
            said(&after);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound is the fix for a list that grew with every refusal: the oldest mark goes, and the
    /// newest still answers, which is the one a live request is about to meet again.
    #[test]
    fn a_session_keeps_no_more_than_the_bound_and_the_oldest_goes_first() {
        let mut m = Marks::new();
        for i in 0..MARKS_MAX + 3 {
            assert!(
                m.mark_picture(&format!("k{i}"), "no"),
                "a new picture was refused"
            );
        }
        assert_eq!(m.pictures().len(), MARKS_MAX);
        assert!(
            m.picture_why("k0").is_none(),
            "the oldest mark survived the bound"
        );
        assert!(m.picture_why(&format!("k{}", MARKS_MAX + 2)).is_some());

        for i in 0..MARKS_MAX + 2 {
            m.mark_model(&format!("glm/m{i}"));
        }
        assert_eq!(m.models().len(), MARKS_MAX);
        assert!(!m.has_model("glm/m0"), "the oldest model mark survived");
    }

    /// A mark already held is not learned again, which is what makes the notice to the operator
    /// once and leaves the bytes of a prompt unchanged.
    #[test]
    fn a_mark_already_held_is_not_new_and_keeps_the_words_it_was_given() {
        let mut m = Marks::new();
        assert!(m.mark_picture("k", "unsupported image"));
        assert!(!m.mark_picture("k", "something else"));
        assert_eq!(m.picture_why("k"), Some("unsupported image"));
        assert!(m.mark_model("glm/g"));
        assert!(!m.mark_model("glm/g"));
    }

    /// A session's marks travel through its row as JSON, and a row this build cannot read is an
    /// empty set rather than a refusal to open the session.
    #[test]
    fn marks_round_trip_through_the_text_a_session_row_carries() {
        let mut m = Marks::new();
        m.mark_picture("k", "unsupported image");
        m.mark_model("glm/glm-5.3");
        let back = Marks::from_json(&m.to_json()).expect("its own text parses");
        assert_eq!(back, m);
        assert!(Marks::from_json("not json").is_none());
    }

    /// The scope is what a session owns: a mark made under one session's guard is invisible under
    /// another's, and the guard restores the holder it found.
    #[test]
    fn a_mark_is_learned_by_the_session_that_entered_and_by_no_other() {
        let a = Arc::new(Mutex::new(Marks::new()));
        let b = Arc::new(Mutex::new(Marks::new()));
        {
            let _held = enter(a.clone());
            assert_eq!(update(|m| m.mark_picture("k", "no")), Some(true));
        }
        {
            let _held = enter(b.clone());
            assert_eq!(read(|m| m.picture_why("k").map(String::from)), Some(None));
        }
        assert!(a.lock().unwrap().picture_why("k").is_some());
        assert!(b.lock().unwrap().is_empty());

        // And no session at all is not a session: nothing is remembered, so a refusal cannot
        // reach a prompt the caller did not enter marks for.
        assert_eq!(update(|m| m.mark_picture("k2", "no")), None);
        assert!(read(|m| m.picture_why("k").is_some()).is_none());
    }

    /// The backend a session is given: the call runs under that session's marks, and a mark it
    /// learns is handed to the writer once - never again for one it already holds.
    #[test]
    fn the_scoped_backend_learns_into_the_session_and_reports_it_once() {
        struct Refuses;

        impl MessagesBackend for Refuses {
            fn caps(&self) -> BackendCaps {
                BackendCaps::METERED_API
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
                _req: &TurnRequest<'_>,
                _on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
            ) -> Result<Completion, BackendError> {
                crate::messages::mark_bad_picture("data:image/png;base64,AAAA", "unsupported");
                Err(BackendError::Refused {
                    status: 400,
                    body: "unsupported".into(),
                    retry_after: None,
                })
            }
        }

        let marks = Arc::new(Mutex::new(Marks::new()));
        let written: Arc<Mutex<Vec<Marks>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = written.clone();
        let p = scoped(
            Box::new(Refuses),
            marks.clone(),
            Some(Arc::new(move |m: &Marks| {
                sink.lock().unwrap().push(m.clone())
            })),
        );
        let req = TurnRequest {
            system: "",
            tools_json: &[],
            items: &[],
            tail: &[],
            max_output_tokens: None,
        };
        assert!(p.complete(&req, &mut |_| StreamFlow::Continue).is_err());
        assert_eq!(
            written.lock().unwrap().len(),
            1,
            "the learned mark was not written"
        );
        assert!(p.complete(&req, &mut |_| StreamFlow::Continue).is_err());
        assert_eq!(
            written.lock().unwrap().len(),
            1,
            "a mark already held was written again"
        );
        // The scope ends with the call: nothing is left in force for whatever runs next on this
        // thread, which is what keeps one session's refusal out of another's prompt.
        assert!(read(|m| m.is_empty()).is_none());
    }
}
