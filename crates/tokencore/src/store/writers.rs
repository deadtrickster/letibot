//! One writer per session's transcript, and the writer is a row: a claim another live process
//! holds refuses the open by name. The trigger caught the old interleaving only after the damage:
//! ```text
//! store: rows 11..12 (s-gatekeeper#t0.11..): row 11 of s-gatekeeper#t0: sqlite: transcript_item
//!        seq must be the next one
//! store: rows 11..12 (s-gatekeeper#t0.11..): sqlite: transcript_item tok_offset must continue the
//!        previous row
//! ```

use rusqlite::{OptionalExtension, params};

use super::{Result, Store};

/// **The table both a fresh store and the v32 migration create.**
pub const WRITERS_SQL: &str = r#"
-- **Who may append to a session's transcript** (schema 32). One row per session, and only while
-- somebody has it open: the harness that claimed it deletes the row when it goes. Nothing here
-- names a transcript, deliberately — a compaction forks one transcript into another inside the
-- session the claim already covers, and a guard that had to move with a fork would be a second
-- thing to get right on the path that replaces a conversation.
CREATE TABLE IF NOT EXISTS session_writer (
    session_id   TEXT PRIMARY KEY,
    owner        TEXT NOT NULL,   -- e.g. `host/pid/ns`; unique per claim, not per process
    pid          INTEGER NOT NULL,
    host         TEXT NOT NULL,
    heartbeat_ms INTEGER NOT NULL, -- when the claim was taken (see the module docs)
    claimed_ms   INTEGER NOT NULL
);
"#;

/// **One session's writer, as the store holds it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWriter {
    pub session_id: String,
    /// The claim's identity - `host/pid/ns`, unique per CLAIM rather than per process: two
    /// `Sessions` in one process that open one session must be two claims, or the guard would
    /// admit the second and the interleaving would happen where no pid can see it.
    pub owner: String,
    pub pid: u32,
    pub host: String,
    pub heartbeat_ms: u64,
    pub claimed_ms: u64,
}

/// What a claim came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterOutcome {
    /// **Yours.** `previous` is the claim it displaced, which is `None` for a free session and
    /// `Some` when the claimant judged the holder gone (that holder is named in the caller's log
    /// line, so a takeover is never silent).
    Claimed { previous: Option<SessionWriter> },
    /// **Somebody else is writing it.** The row is theirs and it was not displaced.
    HeldBy(SessionWriter),
}

const WRITER_COLUMNS: &str = "session_id, owner, pid, host, heartbeat_ms, claimed_ms";

fn writer_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionWriter> {
    // `.max(0) as u64` for the two timestamps, the way the queue lease's rows are read: the
    // column is an INTEGER and the field is a `u64`, and a negative value can only be a row
    // written by something else — this reads it as *long ago* rather than refusing the whole row.
    Ok(SessionWriter {
        session_id: r.get(0)?,
        owner: r.get(1)?,
        pid: r.get(2)?,
        host: r.get(3)?,
        heartbeat_ms: r.get::<_, i64>(4)?.max(0) as u64,
        claimed_ms: r.get::<_, i64>(5)?.max(0) as u64,
    })
}

