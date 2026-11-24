//! A session whose tools run in a firecode VM: the filesystem is the guest's.
//!
//! `allow-all` inside a VM means nothing is refused and nothing is asked, because there is nothing
//! of this box's to protect there - the operator's stores were never copied in, and a path is a
//! file in the guest. What is NOT exempt is a call whose own files are here: a copy-out target on
//! the host is a host path, and the host's rules still hold.

use super::*;

/// The surroundings of a session whose tools run in a guest, with a pinned shell (a VM's commands
/// run through `firecode in`, so a bare name resolves).
fn in_a_guest() -> crate::intent::Surroundings {
    let mut env = crate::intent::Surroundings::in_a_guest("/w");
    env.shell = crate::intent::ShellTrust::Pinned {
        how: "test fixture".into(),
    };
    env
}

fn gate_in(env: crate::intent::Surroundings, mode: crate::mode::Mode) -> AdjudicatedGate {
    AdjudicatedGate::closed()
        .with_surroundings(env)
        .with_mode(mode)
}

/// The calls the operator ran, unrefused and unasked in a VM, against the same three on this box.
#[test]
fn allow_all_in_a_vm_refuses_nothing_that_this_box_would_refuse() {
    for cmd in [
        "cat ~/.ssh/id_ed25519",
        "grep .netrc -r .",
        "grep -rn netrc .",
        "cat /home/dead/.ssh/id_rsa",
    ] {
        let args = json!({ "command": cmd });
        assert_eq!(
            gate_in(in_a_guest(), crate::mode::Mode::ALLOW_ALL).admit(&bash(&args)),
            GateDecision::Admit,
            "`{cmd}` in a VM"
        );
        // The same call on this box is refused where it names a store, which is what makes the
        // VM's answer about the seat and not about the text.
        if cmd.contains(".ssh") {
            assert!(
                matches!(
                    gate_in(pinned(), crate::mode::Mode::ALLOW_ALL).admit(&bash(&args)),
                    GateDecision::Refuse { .. }
                ),
                "`{cmd}` on this box was not refused"
            );
        }
    }
}

/// The never-write list is a list of paths on THIS box.
#[test]
fn the_never_write_list_does_not_reach_a_guest() {
    let args = json!({ "path": "~/.ssh/id_ed25519", "content": "x" });
    let w = call("write", &args);
    match gate_in(pinned(), crate::mode::Mode::WRITES_ALLOWED).admit(&w) {
        GateDecision::Refuse { tell, .. } => assert!(tell.contains("never-write"), "{tell}"),
        other => panic!("a write into the operator's key store on this box: {other:?}"),
    }
    assert_eq!(
        gate_in(in_a_guest(), crate::mode::Mode::WRITES_ALLOWED).admit(&w),
        GateDecision::Admit,
        "a guest's own ~/.ssh is the guest's"
    );
}

/// `task`'s "writing ~/.config/x": a path outside every store, admitted in the guest like any
/// other, and admitted here too - so the test says which half of the answer is the seat's.
#[test]
fn a_plain_write_outside_the_project_asks_nothing_in_a_vm() {
    let args = json!({ "path": "~/.config/x", "content": "x" });
    assert_eq!(
        gate_in(in_a_guest(), crate::mode::Mode::ALLOW_ALL).admit(&call("write", &args)),
        GateDecision::Admit
    );
}

/// The escape surface: a copy-out whose destination is a HOST path is a call this box answers, and
/// it is still refused. The exemption above is the guest's filesystem, not "the session is a VM".
#[test]
fn the_guest_exemption_is_the_filesystem_and_not_the_placement() {
    let args = json!({
        "command": "firecode cp vm:/work/tree ~/.ssh/id_ed25519"
    });
    match gate_in(pinned(), crate::mode::Mode::WRITES_ALLOWED).admit(&bash(&args)) {
        GateDecision::Refuse { tell, .. } => {
            assert!(
                tell.contains(".ssh") || tell.contains("secret"),
                "the refusal does not name what it protects: {tell}"
            );
        }
        other => panic!("a copy-out onto the operator's key was not refused: {other:?}"),
    }
}

/// The one door out of a VM is judged HERE: `host` runs a command on this box, so the guest's
/// `allow-all` - right about a command inside the guest - must not admit it in silence. The
/// command is one whose answer DEPENDS on the point: a network call, admitted at `allow-all` and
/// a card at `automode`.
#[test]
fn the_host_tool_is_judged_as_a_host_seat_and_not_as_the_guest() {
    let args = json!({ "command": "curl -sS https://example.com" });
    let guest = || {
        gate_in(in_a_guest(), crate::mode::Mode::ALLOW_ALL)
            .with_host_seat_tool(Some(crate::builtins::host::HOST_TOOL.to_string()))
    };
    assert_eq!(
        guest().admit(&bash(&args)),
        GateDecision::Admit,
        "a command inside the guest is the guest's"
    );
    let leaving = GateCall {
        name: crate::builtins::host::HOST_TOOL,
        access: Access::Exec,
        ..bash(&args)
    };
    // `automode` is the point read for it, and this gate has nobody to ask, so the answer is not
    // `Admit`. What the test is about is WHICH point was read.
    assert_ne!(
        guest().admit(&leaving),
        GateDecision::Admit,
        "a command on THIS box is not covered by the guest's allow-all"
    );
    // A host seat carries no such door, and its own point is unchanged.
    assert_eq!(
        gate_in(pinned(), crate::mode::Mode::ALLOW_ALL).admit(&leaving),
        GateDecision::Admit,
        "nothing about a host seat changed"
    );
}
