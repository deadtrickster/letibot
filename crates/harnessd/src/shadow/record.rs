//! The shadow's stored session, parented to the one it watches. Its rows are display-only and carry
//! no tokens, so they are written straight to the store on the shadow's own empty prefix.

use std::sync::Mutex;

use letibot_tokencore::ledger::{LedgerRow, chain};
use letibot_tokencore::store::{SessionRecord, Store};
use letibot_transcript::{OfferKind, TranscriptItem};

use super::Job;

/// Where the shadow's rows go. A trait so the runner's tests record into a vector.
pub trait Recorder: Send + Sync {
    /// One row: the headline a list draws, and the body a click opens.
    fn row(&self, title: &str, body: &str);
    /// One job's state, for the line the Sessions group shows.
    fn state(&self, _job: Job, _line: &str) {}
    /// Every row body written so far, oldest first: what a persistent shadow resumes from.
    fn bodies(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What one append needs to continue the chain.
struct Tail {
    store: Store,
    seq: u32,
    offset: u32,
    head: [u8; 32],
}

/// Why a shadow session could not be opened: the watched session is not stored yet, or a fault.
#[derive(Debug)]
pub enum OpenError {
    NotYet(String),
    Fault(String),
}

/// The shadow's own prefix: no tools and no tokens, the job prompts as its system. Its record then
/// says what the shadow is, rather than carrying the watched session's prompt and tools. The
/// dialect and vocabulary are the watched one's, which is what the store keys a prefix by.
pub fn shadow_prefix(
    like: &letibot_tokencore::store::StablePrefixRecord,
) -> letibot_tokencore::store::StablePrefixRecord {
    let mut system = String::new();
    for job in Job::ALL {
        system.push_str(&format!("[{}]\n", job.as_str()));
        system.push_str(super::prompts::builtin(job));
        // The permission job is two questions, and both are its prefix: a shadow whose second
        // prompt changed is a different prefix, as a changed first one is.
        if let Some(asking) = super::prompts::builtin_asking(job) {
            system.push_str(&format!("\n[{} asking]\n{asking}", job.as_str()));
        }
        system.push('\n');
    }
    letibot_tokencore::store::StablePrefixRecord {
        dialect_sha: like.dialect_sha.clone(),
        system,
        tools_json: Vec::new(),
        tokens: Vec::new(),
        h_init: letibot_tokencore::ledger::hash_tokens(&[]),
        vocab_source: like.vocab_source.clone(),
        effort: None,
    }
}

pub struct StoreRecorder {
    session_id: String,
    transcript_id: String,
    tail: Mutex<Tail>,
    states: Mutex<std::collections::BTreeMap<Job, String>>,
}

/// The title the shadow session is listed under, with the job states after it.
pub const TITLE: &str = "shadow";

impl StoreRecorder {
    /// The shadow session of `watched` in `store`, created on first use. It needs the watched
    /// session's row and transcript: it is parented to one and takes the other's dialect.
    pub fn open(
        store: Store,
        watched: &str,
        model_label: &str,
    ) -> Result<StoreRecorder, OpenError> {
        let e = |what: &str, err: letibot_tokencore::store::StoreError| {
            OpenError::Fault(format!("{what}: {err}"))
        };
        let id = super::session_id(watched);
        if store
            .session(&id)
            .map_err(|x| e("the shadow's row", x))?
            .is_none()
        {
            let parent = store
                .session(watched)
                .map_err(|x| e("the watched row", x))?
                .ok_or_else(|| OpenError::NotYet(format!("{watched} has no stored row yet")))?;
            let ptid = store
                .current_transcript_id(watched)
                .map_err(|x| e("the watched transcript", x))?
                .ok_or_else(|| OpenError::NotYet(format!("{watched} has no transcript yet")))?;
            let watched_prefix = store
                .transcript_prefix_id(&ptid)
                .map_err(|x| e("the watched prefix", x))?
                .ok_or_else(|| OpenError::Fault(format!("{ptid} has no stable prefix")))?;
            let like = store
                .stable_prefix_record(&watched_prefix)
                .map_err(|x| e("the watched prefix", x))?;
            let prefix = store
                .put_stable_prefix(&shadow_prefix(&like))
                .map_err(|x| e("the shadow's prefix", x))?;
            store
                .put_session(&SessionRecord {
                    id: id.clone(),
                    title: Some(TITLE.to_string()),
                    model_id: model_label.to_string(),
                    dialect_sha: parent.dialect_sha,
                    workspace_root: parent.workspace_root,
                    owner: parent.owner,
                    approvers: vec![],
                    role: Some(letibot_sessionlog::SHADOW_SEAT.to_string()),
                    parent_session_id: Some(watched.to_string()),
                })
                .map_err(|x| e("seating the shadow", x))?;
            store
                .set_session_origin(&id, letibot_sessionlog::SessionOrigin::Shadow.as_str())
                .map_err(|x| e("the shadow's origin", x))?;
            store
                .put_transcript(&format!("{id}#t0"), &id, &prefix)
                .map_err(|x| e("the shadow's transcript", x))?;
        }
        let tid = store
            .current_transcript_id(&id)
            .map_err(|x| e("the shadow's transcript", x))?
            .ok_or_else(|| OpenError::Fault(format!("{id} has no transcript")))?;
        let loaded = store
            .load_transcript(&tid)
            .map_err(|x| e("loading the shadow", x))?;
        let offset = loaded.prefix_tokens.len() as u32
            + loaded.items.iter().map(|(_, r, _)| r.tok_len).sum::<u32>();
        let head = loaded.items.last().map_or(loaded.h_init, |(_, r, _)| r.h_k);
        Ok(StoreRecorder {
            session_id: id,
            transcript_id: tid,
            tail: Mutex::new(Tail {
                seq: loaded.items.len() as u32,
                store,
                offset,
                head,
            }),
            states: Mutex::default(),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn append(&self, title: &str, body: &str) -> Result<(), String> {
        // Another writer (a head that opened the shadow) may have moved the tail: re-read it once.
        if self.try_append(title, body).is_ok() {
            return Ok(());
        }
        self.resync()?;
        self.try_append(title, body)
    }

    fn resync(&self) -> Result<(), String> {
        let mut t = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        let loaded = t
            .store
            .load_transcript(&self.transcript_id)
            .map_err(|e| e.to_string())?;
        t.offset = loaded.prefix_tokens.len() as u32
            + loaded.items.iter().map(|(_, r, _)| r.tok_len).sum::<u32>();
        t.head = loaded.items.last().map_or(loaded.h_init, |(_, r, _)| r.h_k);
        t.seq = loaded.items.len() as u32;
        Ok(())
    }

    fn try_append(&self, title: &str, body: &str) -> Result<(), String> {
        let mut t = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        let item = TranscriptItem::Offer {
            kind: OfferKind::Shadow,
            title: title.to_string(),
            body: body.to_string(),
        };
        let row = LedgerRow {
            item_id: format!("{}.{}", self.transcript_id, t.seq),
            tok_offset: t.offset,
            tok_len: 0,
            h_k: chain(&t.head, &[]),
        };
        t.store
            .append_item(&self.transcript_id, t.seq, &item, &row, &[])
            .map_err(|e| e.to_string())?;
        t.seq += 1;
        t.head = row.h_k;
        Ok(())
    }
}

impl Recorder for StoreRecorder {
    fn row(&self, title: &str, body: &str) {
        // A record that cannot be written is said on stderr: the session it watches goes on.
        if let Err(e) = self.append(title, body) {
            eprintln!("  {} · shadow row not written: {e}", self.session_id);
        }
    }

    fn bodies(&self) -> Vec<String> {
        let t = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(loaded) = t.store.load_transcript(&self.transcript_id) else {
            return Vec::new();
        };
        loaded
            .items
            .into_iter()
            .filter_map(|(item, _, _)| match item {
                TranscriptItem::Offer {
                    kind: OfferKind::Shadow,
                    body,
                    ..
                } => Some(body),
                _ => None,
            })
            .collect()
    }

    fn state(&self, job: Job, line: &str) {
        let title = {
            let mut s = self.states.lock().unwrap_or_else(|e| e.into_inner());
            s.insert(job, line.to_string());
            let lines: Vec<&str> = s.values().map(String::as_str).collect();
            format!("{TITLE} · {}", lines.join(" · "))
        };
        let t = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        let _ = t.store.set_title(&self.session_id, &title);
    }
}

/// The last turn_end verdict the shadow of `watched` recorded: its position, its word, and the
/// promise an `owed` quoted. Read from the rows' own titles, so a restart needs no other state.
pub fn last_turn_end(store: &Store, watched: &str) -> Option<(u64, String, Option<String>)> {
    let id = super::session_id(watched);
    let tid = store.current_transcript_id(&id).ok()??;
    let loaded = store.load_transcript(&tid).ok()?;
    loaded.items.iter().rev().find_map(|(item, _, _)| {
        let TranscriptItem::Offer {
            kind: OfferKind::Shadow,
            title,
            body,
        } = item
        else {
            return None;
        };
        let rest = title.strip_prefix("turn_end · ")?;
        let word = rest.split(" · ").next()?.to_string();
        let position = title.rsplit_once(" at #")?.1.trim().parse().ok()?;
        // The last sections are ours: a watched row quoting a marker earlier forges nothing.
        let body = body.trim_end_matches('\n');
        let body = match body.rsplit_once('\n') {
            Some((head, last)) if last.starts_with(super::resume::MARK) => head,
            _ => body,
        };
        let promise = body
            .rsplit_once("--- promise\n")
            .map(|(_, p)| p.trim_end_matches('\n').to_string());
        Some((position, word, promise))
    })
}
