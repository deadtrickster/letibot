//! What a closed session leaves that the worker cannot reach: its VM, and the owner row that says
//! its compute is gone.
//!
//! A close is answered on the connection's thread, but the harness - and with it the firecode
//! backend that owns the guest - belongs to the worker, which may be inside that guest's tool call.
//! Then `Work::Close` is served minutes later, and until it is served nothing else on the worker is
//! served either: a VM stays up and the next session never boots. This is the door that settles it
//! without waiting for the worker, and it is deliberately the backend's OWN `down`, so the copy-out
//! the operator relies on still happens.
//!
//! The owner's row is the other half (`docs/lifecycle.md` d.3): `closed` is a state nothing goes
//! back from on its own, so the plan stops calling a closed session's leftovers live, and the
//! idle-exit test stops holding a daemon for a session that is no longer running.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::registry::{CloseSettler, Registry};
use letibot_tokencore::store::{OwnerState, ReapAct, ReapEvent};

use crate::config::StoreAt;

/// How long the worker gets to close the harness itself first. A worker between items answers in
/// milliseconds; one that has not after this is inside a call, and that call is holding the guest.
pub const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// How often that window is looked at: the answer is usually the first look, so the wait is cheap.
const POLL: Duration = Duration::from_millis(250);

/// Install the settler on a daemon's registry. `at` is where the owner row lives: a daemon with no
/// store settles only the VM.
pub fn install(registry: &Arc<Registry>, at: Option<StoreAt>) {
    registry.set_close_settler(Arc::new(Settles { at }));
}

/// [`CloseSettler`] over this process's own record of the VMs it booted, and over the owner row.
struct Settles {
    at: Option<StoreAt>,
}

impl CloseSettler for Settles {
    fn closed(&self, session_id: &str) {
        let session = session_id.to_string();
        let at = self.at.clone();
        // Its own thread: the connection's next act is the ack a head is waiting for, and a `down`
        // waits on a client and then on the copy-out.
        std::thread::spawn(move || {
            owner_closed(at.as_ref(), &session);
            if !letibot_tools::firecode::held_for(&session) {
                return;
            }
            let started = Instant::now();
            while started.elapsed() < CLOSE_GRACE {
                // The worker got there first, which is the ordinary case, and it already did the
                // copy-out: nothing here to settle.
                if !letibot_tools::firecode::held_for(&session) {
                    return;
                }
                std::thread::sleep(POLL);
            }
            if letibot_tools::firecode::down_session(&session) {
                eprintln!(
                    "  {session} · closed while its worker was busy; its VM was brought down here, \
                     and the client blocked in it comes back"
                );
            }
        });
    }
}

/// Write the owner's `closed` state down, once: this is the close trigger the design calls
/// `leticode_stop`, and what it leaves is the row the plan and the daemon's idle exit read.
fn owner_closed(at: Option<&StoreAt>, session: &str) {
    let Some(at) = at else {
        return;
    };
    let Ok(store) = at.open() else {
        return;
    };
    let Ok(Some(owner)) = store.owner(session) else {
        return;
    };
    if owner.state.compute_gone() {
        return;
    }
    let now = letibot_metrics::now_ms() as i64;
    let why =
        "closed by `leticode_stop` (or a head quitting): its compute is gone and the rows stay";
    if store
        .set_owner_state(session, OwnerState::Closed, why, now)
        .is_err()
    {
        return;
    }
    let _ = store.put_reap_event(&ReapEvent {
        at_ms: now,
        owner_id: session.to_string(),
        kind: None,
        host: None,
        handle: None,
        act: ReapAct::Close.word().to_string(),
        said: why.to_string(),
        by: "the session's close".into(),
    });
}
