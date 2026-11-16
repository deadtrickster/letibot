//! The engine the daemon loads its layers with, from `letibot-plugins-rano`, shared with the head
//! and the bridge so each built-in compiles once. Re-exported so call sites and the daemon's
//! tests ask the daemon for its engine.

pub use letibot_plugins_rano::RanoLoader;
