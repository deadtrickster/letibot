//! Operator prompts a session accepted and has not started, kept so a killed daemon does not lose
//! them. The hub's queue is the truth while the daemon lives and this table is its copy.

use rusqlite::params;

use super::{Result, Store};

pub const PROMPTS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS queued_prompt (
    session_id         TEXT NOT NULL,
    seq                INTEGER NOT NULL,   -- position in the queue, oldest first
    enqueued_ms        INTEGER NOT NULL,
    head_id            TEXT NOT NULL,
    identity           TEXT NOT NULL,
    client_request_id  TEXT NOT NULL,
    text               TEXT NOT NULL,
    attachments_json   TEXT NOT NULL,
    -- the editor context the prompt carried, empty when it carried none
    context_json       TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (session_id, seq)
);
"#;

/// One accepted, unstarted prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPrompt {
    pub enqueued_ms: u64,
    pub head_id: String,
    pub identity: String,
    pub client_request_id: String,
    pub text: String,
    /// The attachments as the wire spells them.
    pub attachments_json: String,
    /// What the operator pointed at in an editor, as the wire spells it. Empty is none: the
    /// common case, and the one that must not cost a JSON document per queued prompt.
    pub context_json: String,
}

impl Store {
    /// Make the stored queue of `session_id` exactly `prompts`, in order.
    pub fn replace_queued_prompts(&self, session_id: &str, prompts: &[QueuedPrompt]) -> Result<()> {
        self.write_tx_at("queued_prompt", |tx| {
            tx.execute(
                "DELETE FROM queued_prompt WHERE session_id = ?1",
                params![session_id],
            )?;
            for (seq, p) in prompts.iter().enumerate() {
                tx.execute(
                    "INSERT INTO queued_prompt (session_id, seq, enqueued_ms, head_id, identity, \
                     client_request_id, text, attachments_json, context_json) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        session_id,
                        seq as i64,
                        p.enqueued_ms as i64,
                        p.head_id,
                        p.identity,
                        p.client_request_id,
                        p.text,
                        p.attachments_json,
                        p.context_json
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// The stored queue of `session_id`, oldest first.
    pub fn queued_prompts(&self, session_id: &str) -> Result<Vec<QueuedPrompt>> {
        let conn = self.at("queued_prompt");
        let mut st = conn.prepare(
            "SELECT enqueued_ms, head_id, identity, client_request_id, text, attachments_json, \
             context_json FROM queued_prompt WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = st.query_map(params![session_id], |r| {
            Ok(QueuedPrompt {
                enqueued_ms: r.get::<_, i64>(0)?.max(0) as u64,
                head_id: r.get(1)?,
                identity: r.get(2)?,
                client_request_id: r.get(3)?,
                text: r.get(4)?,
                attachments_json: r.get(5)?,
                context_json: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
