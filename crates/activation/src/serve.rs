//! The accept loop an activated helper runs, and the bookkeeping its idle exit reads. Accepting
//! and deciding to exit happen on one thread, so a connection is either accepted and counted
//! busy, or still in the backlog the manager keeps for the next start.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// When the helper was last used, and how many connections it is serving now.
pub struct Activity {
    last: Mutex<Instant>,
    busy: AtomicUsize,
}

impl Default for Activity {
    fn default() -> Self {
        Activity {
            last: Mutex::new(Instant::now()),
            busy: AtomicUsize::new(0),
        }
    }
}

impl Activity {
    pub fn touch(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    pub fn idle_for(&self) -> Duration {
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed()
    }

    pub fn in_flight(&self) -> usize {
        self.busy.load(Ordering::SeqCst)
    }

    /// Held by a connection's thread for its whole life. Taken on the accepting thread.
    pub fn busy(self: &Arc<Self>) -> Busy {
        self.busy.fetch_add(1, Ordering::SeqCst);
        Busy(self.clone())
    }
}

pub struct Busy(Arc<Activity>);

impl Drop for Busy {
    // The end of a long stream is use too: the idle clock starts when it closes.
    fn drop(&mut self) {
        self.0.touch();
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Whether an idle helper may exit: nothing held open, and nothing asked for `limit`.
pub fn idle_due(idle_for: Duration, limit: Duration, holds: usize) -> bool {
    holds == 0 && idle_for >= limit
}

/// Poll `fds` for readiness, calling `ready(i)` for each readable one, until a poll that found
/// nothing within `tick` is followed by `stop()` saying so. Returning stops the polling.
pub fn poll_loop(
    fds: &[RawFd],
    tick: Duration,
    mut ready: impl FnMut(usize),
    mut stop: impl FnMut() -> bool,
) {
    let mut set: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let ms = tick.as_millis().min(i32::MAX as u128) as libc::c_int;
    loop {
        for p in set.iter_mut() {
            p.revents = 0;
        }
        // SAFETY: `set` is a live array of `set.len()` pollfds.
        let n = unsafe { libc::poll(set.as_mut_ptr(), set.len() as libc::nfds_t, ms) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if n == 0 {
            if stop() {
                return;
            }
            continue;
        }
        for (i, p) in set.iter().enumerate() {
            if p.revents != 0 {
                ready(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn idle_needs_both_quiet_and_nothing_held() {
        let ten = Duration::from_secs(600);
        assert!(idle_due(ten, ten, 0));
        assert!(!idle_due(ten - Duration::from_secs(1), ten, 0));
        assert!(!idle_due(ten * 2, ten, 1));
    }

    #[test]
    fn busy_counts_and_its_end_restarts_the_clock() {
        let a = Arc::new(Activity::default());
        let b = a.busy();
        assert_eq!(a.in_flight(), 1);
        std::thread::sleep(Duration::from_millis(30));
        drop(b);
        assert_eq!(a.in_flight(), 0);
        assert!(a.idle_for() < Duration::from_millis(30));
    }

    #[test]
    fn a_pending_connection_is_left_in_the_backlog_when_the_loop_stops() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap();
        let mut accepted = 0;
        let mut polls = 0;
        poll_loop(
            &[l.as_raw_fd()],
            Duration::from_millis(20),
            |_| {
                while l.accept().is_ok() {
                    accepted += 1;
                }
            },
            || {
                polls += 1;
                polls >= 2
            },
        );
        assert_eq!(accepted, 0);
        let _c = std::net::TcpStream::connect(addr).unwrap();
        // Not polled any more: the connection waits, and the next accept takes it.
        std::thread::sleep(Duration::from_millis(50));
        assert!(l.accept().is_ok());
    }
}
