//! A host's refusals in one queue: what a layer that would not load, trapped, or answered something
//! the host could not use owes the person who wrote it - said once per layer, hook and reason, however
//! many frames, pushes or tool calls ask again.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::Failure;

/// How many refusals a host remembers, and how many may wait to be said. Past this the oldest
/// identity is forgotten and the oldest waiting sentence dropped, so a layer whose reason changes
/// every frame cannot grow the host it broke in.
pub const KEEP: usize = 256;

/// One refusal's identity: the layer, the hook it was answering (none for a layer that would not
/// load at all) and the reason. Those three and nothing else - a stack's own failure record already
/// has them - so a layer trapping on every frame is one sentence rather than one per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub layer: String,
    pub hook: Option<String>,
    pub why: String,
}

impl Refusal {
    /// The identity of a failure the stack recorded.
    pub fn of(failure: &Failure) -> Refusal {
        Refusal {
            layer: failure.layer.clone(),
            hook: failure.hook.clone(),
            why: failure.why.clone(),
        }
    }

    /// A refusal a host knows by name itself: an answer it refused to draw, which the stack read as
    /// a fine answer and the host as a shape it will not hand a person.
    pub fn new(layer: impl Into<String>, hook: Option<&str>, why: impl Into<String>) -> Refusal {
        Refusal {
            layer: layer.into(),
            hook: hook.map(str::to_string),
            why: why.into(),
        }
    }
}

/// A host's refusals: a bounded, deduped queue. [`push`](Refusals::push) answers `false` for a
/// refusal the host has already queued or said, which is what a frame loop calls without checking.
#[derive(Default)]
pub struct Refusals {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Every identity this host has told somebody about, oldest first.
    said: VecDeque<Refusal>,
    /// The sentences owed and not yet taken, oldest first.
    waiting: VecDeque<String>,
}

impl Refusals {
    pub fn new() -> Refusals {
        Refusals::default()
    }

    /// Queue `said` for this refusal, unless this host has had it already. `false` is the second
    /// and every later copy - the whole point, since the reason it is a queue is that a broken
    /// layer fails on every frame.
    pub fn push(&self, of: Refusal, said: impl Into<String>) -> bool {
        let mut inner = self.lock();
        if inner.said.contains(&of) {
            return false;
        }
        if inner.said.len() >= KEEP {
            inner.said.pop_front();
        }
        inner.said.push_back(of);
        inner.waiting.push_back(said.into());
        while inner.waiting.len() > KEEP {
            inner.waiting.pop_front();
        }
        true
    }

    /// Queue a sentence that carries its own identity - news with no [`Failure`] behind it, such as
    /// what admission says about the layers that never became a stack. The same sentence twice is
    /// the same news, which is all a caller with no failure record can promise.
    pub fn push_said(&self, said: impl Into<String>) -> bool {
        let said = said.into();
        self.push(Refusal::new("", None, said.clone()), said)
    }

    /// Everything owed and not said yet, oldest first, handed over and forgotten.
    pub fn drain(&self) -> Vec<String> {
        self.lock().waiting.drain(..).collect()
    }

    /// The next sentence without taking it: a caller that only draws can read it and leave the
    /// notice it is owed to whoever has a line to put it on.
    pub fn peek(&self) -> Option<String> {
        self.lock().waiting.front().cloned()
    }

    /// How many sentences are owed right now - said nowhere yet.
    pub fn len(&self) -> usize {
        self.lock().waiting.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity is the three fields and not the sentence: the same layer failing at the same
    /// hook for the same reason is one line, whatever a host would have spelled around it.
    #[test]
    fn the_same_refusal_queues_once_and_a_new_reason_is_news_again() {
        let queue = Refusals::new();
        let trap = Refusal::new("board", Some("link_gh"), "the layer trapped");
        assert!(queue.push(trap.clone(), "first wording: the layer trapped"));
        assert!(!queue.push(trap.clone(), "second wording, same refusal"));
        assert_eq!(queue.len(), 1, "one sentence owed");
        assert_eq!(
            queue.peek().as_deref(),
            Some("first wording: the layer trapped")
        );

        // The same layer and hook for another reason is another refusal: a person fixing a trap has
        // not fixed the answer the layer gives once it runs.
        let other = Refusal::new("board", Some("link_gh"), "an answer a head cannot read");
        assert!(queue.push(other, "second wording"));
        assert_eq!(
            queue.drain(),
            vec![
                "first wording: the layer trapped".to_string(),
                "second wording".to_string(),
            ],
            "oldest first, and taken once"
        );
        assert!(queue.is_empty(), "taken, so not owed again");
        assert!(
            !queue.push(
                Refusal::new("board", Some("link_gh"), "the layer trapped"),
                "again"
            ),
            "and a refusal already said is remembered after it was taken"
        );
    }

    /// A host that never drains - a bridge with no line to draw on - is bounded anyway: past `KEEP`
    /// the oldest sentence goes, and a layer that names a new reason every frame cannot grow it.
    #[test]
    fn the_queue_is_bounded() {
        let queue = Refusals::new();
        for n in 0..KEEP + 10 {
            queue.push(
                Refusal::new("noisy", Some("pane_x"), n.to_string()),
                n.to_string(),
            );
        }
        assert_eq!(queue.len(), KEEP);
        let drained = queue.drain();
        let last = (KEEP + 9).to_string();
        assert_eq!(drained.first().map(String::as_str), Some("10"));
        assert_eq!(drained.last().map(String::as_str), Some(last.as_str()));
        // And the identities went with them: the oldest ten may be said again, the newest may not.
        assert!(!queue.push(
            Refusal::new("noisy", Some("pane_x"), KEEP.to_string()),
            "again"
        ));
    }

    /// A sentence with no failure behind it is its own identity, which is what admission's news is.
    #[test]
    fn a_sentence_is_its_own_identity() {
        let queue = Refusals::new();
        assert!(queue.push_said("the plugin layer x: would not load"));
        assert!(!queue.push_said("the plugin layer x: would not load"));
        assert_eq!(queue.drain().len(), 1);
    }
}