impl Store {
    /// Claim, or renew, the right to append to `session_id`'s transcript, in one immediate
    /// transaction - a holder that beat in between keeps its claim. `expired_before_ms` is the
    /// caller's judgement (0 never expires), clamped to `i64::MAX` so `u64::MAX` means take it now.
    pub fn claim_session_writer(
        &self,
        session_id: &str,
        owner: &str,
        pid: u32,
        host: &str,
        now_ms: u64,
        expired_before_ms: u64,
    ) -> Result<WriterOutcome> {
        self.write_tx(|tx| {
            // The clamp the doc above explains: `u64::MAX as i64` is `-1`, and a cutoff of `-1`
            // would make "take this over" a comparison that never fires.
            let cutoff = expired_before_ms.min(i64::MAX as u64) as i64;
            let held: Option<SessionWriter> = tx
                .query_row(
                    &format!("SELECT {WRITER_COLUMNS} FROM session_writer WHERE session_id = ?1"),
                    params![session_id],
                    writer_from_row,
                )
                .optional()?;
            let previous = match held {
                // Nobody holds it, or the claimant does (a renewal).
                None => None,
                Some(w) if w.owner == owner => {
                    return Ok(WriterOutcome::Claimed { previous: None });
                }
                // **The holder judged gone**: take the row over, but only if it has not beaten
                // since the caller looked.
                Some(w) if w.heartbeat_ms < expired_before_ms => Some(w),
                Some(w) => return Ok(WriterOutcome::HeldBy(w)),
            };
            let n = tx.execute(
                "INSERT INTO session_writer (session_id, owner, pid, host, heartbeat_ms, claimed_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(session_id) DO UPDATE SET owner = excluded.owner,
                     pid = excluded.pid, host = excluded.host,
                     heartbeat_ms = excluded.heartbeat_ms, claimed_ms = excluded.claimed_ms
                 WHERE session_writer.heartbeat_ms < ?6",
                params![
                    session_id,
                    owner,
                    pid as i64,
                    host,
                    now_ms as i64,
                    cutoff
                ],
            )?;
            if n == 0 {
                // A holder that beat between the read above and this write: it is live, it keeps
                // its session, and this says so rather than reporting a claim it did not get.
                let now_held: SessionWriter = tx.query_row(
                    &format!("SELECT {WRITER_COLUMNS} FROM session_writer WHERE session_id = ?1"),
                    params![session_id],
                    writer_from_row,
                )?;
                return Ok(WriterOutcome::HeldBy(now_held));
            }
            Ok(WriterOutcome::Claimed { previous })
        })
    }

    /// The writer `session_id` has right now, or `None` when nobody has it open.
    pub fn session_writer(&self, session_id: &str) -> Result<Option<SessionWriter>> {
        Ok(self
            .connection()
            .query_row(
                &format!("SELECT {WRITER_COLUMNS} FROM session_writer WHERE session_id = ?1"),
                params![session_id],
                writer_from_row,
            )
            .optional()?)
    }

