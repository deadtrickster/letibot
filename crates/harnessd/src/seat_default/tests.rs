//! The placement decision, as four answers: a headless seat gets the VM, an interactive one the
//! host, an ask beats the file, and a machine that cannot start a VM says why and runs here.

use super::*;

/// A machine that can, or one that cannot, without a firecode anywhere.
struct Machine(Result<PathBuf, String>);

impl Ready for Machine {
    fn ready(&self) -> Result<PathBuf, String> {
        self.0.clone()
    }
}

fn can() -> Machine {
    Machine(Ok(PathBuf::from("/tmp/firecode")))
}

fn cannot(why: &str) -> Machine {
    Machine(Err(why.to_string()))
}

#[test]
fn a_headless_seat_is_a_vm_and_an_interactive_one_is_the_host() {
    let vm = decide(None, Seat::Headless, SeatDefault::Vm, &can());
    assert_eq!(vm.placement, Placement::Firecode, "{vm:?}");
    assert_eq!(vm.warning, None, "the ask was met, so nothing is owed");

    // A person at the head is already there: their own box, with their own card in front of them.
    let host = decide(None, Seat::Interactive, SeatDefault::Vm, &can());
    assert_eq!(host.placement, Placement::Host, "{host:?}");
    assert_eq!(host.warning, None);

    // And a file that names the host places a headless seat there, with nothing said: the answer
    // is what was asked for.
    let asked_host = decide(None, Seat::Headless, SeatDefault::Host, &can());
    assert_eq!(asked_host.placement, Placement::Host);
    assert_eq!(asked_host.warning, None);
}

#[test]
fn an_ask_beats_the_file_and_is_never_second_guessed() {
    use letibot_tools::builtins::task::Placement;
    // `vm: false` on a box whose file says `vm`: the ask wins over the default.
    let off = decide(
        Some(Placement::Host),
        Seat::Headless,
        SeatDefault::Vm,
        &can(),
    );
    assert_eq!(off.placement, Placement::Host);
    assert_eq!(off.warning, None);

    // `vm: true` on a box with no firecode is still a VM: the refusal for one this machine cannot
    // start belongs to the open, by name (and to `placed_as_asked`, which names it to the model).
    // Answering `host` here would report a placement nobody chose.
    let on = decide(
        Some(Placement::Firecode),
        Seat::Headless,
        SeatDefault::Host,
        &cannot("there is no `firecode` on this host's PATH"),
    );
    assert_eq!(on.placement, Placement::Firecode);
    assert_eq!(on.warning, None);

    // A placement on another machine rides through as it was asked, VM or not.
    let remote = decide(
        Some(Placement::RemoteVm("lab2x1".into())),
        Seat::Headless,
        SeatDefault::Vm,
        &can(),
    );
    assert_eq!(remote.placement, Placement::RemoteVm("lab2x1".into()));
}

#[test]
fn a_machine_that_cannot_start_a_vm_runs_the_seat_here_and_says_why() {
    let why = "`firecode doctor` failed: no /dev/kvm on this machine";
    let d = decide(None, Seat::Headless, SeatDefault::Vm, &cannot(why));
    assert_eq!(d.placement, Placement::Host, "a package must still start");
    let (code, said) = d.warning.expect("a fallback is owed a sentence");
    assert_eq!(code, FELL_BACK);
    assert!(said.contains("[seat] default = \"vm\""), "{said}");
    // The doctor's own words, because they are the half a person acts on.
    assert!(said.contains(why), "{said}");
    assert!(
        !said.contains("refus") && !said.contains("card"),
        "a fallback is a warning, not an error and not a card: {said}"
    );
}
