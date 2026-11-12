//! What a closed session leaves that the worker cannot reach: its VM.
//!
//! A close is answered on the connection's thread, but the harness - and with it the firecode
//! backend that owns the guest - belongs to the worker, which may be inside that guest's tool call.
//! Then `Work::Close` is served minutes later, and until it is served nothing else on the worker is
//! served either: a VM stays up and the next session never boots. This is the door that settles it
//! without waiting for the worker, and it is deliberately the backend's OWN `down`, so the copy-out
//! the operator relies on still happens.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::registry::{CloseSettler, Registry};

/// How long the worker gets to close the harness itself first. A worker between items answers in
/// milliseconds; one that has not after this is inside a call, and that call is holding the guest.
pub const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// How often that window is looked at: the answer is usually the first look, so the wait is cheap.
const POLL: Duration = Duration::from_millis(250);

/// Install the settler on a daemon's registry.
pub fn install(registry: &Arc<Registry>) {
    registry.set_close_settler(Arc::new(Settles));
}

/// [`CloseSettler`] over this process's own record of the VMs it booted.
struct Settles;

impl CloseSettler for Settles {
    fn closed(&self, session_id: &str) {
        let session = session_id.to_string();
        // Its own thread: the connection's next act is the ack a head is waiting for, and a `down`
        // waits on a client and then on the copy-out.
        std::thread::spawn(move || {
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
