//! The store's rows as the room's claim checker reads them: a merge row, its target's gate, and the
//! message's own words. The judging is the room crate's (`letibot_room::claim`); this is the
//! reading, the only half that needs a store, and it is why the check lives in the daemon.

use std::path::Path;

use letibot_room::claim::{Fact, check};
use letibot_tokencore::store::{HealthState, Store};

/// Every merge row the store has, each with the gate record of its own target. An unreadable store
/// is said rather than read as *no rows*: an empty answer would read as NOT COVERED for everything.
pub fn facts(db: &Path) -> Result<Vec<Fact>, String> {
    let store = Store::open(db).map_err(|e| format!("{}: {e}", db.display()))?;
    let health = store.target_healths().map_err(|e| e.to_string())?;
    let rows = store.merge_rows(None).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let gate = health
                .iter()
                .find(|h| h.repository == r.repository && h.target == r.target)
                .map(|h| h.state == HealthState::Green);
            Fact {
                id: r.id,
                repository: r.repository,
                target: r.target,
                branch: r.branch,
                state: r.state.as_str().to_string(),
                approved_sha: r.approved_sha,
                landed_sha: r.landed_sha.unwrap_or_default(),
                pushed: !r.pushed.trim().is_empty(),
                gate_green: gate,
                evidence: r.evidence,
            }
        })
        .collect())
}

/// One line per message that claims a state: its verdict, the row that decided it, and nothing
/// about the messages that claim none - a check that always speaks would be noise on every read.
pub fn lines(rows: &[letibot_room::Message], facts: &[Fact]) -> String {
    let said: Vec<String> = rows
        .iter()
        .filter_map(|m| {
            let checked = check(&m.text, m.evidence(), facts)?;
            Some(format!(
                "#{} ({}): {} - {}",
                m.seq,
                m.author,
                checked.verdict.word(),
                checked.why
            ))
        })
        .collect();
    if said.is_empty() {
        return format!("check: no state claim in these {} message(s)", rows.len());
    }
    format!(
        "check ({} claim(s) against {} merge row(s)):\n  {}",
        said.len(),
        facts.len(),
        said.join("\n  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_room::{Message, Ref, Seat};

    fn message(seq: u64, text: &str, evidence: Option<&str>) -> Message {
        Message {
            seq,
            ts: 0,
            room: "host".into(),
            author: "s-peer".into(),
            kind: Seat::Session,
            to: None,
            thread: None,
            refs: evidence
                .map(|id| {
                    vec![Ref {
                        kind: letibot_room::EVIDENCE.into(),
                        id: id.into(),
                    }]
                })
                .unwrap_or_default(),
            attachments: Vec::new(),
            text: text.into(),
        }
    }

    fn landed() -> Fact {
        Fact {
            id: "m-7".into(),
            repository: "/box/letibot".into(),
            target: "main".into(),
            branch: "topic/x".into(),
            state: "landed".into(),
            approved_sha: "aaaaaaa1111".into(),
            landed_sha: "2b97a092".into(),
            pushed: true,
            gate_green: Some(true),
            evidence: "landed on main".into(),
        }
    }

    /// A claim is answered with the verdict and the row behind it, and a message that claims
    /// nothing gets no line at all.
    #[test]
    fn a_check_names_the_verdict_and_the_row_it_rests_on() {
        let rows = vec![
            message(4, "landed in 2b97a092", None),
            message(5, "landed as m-8", None),
            message(6, "who has the disk?", None),
            message(7, "port 5433 is free", Some("q-3")),
        ];
        let said = lines(&rows, &[landed()]);
        assert!(
            said.starts_with("check (3 claim(s) against 1 merge row(s)):"),
            "{said}"
        );
        assert!(
            said.contains("#4 (s-peer): SUPPORTED - merge m-7 is `landed`"),
            "{said}"
        );
        assert!(
            said.contains("#5 (s-peer): NOT COVERED - no merge or gate row names `m-8`"),
            "{said}"
        );
        assert!(
            said.contains("#7 (s-peer): NOT COVERED - the store holds no port rows"),
            "{said}"
        );
        assert!(
            !said.contains("#6 "),
            "nothing claimed, nothing checked: {said}"
        );
        assert_eq!(
            lines(&rows[2..3], &[]),
            "check: no state claim in these 1 message(s)"
        );
    }

    /// The reading is the store's own rows: a landed merge row and a green target are what make a
    /// claim SUPPORTED, and a repository's own gate is read for the row's target only.
    #[test]
    fn the_rows_come_out_of_the_store_itself() {
        let dir = std::env::temp_dir().join(format!("letibot-claim-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let store = Store::open(&db).expect("a store");
        assert!(
            facts(&db).expect("a read").is_empty(),
            "an empty store covers nothing"
        );
        store
            .connection()
            .execute_batch(
                "INSERT INTO merge_queue (id, review_id, repository, target, branch, approved_sha,
                    approved_by, priority, state, landed_sha, pushed, evidence, created_ms, state_ms,
                    updated_ms)
                 VALUES ('m-3', 'r-3', '/box/letibot', 'main', 'topic/y', 'aaaaaaa1111', 'queue',
                    'urgent', 'landed', '2b97a092', 'pushed origin/main', 'landed', 1, 1, 1);
                 INSERT INTO target_health (repository, target, sha, state, checked_ms, evidence)
                 VALUES ('/box/letibot', 'main', '2b97a092', 'green', 1, 'the gate passed');",
            )
            .expect("a landed row and a green target");
        let read = facts(&db).expect("a read");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, "m-3");
        assert_eq!(read[0].state, "landed");
        assert!(read[0].pushed, "the row's own push note");
        assert_eq!(read[0].gate_green, Some(true));
        let said = lines(&[message(9, "landed: gate green", Some("m-3"))], &read);
        assert!(said.contains("SUPPORTED"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
