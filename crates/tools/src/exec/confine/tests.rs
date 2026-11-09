use super::*;

fn boundary_with(states: Vec<(Namespace, NsState)>) -> Boundary {
    let mut ns = BTreeMap::new();
    for n in Namespace::ALL {
        ns.insert(
            n,
            NsState::Entered {
                inode: format!("{n}:[1000{}]", n as u8),
            },
        );
    }
    for (n, s) in states {
        ns.insert(n, s);
    }
    let mut seals = BTreeMap::new();
    for k in SealKind::ALL {
        seals.insert(
            k,
            Seal::Held {
                how: format!("{k} was tested from inside the fixture"),
            },
        );
    }
    Boundary {
        mechanism: "bubblewrap 0.0.0 (fixture)".into(),
        platform: Mechanism::Namespaces,
        view: ViewSpec::project_only("/proj"),
        egress: Egress::Denied,
        ns,
        seals,
        props: BTreeMap::new(),
        measured_at: SystemTime::now(),
    }
}

#[test]
fn a_partial_boundary_describes_itself_as_partial() {
    // The sentence is computed from the state, so changing one entry in the map must change
    // the disclosure, and a hard-coded `confined` cannot pass.
    let full = boundary_with(vec![]);
    assert!(full.complete());
    assert!(!full.partial());
    assert!(
        full.describe().starts_with("confined;"),
        "{}",
        full.describe()
    );

    let partial = boundary_with(vec![(
        Namespace::Ipc,
        NsState::NotEntered {
            why: "the inode inside is the harness's own".into(),
        },
    )]);
    // Required namespaces are all there, so it still confines, and it must not describe
    // itself as if nothing were missing.
    assert!(partial.complete());
    assert!(partial.partial());
    let d = partial.describe();
    assert!(d.contains("PARTIALLY"), "{d}");
    assert!(d.contains("NOT ENTERED"), "{d}");
    assert!(d.contains("ipc"), "{d}");
    assert_eq!(partial.missing().len(), 1);
    assert_eq!(partial.missing()[0].0, Namespace::Ipc);
}

#[test]
fn a_boundary_missing_a_required_namespace_says_a_command_is_refused() {
    let broken = boundary_with(vec![(
        Namespace::Pid,
        NsState::NotEntered {
            why: "`--unshare-pid` did not take effect".into(),
        },
    )]);
    assert!(!broken.complete());
    let d = broken.describe();
    assert!(d.contains("NOT CONFINED"), "{d}");
    assert!(d.contains("refused"), "{d}");
    // The missing namespace is named with what it would have bought, so the reader knows
    // what they do not have.
    assert!(d.contains("cannot see or signal the daemon"), "{d}");
}

#[test]
fn a_shared_namespace_is_a_decision_and_not_a_failure() {
    // The two look identical from inside (the same inode as the harness) and are
    // different facts: collapsing them would report a declared egress as a broken
    // boundary, and the reverse.
    let b = Boundary {
        egress: Egress::Host {
            why: "the operator authorised `git push` to origin".into(),
        },
        ..boundary_with(vec![(
            Namespace::Net,
            NsState::SharedByDecision {
                why: "the operator authorised `git push` to origin".into(),
            },
        )])
    };
    assert!(b.complete(), "net is not required");
    assert!(!b.partial(), "a decision is not a missing namespace");
    let d = b.describe();
    assert!(d.contains("SHARED WITH THE HOST by decision"), "{d}");
    assert!(d.contains("git push"), "{d}");
    assert!(!d.contains("NOT ENTERED"), "{d}");
}

#[test]
fn the_default_disclosure_states_the_tmpfs_home_cost() {
    // A surprise slowdown nobody can explain is a worse disclosure than a sentence here.
    let d = boundary_with(vec![]).describe();
    assert!(d.contains("FRESH TMPFS"), "{d}");
    assert!(d.contains("build cache"), "{d}");
    assert!(d.contains("DENIED"), "{d}");
}

