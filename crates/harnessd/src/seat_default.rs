//! Where a session with no head runs its tools.
//!
//! `[seat] default` (a `leticode.toml` key) says `vm` or `host`, and this module is the one place
//! that turns the three things a start knows - what was asked, who is in front of the session, and
//! what the file says - into a [`Placement`], with the one sentence a fallback owes the reader.
//!
//! A firecode VM is the default because it is the only seat where `allow-all` refuses nothing: the
//! VM is the boundary, so a command inside it cannot reach this box. That buys nothing if it needs
//! a step from a person, so a machine that cannot start one is a HOST seat and a warning, never an
//! error and never a card: a package that refuses to start is a package somebody switches off.
//!
//! An interactive top-level session - a person at this box's head - stays on the host. It is
//! already under their own eyes, and its card is answered by the person it would be asking.

use std::path::PathBuf;

use letibot_tools::builtins::task::Placement;

pub use leticode_config::SeatDefault;

/// The warning code a fallback is filed under: the placement the file asked for could not be
/// given, and the session runs on the host instead. One code, so a head styles one kind of line.
pub const FELL_BACK: &str = "seat_host";

/// Who is in front of the session. The one distinction a placement turns on: a seat nobody watches
/// gets the VM, and a person at a head gets the host they are sitting at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    /// A person at this box's head, or the session their daemon opened for them.
    Interactive,
    /// A package, a subagent, a queue gate: nobody is attached to answer a card.
    Headless,
}

/// Whether this machine can start a firecode VM. A trait so a test can answer it without a
/// firecode: the decision is the thing under test, and a boot is minutes.
pub trait Ready: Send + Sync {
    fn ready(&self) -> Result<PathBuf, String>;
}

/// The real answer, and the same criterion `[queue] gate_in = "firecode"` uses
/// ([`crate::mergequeue::gate_firecode::Doctor`]): the CLI is installed, `/dev/kvm` is there on
/// Linux, and `firecode doctor` passes. Asked at most once per process, because a session start
/// must not pay a doctor per boot and the answer does not change under it.
pub struct Doctor;

impl Ready for Doctor {
    fn ready(&self) -> Result<PathBuf, String> {
        use crate::mergequeue::gate_firecode::Ready as _;
        static ASKED: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
        ASKED
            .get_or_init(|| crate::mergequeue::gate_firecode::Doctor.ready())
            .clone()
    }
}

/// What a start was told to do, and what a fallback owes the reader when it could not be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub placement: Placement,
    /// The one warning a fallback is worth: a code ([`FELL_BACK`]) and the sentence naming why.
    /// `None` when the ask was met, and when nobody was asked for anything.
    pub warning: Option<(String, String)>,
}

/// The placement for one seat.
///
/// `asked` is what somebody SAID - a `vm: true`/`vm: false`, a `--vm`, a `--host`, a `where`.
/// A placement somebody asked for is returned as it was asked and never second-guessed: the
/// refusal for a VM this box cannot start is the open's, by name, and swallowing it here would
/// report a placement nobody chose.
pub fn decide(
    asked: Option<Placement>,
    seat: Seat,
    default: SeatDefault,
    ready: &dyn Ready,
) -> Decided {
    let met = |p: Placement| Decided {
        placement: p,
        warning: None,
    };
    if let Some(asked) = asked {
        return met(asked);
    }
    if seat == Seat::Interactive || default == SeatDefault::Host {
        return met(Placement::Host);
    }
    match ready.ready() {
        Ok(_) => met(Placement::Firecode),
        // Not a refusal: the session runs here, and the one line says which fact of this machine
        // made it so. `firecode doctor`'s own words, because they are what a person would act on.
        Err(why) => Decided {
            placement: Placement::Host,
            warning: Some((
                FELL_BACK.to_string(),
                format!(
                    "[seat] default = \"vm\" asks for a firecode VM, and this machine cannot \
                     start one: {why}. The session's tools run on this box instead"
                ),
            )),
        },
    }
}

/// `[seat] default` for a workspace: the project file's word, else the user file's, else
/// [`SeatDefault::Vm`]. The second half is a sentence for a `leticode.toml` that would not parse,
/// which is not a refusal - the placement is a preference with a fallback, and a session that
/// would not start over it is worse than one running in the wrong place.
pub fn default_for(
    workspace: &std::path::Path,
    read_project: bool,
) -> (SeatDefault, Option<String>) {
    match leticode_config::load_user() {
        Ok(user) => leticode_config::seat_default(&user, workspace, read_project),
        Err(why) => (
            SeatDefault::default(),
            Some(format!(
                "the user's leticode.toml could not be read, so [seat] default is the built-in \
                 \"vm\": {why}"
            )),
        ),
    }
}

#[cfg(test)]
mod tests;
