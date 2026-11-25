//! The agent a provider's model calls are made through, and the one door a test may put in it.
//!
//! ureq's own agent and resolver unless a fault is installed: a test that must show a turn living
//! through a name that will not resolve installs one and clears it again, and no DNS is involved.

use std::sync::{Arc, OnceLock, RwLock};

/// The error a name fails with, or `None` to let the real resolver answer.
pub type Fault = Arc<dyn Fn(&str) -> Option<std::io::Error> + Send + Sync>;

static FAULT: OnceLock<RwLock<Option<Fault>>> = OnceLock::new();

fn slot() -> &'static RwLock<Option<Fault>> {
    FAULT.get_or_init(|| RwLock::new(None))
}

/// Install this process's resolution fault, or clear it with `None`.
pub fn fail_resolution_with(f: Option<Fault>) {
    *slot().write().unwrap_or_else(|e| e.into_inner()) = f;
}

/// The agent a provider's deadlines were built into, over this process's resolver.
pub fn build(config: ureq::config::Config) -> ureq::Agent {
    let fault = slot().read().unwrap_or_else(|e| e.into_inner()).clone();
    match fault {
        None => ureq::Agent::new_with_config(config),
        Some(fault) => ureq::Agent::with_parts(
            config,
            ureq::unversioned::transport::DefaultConnector::default(),
            Faulty { fault },
        ),
    }
}

/// ureq's resolver with a fault in front of it: the names it answers are the ones that fail.
struct Faulty {
    fault: Fault,
}

impl std::fmt::Debug for Faulty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Faulty")
    }
}

impl ureq::unversioned::resolver::Resolver for Faulty {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        if let Some(host) = uri.host()
            && let Some(e) = (self.fault)(host)
        {
            return Err(ureq::Error::Io(e));
        }
        ureq::unversioned::resolver::DefaultResolver::default().resolve(uri, config, timeout)
    }
}
