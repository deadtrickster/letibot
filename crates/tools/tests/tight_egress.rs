//! Decision C10: a tight seat reaches no network at all until the operator grants it, so the
//! boundary's free admit is a rule production reaches rather than a fixture's. A wide seat is the
//! operator's own session and keeps the host's network by ruling.

use std::path::Path;

use letibot_tools::exec::boundary::{self, BoundaryConfig, Entry, Origin};
use letibot_tools::exec::confine::{ConfinePlan, Confinement, Egress, ViewSpec};
use letibot_tools::exec::seat::{Level, seat_for_level, seat_view};
use letibot_tools::exec::{Bwrap, ExecError};
use letibot_tools::mode::Mode;

/// What a skip here names, for [`letibot_tokencore::apparatus::absent`]: no namespace means no
/// network namespace either, so the command below cannot be run anywhere on this box.
const BOUNDARY: &str = "a usable confinement boundary (bwrap, an unprivileged user namespace)";

/// A skip the require-switch can refuse, announced before the absence so a run that demands the
/// apparatus fails on the refusal rather than reading a skip as a pass.
fn boundary_absent(e: &ExecError) {
    let m = format!("{e}");
    assert!(
        m.contains("was NOT run") || m.contains("nothing would reap it"),
        "a refusal must say that nothing ran: {m}"
    );
    letibot_tokencore::apparatus::absent(BOUNDARY);
}

/// The pairs a tight seat hands a command, over a home with nothing in it: what the view's egress
/// is has to be read off the view, because that is what the probe is handed.
fn tight_seat() -> letibot_tools::exec::seat::SeatView {
    seat_view(
        &Mode::WRITES_ALLOWED,
        "this machine",
        &[] as &[&str],
        Path::new("/nonexistent-operator-home"),
    )
}

/// The whole finding: `writes-allowed` seats network calls for the wide seat's sake, and the tight
/// view is the one that must not inherit it. Both built-in modes that execute are tried, because a
/// rule that held for one and not the other would be a rule about the mode.
#[test]
fn a_tight_seat_denies_the_network_whatever_the_mode_seats() {
    for mode in [Mode::READ_ONLY, Mode::WRITES_ALLOWED] {
        let seat = seat_view(&mode, "this machine", &[] as &[&str], Path::new("/h"));
        assert_eq!(
            seat.egress,
            Egress::Denied,
            "a tight seat at `{}` was handed the host's network, so every child shares loopback \
             with the daemon and the boundary can never enclose",
            mode.name
        );
    }
}

/// The half the decision keeps: the operator's own session is not confined from the machine.
#[test]
fn the_wide_seat_keeps_the_hosts_network() {
    let wide = seat_for_level(
        Level::Wide,
        &Mode::WRITES_ALLOWED,
        "this machine",
        &[] as &[&str],
        Path::new("/h"),
        None,
    );
    assert!(
        wide.egress.allowed(),
        "wide is the operator's own seat: {:?}",
        wide.egress
    );
}

/// The way out of the default, and the only one: an entry the operator wrote. It is a config key
/// rather than a mode, because the mode answers for the operator's own seat.
#[test]
fn a_granted_seat_may_reach_the_network() {
    let home = Path::new("/h");
    let cfg = BoundaryConfig {
        network: vec![Entry {
            value: "the crates.io fetch this session was seated to do".to_string(),
            from: Origin {
                file: "leticode.toml".into(),
                line: 7,
                user: true,
            },
        }],
        ..BoundaryConfig::default()
    };
    let r = boundary::resolve(&cfg, home).expect("a grant of egress resolves");
    match &boundary::apply(tight_seat(), &r).egress {
        Egress::Host { why } => assert!(
            why.contains("leticode.toml") && why.contains("network"),
            "a grant's sentence has to name the key a reader can edit: {why}"
        ),
        Egress::Denied => panic!("an operator's grant of egress was not applied to the seat"),
    }
}

