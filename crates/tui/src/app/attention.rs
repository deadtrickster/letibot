//! **What the head says outside the frame**: desktop notifications, the tab's progress bar,
//! the clipboard, and the images it has uploaded to the terminal.

use super::*;
use letibot_sessionlog::registry::short_id;
use letibot_sessionlog::view::TurnState;
use letibot_transcript::TranscriptItem;

impl App {
    /// **Tell the head's state what the terminal speaks** (see `crate::features`).
    pub fn set_features(&mut self, f: crate::backend::features::Features) {
        if self.features != f {
            self.features = f;
            self.invalidate_history();
            self.redraw = true;
        }
    }

    /// **The tab's progress bar** (OSC 9;4): what it should show this tick. Somebody waiting on
    /// the person outranks the model working — a tab that wants you must not look like one
    /// that is merely busy.
    pub fn progress(&self) -> crate::backend::terminal::Progress {
        use crate::backend::terminal::Progress;
        if !self.open.is_empty() || self.secret.is_some() {
            return Progress::Waiting;
        }
        if self.turn_busy() {
            return Progress::Busy;
        }
        match self.turn.as_ref().and_then(|t| t.state.as_ref()) {
            Some(TurnState::Failed { .. }) => Progress::Failed,
            _ => Progress::Idle,
        }
    }

    /// What needs the person right now, as counts and ids — compared tick to tick by
    /// [`App::take_notification`], so a notification is an EDGE and never a level.
    pub(crate) fn attention_now(&self) -> Attention {
        Attention {
            busy: self.turn_busy(),
            asks: self.open.iter().map(|d| d.req_id.clone()).collect(),
            secret: self.secret.as_ref().map(|s| s.req_id.clone()),
            answered: self
                .subagents
                .iter()
                .filter(|s| matches!(s.state.as_str(), "done" | "failed"))
                .map(|s| s.session_id.clone())
                .collect(),
        }
    }

    /// **A desktop notification, when something just started needing the person and they are
    /// not looking.** The operator's ask that started this: a session sat for minutes on a
    /// wait nobody was watching. Four edges, in the order they matter: a permission card
    /// arrived, a key or password card arrived, a subagent answered, the turn ended.
    ///
    /// The snapshot moves every call, focused or not — so coming back to the window and
    /// leaving again does not replay what was already on the screen.
    pub fn take_notification(&mut self) -> Option<String> {
        let now = self.attention_now();
        let before = self.attention.replace(now.clone())?;
        if !self.features.notify || self.focused != Some(false) {
            return None;
        }
        let who = self.window_title();
        if let Some(d) = self
            .open
            .iter()
            .rev()
            .find(|d| !before.asks.contains(&d.req_id))
        {
            return Some(format!("{who}: permission needed — {}", d.summary));
        }
        if let Some(s) = &self.secret
            && before.secret.as_ref() != Some(&s.req_id)
        {
            let first = s.prompt.lines().next().unwrap_or("").trim();
            return Some(format!("{who}: {first}"));
        }
        if let Some(id) = now
            .answered
            .iter()
            .find(|id| !before.answered.contains(*id))
        {
            return Some(format!("{who}: subagent {} finished", short_id(id)));
        }
        if before.busy && !now.busy && now.asks.is_empty() && now.secret.is_none() {
            let how = match self.turn.as_ref().and_then(|t| t.state.as_ref()) {
                Some(TurnState::Failed { .. }) => "the turn failed",
                Some(TurnState::Interrupted { .. }) => "the turn was interrupted",
                _ => "done",
            };
            return Some(format!("{who}: {how}"));
        }
        None
    }

    /// **`/copy`: the open ctrl-v window's output, or else the model's last reply, onto the
    /// system clipboard** (OSC 52 — which works over ssh, where `pbcopy` on the far side would
    /// fill the wrong machine's clipboard). A slash verb rather than a key, because the
    /// composer is live under the window and every bare letter is typing.
    pub(crate) fn copy_command(&mut self) {
        if !self.features.clipboard {
            self.say(
                "this terminal is not known to take OSC 52, so nothing was copied — \
                 LETIBOT_TERM_FEATURES=clipboard turns it on",
            );
            return;
        }
        let window = self.payload_sel.as_ref().and_then(|id| {
            self.items
                .iter()
                .find(|r| &r.item_id == id)
                .and_then(|r| match r.item.as_ref() {
                    Some(TranscriptItem::ToolResult { payload, .. }) => {
                        Some(("the open output", payload.clone()))
                    }
                    _ => None,
                })
        });
        let found = window.or_else(|| {
            self.items.iter().rev().find_map(|r| match r.item.as_ref() {
                Some(TranscriptItem::Assistant { text, .. }) if !text.trim().is_empty() => {
                    Some(("the last reply", text.clone()))
                }
                _ => None,
            })
        });
        match found {
            Some((what, text)) => {
                let lines = text.lines().count();
                self.clipboard_out = Some(text);
                self.say(&format!("copied {what} — {lines} line(s)"));
            }
            None => self.say("nothing to copy: no output is open and the model has not replied"),
        }
    }

