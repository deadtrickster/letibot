//! The recorded inputs: the last N inputs of every hook, which is what a replay can be run over.
//!
//! §2.4's step 4 is *"every recorded input of every hook the change touches, through the live
//! stack and the shadow stack"* — so the shadow gate is only as good as this ring. It holds the
//! last [`Recorder::DEFAULT`] inputs per hook, because a hook's history is not the point: what
//! arrives is a stream, and a diff over the last handful of real inputs is what tells a
//! refactor that changed nothing from a fix that changed something.
//!
//! **Inputs only, never answers.** The answer a live stack gave at the time is not what a replay
//! compares against; both stacks are asked the same input now, side by side, so a bug that
//! changed the answer in between is not mistaken for an intended difference.

use std::collections::BTreeMap;

use serde_json::Value;

/// The last inputs of each hook, oldest first.
#[derive(Debug, Clone)]
pub struct Recorder {
    cap: usize,
    rings: BTreeMap<String, Vec<Value>>,
}

impl Recorder {
    /// The default depth: 32 inputs per hook.
    pub const DEFAULT: usize = 32;

    pub fn new() -> Recorder {
        Recorder::with_capacity(Recorder::DEFAULT)
    }

    /// A ring of `cap` inputs per hook. A capacity of zero is raised to one: a recorder that
    /// records nothing is not a recorder, and a caller who asked for one meant a small one.
    pub fn with_capacity(cap: usize) -> Recorder {
        Recorder {
            cap: cap.max(1),
            rings: BTreeMap::new(),
        }
    }

    /// Record one input of one hook, dropping the oldest when the ring is full.
    pub fn record(&mut self, hook: &str, input: &Value) {
        let ring = self.rings.entry(hook.to_string()).or_default();
        if ring.len() == self.cap {
            ring.remove(0);
        }
        ring.push(input.clone());
    }

    /// The recorded inputs of a hook, oldest first. A hook nobody has recorded answers empty,
    /// which is what makes a change to a never-called hook replay nothing and diff nothing.
    pub fn inputs(&self, hook: &str) -> &[Value] {
        self.rings.get(hook).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every hook with at least one recorded input.
    pub fn hooks(&self) -> Vec<&str> {
        self.rings
            .iter()
            .filter(|(_, ring)| !ring.is_empty())
            .map(|(hook, _)| hook.as_str())
            .collect()
    }

    /// How many inputs this hook has kept.
    pub fn len(&self, hook: &str) -> usize {
        self.inputs(hook).len()
    }

    /// Forget everything — after a squash, the inputs recorded against the old stack describe a
    /// stack that no longer exists.
    pub fn clear(&mut self) {
        self.rings.clear();
    }
}

impl Default for Recorder {
    fn default() -> Recorder {
        Recorder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_ring_keeps_the_last_inputs_and_forgets_the_oldest() {
        let mut rec = Recorder::new();
        for i in 0..(Recorder::DEFAULT + 3) {
            rec.record("pane_dashboard", &json!({ "v": 1, "n": i }));
        }
        let kept = rec.inputs("pane_dashboard");
        assert_eq!(kept.len(), Recorder::DEFAULT);
        assert_eq!(kept[0]["n"], json!(3), "the first three fell off");
        assert_eq!(
            kept[Recorder::DEFAULT - 1]["n"],
            json!(Recorder::DEFAULT + 2)
        );
        assert_eq!(rec.len("pane_dashboard"), Recorder::DEFAULT);
    }

    #[test]
    fn hooks_are_recorded_apart() {
        let mut rec = Recorder::new();
        rec.record("notice_fired", &json!({"text": "a"}));
        rec.record("pane_dashboard", &json!({"size": {"w": 80}}));
        assert_eq!(rec.hooks(), vec!["notice_fired", "pane_dashboard"]);
        assert_eq!(rec.inputs("pane_dashboard").len(), 1);
        assert!(
            rec.inputs("condition_github_pr").is_empty(),
            "a hook nobody called has nothing to replay"
        );
        rec.clear();
        assert!(rec.hooks().is_empty());
    }

    #[test]
    fn a_small_ring_holds_what_it_was_told_to_and_a_zero_ring_holds_one() {
        let mut rec = Recorder::with_capacity(2);
        for i in 0..4 {
            rec.record("notice_fired", &json!({ "n": i }));
        }
        assert_eq!(rec.inputs("notice_fired").len(), 2);

        let mut rec = Recorder::with_capacity(0);
        rec.record("notice_fired", &json!({"n": 0}));
        assert_eq!(rec.inputs("notice_fired").len(), 1);
    }
}
