//! **One process appends to a session's transcript, and this is what says so** — the daemon's half
//! of `letibot_tokencore::store::writers`.
//!
//! # The measurement this exists for
//!
//! MEASURED 2026-10-26 on the operator's Mac, in their log and then in their store: two daemons
//! opened the same session — the review queue's, whose id was one constant for the whole box —
//! and both appended to `s-gatekeeper#t0`. What the log said, twice:
//!
//! ```text
//! store: rows 11..12 (s-gatekeeper#t0.11..): row 11 of s-gatekeeper#t0: sqlite: transcript_item
//!        seq must be the next one
//! store: rows 11..12 (s-gatekeeper#t0.11..): sqlite: transcript_item tok_offset must continue the
//!        previous row
//! ```
//!
//! The trigger noticed, and it noticed too late to help: it refuses the second writer's row, so
//! what is left is one conversation with a hole in it — a transcript that does not restore, from
//! then on. **No daemon could see the other**, and nothing in the store said who was writing.
//!
//! The per-workspace review session id (`letibot_sessionlog::gatekeeper_session`) removes the
//! CAUSE. This removes the CLASS: a session is claimed when a harness opens it, the claim is a
//! row naming the process, and **a claim another live process holds refuses the open by name** —
//! before a single row is written into a transcript somebody else is writing.
//!
//! # Why the claim is taken at open and not at the first append
//!
//! Because an open is where the harm is decided: `Harness::open` rebuilds the ledger, seats the
//! tools and is the object every later write goes through. A second harness for one session is
//! the defect — refusing it at the append would be refusing the SYMPTOM after the turn has begun,
//! with the model already answered by a session that turns out to be two.
//!
//! # What the claim is not
//!
//! It is not a lock on the transcript for the duration of a write: SQLite's own write lock is
//! that, and it is already held per statement. This is the durable, cross-process answer to *may
//! this process have this session at all*, which is a question with no statement to hang it on.
//! It is also not the queue lease (`mergequeue::lease`): that is per repository and says who
//! serves a queue, this is per session and says who may speak in it.

use std::path::{Path, PathBuf};

use letibot_tokencore::store::Store;
use letibot_tokencore::store::writers::{SessionWriter, WriterOutcome};

/// **How old a claim's heartbeat may be before another process may take it over** — the queue
/// lease's [`LEASE_TTL_MS`](crate::mergequeue::lease::LEASE_TTL_MS), and the same argument: long
/// enough that a daemon held up by a slow store is not mistaken for a dead one.
///
/// It is read for a claim from ANOTHER host, and for one from this host whose pid is gone. A
/// claim from this host whose pid is alive is never taken over, however old it is: a session
/// nobody has typed into for a week is still held by the daemon that opened it, and *the
/// heartbeat is old* is not evidence against a running process.
pub const TTL_MS: u64 = crate::mergequeue::lease::LEASE_TTL_MS;

/// **The right to append to one session's transcript, for as long as this value lives.**
///
/// Dropping it gives the session back, and the release is guarded on the owner so a claim that
/// was taken over (its holder judged gone, then coming back) cannot delete the successor's row.
/// It reaches the store by PATH on the way out because the harness's own handle may already be
/// dropped; `Store::open` answers the process's one connection, so the release goes through that
/// rather than through a second one.
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

/// **This daemon's identity for one claim** — host and pid from the lease identity (the process's
/// own, decided once) and a nanosecond suffix so two claims **in one process** are two claims.
///
/// The suffix is load-bearing rather than tidy: two `Sessions` over one store file, inside one
/// test process, must contend for a session the way two daemons do — and a pid cannot tell them
/// apart, because it is the same pid. A claim identity per PROCESS would admit the second and let
/// exactly the interleaving this exists to stop happen inside one process, where no pid and no
/// heartbeat would ever see it.
fn me() -> (String, String, u32) {
    let lease = crate::mergequeue::lease::this_daemon();
    let owner = format!("{}/{}/{}", lease.host, lease.pid, crate::config::now_ns());
    (owner, lease.host.clone(), lease.pid)
}

/// Whether `pid` names a live, non-zombie process — `mergequeue::lease`'s reading, one module
/// over, so a claim and a lease agree about what *gone* means on this host.
fn alive(pid: u32) -> bool {
    letibot_tools::exec::procs::read(pid).is_some_and(|p| p.state != 'Z')
}

/// **Claim this session's transcript for the harness about to open it** — or refuse, naming who
/// has it.
///
/// The refusal is the deliverable and it is one sentence, carrying the four facts a person needs
/// to act: **which session**, **which process** (pid, host, and since when), **what it means**
/// (two writers, and what the store does about them), and **what to do** (attach to it, or stop
/// it). A refusal that said only *busy* would leave the reader to guess whether the store was
/// locked, the session missing, or the box out of memory.
pub fn claim(store: &Store, session_id: &str) -> Result<Claim, String> {
    let (owner, host, pid) = me();
    let now = letibot_sessionlog::registry::now_ms();
    // **The cutoff is the caller's judgement, and it is the whole of the liveness rule.** `0` is a
    // cutoff nothing is older than — *never take this over* — and `u64::MAX` is *every claim is
    // older than this*. On this host the pid decides between them: a live holder's claim is
    // nobody's however old it is, and a dead holder's is anyone's at once, because the row's
    // heartbeat dates the claim rather than beating. A claim from another host expires on the
    // TTL, since its pid cannot be asked from here.
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
                // **A takeover is never silent.** The other side of it is a process whose claim
                // this one decided was dead, and the daemon's log is where the judgement lands so
                // that a wrong one is legible afterwards.
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

/// The one sentence a refused open answers with — see [`claim`].
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
