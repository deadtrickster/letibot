//! What `alt-s` puts in the composer: a chip naming the place in the editor, and the context a
//! prompt carries. The words the model reads are the DAEMON's to write, so nothing here builds
//! prose - the chip is a label on the composer's edge and a `ContextRef` on the wire.

use letibot_sessionlog::EditorContext;
use letibot_sessionlog::protocol::{ContextPos, ContextRange, ContextRef, OpenBuffer};
use rano::send::SendEvent;

use super::App;

/// One place the operator attached, with the label the composer draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextChip {
    /// `[context 1 · notes.txt:11-12 · unsaved]`. A number and not an index, for the reason a
    /// picture chip's is: a chip taken out of the middle renumbers nothing.
    pub(crate) label: String,
    pub(crate) r: ContextRef,
}

impl App {
    /// Attach the place rano sent, and move the keyboard to the composer: the words are the
    /// operator's to write, and nothing is sent by `alt-s`.
    pub(crate) fn context_in(&mut self, e: &SendEvent) {
        let r = self.context_ref(e);
        let nth = self.contexts.len() + 1;
        let label = chip_label(nth, &r, &|p| self.workspace_relative(p));
        self.contexts.push(ContextChip {
            label: label.clone(),
            r,
        });
        if let Some(p) = self.edit_pane.as_mut() {
            p.focused = false;
            p.dirty = true;
        }
        self.say(&format!("{label} rides with your next message"));
    }

    /// rano's send as the wire's shape: the file, the range, the caret and whether the text is the
    /// buffer's rather than the disk's.
    fn context_ref(&self, e: &SendEvent) -> ContextRef {
        let (file, lang) = match &e.path {
            Some(p) => (
                p.display().to_string(),
                p.extension()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            // An unnamed buffer has no path to name and no file to read: the text is all there is,
            // and it is unsaved by definition.
            None => (String::new(), String::new()),
        };
        let range = e.selection.as_ref().map(|s| ContextRange {
            start: ContextPos {
                line: s.start.line as u32,
                column: s.start.column as u32,
            },
            end: ContextPos {
                line: s.end.line as u32,
                column: s.end.column as u32,
            },
        });
        ContextRef {
            file,
            range,
            cursor: Some(ContextPos {
                line: e.cursor.line as u32,
                column: e.cursor.column as u32,
            }),
            text: e
                .selection
                .as_ref()
                .map(|s| s.text.clone())
                .unwrap_or_default(),
            unsaved: e.modified,
            lang,
            title: String::new(),
        }
    }

    /// The context this message carries, or `None` when the composer holds no chips. `open` is read
    /// HERE and only while the pane is open: it is a snapshot of one moment, not a watch.
    pub(crate) fn contexts_to_send(&self) -> Option<EditorContext> {
        if self.contexts.is_empty() {
            return None;
        }
        Some(EditorContext {
            origin: "rano".into(),
            refs: self.contexts.iter().map(|c| c.r.clone()).collect(),
            open: self.open_buffers(),
            ask: None,
            outcomes: Vec::new(),
        })
    }

    /// The editor's other buffers, names only.
    fn open_buffers(&self) -> Vec<OpenBuffer> {
        let Some(p) = self.edit_pane.as_ref() else {
            return Vec::new();
        };
        p.ed.buffers
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let name = b.buf.name.as_ref()?;
                Some(OpenBuffer {
                    file: name.display().to_string(),
                    unsaved: b.buf.modified,
                    visible: i == p.ed.cur,
                })
            })
            .collect()
    }

    /// The chips as one line for the composer's bottom edge - the row `/` completions already use,
    /// so a chip costs no row and turns an empty line into a question, which the chips are.
    pub(crate) fn context_legend(&self) -> Option<rano::render::Line> {
        if self.contexts.is_empty() {
            return None;
        }
        let said = self
            .contexts
            .iter()
            .map(|c| c.label.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        Some(rano::render::Line::raw(said))
    }
}

/// The chip's own words: `[context 1 · notes.txt:11-12 · unsaved]`. The number is a label, the
/// place is what a person recognises, and `unsaved` is the one fact that changes what the model
/// can believe about the file.
pub(crate) fn chip_label(
    nth: usize,
    r: &ContextRef,
    relative: &dyn Fn(&std::path::Path) -> String,
) -> String {
    let path = if r.file.is_empty() {
        "(unsaved buffer)".to_string()
    } else {
        relative(std::path::Path::new(&r.file))
    };
    let place = match (&r.range, r.cursor) {
        (Some(range), _) => {
            let last = if range.end.column == 1 && range.end.line > range.start.line {
                range.end.line - 1
            } else {
                range.end.line
            };
            if last == range.start.line {
                format!("{path}:{}", range.start.line)
            } else {
                format!("{path}:{}-{last}", range.start.line)
            }
        }
        (None, Some(at)) => format!("{path}:{}", at.line),
        (None, None) => path,
    };
    match r.unsaved {
        true => format!("[context {nth} · {place} · unsaved]"),
        false => format!("[context {nth} · {place}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_ref() -> ContextRef {
        ContextRef {
            file: "/ws/notes.txt".into(),
            range: Some(ContextRange {
                start: ContextPos {
                    line: 11,
                    column: 1,
                },
                end: ContextPos {
                    line: 13,
                    column: 1,
                },
            }),
            cursor: None,
            text: "eleven\ntwelve\n".into(),
            unsaved: true,
            lang: "txt".into(),
            title: String::new(),
        }
    }

    /// A range that ends at the start of a line does not claim that line - the same rule the old
    /// pasted reference kept, and the reason a selection of two lines reads as `11-12`.
    #[test]
    fn the_label_names_the_range_and_its_unsaved_state() {
        let named = chip_label(1, &a_ref(), &|p| p.display().to_string());
        assert_eq!(named, "[context 1 · /ws/notes.txt:11-12 · unsaved]");
        let mut clean = a_ref();
        clean.unsaved = false;
        assert_eq!(
            chip_label(2, &clean, &|p| p.display().to_string()),
            "[context 2 · /ws/notes.txt:11-12]"
        );
    }
}
