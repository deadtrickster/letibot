//! The engine the daemon loads its layers with. It lives in `letibot-plugins-rano` now: the daemon,
//! the head and the bridge each had a copy of this loader, and copying it is how the head came to
//! compile thirteen built-ins per host build while the daemon compiled each one once per process.
//! Re-exported rather than renamed at every call site, so the daemon's own tests still ask the
//! daemon for its engine and get the one the daemon uses.

pub use letibot_plugins_rano::RanoLoader;