#[test]
fn no_confinement_refuses_and_names_what_is_missing() {
    let c = NoConfinement::new("`bwrap` is not on `PATH`");
    let e = c
        .wrap(&ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        })
        .unwrap_err();
    let m = format!("{e}");
    assert!(m.contains("bwrap"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");
    assert!(c.boundary().is_none());
    assert!(c.describe().contains("refuses to exec"));
}

/// A protected path that is not there is a name a command can claim, and bwrap can only mount
/// over what exists: each kind gets an empty read-only placeholder of its own kind, and a path
/// whose parent is absent gets none, because bwrap would make the parents in the real tree.
#[test]
fn a_protected_path_that_is_not_there_is_still_bound_read_only() {
    let dir = std::env::temp_dir().join(format!("letibot-protect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("common/present")).expect("a fixture tree");
    let view = ViewSpec::project_only(&dir)
        .protecting(Protection::dir(dir.join("common/present")))
        .protecting(Protection::dir(dir.join("common/hooks")))
        .protecting(Protection::file(dir.join("common/packed-refs")))
        .protecting(Protection::dir(dir.join("common/gone/child")));
    let argv = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    );
    let joined = argv.join(" ");
    let present = dir.join("common/present").display().to_string();
    assert!(
        joined.contains(&format!("--ro-bind {present} {present}")),
        "a path that is there is bound over itself: {joined}"
    );
    let hooks = dir.join("common/hooks").display().to_string();
    assert!(
        joined.contains(&format!("--tmpfs {hooks} --remount-ro {hooks}")),
        "an absent directory gets an empty one it cannot write: {joined}"
    );
    let packed = dir.join("common/packed-refs").display().to_string();
    assert!(
        joined.contains(&format!("--ro-bind /dev/null {packed}")),
        "an absent file is the empty read-only one every box has: {joined}"
    );
    assert!(
        !joined.contains(&dir.join("common/gone").display().to_string()),
        "a parent that is absent is not made in the operator's tree: {joined}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The wide seat's argv: the machine at its own paths and writable, with the stores masked out
/// of it. Nothing is bound twice, and the mask is the whole of what the view hides.
#[test]
fn a_wide_view_binds_the_machine_and_masks_the_stores_it_is_given() {
    let dir = std::env::temp_dir().join(format!("letibot-wide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".ssh")).expect("a fake store");
    std::fs::write(home.join(".npmrc"), "//registry/:_authToken=fake\n").expect("a fake file");
    std::fs::create_dir_all(dir.join("project")).expect("a project");
    let tmp = dir.join("tmp");
    let view = ViewSpec::project_only(dir.join("project"))
        .with_home(HomeView::Dir(home.clone()))
        .with_tmp(TmpView::Session(tmp.clone()))
        .wide()
        .masking([home.join(".ssh"), home.join(".npmrc"), home.join(".aws")]);
    let argv = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    );
    let joined = argv.join(" ");
    assert!(joined.contains("--dev-bind / /"), "{joined}");
    // Still this command's own `/proc`, because the PID namespace above it is.
    assert!(joined.contains("--proc /proc"), "{joined}");
    let ssh = home.join(".ssh").display().to_string();
    assert!(
        joined.contains(&format!("--tmpfs {ssh} --remount-ro {ssh}")),
        "a store that is a directory is an empty one: {joined}"
    );
    let npmrc = home.join(".npmrc").display().to_string();
    assert!(
        joined.contains(&format!("--dev-bind /dev/null {npmrc}")),
        "a store that is a file is the unreadable empty one: {joined}"
    );
    assert!(
        !joined.contains(&home.join(".aws").display().to_string()),
        "a store that is not there has no mount point, and making one writes the operator's \
             home: {joined}"
    );
    // The machine IS the view, so nothing is bound a second time: no system roots, no project,
    // and the machine keeps its own `/tmp`.
    assert!(!joined.contains("--ro-bind-try"), "{joined}");
    assert!(!joined.contains("--tmpfs /tmp "), "{joined}");
    let project = dir.join("project").display().to_string();
    assert!(
        !joined.contains(&format!("--bind {project} {project}")),
        "{joined}"
    );
    assert!(
        joined.contains(&format!("--setenv HOME {}", home.display())),
        "{joined}"
    );
    assert!(
        joined.contains(&format!("--setenv TMPDIR {}", tmp.display())),
        "the session's scratch reaches a wide command as $TMPDIR: {joined}"
    );
    assert!(
        joined.contains("--unshare-net"),
        "a read-only seat has no network: {joined}"
    );
    // And the same view with the network seated does not take it away.
    let open = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Host {
            why: "a test".into(),
        },
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    )
    .join(" ");
    assert!(!open.contains("--unshare-net"), "{open}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A file inside a directory that is masked as an empty tmpfs is already hidden, and bwrap
/// cannot make a mount point in that read-only tmpfs: no mount is emitted for it.
#[test]
fn nothing_is_mounted_inside_a_directory_masked_as_empty() {
    let dir = std::env::temp_dir().join(format!("letibot-wide-in-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    let store = home.join(".local/share/letibot");
    std::fs::create_dir_all(&store).expect("a fake store");
    let lease = store.join("room-x.lease");
    std::fs::write(&lease, "").expect("a fake lease");
    std::fs::create_dir_all(dir.join("project")).expect("a project");
    let view = ViewSpec::project_only(dir.join("project"))
        .with_home(HomeView::Dir(home.clone()))
        .wide()
        .masking([store.clone()])
        .hiding(lease.clone());
    let joined = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    )
    .join(" ");
    let s = store.display().to_string();
    assert!(joined.contains(&format!("--tmpfs {s} ")), "{joined}");
    assert!(
        !joined.contains(&format!("/dev/null {}", lease.display())),
        "a mount under the emptied store fails every spawn: {joined}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The masked store keeps its data: bound read-only into the empty tmpfs before the remount,
/// with a token inside a data directory covered and a top-level secret not mounted at all.
#[test]
fn the_masked_store_mounts_its_data_read_only_and_covers_its_secrets() {
    let dir = std::env::temp_dir().join(format!("letibot-store-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    let store = home.join(".local/share/letibot");
    std::fs::create_dir_all(store.join("logs")).expect("a fake store");
    std::fs::create_dir_all(dir.join("project")).expect("a project");
    for f in [
        "sessions.db",
        "room-x.token",
        "logs/a.jsonl",
        "logs/y.token",
    ] {
        std::fs::write(store.join(f), "").expect("a store file");
    }
    let view = ViewSpec::project_only(dir.join("project"))
        .with_home(HomeView::Dir(home.clone()))
        .wide()
        .masking([store.clone()]);
    let joined = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    )
    .join(" ");
    let s = store.display().to_string();
    let db = format!("--ro-bind {s}/sessions.db {s}/sessions.db");
    let logs = format!("--ro-bind {s}/logs {s}/logs");
    let cover = format!("--ro-bind /dev/null {s}/logs/y.token");
    let remount = format!("--remount-ro {s}");
    let at = |needle: &str| {
        joined
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}: {joined}"))
    };
    assert!(at(&format!("--tmpfs {s} ")) < at(&db), "{joined}");
    assert!(
        at(&db) < at(&remount) && at(&logs) < at(&remount),
        "{joined}"
    );
    assert!(
        at(&logs) < at(&cover) && at(&cover) < at(&remount),
        "{joined}"
    );
    assert!(!joined.contains("room-x.token"), "{joined}");
    std::fs::remove_dir_all(&dir).ok();
}

/// One list, two renderings: the Linux seat's masks are the paths the macOS profile denies,
/// over the same home, so a store added to one cannot be missing from the other.
#[test]
fn the_wide_seats_masks_are_the_list_the_macos_seat_denies() {
    use crate::exec::seat;
    let dir = std::env::temp_dir().join(format!("letibot-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("operator");
    let mut want: Vec<PathBuf> = crate::exec::seatbelt::deny_under_home(true)
        .into_iter()
        .map(|rel| home.join(rel))
        .collect();
    want.sort();
    want.dedup();
    // Every store made, so the argv carries one mount per entry and the comparison is about
    // the list rather than about what this box happens to have.
    for p in &want {
        std::fs::create_dir_all(p).expect("a fake store");
    }
    std::fs::create_dir_all(dir.join("project")).expect("a project");
    let wide = seat::seat_for_level(
        seat::Level::Wide,
        &crate::mode::Mode::WRITES_ALLOWED,
        "this machine",
        &[] as &[&str],
        &home,
        None,
    );
    assert_eq!(
        wide.masked, want,
        "the seat holds the list both renderings read"
    );
    assert!(
        !want.iter().any(|p| p.ends_with(".config/gh")),
        "a top-level seat keeps gh's config, which is what the token it injects is for: {want:?}"
    );
    let view = ViewSpec::project_only(dir.join("project"))
        .with_home(HomeView::Dir(home.clone()))
        .wide()
        .masking(wide.masked.iter().cloned());
    let argv = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    );
    let joined = argv.join(" ");
    for p in &want {
        let p = p.display().to_string();
        assert!(
            joined.contains(&format!("--tmpfs {p} --remount-ro {p}")),
            "{p} is on the macOS deny list and not masked here: {joined}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// The daemon's own socket and the ssh masters are found by scanning; a plain file and a
/// socket outside the letibot directories are not.
#[test]
fn the_daemons_sockets_are_found_in_the_runtime_dirs() {
    let dir = std::env::temp_dir().join(format!("letibot-dsock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let run = dir.join("run/letibot");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("abc.sock"), "").unwrap();
    std::fs::write(run.join("abc.json"), "").unwrap();
    std::fs::write(dir.join("letibot-ssh-1-x-call.sock"), "").unwrap();
    std::fs::write(dir.join("other.sock"), "").unwrap();
    let got = daemon_sockets_in(
        std::slice::from_ref(&run),
        &dir,
        Some(dir.join("named.sock")),
    );
    let has = |p: PathBuf| got.contains(&p) || got.contains(&p.canonicalize().unwrap_or(p));
    assert!(has(run.join("abc.sock")), "{got:?}");
    assert!(has(dir.join("letibot-ssh-1-x-call.sock")), "{got:?}");
    assert!(has(dir.join("named.sock")), "{got:?}");
    assert!(!got.iter().any(|p| p.ends_with("abc.json")), "{got:?}");
    assert!(!got.iter().any(|p| p.ends_with("other.sock")), "{got:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// The wide seat's persistence, history and git-config rules, read from the same lists by both
/// renderings: denied outright on the macOS profile and masked here, and the read-only files
/// written nowhere in either.
#[test]
fn the_wide_seats_persistence_and_history_rules_hold_in_both_renderings() {
    use crate::exec::{seat, seatbelt};
    let dir = std::env::temp_dir().join(format!("letibot-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("operator");
    std::fs::create_dir_all(home.join(".config/git")).expect("a fake home");
    std::fs::create_dir_all(dir.join("project")).expect("a project");
    std::fs::write(home.join(".gitconfig"), "[user]\n").unwrap();
    std::fs::write(home.join(".config/git/config"), "").unwrap();
    let denied = [
        "Library/LaunchAgents",
        ".zlogin",
        ".zprofile",
        ".bash_profile",
        ".profile",
        ".oh-my-zsh/custom",
        ".zsh_history",
        ".bash_history",
    ];
    let deny = seatbelt::deny_under_home(true);
    for d in denied {
        assert!(deny.contains(&d), "{d} is not on the seat's deny list");
    }
    let mut view = ViewSpec::project_only(dir.join("project"))
        .with_home(HomeView::Dir(home.clone()))
        .wide();
    let wide = seat::seat_for_level(
        seat::Level::Wide,
        &crate::mode::Mode::WRITES_ALLOWED,
        "this machine",
        &[] as &[&str],
        &home,
        None,
    );
    assert!(
        !deny.contains(&".config/gh") && !wide.masked.iter().any(|p| p.ends_with(".config/gh")),
        "gh stays readable in the top-level seat"
    );
    view = view.masking(wide.masked.iter().cloned());
    for p in seat::wide_protected(&home) {
        view = view.protecting(p);
    }
    let argv = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    )
    .join(" ");
    for rel in crate::secrets::SEAT_READ_ONLY {
        let p = home.join(rel);
        if p.exists() {
            let p = p.display().to_string();
            assert!(argv.contains(&format!("--ro-bind {p} {p}")), "{p}: {argv}");
        }
    }
    let layout = seatbelt::Layout::of(&view, &home, &dir);
    let profile = seatbelt::render_profile(&view, &Egress::Denied, &layout);
    for rel in crate::secrets::SEAT_READ_ONLY {
        let p = home.join(rel);
        assert!(
            profile.contains(&format!("(subpath \"{}\")", p.display())),
            "{rel} is writable on macOS: {profile}"
        );
    }
    for d in denied {
        assert!(
            layout.deny.contains(&home.join(d)),
            "{d} is not denied on macOS"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A tight view reaches the operator's home only through a grant, and a grant of a toolchain
/// directory carries a credential store with it: the store is masked, the toolchain is not, and a
/// store the view does not reach gets no mount at all.
#[test]
fn a_grant_reaches_a_store_and_the_store_is_masked_and_the_toolchain_is_not() {
    use crate::exec::seat;
    let dir = std::env::temp_dir().join(format!("letibot-grantmask-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("operator");
    let cargo = home.join(".cargo");
    std::fs::create_dir_all(&cargo).expect("a fake toolchain");
    std::fs::create_dir_all(home.join(".ssh")).expect("a fake store");
    std::fs::write(cargo.join("credentials.toml"), "token\n").expect("a fake store");
    std::fs::create_dir_all(dir.join("project")).expect("a project");
    let seat = seat::seat_view(
        &crate::mode::Mode::WRITES_ALLOWED,
        "this machine",
        &[cargo.display().to_string()],
        &home,
    );
    let view = seat.grants.iter().fold(
        ViewSpec::project_only(dir.join("project")).masking(seat.masked.iter().cloned()),
        |v, g| v.granting(g.clone()),
    );
    let argv = build_argv_with(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
        Session::Detached,
    );
    let joined = argv.join(" ");
    let granted = cargo.display().to_string();
    assert!(
        joined.contains(&format!("--ro-bind {granted} {granted}")),
        "the toolchain is bound read-only: {joined}"
    );
    let store = cargo.join("credentials.toml").display().to_string();
    assert!(
        joined.contains(&format!("--dev-bind /dev/null {store}")),
        "the store inside the grant is masked: {joined}"
    );
    assert!(
        !joined.contains(&cargo.join("config.toml").display().to_string()),
        "the toolchain's own config is not the fence's business: {joined}"
    );
    assert!(
        !joined.contains(&home.join(".ssh").display().to_string()),
        "a store the view does not reach gets no mount point: {joined}"
    );
    // And the disclosure says so, in the view's own summary and in the model's brief.
    assert!(
        view.summary().contains("masked inside the grants"),
        "{}",
        view.summary()
    );
    assert!(
        view.summary().contains(&store),
        "the disclosure names the path: {}",
        view.summary()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unconfined_says_so_rather_than_saying_nothing() {
    let c = Unconfined::because("HostBackend::executable asks for no boundary");
    assert!(
        c.wrap(&ConfinePlan {
            cwd: Path::new("/"),
            env: &[]
        })
        .unwrap()
        .is_empty()
    );
    let d = c.describe();
    assert!(d.contains("NOT CONFINED"), "{d}");
    assert!(d.contains("whole filesystem"), "{d}");
    // It claims no view, so nothing produces an absence note blaming a boundary that is
    // not there.
    assert!(!c.outside_view(Path::new("/home/dead/.ssh/id_rsa")));
    assert!(
        c.absence_notes("cat: /etc/shadow: No such file or directory")
            .is_empty()
    );
}

#[test]
fn the_view_is_lexical_and_the_parent_chain_is_inside_it() {
    let v = ViewSpec::project_only("/home/dead/Projects/letibot");
    assert!(!v.outside(Path::new("/home/dead/Projects/letibot/crates/tools")));
    assert!(v.outside(Path::new("/home/dead/.ssh/id_rsa")));
    assert!(v.outside(Path::new("/home/dead/Projects/other")));
    assert!(v.outside(Path::new("/root/.ssh/id_rsa")));
    assert!(v.outside(Path::new("/etc/shadow")));
    // `/usr` is a system root, so it is in the view and must not produce a note.
    assert!(!v.outside(Path::new("/usr/bin/cc")));
    // The parent chain to the project exists inside as empty directories, so a listing of
    // it succeeds and calling it absent would be a note about something the command sees.
    assert!(!v.outside(Path::new("/home/dead")));
    assert!(!v.outside(Path::new("/home")));
    // A relative path is not this function's question.
    assert!(!v.outside(Path::new("crates/tools")));
}

#[test]
fn a_granted_path_is_in_the_view_and_its_consequence_is_printed() {
    let v = ViewSpec::project_only("/proj").granting(Grant::ReadOnly {
        path: PathBuf::from("/home/dead/.cargo"),
        why: "the crate registry cache, so a build does not re-download".into(),
    });
    assert!(!v.outside(Path::new("/home/dead/.cargo/registry")));
    assert!(v.outside(Path::new("/home/dead/.ssh")));
    let s = v.summary();
    assert!(s.contains("READABLE INTO CONTEXT"), "{s}");
    assert!(s.contains("re-download"), "{s}");
}

#[test]
fn an_absent_path_is_reported_as_absent_from_the_view_and_not_as_missing() {
    // A bare `ENOENT` is read as "the file does not exist", an empty-haystack answer.
    let b = boundary_with(vec![]);
    let notes = b.absence_notes("cat: /home/dead/.ssh/id_rsa: No such file or directory");
    assert_eq!(notes.len(), 1, "{notes:?}");
    let n = &notes[0];
    assert!(n.contains("not in this session's filesystem view"), "{n}");
    assert!(n.contains("/home/dead/.ssh/id_rsa"), "{n}");
    assert!(n.contains("no other spelling of the path"), "{n}");
    // The note must not claim the path exists on the host: nothing was stat'ed, and the
    // sentence does not assert it either way.
    assert!(!n.contains("exists on"), "{n}");
}

/// A refusal names the key that would grant the path, and says plainly when none can.
#[test]
fn an_absence_note_names_the_boundary_key_or_says_none_grants_it() {
    let home = Path::new("/home/dead");
    let ask = key_hint_under(Path::new("/home/dead/datasets"), Some(home));
    assert!(
        ask.contains("[boundary] grants_ro") && ask.contains("grants_rw"),
        "{ask}"
    );
    let never = key_hint_under(Path::new("/home/dead/.ssh/id_rsa"), Some(home));
    assert!(
        never.contains("never granted") && !never.contains("grants_ro"),
        "{never}"
    );
    let b = boundary_with(vec![]);
    let notes = b.absence_notes("cat: /nowhere/data: No such file or directory");
    assert!(notes[0].contains("[boundary] grants_ro"), "{}", notes[0]);
}

#[test]
fn a_hidden_file_inside_the_view_is_replaced_by_dev_null_and_one_outside_is_left_alone() {
    let dir = std::env::temp_dir().join(format!("letibot-hidden-argv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("room-x.sock");
    std::fs::write(&sock, "").unwrap();
    let outside = std::env::temp_dir().join("elsewhere-room.sock");
    let _ = std::fs::write(&outside, "");
    let view = ViewSpec::project_only(&dir)
        .hiding(&sock)
        .hiding("/nope/room.sock");
    let a = build_argv(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: &dir,
            env: &[],
        },
    )
    .join(" ");
    assert!(
        a.contains(&format!("--ro-bind /dev/null {}", sock.display())),
        "{a}"
    );
    assert!(
        !a.contains("/nope/room.sock"),
        "an absent path is not made to appear: {a}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&outside);
}

#[test]
fn the_failure_note_names_both_grants_that_would_let_the_path_through() {
    // The card offers a read-only and a read-write answer, so a note naming only the first
    // sends a model that needs to WRITE to ask for a grant that cannot let it through.
    let b = boundary_with(vec![]);
    let notes = b.absence_notes("cat: /home/dead/.cargo/bin/cargo: No such file or directory");
    assert_eq!(notes.len(), 1, "{notes:?}");
    let n = &notes[0];
    assert!(n.contains("`/home/dead/.cargo/bin/cargo`"), "{n}");
    assert!(
        n.contains("`ReadOnly`"),
        "the read-only grant is named: {n}"
    );
    assert!(
        n.contains("`ReadWrite`"),
        "the write spelling the card offers is named too: {n}"
    );
    assert!(n.contains("the transcript"), "{n}");
    assert!(n.contains("the operator's to decide"), "{n}");
}

#[test]
fn the_model_is_told_what_its_view_holds_before_anything_fails() {
    // The failure note alone would leave a hermetic view reading as a broken
    // toolchain. The brief is what the prompt carries at open: the paths, and what `~` is.
    let b = boundary_with(vec![]);
    let t = b.model_brief();
    assert!(t.contains("`/proj`"), "the project is named: {t}");
    assert!(t.contains("/usr"), "the system roots are named: {t}");
    assert!(
        t.contains(TMPFS_HOME),
        "the home inside the view is named: {t}"
    );
    assert!(
        t.contains("not this machine's"),
        "and it says the home is the view's own: {t}"
    );
    assert!(
        t.contains("absent"),
        "a path outside is absent, not forbidden: {t}"
    );
    assert!(
        t.contains("no network") && t.contains("no routes"),
        "egress is stated: {t}"
    );
    assert!(t.contains("`ReadOnly`") && t.contains("`ReadWrite`"), "{t}");
    assert!(t.contains("the operator's to decide"), "{t}");
    // Nothing is granted in a plain view, and a brief that listed one would be a claim.
    assert!(!t.contains("Bound in"), "{t}");

    // A path bound in on purpose is in the brief, with the consequence the grant carries.
    let granted = Boundary {
        view: ViewSpec::project_only("/proj").granting(Grant::ReadOnly {
            path: PathBuf::from("/home/dead/.cargo"),
            why: "the registry, so a build does not re-download".into(),
        }),
        ..boundary_with(vec![])
    };
    let t = granted.model_brief();
    assert!(t.contains("/home/dead/.cargo"), "{t}");
    assert!(t.contains("READABLE INTO CONTEXT"), "{t}");
}

/// The wide seat's `$HOME` is the operator's own, and the brief said letibot owned it: a model
/// reading that treats `~` as scratch and somebody else's files as its own to tidy.
#[test]
fn the_brief_says_when_the_home_is_the_operators_own() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let wide = Boundary {
        view: ViewSpec::project_only("/proj").with_home(HomeView::Dir(home.clone())),
        ..boundary_with(vec![])
    };
    let t = wide.model_brief();
    assert!(
        t.contains("THIS MACHINE'S OWN HOME"),
        "the wide home must not read as this view's own: {t}"
    );
    assert!(!t.contains("a directory letibot owns"), "{t}");
    assert!(t.contains("writable"), "{t}");

    // And a home the daemon made for the session still says so.
    let narrow = Boundary {
        view: ViewSpec::project_only("/proj").with_home(HomeView::Dir(home.join("letibot-home"))),
        ..boundary_with(vec![])
    };
    let t = narrow.model_brief();
    assert!(t.contains("a directory letibot owns"), "{t}");
    assert!(!t.contains("THIS MACHINE'S OWN HOME"), "{t}");
}

#[test]
fn a_session_tmp_is_bound_at_tmp_and_nothing_replaces_it() {
    // The two-call scratch idiom: a file one call wrote was gone in the next, and nothing
    // announces it. This is the argv half, `--tmpfs /tmp` gone and `--bind <dir> /tmp` there.
    let dir = PathBuf::from("/var/lib/letibot/scratch-1/tmp/s-a");
    let a = build_argv(
        Path::new("/usr/bin/bwrap"),
        &ViewSpec::project_only("/proj").with_tmp(TmpView::Session(dir.clone())),
        &Egress::Denied,
        &ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        },
    );
    let joined = a.join(" ");
    assert!(!joined.contains("--tmpfs /tmp"), "{joined}");
    assert!(
        joined.contains(&format!("--bind {} /tmp", dir.display())),
        "the session's own directory, bound at /tmp: {joined}"
    );
    // `/var/tmp` and `/run` are still fresh filesystems: the one thing `/var/tmp` means on
    // a host is that it survives a reboot, and `/run` holds the agent socket and the home.
    assert!(joined.contains("--tmpfs /var/tmp"), "{joined}");
    assert!(joined.contains("--tmpfs /run"), "{joined}");

    // And the default view is unchanged: one fresh tmpfs at `/tmp`, bound to nothing.
    let d = build_argv(
        Path::new("/usr/bin/bwrap"),
        &ViewSpec::project_only("/proj"),
        &Egress::Denied,
        &ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        },
    )
    .join(" ");
    assert!(d.contains("--tmpfs /tmp"), "{d}");
}

#[test]
fn a_replaced_tmp_says_the_session_owns_it_and_not_that_it_starts_empty() {
    // The disclosure and the absence note both read off the view, so a `/tmp` that outlives
    // a command must not describe itself as one that does not.
    let dir = PathBuf::from("/var/lib/letibot/scratch-1/tmp/s-a");
    let v = ViewSpec::project_only("/proj").with_tmp(TmpView::Session(dir.clone()));
    let Presence::Replaced { what } = v.classify(Path::new("/tmp/host-file")) else {
        panic!("a session /tmp is still not the host's /tmp");
    };
    assert!(what.contains("this session's own"), "{what}");
    assert!(what.contains("survives between the calls"), "{what}");
    assert!(
        v.summary().contains(&dir.display().to_string()),
        "{}",
        v.summary()
    );
    assert!(
        v.summary().contains("there for the next"),
        "{}",
        v.summary()
    );
    // The note an `ENOENT` under it produces follows the same fact.
    let b = Bwrap {
        helper: PathBuf::from("/usr/bin/bwrap"),
        boundary: Boundary {
            view: v,
            ..boundary_with(vec![])
        },
    };
    let notes = b.absence_notes("cat: /tmp/gone.txt: No such file or directory");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("survives between the calls"),
        "{}",
        notes[0]
    );
    assert!(!notes[0].contains("starts empty"), "{}", notes[0]);
}

/// A `/run` grant is a socket at a path a program writes out of habit (`/var/run/docker.sock`), and
/// the view empties `/run` per command. The bind has to come after that tmpfs, and the two spellings
/// have to be one directory, or the socket is there for one of them and absent for the other.
#[test]
fn a_grant_under_run_binds_over_the_fresh_tmpfs_and_is_there_under_both_spellings() {
    let docker = |p: &str| crate::exec::confine::Grant::ReadWrite {
        path: PathBuf::from(p),
        why: "the operator's `[boundary] grants_rw`".to_string(),
    };
    let view = ViewSpec::project_only("/proj").granting(docker("/run/docker.sock"));
    let argv = build_argv(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        },
    );
    let at = |needle: &str| {
        argv.iter()
            .position(|a| a == needle)
            .unwrap_or_else(|| panic!("{needle} is not in {argv:?}"))
    };
    // The whole file is a fresh tmpfs under the view, and the grant is a mount over it: bwrap takes
    // the last mount at a path, so the other order would hide the socket inside the empty one.
    let tmpfs = at("/run");
    assert_eq!(argv[tmpfs - 1], "--tmpfs", "{argv:?}");
    let bound = argv
        .windows(3)
        .position(|w| w[0] == "--bind" && w[1] == "/run/docker.sock")
        .expect("the grant is bound");
    assert!(
        bound > tmpfs,
        "the grant must be bound after the tmpfs that empties `/run`: {argv:?}"
    );
    assert_eq!(argv[bound + 2], "/run/docker.sock");
    // One directory, two spellings, so a program that asks for `/var/run/docker.sock` finds it.
    let link = argv
        .windows(3)
        .position(|w| w[0] == "--symlink" && w[1] == "/run" && w[2] == "/var/run")
        .expect("`/var/run` is linked to `/run`");
    assert!(link > tmpfs && link < bound, "{argv:?}");
    assert_eq!(
        view.classify(Path::new("/var/run/docker.sock")),
        Presence::Inside,
        "the spelling a program uses is in the view too"
    );

    // The grant the other way round is the same directory: `/var/run/x` puts `/run/x` in the view.
    let other = ViewSpec::project_only("/proj").granting(docker("/var/run/docker.sock"));
    assert_eq!(
        other.classify(Path::new("/run/docker.sock")),
        Presence::Inside
    );
    // bwrap cannot make a mount point through the symlink, so that grant is mounted at `/run`.
    let plan = ConfinePlan {
        cwd: Path::new("/proj"),
        env: &[],
    };
    let mounted_at_run = |argv: &[String]| {
        argv.windows(3).any(|w| {
            w[0] == "--bind" && w[1] == "/var/run/docker.sock" && w[2] == "/run/docker.sock"
        })
    };
    let other_argv = build_argv(Path::new("/usr/bin/bwrap"), &other, &Egress::Denied, &plan);
    assert!(mounted_at_run(&other_argv), "{other_argv:?}");
    let wide = ViewSpec::project_only("/proj")
        .wide()
        .granting(docker("/var/run/docker.sock"));
    let wide_argv = build_argv(Path::new("/usr/bin/bwrap"), &wide, &Egress::Denied, &plan);
    assert!(mounted_at_run(&wide_argv), "{wide_argv:?}");
    // And with no such grant, neither spelling is: `/run` is a fresh tmpfs and `/var/run` is
    // nothing at all, so a claim of `Inside` here would be the view lying.
    let plain = ViewSpec::project_only("/proj");
    assert!(matches!(
        plain.classify(Path::new("/var/run/docker.sock")),
        Presence::Outside
    ));
    assert!(matches!(
        plain.classify(Path::new("/run/docker.sock")),
        Presence::Replaced { .. }
    ));
    let no_link = build_argv(
        Path::new("/usr/bin/bwrap"),
        &plain,
        &Egress::Denied,
        &ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        },
    );
    assert!(
        !no_link.iter().any(|a| a == "--symlink"),
        "a view that grants nothing under `/run` is the argv it always was: {no_link:?}"
    );
}

#[test]
fn a_replaced_root_is_its_own_answer_and_not_either_of_the_other_two() {
    // The middle answer, and the note that would have been wrong: `/tmp` exists inside, so
    // calling it outside claims it cannot be reached; it is a fresh tmpfs, so calling it
    // inside leaves a bare `ENOENT` about a host file that really is invisible.
    let v = ViewSpec::project_only("/proj");
    assert_eq!(v.classify(Path::new("/proj/src")), Presence::Inside);
    assert_eq!(v.classify(Path::new("/opt/thing")), Presence::Outside);
    assert!(matches!(
        v.classify(Path::new("/tmp/host-file")),
        Presence::Replaced { .. }
    ));
    assert!(matches!(
        v.classify(Path::new("/proc/1234/cmdline")),
        Presence::Replaced { .. }
    ));
    // A project under `/tmp` is Inside, not Replaced: the bound roots are checked first,
    // and getting that order wrong would tell every fixture its workspace was a tmpfs.
    let under_tmp = ViewSpec::project_only("/tmp/letibot-fixture");
    assert_eq!(
        under_tmp.classify(Path::new("/tmp/letibot-fixture/src")),
        Presence::Inside
    );

    let b = boundary_with(vec![]);
    let notes = b.absence_notes("cat: /tmp/host-file: No such file or directory");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("fresh tmpfs"), "{}", notes[0]);
    assert!(notes[0].contains("starts empty"), "{}", notes[0]);
    // And it must NOT say the path is outside the view, because it is not.
    assert!(
        !notes[0].contains("not in this session's filesystem view"),
        "{}",
        notes[0]
    );
}

#[test]
fn a_miss_inside_the_view_gets_no_boundary_note() {
    // The false positive that would matter: a genuinely missing project file blamed on the
    // boundary sends the model looking for a grant it does not need.
    let b = boundary_with(vec![]);
    assert!(
        b.absence_notes("cat: /proj/src/nope.rs: No such file or directory")
            .is_empty()
    );
    assert!(b.absence_notes("hello, nothing is missing here").is_empty());
    // And a relative path, which is what most tools print.
    assert!(
        b.absence_notes("cat: src/nope.rs: No such file or directory")
            .is_empty()
    );
}

#[test]
fn the_absence_note_is_bounded_like_a_success_body() {
    // A miss must not produce an unbounded reply: see [`MAX_ABSENCE_PATHS`].
    let b = boundary_with(vec![]);
    let mut text = String::new();
    for i in 0..50 {
        text.push_str(&format!("cat: /opt/secret{i}: No such file or directory\n"));
    }
    let notes = b.absence_notes(&text);
    assert_eq!(notes.len(), 1);
    // Each path is named in the list and once more by its own grant sentence.
    let named: std::collections::BTreeSet<&str> = notes[0]
        .split('`')
        .filter(|t| t.starts_with("/opt/secret"))
        .collect();
    assert!(named.len() <= MAX_ABSENCE_PATHS, "named {}", named.len());
    assert!(
        notes[0].matches("/opt/secret").count() <= 2 * MAX_ABSENCE_PATHS,
        "{}",
        notes[0]
    );
}

/// A boundary over a real project directory, so the view's roots are paths a `stat` agrees
/// with rather than `/proj`. Its SYSTEM roots are empty on purpose: nothing of this box's own
/// is in the view, so a program that really exists here really is outside it.
fn a_boundary_over(project: &Path) -> Boundary {
    Boundary {
        view: ViewSpec {
            system: Vec::new(),
            ..ViewSpec::project_only(project)
        },
        ..boundary_with(vec![])
    }
}

/// A real directory this view does not contain, proved rather than hoped for: the note is about
/// a path that exists outside, and a fixture that cannot find one must fail here rather than
/// assert nothing and be counted as a pass.
fn a_dir_outside(view: &ViewSpec) -> PathBuf {
    let found = ["sh", "ls", "cat"]
        .into_iter()
        .filter_map(on_path)
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .find(|d| view.outside(d));
    match found {
        Some(d) => d,
        None => panic!(
            "no directory on this PATH is outside the fixture's view, so the case cannot be \
                 built here: PATH is {:?}",
            std::env::var_os("PATH")
        ),
    }
}

/// A program this box really has and the fixture's view really does not contain, with the word a
/// shell prints for it. Proved for [`a_dir_outside`]'s reason.
fn a_program_outside(view: &ViewSpec) -> (String, PathBuf) {
    let found = ["cargo", "sh", "ls"]
        .into_iter()
        .find_map(|n| on_path(n).map(|p| (n.to_string(), p)))
        .filter(|(_, p)| view.outside(p));
    match found {
        Some(pair) => pair,
        None => panic!(
            "no program on this PATH is outside the fixture's view, so the case cannot be \
                 built here: PATH is {:?}",
            std::env::var_os("PATH")
        ),
    }
}

/// The first hit for `name` on this process's `PATH`, resolved the way the note resolves it.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// And the guard that makes the two above real: a view that contains every program on this
/// PATH leaves nothing to assert, and that is a failure rather than a pass.
#[test]
#[should_panic(expected = "no program on this PATH is outside the fixture's view")]
fn a_fixture_that_finds_no_program_outside_fails_instead_of_passing() {
    let mut view = ViewSpec::project_only(std::env::temp_dir());
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        view = view.granting(Grant::ReadOnly {
            path: dir,
            why: "the fixture puts everything this process can run in the view".into(),
        });
    }
    a_program_outside(&view);
}

#[test]
fn an_absence_note_names_the_grant_that_would_allow_the_path_in() {
    // The grant is the only thing that changes the answer, so a note that stops at "absent"
    // leaves the model with no move but to ask again.
    let project = std::env::temp_dir().join(format!("letibot-view-{}", std::process::id()));
    std::fs::create_dir_all(&project).expect("fixture project");
    let b = a_boundary_over(&project);
    let real = a_dir_outside(&b.view);
    let file = real.join("config.toml");
    let said = format!("cat: {}: No such file or directory", file.display());

    let notes = b.absence_notes(&said);
    assert_eq!(notes.len(), 1, "{notes:?}");
    let n = &notes[0];
    assert!(n.contains(&file.display().to_string()), "{n}");
    assert!(n.contains("`ReadOnly` binds"), "{n}");
    assert!(n.contains("reach the transcript"), "{n}");

    // A granted path is in the view, so there is nothing absent to report about it.
    let granted = Boundary {
        view: ViewSpec::project_only(&project).granting(Grant::ReadOnly {
            path: real.clone(),
            why: "the fixture's own directory".into(),
        }),
        ..boundary_with(vec![])
    };
    assert!(granted.absence_notes(&said).is_empty());
}

#[test]
fn a_missing_program_is_named_as_the_path_the_view_does_not_contain() {
    // `cargo: command not found` names no path at all, so the scan found nothing and the
    // model was left reading the boundary as a broken toolchain.
    let project = std::env::temp_dir().join(format!("letibot-view-cmd-{}", std::process::id()));
    std::fs::create_dir_all(&project).expect("fixture project");
    let b = a_boundary_over(&project);
    let (name, program) = a_program_outside(&b.view);
    for said in [
        format!("{name}: command not found"),
        // What this box's `/bin/sh` really prints, measured rather than assumed.
        format!("/bin/sh: 1: {name}: not found"),
    ] {
        let notes = b.absence_notes(&said);
        assert_eq!(notes.len(), 1, "{said}: {notes:?}");
        let n = &notes[0];
        assert!(n.contains(&program.display().to_string()), "{said}: {n}");
        assert!(n.contains("`ReadOnly` binds"), "{said}: {n}");
        assert!(n.contains("reach the transcript"), "{said}: {n}");
    }
    // A name this host does not have is a typo, and a typo must not become a claim about
    // the boundary.
    assert!(
        b.absence_notes("nosuchtool_xyz: command not found")
            .is_empty()
    );
}

/// The note promises a grant, so the door that raises grants must name the same path. A
/// A container's own paths are not this host's: `ls /hosttmp/c-*` failing inside
/// `docker run -v /tmp:/hosttmp` must not ask the operator to grant `/hosttmp/c-*`.
#[test]
fn a_path_this_host_does_not_have_raises_no_grant_card() {
    let project = std::env::temp_dir().join(format!("letibot-view-host-{}", std::process::id()));
    std::fs::create_dir_all(&project).expect("fixture project");
    let b = a_boundary_over(&project);
    for said in [
        "ls: cannot access '/hosttmp/c-*': No such file or directory",
        "cat: /letibot-no-such-dir-anywhere/x: No such file or directory",
    ] {
        assert!(b.outside_paths(said).is_empty(), "{said}");
    }
    let _ = std::fs::remove_dir_all(&project);
}

/// missing program folded into the note and not into the ask leaves the model waiting for a
/// decision nobody was asked for.
#[test]
fn a_missing_program_is_a_path_the_grant_card_is_raised_for() {
    let project = std::env::temp_dir().join(format!("letibot-view-grant-{}", std::process::id()));
    std::fs::create_dir_all(&project).expect("fixture project");
    let b = a_boundary_over(&project);
    let (name, program) = a_program_outside(&b.view);
    for said in [
        format!("{name}: command not found"),
        // What this box's `/bin/sh` really prints, measured rather than assumed. It names the
        // shell too, so the claim is that the program is one of the paths asked about.
        format!("/bin/sh: 1: {name}: not found"),
    ] {
        let asked = b.outside_paths(&said);
        assert!(
            asked.contains(&program),
            "{said}: the note names {}, so the grant door has to ask about it: it asked \
                 about {asked:?}",
            program.display()
        );
    }
    // A typo is not a path: nothing is asked about it, and nothing is said.
    assert!(
        b.outside_paths("nosuchtool_xyz: command not found")
            .is_empty()
    );
}

#[test]
fn an_agent_socket_is_forwardable_and_a_key_file_is_not() {
    // The subtle credential case as a test: the mechanism accepts a socket and refuses
    // anything else, because usable-without-readable is a property of the socket.
    let dir = std::env::temp_dir().join(format!("letibot-agent-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let key = dir.join("id_rsa");
    std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\n").expect("fixture key");

    // Not a socket: refused, with the mechanism named.
    // SAFETY: single-threaded test scope; the variable is restored below.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &key) };
    let e = Grant::agent_from_env("authorised ssh").unwrap_err();
    let m = format!("{e}");
    assert!(m.contains("not a socket"), "{m}");
    assert!(m.contains("Nothing was bound"), "{m}");

    // Absent: refused, and the refusal names ssh-agent rather than a bind.
    unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
    let e = Grant::agent_from_env("authorised ssh").unwrap_err();
    let m = format!("{e}");
    assert!(m.contains("ssh-add"), "{m}");
    assert!(m.contains("never the key"), "{m}");
    // And it names the SECOND decision the authorised case needs, in words a model can act
    // on rather than the enum it is built from.
    assert!(m.contains("second decision"), "{m}");
    for internal in ["Egress::Host", "§"] {
        assert!(!m.contains(internal), "{m}");
    }

    // A real socket is accepted and bound at a fixed path, so the operator's
    // `/run/user/<uid>` path does not travel.
    let sock = dir.join("agent.sock");
    let _ = std::fs::remove_file(&sock);
    let l = std::os::unix::net::UnixListener::bind(&sock).expect("bind agent socket");
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };
    let g = Grant::agent_from_env("the operator said `ssh build-box`").expect("socket");
    assert!(matches!(g, Grant::AgentSocket { .. }));
    assert!(g.line().contains("NOT readable"), "{}", g.line());
    assert!(g.line().contains(AGENT_SOCKET), "{}", g.line());
    drop(l);
    unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_argv_puts_the_project_in_and_leaves_the_home_directory_out() {
    let view = ViewSpec::project_only("/home/dead/Projects/letibot");
    let a = build_argv(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: Path::new("/home/dead/Projects/letibot/crates"),
            env: &[("CARGO_TERM_COLOR".into(), "never".into())],
        },
    );
    let joined = a.join(" ");
    for f in [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
    ] {
        assert!(joined.contains(f), "missing {f} in {joined}");
    }
    // The project is bound read-write at its own path.
    let i = a.iter().position(|x| x == "--bind").expect("a --bind");
    assert_eq!(a[i + 1], "/home/dead/Projects/letibot");
    assert_eq!(a[i + 2], "/home/dead/Projects/letibot");
    // `$HOME` is NOT the operator's home, and the operator's home is not bound.
    let home = a.iter().position(|x| x == "HOME").expect("HOME is set");
    assert_eq!(a[home + 1], TMPFS_HOME);
    assert!(
        !a.iter().any(|x| x == "/home/dead"),
        "the operator's home must never be an argument: {joined}"
    );
    // The caller's env survives `--clearenv`, because it is re-declared.
    assert!(joined.contains("CARGO_TERM_COLOR never"), "{joined}");
    // The command is separated from the options by `--`, so a command beginning with a
    // dash is a command.
    assert_eq!(a.last().map(String::as_str), Some("--"));
}

#[test]
fn declared_egress_drops_the_network_unshare_and_binds_the_resolver() {
    let view = ViewSpec::project_only("/proj");
    let denied = build_argv(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Denied,
        &ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        },
    );
    let host = build_argv(
        Path::new("/usr/bin/bwrap"),
        &view,
        &Egress::Host {
            why: "authorised".into(),
        },
        &ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        },
    );
    assert!(denied.iter().any(|a| a == "--unshare-net"));
    assert!(!host.iter().any(|a| a == "--unshare-net"));
    // The resolver's files come with the network and only with it.
    for p in NETWORK_ROOTS {
        assert!(host.iter().any(|a| a == p), "{p} is bound with the network");
        assert!(
            !denied.iter().any(|a| a == p),
            "{p} is not bound without it"
        );
    }
    // Those two differences, and only those: everything else is identical.
    let d: Vec<&String> = denied.iter().filter(|a| *a != "--unshare-net").collect();
    let mut h: Vec<&String> = Vec::new();
    let mut i = 0;
    while i < host.len() {
        if host[i] == "--ro-bind-try" && NETWORK_ROOTS.contains(&host[i + 1].as_str()) {
            i += 3;
            continue;
        }
        h.push(&host[i]);
        i += 1;
    }
    assert_eq!(d, h);
}

#[test]
fn a_boundary_a_child_can_unshare_out_of_is_not_confined() {
    // An open seal fails `complete()` rather than merely making the boundary partial:
    // "partial" implies the rest still holds, and this is what makes the rest hold.
    let mut b = boundary_with(vec![]);
    assert!(b.complete());
    b.seals.insert(
        SealKind::NestedNamespaces,
        Seal::Open {
            why: "`unshare -U true` inside the boundary SUCCEEDED".into(),
        },
    );
    assert!(!b.complete(), "an open seal is not a boundary");
    let d = b.describe();
    assert!(d.contains("NOT CONFINED"), "{d}");
    assert!(d.contains("seal is open"), "{d}");
    assert!(d.contains("**OPEN**"), "{d}");
    assert!(d.contains("cannot `unshare` itself back out"), "{d}");
}

#[test]
fn a_seal_that_could_not_be_tested_is_partial_and_never_held() {
    // An absent measurement is not a passing one: it still confines, because the
    // namespaces were measured, but the disclosure must not read as fully checked.
    let mut b = boundary_with(vec![]);
    b.seals.insert(
        SealKind::NestedNamespaces,
        Seal::Unverified {
            why: "`unshare` is not in this view".into(),
        },
    );
    assert!(b.complete(), "unknown is not open");
    assert!(b.partial(), "and unknown is not held either");
    let d = b.describe();
    assert!(d.contains("PARTIALLY"), "{d}");
    assert!(d.contains("UNVERIFIED"), "{d}");
}

#[test]
fn the_seals_are_tested_and_a_missing_verdict_is_unknown_not_ok() {
    // The parse, which is where "an absent measurement counts as a pass" would creep in.
    let held = parse_seals("ns user user:[1]\nseal nested held\nseal nnp 1\n");
    assert!(held[&SealKind::NestedNamespaces].held());
    assert!(held[&SealKind::NoNewPrivs].held());

    let open = parse_seals("seal nested open\nseal nnp 0\n");
    assert!(matches!(
        open[&SealKind::NestedNamespaces],
        Seal::Open { .. }
    ));
    assert!(matches!(open[&SealKind::NoNewPrivs], Seal::Open { .. }));

    // Nothing at all: unknown, both of them.
    let silent = parse_seals("ns user user:[1]\n");
    for k in SealKind::ALL {
        assert!(
            matches!(silent[&k], Seal::Unverified { .. }),
            "{k} must be unknown, not held"
        );
    }
    // `unshare` absent from the view is its own reason, and it names the independent
    // mechanism without claiming that mechanism as a measurement.
    let no_tool = parse_seals("seal nested unverified\nseal nnp 1\n");
    let Seal::Unverified { why } = &no_tool[&SealKind::NestedNamespaces] else {
        panic!("expected unverified");
    };
    assert!(why.contains("assert-userns-disabled"), "{why}");
    assert!(why.contains("not a measurement"), "{why}");
}

#[test]
fn the_argv_seals_the_boundary_and_asserts_the_seal_took() {
    let a = build_argv(
        Path::new("/usr/bin/bwrap"),
        &ViewSpec::project_only("/proj"),
        &Egress::Denied,
        &ConfinePlan {
            cwd: Path::new("/proj"),
            env: &[],
        },
    );
    assert!(a.iter().any(|x| x == "--disable-userns"), "{a:?}");
    assert!(
        a.iter().any(|x| x == "--assert-userns-disabled"),
        "the request must be checked by the helper too: {a:?}"
    );
}

#[test]
fn a_helper_setup_failure_is_not_reported_as_the_commands_answer() {
    // The shape that is easy to miss: the wrong answer here is not `ok`, it is "the
    // command ran and exited non-zero".
    let dir = std::env::temp_dir();
    let b = Bwrap {
        helper: PathBuf::from("/usr/bin/bwrap"),
        boundary: Boundary {
            view: ViewSpec::project_only(&dir),
            ..boundary_with(vec![])
        },
    };
    let n = b
        .launcher_failure("bwrap: Can't bind mount /nope on /nope: No such file\n")
        .expect("a bwrap diagnostic is a launcher failure");
    assert!(n.contains("NOT the command's answer"), "{n}");
    assert!(n.contains("may never have run"), "{n}");
    // And ordinary output is not misread as one.
    assert!(
        b.launcher_failure("error: could not compile `x`\n")
            .is_none()
    );
    assert!(b.launcher_failure("").is_none());
}

#[test]
fn a_readback_that_matches_the_harness_is_not_a_boundary() {
    // The parse and the comparison, without a helper: this is what decides `Entered` from
    // `NotEntered`, and it is what would let a hard-coded `confined` through if wrong.
    let inside = parse_readback("ns user user:[42]\nns mnt mnt:[43]\nnoise\nns pid\n");
    assert_eq!(inside.get("user"), Some(&"user:[42]".to_string()));
    assert_eq!(inside.get("mnt"), Some(&"mnt:[43]".to_string()));
    assert_eq!(
        inside.get("pid"),
        None,
        "a line with no inode is not a reading"
    );
    assert!(parse_readback("total nonsense").is_empty());
}

#[test]
fn a_missing_helper_is_a_refusal_that_names_the_kernel_policy_too() {
    // The refusal has to be actionable: the interesting half is not "install bubblewrap"
    // but why a direct unshare is not the answer.
    // SAFETY: single-threaded test scope; restored below.
    unsafe { std::env::set_var(BWRAP_ENV, "/nonexistent/bwrap") };
    let e = find_helper().unwrap_err();
    unsafe { std::env::remove_var(BWRAP_ENV) };
    let m = format!("{e}");
    assert!(m.contains("/nonexistent/bwrap"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");
}
