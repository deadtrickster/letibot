//! The composer while `/rooms` is focused: `@` completion, room `/` verbs, per-channel history and
//! drafts, the reply target, and what a submitted line becomes once it may carry a picture. P0
//! defines the seam and declines every call, so a key that reaches one falls through to the
//! composer exactly as it did before; P3 fills them.

#![allow(dead_code)] // The seam is P0's and its readers are P3's.

use std::ops::ControlFlow;

use crate::app::rooms::Chan;
use crate::app::*;

impl App {
    /// Tab in a room: open or cycle the `@`/`/` completion. `false` is *not ours* - the key falls
    /// through to whatever had it before.
    pub(crate) fn rooms_complete(&mut self) -> bool {
        false
    }

    /// Up or down on an empty composer: walk this channel's sent lines. `false` is *not ours*.
    pub(crate) fn rooms_history(&mut self, _up: bool) -> bool {
        false
    }

    /// The selected channel moved: keep the draft of the one being left, restore the one arriving.
    pub(crate) fn rooms_switched(&mut self, _from: Option<&Chan>, _to: &Chan) {}

    /// The message the next line answers, when `R` set one.
    pub(crate) fn rooms_reply_target(&self) -> Option<u64> {
        None
    }

    pub(crate) fn rooms_set_reply(&mut self, _seq: Option<u64>) {}

    /// A submitted line as it goes out: the text with each chip's `![name](blob:ID)` in place, and
    /// the blob ids the say carries.
    pub(crate) fn rooms_outgoing(&mut self, text: &str) -> (String, Vec<String>) {
        (text.to_string(), Vec::new())
    }

    /// `@` completion and the room's `/` verbs are the COMPOSER's dispatch (`composer.rs`), which P0
    /// does not own: P3 hooks them there once this module has something to offer.
    pub(crate) fn rooms_compose_hooks_here() -> ControlFlow<Option<Action>> {
        ControlFlow::Continue(())
    }
}