/// And a project file may not write it: a file a session can edit must not hand itself the
/// machine's network, which is the widening the whole default exists to refuse.
#[test]
fn a_project_file_may_not_grant_the_network() {
    let cfg = BoundaryConfig {
        network: vec![Entry {
            value: "cargo fetch".to_string(),
            from: Origin {
                file: "leticode.toml".into(),
                line: 3,
                user: false,
            },
        }],
        ..BoundaryConfig::default()
    };
    let err = boundary::resolve(&cfg, Path::new("/h")).expect_err("a project file may not grant");
    assert!(
        err.contains("network") && err.contains("operator"),
        "the refusal has to name the key and whose call it is: {err}"
    );
}

/// The point of the change: `Boundary::encloses` needs `Egress::Denied`, so while the tight view
/// carried the host's network the free admit could only ever fire on a hand-built fixture. This
/// takes the egress from the production builder, so a regression turns it red.
#[test]
fn a_production_tight_seat_can_enclose() {
    let seat = tight_seat();
    assert_eq!(seat.egress, Egress::Denied);
    // Everything bar the egress is the probe's own measurement, which no box here can make: what
    // a seat CONTRIBUTES to `encloses` is its egress, and that is the half under test.
    assert!(
        letibot_tools::exec::Boundary {
            egress: seat.egress,
            ..measured_boundary()
        }
        .encloses(),
        "a tight seat's egress has to be one a measured boundary can enclose"
    );
}

/// A boundary with every namespace entered and both seals held, as the probe answers on a box
/// that can confine. Only the egress varies in the assertion above, and it is the seat's.
fn measured_boundary() -> letibot_tools::exec::Boundary {
    use letibot_tools::exec::{Namespace, NsState, Seal, SealKind};
    let mut ns = std::collections::BTreeMap::new();
    for n in Namespace::ALL {
        ns.insert(
            n,
            NsState::Entered {
                inode: format!("{}:[1]", n.proc_name()),
            },
        );
    }
    let mut seals = std::collections::BTreeMap::new();
    for k in SealKind::ALL {
        seals.insert(
            k,
            Seal::Held {
                how: "fixture".into(),
            },
        );
    }
    letibot_tools::exec::Boundary {
        mechanism: "bubblewrap 0.11.1 (fixture)".into(),
        platform: letibot_tools::exec::confine::Mechanism::Namespaces,
        view: ViewSpec::project_only("/w"),
        egress: Egress::Denied,
        ns,
        seals,
        props: std::collections::BTreeMap::new(),
        measured_at: std::time::SystemTime::now(),
    }
}

/// The e2e the finding names. It cannot run on a box with no user namespace, and it says so
/// rather than passing quietly.
#[test]
fn a_tight_seat_command_cannot_reach_loopback() {
    let seat = tight_seat();
    // The listener is the daemon's own shape: something on this machine's loopback that a child
    // has no business with.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("an address").port();
    let dir = std::env::temp_dir().join(format!("letibot-tightegress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a fixture project");
    let b = match Bwrap::probe(ViewSpec::project_only(&dir), seat.egress.clone()) {
        Ok(b) => b,
        Err(e) => {
            boundary_absent(&e);
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    };
    let argv = b
        .wrap(&ConfinePlan {
            cwd: &dir,
            env: &[],
        })
        .expect("a measured boundary wraps");
    let connect = format!(
        "python3 -c 'import socket,sys; s=socket.socket(); s.settimeout(2); \
         s.connect((\"127.0.0.1\", {port})); print(\"REACH\" + \"ED\")' 2>&1"
    );
    // Split in the source: a traceback quotes the line, and the word must come only from a connect.
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg("/bin/sh")
        .arg("-c")
        .arg(&connect)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the wrapped command runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !said.contains("REACHED"),
        "a tight seat's command reached the daemon's own loopback: {said}"
    );
}
