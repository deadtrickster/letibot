//! The operator's answer to a shadow note proposal: accept writes it through the `notes` tool, so
//! the near-copy gate and the notes commit apply; decline writes nothing.

use super::*;
use crate::note_accept;
use crate::slash::SlashReply;

impl Harness {
    pub(crate) fn answer_note_proposal(&mut self, accept: bool, row: Option<usize>) -> SlashReply {
        let fail = |line: String| SlashReply {
            lines: vec![line],
            ok: false,
        };
        let Some(at) = row.or_else(|| note_accept::latest(&self.session.items)) else {
            return fail("no shadow note proposal in this session".into());
        };
        let Some(p) = self
            .session
            .items
            .get(at)
            .and_then(note_accept::proposal_of)
        else {
            return fail(format!("row {at} is not a shadow note proposal"));
        };
        if !accept {
            return SlashReply {
                lines: vec![format!(
                    "declined the note proposed on row {at}; nothing written"
                )],
                ok: true,
            };
        }
        let quoted_at = p.row.and_then(|r| {
            let store = self.cfg.store_at()?.open_existing().ok()?;
            store
                .item_created_at(&self.session.transcript_id, r as u64)
                .ok()?
                .map(note_accept::stamp)
        });
        let call = note_accept::note_call(&p, at, &self.cfg.session_id, quoted_at.as_deref());
        let rt = match self.notes_runtime() {
            Ok(rt) => rt,
            Err(why) => return fail(format!("the note was not written: {why}")),
        };
        let r = rt.invoke_operator("note-accept", &call, &mut letibot_tools::NullToolSink);
        let first = r.payload.lines().next().unwrap_or("").to_string();
        match r.outcome {
            letibot_transcript::ToolOutcome::Ok => SlashReply {
                lines: vec![first],
                ok: true,
            },
            _ => fail(format!("the note was not written: {first}")),
        }
    }

    /// The `notes` tool alone, in its own runtime: the seat may not hold `notes`, and the accept is
    /// the operator's act, not the model's.
    fn notes_runtime(&self) -> Result<letibot_tools::runtime::ToolRuntime, String> {
        let mut reg = letibot_tools::runtime::Registry::new();
        reg.register(Box::new(
            letibot_tools::builtins::notes::NotesTool::new(std::sync::Arc::new(
                crate::notes_scope::ConfigNotes::of(&self.cfg),
            ))
            .with_tier(self.cfg.tier()),
        ))
        .map_err(|e| e.to_string())?;
        // Rooted at the notes directory and writable: a read-only seat binds the model, and the
        // tool itself confines the write to the project notes.
        let dir = self.cfg.project_notes();
        if dir.as_os_str().is_empty() {
            return Err("this session has no notes directory".into());
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let host = letibot_tools::HostBackend::writable(&dir).map_err(|e| e.to_string())?;
        Ok(letibot_tools::runtime::ToolRuntime::new(
            reg,
            Box::new(host),
        ))
    }
}