    /// **Every PNG a row carries that the terminal does not have yet, queued for upload** — each
    /// once, under the id its row draws with. Only the rows added since the last look are
    /// walked; a list that shrank (a switch, a resync) is walked again from the top.
    pub(crate) fn queue_image_uploads(&mut self) {
        // **The size follows the window.** Every image already in the terminal is placed again
        // at the box this frame's width gives — a placement command each, no image bytes — so
        // the rows the renderers draw at this width match what the terminal will fill.
        let box_cols = crate::backend::graphics::image_box(self.cfg.width);
        if box_cols != self.images_box {
            self.images_box = box_cols;
            for (id, (w, h)) in &self.images_sent {
                let (cols, rows) = crate::backend::graphics::image_cells(*w, *h, box_cols);
                self.image_uploads
                    .push(crate::backend::graphics::image_place(*id, cols, rows));
            }
        }
        if self.images_scanned > self.items.len() {
            self.images_scanned = 0;
        }
        let mut scanned = self.images_scanned;
        let mut found: Vec<(u32, Option<u32>, Option<u32>, String)> = Vec::new();
        for it in &self.items[self.images_scanned..] {
            // **A row whose content has not arrived stops the walk**, and is looked at again
            // next frame: `TranscriptAppended` and its content are separate events, and a mark
            // moved past an empty row would never come back for the picture in it.
            let Some(item) = it.item.as_ref() else { break };
            scanned += 1;
            match item {
                TranscriptItem::ToolResult { media: Some(m), .. } if m.mime == "image/png" => {
                    found.push((
                        crate::backend::graphics::image_id(&it.item_id),
                        m.width,
                        m.height,
                        m.wire_base64().to_string(),
                    ));
                }
                TranscriptItem::Assistant { text, .. } if text.contains("![") => {
                    for (_, target) in crate::render::markdown_images(text) {
                        let Some(m) = self.read_local_png(&target) else {
                            continue;
                        };
                        let id =
                            crate::backend::graphics::image_id(&format!("{}#{target}", it.item_id));
                        crate::render::remember_reply_image(
                            &it.item_id,
                            &target,
                            (id, m.width, m.height),
                        );
                        found.push((id, m.width, m.height, m.wire_base64().to_string()));
                    }
                }
                _ => {}
            }
        }
        self.images_scanned = scanned;
        for (id, w, h, b64) in found {
            if self.images_sent.insert(id, (w, h)).is_none() {
                let (cols, rows) = crate::backend::graphics::image_cells(w, h, box_cols);
                self.image_uploads
                    .push(crate::backend::graphics::image_upload(id, &b64));
                self.image_uploads
                    .push(crate::backend::graphics::image_place(id, cols, rows));
            }
        }
    }

    /// **A PNG a reply named, read by the head** — absolute, `~/`, or relative to the
    /// session's workspace; at most 16 MiB; and only if its bytes say PNG, whatever the name
    /// says. `None` for anything else, which leaves the alt text as the row's only word.
    pub(crate) fn read_local_png(&self, target: &str) -> Option<letibot_transcript::media::Media> {
        let path = if let Some(rest) = target.strip_prefix("~/") {
            std::path::PathBuf::from(std::env::var_os("HOME")?).join(rest)
        } else if target.starts_with('/') {
            std::path::PathBuf::from(target)
        } else {
            std::path::Path::new(&self.wiring.workspace).join(target)
        };
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        letibot_transcript::media::Media::of(&path.to_string_lossy(), &bytes)
            .filter(|m| m.mime == "image/png")
    }

    /// Image uploads for the head to write to the terminal (kitty graphics).
    pub fn take_image_uploads(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.image_uploads)
    }

    /// Text the operator asked to copy, for the head to write to the clipboard.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard_out.take()
    }

    /// Whether the terminal reported a light background (OSC 11).
    pub fn light_background(&self) -> Option<bool> {
        self.light_background
    }
}

/// See [`App::take_notification`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Attention {
    pub(crate) busy: bool,
    pub(crate) asks: Vec<String>,
    pub(crate) secret: Option<String>,
    pub(crate) answered: Vec<String>,
}
