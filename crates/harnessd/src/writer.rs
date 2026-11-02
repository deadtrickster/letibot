//! One process appends to a session's transcript, and this is what says so: the
//! daemon's half of `letibot_tokencore::store::writers`.
//!
//! Two daemons once opened the same session and both appended to it. The store's
//! trigger noticed, and too late to help: it refuses the second writer's row, so
//! what is left is one conversation with a hole in it, and no daemon could see
//! the other.
//!
//! ```text
//! store: rows 11..12 (s-gatekeeper#t0.11..): row 11 of s-gatekeeper#t0: sqlite: transcript_item
//!        seq must be the next one
//! store: rows 11..12 (s-gatekeeper#t0.11..): sqlite: transcript_item tok_offset must continue the
//!        previous row
//! ```
//!
//! So a session is claimed when a harness opens it, and a claim another live
//! process holds refuses the open by name, before a single row is written.
//!
//! Taken at open, not at the first append: an open is where the harm is decided
//! and it is not a write lock (SQLite has that) nor the per-repository lease.

use std::path::{Path, PathBuf};

use letibot_tokencore::store::Store;
use letibot_tokencore::store::writers::{SessionWriter, WriterOutcome};

/// How old a claim's heartbeat may be before another process may take it over long enough
/// that a daemon held up by a slow store is not mistaken for dead.
pub const TTL_MS: u64 = crate::mergequeue::lease::LEASE_TTL_MS;

/// The right to append to one session's transcript, for as long as this value lives.
/// Dropping it gives the session back, guarded on the owner so a claim that was taken over
/// cannot delete the successor's row.
pub struct Claim {
    /// `None` for an in-memory store, which no other process can reach and which has no file for
    /// the release to open.
    path: Option<PathBuf>,
    session_id: String,
    owner: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        // Best effort, and silent: a store that cannot be opened on the way out is a store that
        // has already gone, and the claim it leaves behind is one the pid's death will retire.
        if let Some(path) = &self.path
            && let Ok(store) = Store::open(path)
        {
            let _ = store.release_session_writer(&self.session_id, &self.owner);
        }
    }
}

/// This daemon's identity for one claim: host and pid from the lease identity, and a
/// nanosecond suffix so two claims in one process are two claims. The suffix is
/// load-bearing:
fn me() -> (String, String, u32) {
    let lease = crate::mergequeue::lease::this_daemon();
    let owner = format!("{}/{}/{}", lease.host, lease.pid, crate::config::now_ns());
    (owner, lease.host.clone(), lease.pid)
}

/// Whether `pid` names a live, non-zombie process: `mergequeue::lease`'s
/// reading, so a claim and a lease agree about what *gone* means on this host.
fn alive(pid: u32) -> bool {
    letibot_tools::exec::procs::read(pid).is_some_and(|p| p.state != 'Z')
}

/// Claim this session's transcript for the harness about to open it, or refuse naming who
/// has it. The refusal carries the four facts a person needs: which session, which process,
/// what two writers do to the store, and what to do about it.
pub fn claim(store: &Store, session_id: &str) -> Result<Claim, String> {
    let (owner, host, pid) = me();
    let now = letibot_sessionlog::registry::now_ms();
    // The cutoff is the caller's judgement and the whole of the liveness rule: 0 is never take
    // this over, `u64::MAX` is every claim is older than this.
    let expired_before = match store
        .session_writer(session_id)
        .map_err(|e| e.to_string())?
    {
        None => 0,
        Some(h) if h.host == host && alive(h.pid) => 0,
        Some(h) if h.host == host => u64::MAX,
        Some(_) => now.saturating_sub(TTL_MS),
    };
    let outcome = store
        .claim_session_writer(session_id, &owner, pid, &host, now, expired_before)
        .map_err(|e| format!("claiming the session's writer row: {e}"))?;
    match outcome {
        WriterOutcome::Claimed { previous } => {
            if let Some(p) = previous {
                // A takeover is never silent: the other side of it is a process
                // this one decided was dead, and the log is where the judgement
                // lands so that a wrong one is legible afterwards.
                eprintln!(
                    "  {session_id}: took the session over from {owner_of} — pid {pid} on {host}, \
                     which claimed it {} s ago and is no longer running (or is on a host this \
                     daemon cannot ask about pids)",
                    now.saturating_sub(p.claimed_ms) / 1000,
                    owner_of = p.owner,
                    pid = p.pid,
                    host = p.host,
                );
            }
            Ok(Claim {
                path: store.path().map(Path::to_path_buf),
                session_id: session_id.to_string(),
                owner,
            })
        }
        WriterOutcome::HeldBy(h) => Err(refusal(session_id, &h, now)),
    }
}

/// The one sentence a refused open answers with; see [`claim`].
fn refusal(session_id: &str, holder: &SessionWriter, now_ms: u64) -> String {
    format!(
        "session {session_id} is being written by another process — {owner}, pid {pid} on {host}, \
         which claimed it {held} ago and is still running. Two processes appending to one \
         transcript is what corrupts it: the store's append-only trigger refuses the second \
         writer's rows (`transcript_item seq must be the next one`) and the conversation is left \
         with a hole no restart repairs. Nothing was opened and nothing was written. Attach to \
         the daemon that holds it (`leticode_attach`, or `letibot --status` for the folder), or \
         stop that process and open this one again — its claim is given back when it goes.",
        owner = holder.owner,
        pid = holder.pid,
        host = holder.host,
        held = human_age(now_ms.saturating_sub(holder.claimed_ms)),
    )
}

/// How long ago, in the words a diagnosis wants: seconds up to a minute, then minutes.
fn human_age(ms: u64) -> String {
    match ms / 1000 {
        s if s < 60 => format!("{s} s"),
        s => format!("{} min", s / 60),
    }
}
