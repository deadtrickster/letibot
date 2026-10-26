//! **What the head says outside the frame**: desktop notifications, the tab's progress bar,
//! the clipboard, and the images it has uploaded to the terminal.

use super::*;
use letibot_sessionlog::view::TurnState;
use letibot_transcript::TranscriptItem;

impl App {
    /// **Tell the head's state what the terminal speaks** (see `rano::term::features`), and — in
    /// the same breath, because it is the same value — which picture protocol goes with it
    /// (`Features::proto`). One call, so a head cannot come up with features that say *pictures*
    /// and a protocol that says *none*.
    pub fn set_features(&mut self, f: rano::term::Features) {
        if self.features != f {
            self.features = f;
            self.invalidate_history();
            self.redraw = true;
        }
        self.set_proto(f.proto);
    }

    /// **The tab's progress bar** (OSC 9;4): what it should show this tick. Somebody waiting on
    /// the person outranks the model working — a tab that wants you must not look like one
    /// that is merely busy.
    pub fn progress(&self) -> rano::term::Progress {
        use rano::term::Progress;
        // A question card put away is still a question waiting on the person — in THIS session,
        // the waiting line's rule (R90.13): a card put away in another session kept this tab on
        // Waiting while nothing on its screen waited. Stale todo cards are pruned on every board
        // (`App::prune_todo_asks`), so one answered elsewhere no longer holds it either.
        let away_here = self.away.iter().any(|a| a.session_id == self.session_id);
        let prompt_away = self.prompt.is_some() && self.prompt_away;
        if !self.open.is_empty() || away_here || prompt_away || self.secret.is_some() {
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
            // Put away is still asking: counted here, so bringing a card back is not a new ask
            // and does not notify a second time.
            asks: self
                .open
                .iter()
                .map(|d| d.req_id.clone())
                .chain(self.away.iter().map(|a| a.decision.req_id.clone()))
                .collect(),
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
            // **Named for what it is** (R90.27): every open card was called "permission needed",
            // a model's question and a todo row's alike.
            let what = if d.req_id.starts_with(crate::app::todos::TODO_ASK) {
                "a todo row asks you"
            } else if d.kind == "question" {
                "question for you"
            } else {
                "permission needed"
            };
            return Some(format!("{who}: {what} — {}", d.summary));
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
            return Some(format!("{who}: subagent {} finished", self.child_label(id)));
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
        let box_cols = rano::term::graphics::image_box(self.cfg.width);
        if box_cols != self.images_box {
            self.images_box = box_cols;
            for (id, (w, h)) in &self.images_sent {
                let (cols, rows) = rano::term::graphics::image_cells(*w, *h, box_cols);
                let place = rano::term::graphics::image_place(*id, cols, rows);
                self.image_uploads.push(self.proto.wrap(&place));
            }
        }
        if self.images_scanned > self.items.len() {
            self.images_scanned = 0;
        }
        let mut scanned = self.images_scanned;
        let mut found: Vec<(u32, Option<u32>, Option<u32>, String)> = Vec::new();
        // **A row's thumbnail is drawn from the registry this fills**, so the frame it is
        // registered on has to be drawn again — the flag is spent after the borrow ends, because
        // the walk holds `items` and the rebuild does not.
        let mut remembered = false;
        for it in &self.items[self.images_scanned..] {
            // **A row whose content has not arrived stops the walk**, and is looked at again
            // next frame: `TranscriptAppended` and its content are separate events, and a mark
            // moved past an empty row would never come back for the picture in it.
            let Some(item) = it.item.as_ref() else { break };
            scanned += 1;
            match item {
                TranscriptItem::ToolResult { media: Some(m), .. } if m.mime == "image/png" => {
                    found.push((
                        rano::term::graphics::image_id(&it.item_id),
                        m.width,
                        m.height,
                        m.wire_base64().to_string(),
                    ));
                }
                TranscriptItem::Assistant { text, .. } if text.contains("![") => {
                    for (_, target) in crate::ui::render::markdown_images(text) {
                        let Some(m) = self.read_local_png(&target) else {
                            continue;
                        };
                        let id =
                            rano::term::graphics::image_id(&format!("{}#{target}", it.item_id));
                        crate::ui::render::remember_reply_image(
                            &it.item_id,
                            &target,
                            (id, m.width, m.height),
                        );
                        found.push((id, m.width, m.height, m.wire_base64().to_string()));
                    }
                }
                // **A picture the OPERATOR attached** — the one thing the conversation has been
                // unable to show since paste-attachments landed: the row carried the chip and the
                // bytes, and the head drew only the chip (the operator, from a fresh session:
                // *"image doesnt render"*). The bytes are already here — a `data:` reference on
                // the row — so nothing is read off the disk and no converter runs: kitty carries
                // PNG, which is what the composer produces and what the daemon stores.
                //
                // The size comes out of the header ([`size_of_data_ref`]), which is what keeps
                // the thumbnail's aspect instead of a box of a fixed shape, and the id is the
                // row's plus the part's index, so two pictures in one prompt are two images.
                TranscriptItem::User { parts, .. } => {
                    for (k, p) in parts.iter().enumerate() {
                        let letibot_transcript::UserPart::Image {
                            media_type,
                            data_ref,
                        } = p
                        else {
                            continue;
                        };
                        if media_type != "image/png" {
                            continue;
                        }
                        let id = rano::term::graphics::image_id(&format!(
                            "{}#{}{k}",
                            it.item_id,
                            crate::ui::render::USER_PART
                        ));
                        let (w, h) = letibot_transcript::media::size_of_data_ref(data_ref);
                        // **Registered every time it is walked**, because the renderer reads that
                        // registry and does not care which App filled it: whether the terminal has
                        // the BYTES is `images_sent`'s question, asked below.
                        crate::ui::render::remember_user_image(&it.item_id, k, (id, w, h));
                        remembered = true;
                        found.push((
                            id,
                            w,
                            h,
                            letibot_transcript::media::wire_payload(data_ref).to_string(),
                        ));
                    }
                }
                _ => {}
            }
        }
        self.images_scanned = scanned;
        if remembered {
            self.invalidate_history();
        }
        for (id, w, h, b64) in found {
            if self.images_sent.insert(id, (w, h)).is_none() {
                let (cols, rows) = rano::term::graphics::image_cells(w, h, box_cols);
                // **Wrapped for the multiplexer**, in the one place every picture this head uploads
                // passes through: inside tmux a kitty sequence crosses only as a DCS payload with
                // its escapes doubled (`rano::term::Proto::wrap`), and an unwrapped one is
                // swallowed with nothing said.
                let upload = rano::term::graphics::image_upload(id, &b64);
                self.image_uploads.push(self.proto.wrap(&upload));
                let place = rano::term::graphics::image_place(id, cols, rows);
                self.image_uploads.push(self.proto.wrap(&place));
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