    /// **Give the session back** — on the harness's way out, and only when the row is still
    /// this owner's. Guarded on the owner because a claim taken over (its holder having been
    /// judged gone) must not be deleted by the zombie that comes back.
    pub fn release_session_writer(&self, session_id: &str, owner: &str) -> Result<bool> {
        let n = self.exec(
            "DELETE FROM session_writer WHERE session_id = ?1 AND owner = ?2",
            params![session_id, owner],
        )?;
        Ok(n == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    /// **A free session is claimed, and giving it back leaves the row gone** — the ordinary
    /// life of a claim, which is every open and every shutdown.
    #[test]
    fn a_free_session_is_claimed_and_given_back() {
        let s = store();
        let claimed = s
            .claim_session_writer("s-1", "h/1/100", 1, "h", 1_000, 0)
            .expect("the store takes a claim");
        assert_eq!(claimed, WriterOutcome::Claimed { previous: None });
        let row = s
            .session_writer("s-1")
            .expect("the row reads")
            .expect("there is one");
        assert_eq!(row.owner, "h/1/100");
        assert_eq!(row.pid, 1);
        assert!(
            s.release_session_writer("s-1", "h/1/100")
                .expect("released")
        );
        assert!(s.session_writer("s-1").expect("the row reads").is_none());
    }

    /// **A claim somebody else made is not displaced by a second one** — the defect, at its
    /// smallest: two writers, one row, and the second is told no.
    #[test]
    fn a_second_claim_is_refused_while_the_first_stands() {
        let s = store();
        s.claim_session_writer("s-1", "h/1/100", 1, "h", 1_000, 0)
            .expect("the first claim");
        let second = s
            .claim_session_writer("s-1", "h/1/200", 1, "h", 1_100, 0)
            .expect("the store answers");
        match second {
            WriterOutcome::HeldBy(h) => assert_eq!(h.owner, "h/1/100"),
            other => panic!("the second writer was let in: {other:?}"),
        }
        // And the row is still the first one's.
        assert_eq!(
            s.session_writer("s-1").unwrap().unwrap().owner,
            "h/1/100",
            "the holder was not overwritten"
        );
    }

    /// **A claim older than the caller's cutoff is taken over, and says whom it displaced** —
    /// which is what a daemon that died mid-session leaves for the next one.
    #[test]
    fn a_stale_claim_is_taken_over_and_the_previous_holder_is_named() {
        let s = store();
        s.claim_session_writer("s-1", "h/1/100", 1, "h", 1_000, 0)
            .expect("the first claim");
        let takeover = s
            .claim_session_writer("s-1", "h/2/900", 2, "h", 900_000, 800_000)
            .expect("the store answers");
        match takeover {
            WriterOutcome::Claimed { previous: Some(p) } => {
                assert_eq!(p.owner, "h/1/100", "the log line has a name to print");
                assert_eq!(p.pid, 1);
            }
            other => panic!("the stale claim was not taken over: {other:?}"),
        }
        assert_eq!(s.session_writer("s-1").unwrap().unwrap().owner, "h/2/900");
    }

    /// `u64::MAX` is "every claim is older than this": the caller passes it for a dead pid, and the
    /// clamp to `i64::MAX` is what keeps `u64::MAX as i64` from being `-1` and never firing.
    #[test]
    fn the_largest_cutoff_means_take_it_now() {
        let s = store();
        s.claim_session_writer("s-1", "h/1/100", 1, "h", 1_000, 0)
            .expect("the first claim");
        // A claim a millisecond old, taken over at once.
        let takeover = s
            .claim_session_writer("s-1", "h/2/1001", 2, "h", 1_001, u64::MAX)
            .expect("the store answers");
        assert!(
            matches!(takeover, WriterOutcome::Claimed { previous: Some(_) }),
            "a dead holder's claim is anyone's at once: {takeover:?}"
        );
        assert_eq!(s.session_writer("s-1").unwrap().unwrap().pid, 2);
    }

    /// **A holder that beat between the caller's read and the claim keeps its session.** The
    /// cutoff is the one the caller JUDGED, so a heartbeat newer than it is a live holder —
    /// which is the compare-and-swap that makes a slow claimant harmless.
    #[test]
    fn a_holder_that_beat_after_the_judgement_keeps_its_session() {
        let s = store();
        s.claim_session_writer("s-1", "h/1/100", 1, "h", 900_000, 0)
            .expect("the claim");
        // The caller judged `800_000` (it read a heartbeat of 700_000), and the holder beat
        // to 900_000 in between.
        let outcome = s
            .claim_session_writer("s-1", "h/2/901", 2, "h", 901_000, 800_000)
            .expect("the store answers");
        match outcome {
            WriterOutcome::HeldBy(h) => assert_eq!(h.owner, "h/1/100"),
            other => panic!("a live holder was displaced: {other:?}"),
        }
    }

    /// **The claim's identity is per claim and not per process.** Two `Sessions` in one process
    /// are two owners, which is what makes the guard reachable at all inside one test binary.
    #[test]
    fn a_second_claim_in_one_process_is_a_second_owner() {
        let s = store();
        s.claim_session_writer("s-1", "h/1/100", 1, "h", 1_000, 0)
            .expect("the first claim");
        let second = s
            .claim_session_writer("s-1", "h/1/101", 1, "h", 1_000, 0)
            .expect("the store answers");
        assert!(
            matches!(second, WriterOutcome::HeldBy(_)),
            "same pid, different owner: the guard has to tell them apart: {second:?}"
        );
    }

    /// **The release is only the owner's.** A claim that was taken over must not be deleted by
    /// the process it was taken from.
    #[test]
    fn the_release_is_guarded_on_the_owner() {
        let s = store();
        s.claim_session_writer("s-1", "h/1/100", 1, "h", 1_000, 0)
            .expect("the claim");
        assert!(
            !s.release_session_writer("s-1", "h/9/999")
                .expect("the store answers"),
            "a stranger's release deletes nothing"
        );
        assert!(s.session_writer("s-1").unwrap().is_some());
    }
}
