/// `[embeddings]` is read off the two `leticode.toml` levels, the project's over the user's: a
/// level that wrote none leaves the other's standing, and a section that cannot be built is a
/// sentence kept for the next session's open rather than a silence.
#[test]
fn the_embedder_section_is_read_from_the_project_level_then_the_users() {
    let base = std::env::temp_dir().join(format!("config-embeddings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("a scratch dir");
    let project = base.join("project.toml");
    let user = base.join("user.toml");
    // The project says nothing about embeddings; the user wrote a section that would do nothing,
    // which the reader refuses by name.
    std::fs::write(&project, "[default]\nmain_model = \"m\"\n").expect("the project file");
    std::fs::write(
        &user,
        "[embeddings]\nmode = \"off\"\nmodel = \"/tmp/nowhere\"\n",
    )
    .expect("the user file");

    let mut cfg = Config::for_this_box(&base);
    assert!(
        cfg.embedder.get().is_none() && cfg.embedder.fault().is_none(),
        "a fixture scores the words alone and has nothing to say"
    );
    assert!(cfg.tier().is_none());

    cfg.leticode.path = Some(project);
    cfg.leticode_user.path = Some(user);
    cfg.read_embeddings();
    assert!(
        cfg.embedder.get().is_none(),
        "the section asked for no tier"
    );
    let said = cfg.embedder.fault().expect("and the refusal is kept");
    assert!(said.contains("mode = \"off\""), "{said}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A box that may not reach the network writes it down. With the download off, a built-in
/// model that is not on disk is the refusal this tree already had, and never a fetch.
#[test]
fn a_section_that_turns_the_download_off_keeps_the_refusal() {
    let base = std::env::temp_dir().join(format!("config-no-download-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("a scratch dir");
    let user = base.join("user.toml");
    std::fs::write(
        &user,
        "[embeddings]\nmode = \"internal\"\ndownload = false\n",
    )
    .expect("the user file");

    let mut cfg = Config::for_this_box(&base);
    cfg.leticode_user.path = Some(user);
    cfg.read_embeddings();
    assert!(cfg.embedder.get().is_none(), "nothing was fetched");
    let said = cfg.embedder.fault().expect("and the refusal is kept");
    assert!(said.contains("all-MiniLM-L6-v2"), "{said}");
    let _ = std::fs::remove_dir_all(&base);
}

/// A first-use fetch is off the start path. Offline it waits out the retry ladder before giving up,
/// and a daemon that opens no session until it has is one the operator believes is broken. The tier
/// appears when the bytes land; every score until then is the words alone, and the note says so.
#[test]
fn a_first_use_fetch_does_not_hold_the_daemon_start() {
    use letibot_embeddings::{Cache, Embedder, Engine, Planted};

    let base = std::env::temp_dir().join(format!("config-fetch-thread-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("a scratch dir");
    let data = base.join("data");
    std::fs::create_dir_all(&data).expect("a data dir");

    let mut cfg = Config::for_this_box(&base);
    let settings = letibot_embeddings::Settings {
        mode: letibot_embeddings::Mode::Internal { model: None },
        download: true,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    // The fetch's cost, stood in for: the same delay, on the thread the real one would be on.
    let for_the_thread = data.clone();
    cfg.open_the_embedder(settings, Some(data), move |_, _| {
        std::thread::sleep(std::time::Duration::from_millis(600));
        let cache = Cache::open_in(&for_the_thread).expect("cache");
        Ok(Some(Embedder::with(
            Engine::Planted(Planted::new(&[&["alpha"], &["beta"]])),
            cache,
        )))
    });
    let took = started.elapsed();

    assert!(
        took < std::time::Duration::from_millis(200),
        "the start waited {took:?} for the fetch"
    );
    assert!(
        cfg.embedder.get().is_none(),
        "and until the bytes land every score is the words alone"
    );
    assert!(
        cfg.embedder
            .take_fetch_note()
            .is_some_and(|said| said.contains("being fetched now")),
        "the operator is told why the tier is not there yet"
    );

    // The tier appears when the thread is done, on the same config every session reads.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while cfg.embedder.get().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        cfg.tier().expect("the tier the fetch produced").model(),
        "planted"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// The tree a VM placement copies is the session's own copy source where it has one, and the
/// repository it is about otherwise - the one spelling `Harness::open` reads when it boots the VM
/// and the only thing a test can read on a box it must not boot one on. `None` means the workspace.
#[test]
fn a_vm_placement_copies_the_sessions_own_source_when_it_has_one() {
    let mut cfg = Config::for_this_box("/tmp/the-workspace");
    assert_eq!(
        cfg.vm_copy_source(),
        std::path::Path::new("/tmp/the-workspace"),
        "with no copy source of its own and no repository to name, a VM is booted on the \
             session's workspace — what every build before `vm_source` did"
    );
    cfg.vm_source = Some(PathBuf::from("/tmp/the-workspace/the-repo"));
    assert_eq!(
        cfg.vm_copy_source(),
        std::path::Path::new("/tmp/the-workspace/the-repo"),
        "and a session told which repository the work is in copies THAT, which is the whole \
             of what the review queue sets on a reviewer"
    );
}

/// A box whose daemon serves a FOLDER of repositories boots a VM on the one repository -
/// 8 GiB, the ceiling is 8, and the queue landed nothing.
/// The fixture is that box in miniature: a folder NAMED after the repository inside it.
#[test]
fn a_vm_placement_on_a_folder_of_repositories_copies_the_one_repository() {
    let parent = std::env::temp_dir().join(format!("letibot-vm-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&parent);
    // The daemon's workspace - a folder named after the repository inside it - and two sibling
    // worktrees beside that repository, which is what makes the folder a FOLDER of them.
    let ws = parent.join("letibot");
    let repo = ws.join("letibot");
    a_repository(&repo);
    for wt in ["wt-drill", "wt-jobs"] {
        a_repository(&ws.join(wt));
    }

    let mut cfg = Config::for_this_box(&ws);
    assert_eq!(
        cfg.vm_copy_source(),
        repo,
        "the folder is not what a VM is booted on: the repository is, and the same-named \
             child is what names it — a folder holding the repository AND its worktrees has no \
             single repository child to pick"
    );
    // And the caller's own word still beats the resolution, which is what the review queue
    // relies on: a reviewer is booted on the ENTRY's repository, which need not be this one.
    cfg.vm_source = Some(PathBuf::from("/tmp/the-entries-repo"));
    assert_eq!(
        cfg.vm_copy_source(),
        std::path::Path::new("/tmp/the-entries-repo"),
        "a session told which repository the work is in copies THAT"
    );

    // And a workspace with nothing to name answers the workspace - the honest fallback
    // whose refusal at the ceiling is loud and names the tree rather than booting a VM on some
    // sibling that happens to be a repository.
    let nowhere = parent.join("scratch");
    std::fs::create_dir_all(&nowhere).expect("mkdir");
    assert_eq!(
        Config::for_this_box(&nowhere).vm_copy_source(),
        nowhere,
        "a workspace in no repository and holding none resolves to itself"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

/// A real repository at `at`, with `git` itself. [`crate::mergequeue::repo_of_workspace`] asks
/// git first, so a fixture that only made a `.git` directory would be testing its second arm by
/// accident and this one on purpose.
fn a_repository(at: &Path) {
    std::fs::create_dir_all(at).expect("mkdir");
    let ok = letibot_tools::testing::git(at)
        .args(["init", "-q", "-b", "main"])
        .status()
        .expect("git");
    assert!(ok.success(), "git init in {}", at.display());
    letibot_tools::testing::repo_config(at);
}

/// Every declared local model reaches the picker's row. `/models dense78` worked and bare
/// `/models` - which draws THIS row's `choices` - showed only the compiled-in presets. It passes
/// vacuously on a box that declares nothing, and fails loudly on every box the feature is for.
#[test]
fn the_model_row_offers_every_declared_local_model() {
    let cfg = Config::for_this_box("/tmp");
    let rows = cfg.settings("", false, &[]);
    let model = rows.iter().find(|r| r.key == "model").expect("a model row");
    for m in letibot_provider::keys::local_models(None) {
        assert!(
            model.choices.contains(&m.name),
            "`{}` is declared in providers.toml but the picker does not offer it: {:?}",
            m.name,
            model.choices
        );
    }
    assert!(
        model.choices.iter().any(|c| c == "local"),
        "the daemon's own server stays first: {:?}",
        model.choices
    );
}

/// With a key held, a provider's block is every model it can answer with, not the one default a
/// build-time constant names - which is what the card had, so a DeepSeek account offering twelve
/// models offered one row. Every other provider is still one row, because that row's job is to
/// ask for the key. The lines are the window and the meter, which is what a choice is made on.
#[test]
fn a_keyed_provider_lists_every_model_it_answers_with_and_the_others_list_one() {
    let cat = letibot_provider::catalogue::Catalogue::snapshot();
    assert!(!cat.is_empty(), "the vendored snapshot parses");
    let rows = model_rows(
        letibot_provider::presets::ALL,
        &["deepseek"],
        &cat,
        &[],
        Some(Path::new("/nonexistent/providers.toml")),
    );
    assert_eq!(rows[0].0, "local", "the box's own server comes first");
    let deepseek: Vec<&(String, String)> = rows
        .iter()
        .filter(|(n, _)| n.starts_with("deepseek/"))
        .collect();
    assert!(
        deepseek.len() > 1,
        "one row for a keyed provider is the defect: {deepseek:?}"
    );
    assert_eq!(
        deepseek.len(),
        cat.models("deepseek").len(),
        "a catalogue model the card does not offer cannot be switched to"
    );
    for (name, facts) in &deepseek {
        assert!(facts.contains("window"), "{name}: {facts}");
    }
    // Two rates and not one: a model is chosen on what it charges for what it writes too. Not
    // every row, since a catalogue may leave a model unpriced and that is said, not zeroed.
    assert!(
        deepseek.iter().any(|(_, f)| f.contains(" in /")),
        "not one row carries a rate: {deepseek:?}"
    );
    let glm: Vec<&(String, String)> = rows.iter().filter(|(n, _)| n.starts_with("glm/")).collect();
    assert_eq!(glm.len(), 1, "no key, so one row and a prompt: {glm:?}");
    // Nothing is listed twice, whatever two presets share.
    let mut seen = std::collections::HashSet::new();
    for (name, _) in &rows {
        assert!(seen.insert(name.clone()), "{name} is offered twice");
    }
}

/// A key held for a provider the catalogue says nothing about still gets its row: the card
/// dropping a provider the operator can actually reach is worse than a row admitting it knows no
/// window, and `/models glm/NAME` would then be the only way to it.
#[test]
fn a_keyed_provider_with_no_catalogue_still_gets_its_row() {
    let empty = letibot_provider::catalogue::Catalogue::default();
    let rows = model_rows(
        letibot_provider::presets::ALL,
        &["glm"],
        &empty,
        &[],
        Some(Path::new("/nonexistent/providers.toml")),
    );
    let glm: Vec<&(String, String)> = rows.iter().filter(|(n, _)| n.starts_with("glm/")).collect();
    assert_eq!(glm.len(), 1, "the provider vanished: {rows:?}");
    assert_eq!(
        glm[0].1,
        letibot_provider::catalogue::ModelFacts::missing_line(),
        "said rather than blank: {}",
        glm[0].1
    );
}

/// The fleet's own blocks come before the providers and carry the address they answer on, since
/// two blocks can name one model and only the address says which.
#[test]
fn the_declared_local_models_come_before_every_provider() {
    let cat = letibot_provider::catalogue::Catalogue::snapshot();
    let fleet = vec![("dense78".to_string(), "http://192.168.1.9:8080".to_string())];
    let rows = model_rows(
        letibot_provider::presets::ALL,
        &["deepseek"],
        &cat,
        &fleet,
        Some(Path::new("/nonexistent/providers.toml")),
    );
    assert_eq!(rows[0].0, "local");
    assert_eq!(rows[1].0, "dense78");
    assert!(rows[1].1.contains("192.168.1.9"), "{:?}", rows[1]);
    assert!(rows[2].0.starts_with("deepseek/"), "{:?}", rows[2]);
}

/// The facts travel beside the choices as one row of their own, one line each and never a
/// different number: a card that paired them by guess would give a model another one's window.
#[test]
fn the_facts_row_is_one_line_per_model_choice() {
    let cfg = Config::for_this_box("/tmp");
    let rows = cfg.settings("", false, &[]);
    let model = rows.iter().find(|r| r.key == "model").expect("a model row");
    let facts = rows
        .iter()
        .find(|r| r.key == letibot_sessionlog::protocol::MODEL_FACTS_KEY)
        .expect("the facts row is published beside the choices");
    assert_eq!(
        facts.choices.len(),
        model.choices.len(),
        "{} choices, {} facts",
        model.choices.len(),
        facts.choices.len()
    );
    for (name, line) in model.choices.iter().zip(&facts.choices) {
        assert!(!line.is_empty(), "`{name}` has no facts line");
    }
}

/// Which doors serve the same models is a fact of the preset table and not of a name, so the
/// daemon publishes it: the card draws one row per catalogue, and a box whose only door is
/// `deepseek-anthropic` still gets `deepseek`'s models listed once.
#[test]
fn the_doors_row_pairs_every_preset_with_the_catalogue_it_serves() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.providers_file = Some(providers_file("doors-row", ""));
    let rows = cfg.settings("", false, &[]);
    let doors = rows
        .iter()
        .find(|r| r.key == letibot_sessionlog::protocol::MODEL_DOORS_KEY)
        .expect("the doors row is published beside the choices");
    assert_eq!(
        doors.choices.len(),
        letibot_provider::registry::all(cfg.providers_file.as_deref()).len(),
        "one door per preset: {:?}",
        doors.choices
    );
    let catalogue = |door: &str| {
        doors
            .choices
            .iter()
            .find(|c| c.split_once(' ').is_some_and(|(d, _)| d == door))
            .and_then(|c| c.split_once(' '))
            .map(|(_, cat)| cat.to_string())
    };
    for door in letibot_provider::presets::ALL {
        let cat = catalogue(door.name)
            .unwrap_or_else(|| panic!("no catalogue for {}: {:?}", door.name, doors.choices));
        assert_eq!(
            cat, door.catalogue_id,
            "{} pairs with the wrong catalogue",
            door.name
        );
    }
    // The pairing is what folds them: two doors, one catalogue, one row on the card.
    assert_eq!(catalogue("deepseek"), catalogue("deepseek-anthropic"));
    assert_ne!(catalogue("deepseek"), catalogue("glm"));
}

/// The picker's greening comes from a row, and the row names the presets this box can actually
/// authenticate. The assertion is the agreement, not a list: a row that disagreed with the
/// daemon's own check would green exactly the rows that fail at the first turn.
#[test]
fn the_box_publishes_which_providers_it_holds_a_key_for() {
    let cfg = Config::for_this_box("/tmp");
    let rows = cfg.settings("default", false, &[]);
    let row = rows
        .iter()
        .find(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYS_KEY)
        .expect("the keyed-providers row is published");
    assert!(
        !row.value.contains(' '),
        "the list is comma-joined with no spaces, so a head splits on one thing: {:?}",
        row.value
    );
    let named: Vec<&str> = if row.value.is_empty() {
        Vec::new()
    } else {
        row.value.split(',').collect()
    };
    for n in &named {
        assert!(
            letibot_provider::presets::ALL.iter().any(|p| p.name == *n),
            "`{n}` is not a preset this build knows"
        );
    }
    // `local` is absent on purpose: it needs no credential, and its presence here would
    // be a claim about a key rather than about reachability.
    assert!(!named.contains(&"local"), "{named:?}");
    for p in letibot_provider::presets::ALL {
        let resolves = letibot_provider::keys::resolve(p, None, None).is_ok();
        assert_eq!(
            resolves,
            named.contains(&p.name),
            "`{}` disagrees with the answer `keys::resolve` gives it",
            p.name
        );
    }
    // A plain value row: a head COLOURS with it, it does not choose from it.
    assert!(row.choices.is_empty(), "{row:?}");
}

/// The consented point describes itself, not the one that was asked for. `Mode::ALLOW_ALL`'s
/// summary says the VM is the boundary, which is false of the operator's laptop - printing it for a
/// session on `ALLOW_ALL_HERE` put that sentence two lines above one saying there is no confinement.
#[test]
fn the_two_allow_all_points_do_not_borrow_each_others_sentences() {
    use letibot_tools::mode::Mode;
    assert!(
        Mode::ALLOW_ALL.summary.contains("the VM is the boundary"),
        "the confined point still claims a VM"
    );
    assert!(
        !Mode::ALLOW_ALL_HERE.summary.contains("VM"),
        "the consented point must not claim a boundary it does not have: {}",
        Mode::ALLOW_ALL_HERE.summary
    );
    assert!(
        Mode::ALLOW_ALL_HERE.summary.contains("operator confirmed"),
        "and must say what it DOES stand on: {}",
        Mode::ALLOW_ALL_HERE.summary
    );
    // The names differ too, so a card naming the applied point cannot read as
    // the confined one.
    assert_ne!(Mode::ALLOW_ALL.name, Mode::ALLOW_ALL_HERE.name);
}

/// A carry is quoted in the units the operator can check. `/reseat` refused on `991596 token(s)
/// in front of a 1000000-token window` while the header said 671k: both numbers were right, and the
/// refusal compared the ledger's sum against a window expressed in the provider's units.
#[test]
fn a_carry_is_converted_into_the_providers_units_before_it_is_judged() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.context_window = Some(1_000_000);
    // Unmeasured: there is nothing to convert with, and saying so is the
    // point - a caller must not substitute the ledger's own figure.
    assert_eq!(cfg.provider_tokens(991_596), None);

    cfg.ledger_scale = Some((1_000_699, 671_280));
    let carried = cfg.provider_tokens(991_596).expect("a conversion");
    assert!(
        (660_000..680_000).contains(&carried),
        "the ledger's 991596 is about 665k to the provider, which is what the \
             operator's header showed; got {carried}"
    );
    assert!(
        carried + cfg.headroom() < 1_000_000,
        "and it FITS — {carried} plus {} headroom against 1000000. The refusal \
             the operator saw was the unconverted comparison.",
        cfg.headroom()
    );
    // The unconverted comparison, kept here so the regression is named rather
    // than merely absent: this is the wrong question, and it says no.
    assert!(
        991_596 + cfg.headroom() >= 1_000_000,
        "the premise: the LEDGER figure does not fit, which is why the units \
             mattered"
    );
}

/// The wall continuation asked the same question with the same mistake. `room_for_next_turn` is
/// handed `ledger_len`, so its window has to be the ledger's too - reading `context_window` closed
/// the gate on a conversation the provider had 330k tokens of room for.
#[test]
fn room_for_the_next_turn_is_measured_in_ledger_tokens() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.context_window = Some(1_000_000);
    cfg.ledger_scale = Some((1_000_699, 671_280));
    assert!(
        cfg.room_for_next_turn(991_596),
        "991596 ledger tokens is ~665k to the provider and has room; the raw \
             window said no"
    );
    // And it still says no when there genuinely is none, so the fix is not
    // "always true".
    assert!(!cfg.room_for_next_turn(cfg.planning_window().unwrap()));
}

/// The window is planned in the units the ledger counts in. The ledger counts a LOCAL rendered
/// prompt with reasoning in it; a messages provider is sent neither, so comparing one against the
/// other made a conversation that thinks a lot compact while most of its window was free.
#[test]
fn the_planning_window_follows_the_measured_ledger_to_provider_ratio() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.context_window = Some(1_000_000);
    cfg.auto_compact = true;

    // Before any metered turn there is nothing to convert with, so the
    // provider's number stands.
    assert_eq!(cfg.planning_window(), Some(1_000_000));

    // letibot #t18: the ledger called it 1,072,176; deepseek called it
    // ~607,000. Planning against 1,000,000 ledger tokens compacted a
    // conversation with 39% of its window free.
    cfg.ledger_scale = Some((1_072_176, 607_000));
    let w = cfg.planning_window().expect("a window");
    assert!(
        (1_700_000..1_800_000).contains(&w),
        "a 1M provider window is ~1.77M ledger tokens here, got {w}"
    );
    assert!(
        !cfg.should_compact(1_072_176),
        "this is the turn that compacted early and must not any more"
    );
    // And it still compacts when the conversation really is at the wall.
    assert!(cfg.should_compact(w));

    // leticl #t11 thought less, so its ledger runs closer to the provider's
    // count - the ratio is a measurement, not a constant.
    cfg.ledger_scale = Some((1_495_702, 1_048_607));
    let w2 = cfg.planning_window().expect("a window");
    assert!((1_400_000..1_500_000).contains(&w2), "got {w2}");

    // A nonsense measurement cannot move the wall somewhere absurd.
    cfg.ledger_scale = Some((1, 1_000_000));
    assert_eq!(cfg.planning_window(), Some(250_000), "clamped at a quarter");
    cfg.ledger_scale = Some((1_000_000, 1));
    assert_eq!(
        cfg.planning_window(),
        Some(16_000_000),
        "clamped at sixteen times"
    );
    // And a real reasoning-heavy session is inside the bound, not cut by it : 3,454,266
    // ledger tokens counted ~740k by the provider. At the old bound of four this was a 4M window
    // at the wall with ~860k of the provider's 1M used.
    cfg.ledger_scale = Some((3_454_266, 740_000));
    assert!(!cfg.should_compact(3_454_266), "740k of 1M is not the wall");
    let w3 = cfg.planning_window().expect("a window");
    assert!((4_600_000..4_700_000).contains(&w3), "got {w3}");

    // An unknown window is still not a large one.
    cfg.context_window = None;
    assert_eq!(cfg.planning_window(), None);
    assert!(!cfg.should_compact(u64::MAX / 2));
}

/// A session that starts on a metered provider gets a wall. `harnessd` read the window off
/// `/props` only when there was no provider, and `None` is no window at all. MEASURED: 1,023,545
/// resident tokens against a model the catalogue puts at 1,000,000, never compacted once.
#[test]
fn a_metered_provider_takes_its_window_from_the_catalogue() {
    let dir = std::env::temp_dir().join(format!("letibot-cat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("models.json");
    std::fs::write(
        &path,
        r#"{"deepseek":{"id":"deepseek","models":{
                 "deepseek-flash":{"id":"deepseek-flash","name":"f",
                   "limit":{"context":1000000,"output":384000},
                   "cost":{"input":0.28,"output":0.42}}}}}"#,
    )
    .expect("fixture");
    let cat = letibot_provider::catalogue::Catalogue::read(&path).expect("catalogue");

    let pc = |model: Option<&str>| ProviderConfig {
        name: "deepseek".into(),
        model: model.map(str::to_string),
        api_key: None,
        thinking: false,
    };
    assert_eq!(
        pc(Some("deepseek-flash")).catalogue_window(&cat),
        Some(1_000_000),
        "the wall the operator's session never had"
    );
    // A model the catalogue does not carry stays `None` - the old behaviour
    // for the case the old comment was actually about.
    assert_eq!(pc(Some("no-such-model")).catalogue_window(&cat), None);
    // And a preset this build does not know.
    assert_eq!(
        ProviderConfig {
            name: "nope".into(),
            ..pc(None)
        }
        .catalogue_window(&cat),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}
use super::*;

// --- prompts.toml: per-model system-prompt composition -------------------

/// The full seat: leticode's tools with `bash`, the room and the web, and a scratch
/// directory - every section of the base prompt has something to say for it.
fn seat() -> SeatFacts {
    let mut f = SeatFacts::with_tools(letibot_tools::runtime::roles::leticode().tools);
    for t in ["flowy", "web_search", "web_fetch"] {
        f.tools.insert(t.into());
    }
    f.scratch = Some("/run/user/1000/letibot/scratch-4242".into());
    f.view = Some(view_brief_for("/proj"));
    f
}

/// A confined session's view, as the boundary states it: the fixture asks a real `Boundary`
/// rather than inventing a sentence no boundary would say.
fn view_brief_for(project: &str) -> String {
    let b = letibot_tools::exec::confine::Boundary::namespaces(
        "bubblewrap 0.0.0 (fixture)",
        letibot_tools::exec::confine::ViewSpec::project_only(project),
        letibot_tools::exec::confine::Egress::Denied,
        std::collections::BTreeMap::new(),
        std::collections::BTreeMap::new(),
    );
    b.model_brief()
}

/// Every tool name this build seats anywhere, for the "names no tool it lacks" check.
fn every_tool() -> std::collections::BTreeSet<String> {
    use letibot_tools::runtime::roles;
    let mut all = std::collections::BTreeSet::new();
    for r in [
        roles::orchestrator(),
        roles::coder(),
        roles::leticode(),
        roles::m1_orchestrator(),
        roles::m3_researcher(),
        roles::planner(),
        roles::m2_coder(),
        roles::m2_runner(),
        roles::gatekeeper(),
        roles::notes_keeper(),
    ] {
        all.extend(r.tools);
    }
    // Registered beside a role by the daemon, not named in one.
    for t in [
        "flowy",
        "web_search",
        "web_fetch",
        "review_verdict",
        "digest",
        "decisions",
    ] {
        all.insert(t.into());
    }
    all
}

/// The backticked tool names `text` mentions that `seat` does not hold.
fn names_it_lacks(text: &str, seat: &SeatFacts) -> Vec<String> {
    let all = every_tool();
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(|w| w.split_whitespace().next().unwrap_or("").to_string())
        .filter(|w| all.contains(w) && !seat.has(w))
        .collect()
}

/// The prompt names no tool the seat lacks - for every seat the daemon opens, with
/// and without `--bash`, as a root and as a child. The tool lists are the roles' own, filtered the
/// way `harness::base_role_for_seat` filters them; the integration half is `tests/wired.rs`.
#[test]
fn the_prompt_names_no_tool_the_seat_lacks() {
    use letibot_tools::runtime::roles;
    let exec = [
        "bash",
        "job_list",
        "job_output",
        "job_wait",
        "job_kill",
        "monitor",
        "pkill",
        "ps",
    ];
    let seats = [
        ("leticode", roles::leticode().tools, true),
        ("orchestrator", roles::m1_orchestrator().tools, true),
        ("researcher", roles::m3_researcher().tools, true),
        ("planner", roles::planner().tools, true),
        ("coder", roles::m2_coder().tools, true),
        ("runner", roles::m2_runner().tools, false),
        ("gatekeeper", roles::gatekeeper().tools, false),
        ("notes_keeper", roles::notes_keeper().tools, false),
    ];
    for (name, tools, room) in seats {
        for bash in [true, false] {
            for subagent in [false, true] {
                let mut f = SeatFacts::with_tools(
                    tools
                        .iter()
                        .filter(|t| bash || !exec.contains(&t.as_str()))
                        .cloned(),
                );
                if room && !subagent {
                    f.tools.insert("flowy".into());
                }
                f.subagent = subagent;
                f.scratch = Some("/run/user/1000/letibot/scratch-1".into());
                let system = default_system(&f);
                let lacks = names_it_lacks(&system, &f);
                assert!(
                    lacks.is_empty(),
                    "{name} (bash {bash}, subagent {subagent}) is told about {lacks:?}:\n{system}"
                );
                // And the read-only claim is never made to a seat that can write.
                assert!(!system.contains("read-only tools"), "{name}: {system}");
            }
        }
    }
}

/// What a seat has decides what it is told - the specific sentences found false.
#[test]
fn a_seat_is_told_only_what_is_true_for_it() {
    // A seat with `edit` and `write` is told to change files with them; one without is not.
    assert!(default_system(&seat()).contains("Change files with `edit` and `write`"));
    let reader = SeatFacts::with_tools(["read", "grep", "glob"]);
    assert!(!default_system(&reader).contains("`edit`"));
    // The scratch path is the real one, and only for a seat that can put a file there.
    assert!(default_system(&seat()).contains("`/run/user/1000/letibot/scratch-4242`"));
    assert!(
        !default_system(&SeatFacts {
            scratch: Some("/s/scratch-1".into()),
            ..reader.clone()
        })
        .contains("scratch")
    );
    // A shell without the job verbs is not told about background jobs.
    let gate = SeatFacts {
        subagent: true,
        ..SeatFacts::with_tools(["read", "grep", "glob", "bash", "review_verdict"])
    };
    assert!(!default_system(&gate).contains("background job"));
    // The web is for current facts when it is seated, and not reached for otherwise.
    assert!(default_system(&seat()).contains("use `web_search` or `web_fetch` when"));
    assert!(default_system(&reader).contains("directly, without a tool"));
    // A child is told whose answer its last reply is; a root is told when to end its turn.
    let child = SeatFacts {
        subagent: true,
        ..seat()
    };
    assert!(default_system(&child).contains("You are a subagent"));
    assert!(!default_system(&seat()).contains("You are a subagent"));
    assert!(default_system(&seat()).contains("end your turn and say in one line what it waits on"));
    // The sentence, as the tools say it.
    assert!(default_system(&seat()).contains(
        "completion reaches you automatically — when your current turn ends, or at once if a \
         todo row needs it or wake is \"now\"."
    ));
    // The queue rule: a worktree child commits and never pushes; a seat that starts
    // worktree children is told who lands them.
    let in_worktree = SeatFacts {
        worktree: true,
        ..child
    };
    assert!(default_system(&in_worktree).contains("commit your work there"));
    assert!(default_system(&seat()).contains("landed by the merge queue"));
    // No number about the context, ever.
    for f in [seat(), reader] {
        let system = default_system(&f);
        assert!(system.contains("The harness manages your context"));
        assert!(
            !system.contains('%') && !system.contains("tokens"),
            "{system}"
        );
    }
}

/// The base prompt for the main seats, printed - `cargo test -p letibot-harnessd
/// print_the_main_prompts -- --ignored --nocapture` to read what a seat is told.
#[test]
#[ignore]
fn print_the_main_prompts() {
    use letibot_tools::runtime::roles;
    let mut root = SeatFacts::with_tools(roles::leticode().tools);
    root.tools.insert("flowy".into());
    root.scratch = Some("/run/user/1000/letibot/scratch-4242".into());
    let mut child = SeatFacts::with_tools(roles::m2_coder().tools);
    child.subagent = true;
    child.worktree = true;
    child.scratch = root.scratch.clone();
    let mut gate = SeatFacts::with_tools(roles::gatekeeper().tools);
    gate.tools.insert("review_verdict".into());
    gate.subagent = true;
    gate.scratch = root.scratch.clone();
    for (name, f) in [
        ("leticode root", root),
        ("coder child", child),
        ("gatekeeper", gate),
    ] {
        println!("===== {name}\n{}\n", default_system(&f));
    }
}

/// A temp dir for a `prompts.toml` fixture, unique per test so parallel tests
/// do not share a file. The caller removes it.
fn prompts_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-prompts-{}-{}", std::process::id(), test));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// (a) No file: the composed prompt is the built-in one byte for byte. The safety property that
/// makes the feature safe to land: a daemon with no `prompts.toml` composes the built-in prompt
/// unchanged, for every model.
#[test]
fn no_prompts_file_composes_the_default_byte_for_byte() {
    let dir = prompts_dir("no_file");
    let path = dir.join("prompts.toml");
    let prompts = Prompts::load(&path).expect("a missing file is Ok, not an error");
    assert_eq!(
        prompts.compose("deepseek/deepseek-flash", &seat()),
        default_system(&seat())
    );
    assert_eq!(
        prompts.compose("qwen-3.8-flash-next", &seat()),
        default_system(&seat())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// (b) An empty file: the composed prompt is the built-in one byte for byte - a file that
/// overrides nothing is the same as no file.
#[test]
fn an_empty_prompts_file_composes_the_default_byte_for_byte() {
    let dir = prompts_dir("empty");
    let path = dir.join("prompts.toml");
    std::fs::write(&path, "").expect("fixture");
    let prompts = Prompts::load(&path).expect("an empty file is Ok");
    assert_eq!(
        prompts.compose("deepseek/deepseek-flash", &seat()),
        default_system(&seat())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// (c) A `[model."NAME"]` override applies to that model's session and not to another's - the
/// override is scoped to the name, not global.
#[test]
fn a_model_override_applies_to_its_model_and_not_another() {
    let dir = prompts_dir("model_override");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        r#"
[model."deepseek/deepseek-flash"]
system_extra = "You are a deepseek specialist."
"#,
    )
    .expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");
    // The named model gets the extra, appended to the base.
    assert_eq!(
        prompts.compose("deepseek/deepseek-flash", &seat()),
        format!(
            "{}\n\nYou are a deepseek specialist.",
            default_system(&seat())
        )
    );
    // Another model under the same provider does not.
    assert_eq!(
        prompts.compose("deepseek/deepseek-chat", &seat()),
        default_system(&seat())
    );
    // A model under a different provider does not.
    assert_eq!(
        prompts.compose("glm-coding/glm-5.3", &seat()),
        default_system(&seat())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A provider glob matches every model under that provider, and the exact name wins when both are
/// present - `deepseek/*` is "any deepseek model" without naming each one, and an exact name is a
/// more specific instruction.
#[test]
fn a_provider_glob_matches_its_models_and_the_exact_name_wins() {
    let dir = prompts_dir("glob");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        r#"
[model."deepseek/*"]
system_extra = "DeepSeek house style."

[model."deepseek/deepseek-flash"]
system_extra = "Flash is special."
"#,
    )
    .expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");
    // The exact name wins for the model it names.
    assert_eq!(
        prompts.compose("deepseek/deepseek-flash", &seat()),
        format!("{}\n\nFlash is special.", default_system(&seat()))
    );
    // The glob covers the rest of the provider's models.
    assert_eq!(
        prompts.compose("deepseek/deepseek-chat", &seat()),
        format!("{}\n\nDeepSeek house style.", default_system(&seat()))
    );
    // Another provider is untouched.
    assert_eq!(
        prompts.compose("glm-coding/glm-5.3", &seat()),
        default_system(&seat())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// (d) The `[base]` section replaces that section for every model. Present, the base section is
/// the operator's text, not the built-in; the other sections are left alone, and a model extra still
/// appends.
#[test]
fn the_base_section_replaces_that_section_for_every_model() {
    let dir = prompts_dir("base");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        r#"
[base]
tone = "A wholly different tone."

[model."deepseek/deepseek-flash"]
system_extra = "And a model extra on top."
"#,
    )
    .expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");
    // The base tone replaces the default tone for a model with no override.
    let composed = prompts.compose("glm-coding/glm-5.3", &seat());
    assert!(composed.contains("A wholly different tone."));
    assert!(!composed.contains("Be direct. Prefer the shortest answer that is complete."));
    // The other sections are still the default.
    assert!(composed.contains("You are a careful software engineering assistant"));
    // A model extra appends to the composed base.
    let composed = prompts.compose("deepseek/deepseek-flash", &seat());
    assert!(composed.contains("A wholly different tone."));
    assert!(composed.ends_with("And a model extra on top."));
    let _ = std::fs::remove_dir_all(&dir);
}

/// (e) A malformed file is reported with the path and the parser's message, and the session still
/// runs on the built-in prompt - a refused file leaves `Config.prompts` at `Default`, which
/// composes the built-in prompt.
#[test]
fn a_malformed_prompts_file_is_reported_and_the_session_runs_on_the_default() {
    let dir = prompts_dir("malformed");
    let path = dir.join("prompts.toml");
    std::fs::write(&path, "this is not [valid toml").expect("fixture");
    let err = Prompts::load(&path).expect_err("a malformed file is Err");
    // The report names the path.
    let path_str = path.to_str().expect("utf-8 path");
    assert!(err.contains(path_str), "the path is in the report: {err}");
    // And it carries the parser's own message, not a bare "failed".
    assert!(
        err.len() > path_str.len() + 4,
        "the parser's message is in the report: {err}"
    );
    // The session still runs: a refused file leaves the prompts at `Default`
    // which composes the built-in prompt byte for byte.
    let cfg = Config::for_this_box("/tmp");
    assert_eq!(
        cfg.prompts.compose("deepseek/deepseek-flash", &seat()),
        default_system(&seat())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A top-level section the daemon does not know is a parse error, not a silent ignore.
/// `deny_unknown_fields` refuses a `[foo]` with the parser's own message; a section NAME it does not
/// know inside `[base]` or `[model."NAME"]` is refused by [`Prompts::load`].
#[test]
fn an_unknown_section_is_refused_not_ignored() {
    let dir = prompts_dir("unknown_section");
    let path = dir.join("prompts.toml");
    std::fs::write(&path, "[foo]\nbar = \"baz\"\n").expect("fixture");
    let err = Prompts::load(&path).expect_err("an unknown section is Err");
    let path_str = path.to_str().expect("utf-8 path");
    assert!(err.contains(path_str), "the path is in the report: {err}");
    assert!(err.contains("foo"), "the unknown section is named: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// With no overrides, the composition is the seat's sections joined, in order -
/// every section of [`SECTION_NAMES`] that has something to say for the seat, and
/// nothing else.
#[test]
fn the_default_is_the_seats_sections_joined_in_order() {
    let f = seat();
    let joined = SECTION_NAMES
        .iter()
        .filter_map(|n| default_section(n, &f))
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_eq!(joined, default_system(&f));
    // The full seat has every section.
    for name in SECTION_NAMES {
        assert!(default_section(name, &f).is_some(), "{name}");
    }
}

/// The notes convention is instruction in the base, not data in a block. The injected block is
/// DATA; that notes exist, what belongs in one, and that a note is not ground truth is INSTRUCTION
/// which must hold even with no notes - and must not restate the envelope's own sentence.
#[test]
fn the_default_prompt_carries_the_notes_convention_and_not_the_envelope() {
    let paragraph = default_section("notes", &seat()).expect("a `notes` section");
    let paragraph = paragraph.as_str();
    // It exists, it is read into the prompt, it comes back after a
    // compaction.
    assert!(paragraph.contains("reads into this prompt"));
    assert!(paragraph.contains("compaction"));
    // The purpose the operator named, in their words: keep what is worth
    // keeping - not a running summary.
    assert!(paragraph.contains("interesting, remarkable or surprising"));
    assert!(paragraph.contains("not want to rediscover"));
    assert!(paragraph.contains("not as a summary of what you did"));
    // Where they live and how to write one.
    assert!(paragraph.contains("`notes` tool"));
    assert!(paragraph.contains("notes repository outside the tree"));
    // The caveat, so a note is not read as ground truth.
    assert!(paragraph.contains("what was true or intended when it was written"));
    // And not the injected block's framing - that sentence is the
    // envelope's job and is asserted there.
    assert!(!paragraph.contains("one in force"));
    assert!(!paragraph.contains("[standing-notes"));
}

/// A confined session is told what its view holds BEFORE anything fails, and a session whose
/// commands run on this machine's disk is told nothing at all - a section there would have to
/// deny a view it has not got, and would change every unconfined prompt's bytes.
#[test]
fn a_confined_seat_is_told_what_its_view_holds_and_an_unconfined_one_is_not() {
    let confined = seat();
    let text = default_system(&confined);
    assert!(text.contains("inside a filesystem view"), "{text}");
    assert!(text.contains("`/proj`"), "the project is named: {text}");
    assert!(
        text.contains(letibot_tools::exec::confine::TMPFS_HOME),
        "the home inside the view is named: {text}"
    );
    // In its own place: after the file-editing instruction, before the scratch directory - the
    // reader learns what is reachable, then where to put working files.
    let at = text.find("inside a filesystem view").expect("the section");
    let scratch = text
        .find("Your scratch directory")
        .expect("the scratch section");
    assert!(at < scratch, "the view comes before the scratch: {text}");

    // Nothing confined about a seat whose commands run on this machine's disk.
    let unconfined = SeatFacts {
        view: None,
        ..seat()
    };
    assert!(!default_system(&unconfined).contains("inside a filesystem view"));
    assert!(default_section("view", &unconfined).is_none());
    // And the brief is the boundary's own words, relayed.
    assert_eq!(
        default_section("view", &confined).as_deref(),
        confined.view.as_deref()
    );
}

/// A section replaced by `[model."NAME"]` changes only that section and keeps the order - the
/// named model's `tree` is the operator's text, every other section the default, in the order of
/// [`SECTION_NAMES`].
#[test]
fn a_model_section_replaces_only_that_section_and_keeps_the_order() {
    let dir = prompts_dir("model_section");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        r#"
[model."qwen-3.8-27b"]
tree = "Use your tools, and you use them."
"#,
    )
    .expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");
    let f = seat();
    let composed = prompts.compose("qwen-3.8-27b", &f);
    // The replaced section is the operator's text, not the default.
    assert!(composed.contains("Use your tools, and you use them."));
    assert!(!composed.contains("Answer questions about the world"));
    // Every other section is the default, in the order it appears.
    let expected = SECTION_NAMES
        .iter()
        .map(|n| {
            if *n == "tree" {
                "Use your tools, and you use them.".to_string()
            } else {
                default_section(n, &f).expect("the full seat has every section")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_eq!(composed, expected);
    // Another model is untouched.
    assert_eq!(
        prompts.compose("deepseek/deepseek-flash", &f),
        default_system(&f)
    );
    // A section the seat has nothing true to say in stays out, overridden or not: an
    // `edit_files` override does not give a read-only seat a sentence about `edit`.
    std::fs::write(&path, "[base]\nedit_files = \"Edit with edit.\"\n").expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");
    let reader = SeatFacts::with_tools(["read", "grep", "glob"]);
    assert!(!prompts.compose("m", &reader).contains("Edit with edit."));
    assert!(prompts.compose("m", &f).contains("Edit with edit."));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `[base]` section is overridden by a model one, in place - the base is the fallback, not the
/// ceiling.
#[test]
fn a_base_section_is_overridden_by_a_model_one() {
    let dir = prompts_dir("base_overridden");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        r#"
[base]
tone = "House tone, for every model."

[model."deepseek/deepseek-flash"]
tone = "Flash has its own tone."
"#,
    )
    .expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");
    // The named model gets its own tone, not the base one.
    let composed = prompts.compose("deepseek/deepseek-flash", &seat());
    assert!(composed.contains("Flash has its own tone."));
    assert!(!composed.contains("House tone, for every model."));
    // Another model gets the base tone.
    let composed = prompts.compose("deepseek/deepseek-chat", &seat());
    assert!(composed.contains("House tone, for every model."));
    assert!(!composed.contains("Flash has its own tone."));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `system_extra` is appended last, and not appended when unset - when no layer sets it, the
/// composition is the sections alone.
#[test]
fn system_extra_is_appended_last_and_not_when_unset() {
    let dir = prompts_dir("system_extra");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        r#"
[model."deepseek/deepseek-flash"]
system_extra = "One short tool call beats a long plan."
"#,
    )
    .expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");
    // Set: appended after every section.
    let composed = prompts.compose("deepseek/deepseek-flash", &seat());
    assert!(composed.ends_with("One short tool call beats a long plan."));
    assert_eq!(
        composed,
        format!(
            "{}\n\nOne short tool call beats a long plan.",
            default_system(&seat())
        )
    );
    // Unset for another model: nothing appended, the default byte for byte.
    assert_eq!(
        prompts.compose("deepseek/deepseek-chat", &seat()),
        default_system(&seat())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An unknown section key is refused by name, in each layer - a load error naming the key and the
/// file's path, in `[base]` and `[model."NAME"]` alike; the session runs on the un-overridden
/// composition.
#[test]
fn an_unknown_section_key_is_refused_by_name_in_each_layer() {
    // In `[base]`.
    let dir = prompts_dir("unknown_base_key");
    let path = dir.join("prompts.toml");
    std::fs::write(&path, "[base]\nread_only_tool = \"typo\"\n").expect("fixture");
    let err = Prompts::load(&path).expect_err("an unknown base key is Err");
    let path_str = path.to_str().expect("utf-8 path");
    assert!(err.contains(path_str), "the path is in the report: {err}");
    assert!(err.contains("read_only_tool"), "the key is named: {err}");
    assert!(err.contains("[base]"), "the layer is named: {err}");
    let _ = std::fs::remove_dir_all(&dir);

    // In `[model."NAME"]`.
    let dir = prompts_dir("unknown_model_key");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        "[model.\"deepseek/deepseek-flash\"]\ntonee = \"typo\"\n",
    )
    .expect("fixture");
    let err = Prompts::load(&path).expect_err("an unknown model key is Err");
    let path_str = path.to_str().expect("utf-8 path");
    assert!(err.contains(path_str), "the path is in the report: {err}");
    assert!(err.contains("tonee"), "the key is named: {err}");
    assert!(
        err.contains("deepseek/deepseek-flash"),
        "the model is named: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // The session still runs: a refused file leaves the prompts at `Default`
    // which composes the built-in prompt byte for byte.
    let cfg = Config::for_this_box("/tmp");
    assert_eq!(
        cfg.prompts.compose("deepseek/deepseek-flash", &seat()),
        default_system(&seat())
    );
}

/// The operator's excerpt, end to end. A utility model seated with `web_search` was told not to
/// call a tool for a question about the world - so a model whose job is looking things up was
/// discouraged from it. The fix: replace `tree` for that model, and append a `system_extra`.
#[test]
fn the_operators_excerpt_composes_the_new_tree_section_for_that_model() {
    let dir = prompts_dir("operator_excerpt");
    let path = dir.join("prompts.toml");
    std::fs::write(
        &path,
        r#"
[base]
tone = "Short. Direct. No preamble."

[model."qwen-3.8-27b"]
tree = "You have read-only tools, and a web_search. For a fact about the world — a version, a date, a definition, something that changed last week — call `web_search` first and answer from what comes back. Do not answer from memory, and do not guess."
system_extra = "One short tool call beats a long plan."
"#,
    )
    .expect("fixture");
    let prompts = Prompts::load(&path).expect("parse");

    // The named model: the new `tree`, not the default one.
    let composed = prompts.compose("qwen-3.8-27b", &seat());
    assert!(composed.contains("call `web_search` first and answer from what comes back"));
    assert!(!composed.contains("Answer questions about the world"));
    // The base tone is in place, and the model extra is appended last.
    assert!(composed.contains("Short. Direct. No preamble."));
    assert!(composed.ends_with("One short tool call beats a long plan."));
    // The other sections are the default, in order.
    assert!(composed.contains("You are a careful software engineering assistant"));
    assert!(composed.contains("Find with `grep` and read with `read`"));

    // A session on another model: the default `tree`, the base tone
    // and no model extra.
    let composed = prompts.compose("deepseek/deepseek-flash", &seat());
    assert!(composed.contains("Answer questions about the world"));
    assert!(!composed.contains("call `web_search` first and answer from what comes back"));
    assert!(composed.contains("Short. Direct. No preamble."));
    assert!(!composed.ends_with("One short tool call beats a long plan."));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Config::compose_for_seat` composes from the file and the session's model, and
/// `prompt_model_name` names the model the way the operator sees it - the composition runs on the
/// session's config, using the name the operator would write in `prompts.toml`.
#[test]
fn compose_for_seat_uses_the_session_model_name() {
    // A local session: the model name is the bare alias.
    let mut cfg = Config::for_this_box("/tmp");
    cfg.model = "qwen-3.8-flash-next".into();
    assert_eq!(cfg.prompt_model_name(), "qwen-3.8-flash-next");
    cfg.compose_for_seat(&seat(), &SeatOverlay::default(), None);
    assert_eq!(cfg.system, default_system(&seat()));

    // A metered session: the model name is `provider/model`.
    let mut cfg = Config::for_this_box("/tmp");
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-flash".into()),
        ..Default::default()
    });
    assert_eq!(cfg.prompt_model_name(), "deepseek/deepseek-flash");
    cfg.prompts = Prompts {
        models: [(
            "deepseek/deepseek-flash".to_string(),
            ModelOverrides {
                sections: Default::default(),
                system_extra: Some("for flash".into()),
            },
        )]
        .into_iter()
        .collect(),
        ..Prompts::default()
    };
    cfg.compose_for_seat(&seat(), &SeatOverlay::default(), None);
    assert_eq!(
        cfg.system,
        format!("{}\n\nfor flash", default_system(&seat()))
    );
}

/// The model row names the provider when there is one. The daemon's half of : the operator's
/// turns went to deepseek while the head's header said `qwen-3.8-27b`, because the header prefers
/// this row and falls back to `Hello`'s `wiring.model`.
#[test]
fn the_model_row_names_the_provider_when_one_is_configured() {
    let mut cfg = Config::for_this_box("/tmp/x");
    assert!(
        cfg.provider.is_none(),
        "a fresh config is the local server, or this test says nothing"
    );
    let local = cfg
        .settings("--model", false, &[])
        .into_iter()
        .find(|r| r.key == "model")
        .expect("model");
    assert!(
        local.value.starts_with("local ("),
        "no provider must name the local alias: {}",
        local.value
    );

    // Now a provider, which is what `--provider deepseek` resolves to before any
    // session exists.
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        ..Default::default()
    });
    let row = cfg
        .settings("", false, &[])
        .into_iter()
        .find(|r| r.key == "model")
        .expect("model");
    assert!(
        row.value.starts_with("deepseek/"),
        "a configured provider must be named by the model row, or every head goes on \
             showing the local alias: {}",
        row.value
    );
    assert!(
        !row.value.contains("qwen"),
        "the local alias must not survive a provider: {}",
        row.value
    );
    // And the source is empty rather than `--model`: the model is the provider's
    // not the flag's.
    assert_eq!(row.source, "", "{}", row.source);
}

/// `compose_for_seat` puts the standing notes after `system_extra`, and the session's own paragraph
/// and fabric block after the notes. Asserted by position, and the section is read back with
/// `carried` - the same extraction a base rebuild uses - so what is appended is what is swapped later.
#[test]
fn compose_for_seat_puts_the_notes_after_system_extra_and_before_the_own_paragraph() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.prompts = Prompts {
        base_extra: Some("extra goes before the notes".into()),
        ..Prompts::default()
    };
    let notes = "[standing-notes-begin]\nnonce-notes-order\n[standing-notes-end]";
    let prompt = SeatOverlay {
        own: Some("\nOWN PARAGRAPH".into()),
        fabric: Some(FabricPart::Unreachable("FABRIC BLOCK".into())),
        ..SeatOverlay::default()
    };
    cfg.compose_for_seat(&seat(), &prompt, Some(notes));
    let at = |what: &str| {
        cfg.system
            .find(what)
            .unwrap_or_else(|| panic!("{what}: {}", cfg.system))
    };
    assert!(at("extra goes before the notes") < at("nonce-notes-order"));
    assert!(at("nonce-notes-order") < at("OWN PARAGRAPH"));
    assert!(at("OWN PARAGRAPH") < at("FABRIC BLOCK"));
}

/// The pane's contract: the two rows that change now come first and name
/// their verb; every key is unique (a duplicate would be two rows that
/// disagree); nothing that takes a restart claims a verb.
#[test]
fn settings_rows_lead_with_what_changes_now_and_never_repeat_a_key() {
    let cfg = Config::for_this_box("/tmp/x");
    let rows = cfg.settings("project store (modes.tsv)", false, &[]);
    assert_eq!(rows[0].key, "mode");
    assert_eq!(rows[0].editable, "/mode NAME");
    assert_eq!(rows[0].source, "project store (modes.tsv)");
    // The changeable rows lead, counted without the model card's own `models.*` rows: those are
    // the model row's business and sit beside it, however many the card grows.
    let leading: Vec<&str> = rows
        .iter()
        .map(|r| r.key.as_str())
        .filter(|k| !k.starts_with("models."))
        .take(3)
        .collect();
    assert!(leading.contains(&"supervise"), "{leading:?}");
    assert!(leading.contains(&"model"), "{leading:?}");
    let sup = rows
        .iter()
        .find(|r| r.key == "supervise")
        .expect("supervise");
    assert!(sup.value.starts_with("off"));
    // The model row carries its own choices, so a head can draw the picker
    // without keeping a list of its own.
    let model = rows.iter().find(|r| r.key == "model").expect("model");
    assert!(
        model.value.starts_with("local ("),
        "no provider is the local server, and the row still names its alias: {}",
        model.value
    );
    assert!(
        model.choices.contains(&"local".to_string()),
        "{:?}",
        model.choices
    );
    assert!(
        model.choices.iter().any(|c| c.starts_with("glm/")),
        "{:?}",
        model.choices
    );
    let mut seen = std::collections::HashSet::new();
    for r in &rows {
        assert!(seen.insert(r.key.clone()), "duplicate key {}", r.key);
    }
    assert!(
        rows.iter()
            .any(|r| r.key == "oracle.budget" && r.editable.is_empty())
    );
    assert!(
        rows.iter()
            .any(|r| r.key == "workspace" && r.value == "/tmp/x")
    );
    // Supervision on reads as on.
    let on = cfg.settings("x", true, &[]);
    let sup = on.iter().find(|r| r.key == "supervise").expect("supervise");
    assert!(sup.value.starts_with("on"));
}

/// The round count is a backstop now, not the stop. `max_tool_rounds: 12` was the only thing that
/// could end a runaway turn, so it also ended two working ones. What is asserted is the
/// *relationship*: a progress check armed, and the count outside a real investigation's range.
#[test]
fn the_stop_is_the_progress_check_and_the_round_count_is_the_backstop() {
    let c = Config::for_this_box("/tmp");
    assert!(
        c.stall_rounds > 0,
        "a session with no progress check is the defect"
    );
    assert!(
        c.max_tool_rounds == 0 || c.max_tool_rounds >= 100,
        "a real investigation is dozens of rounds, so the backstop is either \
             unbounded or far outside that range; {} is inside it and would cut one",
        c.max_tool_rounds
    );
    let d = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .find(|d| d.subject == "progress check")
        .expect("which guard is armed is not something an operator should guess");
    assert!(d.active);
    assert!(d.to_string().contains(&c.stall_rounds.to_string()));
}

/// The mode line's second sentence must agree with the mode's own summary at
/// `allow-all (this box, consented)`, whose summary admits the always-ask list and which was then
/// told *"the list reaches you at every one of them"*. The two reasons it can be absent differ.
#[test]
fn the_mode_line_says_what_the_mode_itself_says() {
    use letibot_tools::mode::Mode;
    let detail = |m: Mode| {
        let mut c = Config::for_this_box("/tmp");
        c.mode = m;
        c.disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "mode")
            .expect("the mode is always disclosed")
            .detail
    };

    let consented = detail(Mode::ALLOW_ALL_HERE);
    assert!(
        consented.contains("the always-ask list")
            && consented.contains("does not ask here either")
            && consented.contains("you consented to that"),
        "a consented allow-all admits the list, and says who let it: {consented}"
    );
    assert!(
        !consented.contains("reaches you at every"),
        "and does not then claim the opposite: {consented}"
    );

    let structural = detail(Mode::ALLOW_ALL);
    assert!(
        structural.contains("structural") && structural.contains("does not ask here"),
        "a confined allow-all's reason is the boundary, not a person's answer: {structural}"
    );

    let asking = detail(Mode::ALWAYS_ASK);
    assert!(
        asking.contains("still reaches you at every action on it"),
        "every other point asks: {asking}"
    );

    let reading = detail(Mode::READ_ONLY);
    assert!(
        reading.contains("nothing to ask about"),
        "read-only has no action for the list to reach, and saying it does would describe a \
         capability this point does not have: {reading}"
    );

    for m in [
        Mode::READ_ONLY,
        Mode::ALWAYS_ASK,
        Mode::WRITES_ALLOWED,
        Mode::AUTO,
        Mode::AUTO_EDITS,
        Mode::ALLOW_ALL,
        Mode::ALLOW_ALL_HERE,
    ] {
        let d = detail(m);
        assert!(
            !d.contains("an blocked"),
            "`{}` still says the word for a refused tier as `an blocked`: {d}",
            m.name
        );
    }
}

/// Both guards off is its own state, and says so. Turning the progress check off with the backstop
/// already unbounded leaves nothing between a looping turn and forever - not a refusal, but it must not
/// read as either one alone: a "0-round backstop" would name a guard that is not there.
#[test]
fn both_guards_off_is_disclosed_as_its_own_state() {
    let mut c = Config::for_this_box("/tmp");
    assert_eq!(c.max_tool_rounds, 0, "the premise: unbounded by default");
    c.stall_rounds = 0;
    let line = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .find(|d| d.subject == "progress check")
        .expect("especially when there is nothing left")
        .to_string();
    assert!(
        line.contains("NOTHING STOPS A TURN"),
        "it has to read as its own state: {line}"
    );
    assert!(
        !line.contains("0-round"),
        "and must never name a backstop that is not armed: {line}"
    );
    // Both ways back are named, because either one alone re-arms something.
    assert!(line.contains("--stall-rounds"), "{line}");
    assert!(line.contains("--max-tool-rounds"), "{line}");
}

/// The disclosure says which level won. *"Why is my session on deepseek"* was answered by a file
/// they had to go and read; the fix is that the model, the level and the file are on the screen. A
/// disclosure naming only the model would be the same defect one line shorter.
#[test]
fn the_session_model_disclosure_names_the_level_that_won() {
    use crate::config::ModelSource;
    use crate::leticode_config::Level;
    let mut c = Config::for_this_box("/tmp");

    // No flag, no file: the built-in default, and it says so - the fourth of the
    // four levels rather than a sentence about nothing.
    c.model_source = ModelSource::default();
    let line = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .find(|d| d.subject == "session model")
        .expect("the model is always disclosed, and always with its level")
        .to_string();
    assert!(line.contains("built-in"), "{line}");
    assert!(
        line.contains("the command line beats the project file"),
        "and the precedence is on the same line rather than in a file: {line}"
    );

    // Each of the four levels names itself, and a file level names the file.
    for (level, path, wanted) in [
        (Level::Flag, None, "the command line"),
        (
            Level::Project,
            Some(PathBuf::from("/w/leticode.toml")),
            "/w/leticode.toml",
        ),
        (
            Level::User,
            Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
            "/h/.config/letibot/leticode.toml",
        ),
        (Level::Builtin, None, "the built-in default"),
    ] {
        c.model_source = ModelSource {
            level,
            value: Some("glm-5.3-flash".into()),
            file: path,
            origin: "--provider deepseek".into(),
            note: None,
        };
        let line = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "session model")
            .expect("session model")
            .to_string();
        assert!(
            line.contains(wanted),
            "{level:?} must name {wanted}: {line}"
        );
        assert!(
            line.contains(level.as_str()),
            "and its word is stable: {line}"
        );
    }

    // A fault found at startup rides the same line, because the screen is where
    // the operator asked for it.
    c.model_source = ModelSource {
        level: Level::User,
        value: Some("glm-5.3-flash".into()),
        file: Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
        origin: String::new(),
        note: Some("`glm-5.3-flash` is a local alias, and the server is not serving it".into()),
    };
    let line = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .find(|d| d.subject == "session model")
        .expect("session model")
        .to_string();
    assert!(line.contains("is not serving it"), "{line}");
}

/// The sentence, verbatim. The deliverable of requirement 3 is a sentence on the screen, so it is
/// pinned here rather than described: a change to it is a change to what the operator reads, which is a
/// thing to be deliberate about rather than to notice later.
#[test]
fn the_session_model_sentence_is_this_one() {
    use crate::leticode_config::Level;
    let user = ModelSource {
        level: Level::User,
        value: Some("glm-5.3-flash".into()),
        file: Some(PathBuf::from("/home/dead/.config/letibot/leticode.toml")),
        origin: String::new(),
        note: None,
    };
    assert_eq!(
        user.render("glm-5.3-flash"),
        "main glm-5.3-flash — from the user level \
             (/home/dead/.config/letibot/leticode.toml). The order is: the command line beats \
             the project file, the project file beats the user file, the user file beats the \
             built-in default. The built-in default is `[default]` in \
             ~/.config/letibot/providers.toml (what `/models NAME` writes), else the local \
             server this daemon was launched against."
    );

    // The built-in case says which built-in it is: `[default]` when that is what
    // spoke, and the launch line when nothing did.
    let default_block = ModelSource {
        level: Level::Builtin,
        value: Some("deepseek/deepseek-flash".into()),
        file: Some(PathBuf::from("/home/dead/.config/letibot/providers.toml")),
        origin: "[default] in /home/dead/.config/letibot/providers.toml".into(),
        note: None,
    };
    let line = default_block.render("deepseek/deepseek-flash");
    assert!(
        line.starts_with(
            "main deepseek/deepseek-flash — from the built-in level \
                 (/home/dead/.config/letibot/providers.toml)."
        ),
        "{line}"
    );

    // A fault is appended after the precedence, so the rule is read before the
    // exception to it.
    let faulty = ModelSource {
        note: Some("and `no-such-alias` is not one this daemon can reach.".into()),
        ..user.clone()
    };
    let line = faulty.render("glm-5.3-flash");
    assert!(
        line.ends_with("and `no-such-alias` is not one this daemon can reach."),
        "{line}"
    );
}

/// No file at all is the built-in default, and says so - the fourth level, named, rather than a
/// silence the operator has to interpret.
#[test]
fn no_file_at_all_is_the_builtin_default_and_says_so() {
    let c = Config::for_this_box("/tmp");
    assert!(c.leticode.path.is_none(), "the premise: no file was read");
    assert!(c.leticode_user.path.is_none());
    let line = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .find(|d| d.subject == "session model")
        .expect("session model")
        .to_string();
    assert!(
        line.contains("the built-in default"),
        "nothing spoke, and the screen says which level that is: {line}"
    );
    assert!(
        line.contains(&c.prompt_model_name()),
        "and it names the model, so the line is about this session: {line}"
    );
}

/// The user file is disclosed as its own level, and the project's over it.
/// Two files, two lines, because the operator who wrote one of them needs to see
/// which one spoke - and the project line names the keys in force after the merge.
#[test]
fn the_user_file_is_disclosed_as_its_own_level() {
    let mut c = Config::for_this_box("/tmp");
    let subjects: Vec<String> = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .map(|d| d.subject)
        .collect();
    assert!(
        !subjects.iter().any(|s| s == "user models"),
        "a file that is not there discloses nothing: {subjects:?}"
    );

    c.leticode_user = crate::leticode_config::LeticodeConfig {
        main_model: Some("glm-5.3-flash".into()),
        path: Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
        level: crate::leticode_config::Level::User,
        ..Default::default()
    };
    // The merge the session reads: the user level as the base, nothing over it.
    c.leticode = c.leticode_user.clone();
    let line = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .find(|d| d.subject == "user models")
        .expect("the user file speaks, so it is named")
        .to_string();
    assert!(line.contains("/h/.config/letibot/leticode.toml"), "{line}");
    assert!(line.contains("main glm-5.3-flash"), "{line}");
    assert!(
        line.contains("overrides it key by key"),
        "and it says what can overrule it: {line}"
    );
}

/// The settings row says `unlimited`, not `0`. An operator reading a table of
/// numbers reads `0` as "zero rounds allowed", which is the opposite.
#[test]
fn an_unbounded_backstop_reads_as_unlimited_not_as_zero() {
    let c = Config::for_this_box("/tmp");
    let rows = c.settings("x", false, &[]);
    let r = rows
        .iter()
        .find(|r| r.key == "max-tool-rounds")
        .expect("the row");
    assert_eq!(r.value, "unlimited");
    let mut bounded = c.clone();
    bounded.max_tool_rounds = 12;
    assert_eq!(
        bounded
            .settings("x", false, &[])
            .iter()
            .find(|r| r.key == "max-tool-rounds")
            .expect("the row")
            .value,
        "12",
        "and a real bound still reads as its number"
    );
}

/// Off is reachable and is said out loud, because a refusal that can only be
/// routed around teaches people to route around refusals.
#[test]
fn turning_the_progress_check_off_is_a_declared_state() {
    let mut c = Config::for_this_box("/tmp");
    c.stall_rounds = 0;
    // With a backstop still armed. Both off is a DIFFERENT state and has its
    // own disclosure and its own test below - reading them as one is how an
    // operator comes to believe something is still catching runaway turns.
    c.max_tool_rounds = 200;
    let d = c
        .disclosures(&GateWiring::read_only())
        .into_iter()
        .find(|d| d.subject == "progress check")
        .expect("still disclosed when off — especially when off");
    assert!(!d.active);
    let line = d.to_string();
    assert!(line.contains("OFF"), "{line}");
    assert!(
        line.contains("--stall-rounds"),
        "the disclosure carries the way back on: {line}"
    );
}

#[test]
fn the_default_is_no_spill_and_it_says_so() {
    let c = Config::for_this_box("/tmp");
    assert_eq!(c.spill, SpillPolicy::Unset);
    assert!(
        c.disclosures(&GateWiring::read_only())
            .iter()
            .any(|d| d.to_string().contains("spill: UNSET")),
        "a daemon that does not disclose an unset budget is the defect"
    );
}

/// The defect this replaced: the adjudication line asserted "M1 is read-only tools, which never
/// prompt", printed regardless of what the session had seated. What is checked here is that the wording
/// MOVES when the wiring does - a banner that says the same thing about both is not a disclosure.
#[test]
fn the_adjudication_line_reads_the_wiring_rather_than_asserting_it() {
    let c = Config::for_this_box("/tmp");
    let line = |w: &GateWiring| {
        c.disclosures(w)
            .into_iter()
            .find(|d| d.subject == "adjudication")
            .expect("adjudication is always disclosed")
    };

    let ro = line(&GateWiring::read_only());
    assert!(!ro.active);
    assert!(
        ro.detail.contains("read-only"),
        "a read-only session should say so: {}",
        ro.detail
    );

    // Write tools seated, nobody to ask. This is the case the old sentence got
    // exactly backwards: it reported no boundary while the gate was refusing
    // every call.
    let unattended = GateWiring {
        backend_writable: true,
        has_write_tools: true,
        ..GateWiring::read_only()
    };
    let u = line(&unattended);
    assert_ne!(
        u.detail, ro.detail,
        "seating write tools must change the disclosure; it did not"
    );
    assert!(
        u.detail.contains("WRITE TOOLS"),
        "an operator must be able to see write tools are present: {}",
        u.detail
    );
    assert!(!u.active, "no adjudicator is not an active boundary");

    // Fully wired: an admitted call reaches the disk, and that is a third
    // distinct thing to say.
    let wired = GateWiring {
        adjudicator: "console adjudicator".into(),
        backend_writable: true,
        has_write_tools: true,
        denials_surfaced: true,
        ..GateWiring::read_only()
    };
    let w = line(&wired);
    assert!(
        w.active,
        "an attached adjudicator over a writable backend is on"
    );
    assert_ne!(w.detail, u.detail);

    // A fourth thing, and it is the one the denial rule is about. An adjudicator over a writable backend whose
    // denials go nowhere is not a boundary an operator has: they see a task that stopped and never the
    // decision that stopped it. The line must differ from the fully wired one and must not read as on.
    let blind = GateWiring {
        denials_surfaced: false,
        ..wired.clone()
    };
    let b = line(&blind);
    assert!(
        !b.active,
        "an adjudicator whose denials reach nobody is not an active boundary"
    );
    assert_ne!(
        b.detail, w.detail,
        "losing the denial sink must change the disclosure; it did not"
    );
}

/// Nothing widens by default, as a test rather than as a sentence at the top of the file: an
/// invocation that passes no flag gets exactly what it got before. Four fields decide it, and the way
/// this regresses is somebody making one of them "sensible" in isolation.
#[test]
fn the_default_seat_is_what_shipped_before() {
    let c = Config::for_this_box("/tmp");
    assert_eq!(c.seat, Seat::Orchestrator);
    assert!(!c.allow_bash, "`bash` is off even before a role is chosen");
    assert!(
        !c.intent_prose,
        "the prose half of the intent check has unmeasured false positives"
    );
    assert!(
        !c.seat.needs_writable_backend(),
        "the default seat must get HostBackend::new"
    );
    assert!(!c.seat.needs_exec_backend());
}

/// A typo in `--role` names the list rather than falling back to the default: falling back would give
/// an operator who typed `--role codre` a read-only session that looks like the one they asked for
/// and cannot edit anything - found out one turn later.
#[test]
fn an_unknown_role_is_refused_with_the_list() {
    let e = Seat::parse("codre").unwrap_err();
    for known in [
        "orchestrator",
        "planner",
        "researcher",
        "coder",
        "runner",
        "leticode",
    ] {
        assert!(e.contains(known), "{e}");
    }
    assert_eq!(Seat::parse("coder").unwrap(), Seat::Coder);
    assert_eq!(Seat::parse("runner").unwrap(), Seat::Runner);
    assert_eq!(Seat::parse("leticode").unwrap(), Seat::Leticode);
    assert_eq!(Seat::parse("opencode").unwrap(), Seat::Leticode);
}

/// The model adjudicator parses, and refuses by name without an oracle. It does not fail at parse
/// time; the missing piece is one layer
/// down - an endpoint, whose refusal names `--oracle` (`Harness::open_with_registry`).
#[test]
fn the_model_adjudicator_parses_now_that_an_oracle_exists() {
    assert_eq!(
        AdjudicatorChoice::parse("model").expect("model is a real choice"),
        AdjudicatorChoice::Model
    );
}

/// Every seam this strand wired is disclosed, and each one moves with the wiring rather than being a
/// sentence - the defect already paid for four times in one night. Nothing here asserts a particular
/// wording; what it asserts is that the wording is different when the wiring is.
#[test]
fn every_new_seam_is_disclosed_and_the_line_moves_with_it() {
    let c = Config::for_this_box("/tmp");
    let subjects = |w: &GateWiring| -> Vec<String> {
        c.disclosures(w).into_iter().map(|d| d.subject).collect()
    };
    let detail = |w: &GateWiring, s: &str| -> String {
        c.disclosures(w)
            .into_iter()
            .find(|d| d.subject == s)
            .map(|d| d.detail)
            .unwrap_or_default()
    };
    let state = |w: &GateWiring, s: &str| -> String {
        c.disclosures(w)
            .into_iter()
            .find(|d| d.subject == s)
            .map(|d| d.state)
            .unwrap_or_default()
    };

    let ro = GateWiring::read_only();
    for expected in ["role", "backend", "adjudication", "intent check"] {
        assert!(
            subjects(&ro).iter().any(|s| s == expected),
            "{expected} is not disclosed: {:?}",
            subjects(&ro)
        );
    }
    // A read-only seat has nothing to authorise, so the trail line is absent
    // rather than saying "not collected" about a session where it could not
    // matter. Absence with a reason, not silence.
    assert!(!subjects(&ro).iter().any(|s| s == "auth trail"));

    let gated = GateWiring {
        has_write_tools: true,
        backend_writable: true,
        role: "coder".into(),
        seated: vec!["read".into(), "write".into(), "edit".into()],
        backend: "the host filesystem under /tmp, writable".into(),
        ..GateWiring::read_only()
    };
    assert!(subjects(&gated).iter().any(|s| s == "auth trail"));
    assert!(
        detail(&gated, "auth trail").contains("NOT"),
        "an uninstalled trail must not read as installed"
    );
    let with_trail = GateWiring {
        trail_installed: true,
        ..gated.clone()
    };
    assert_ne!(
        detail(&with_trail, "auth trail"),
        detail(&gated, "auth trail")
    );

    // The backend line quotes the backend. Two different backends must not
    // produce one sentence.
    let other = GateWiring {
        backend: "a project-scoped namespace over /tmp".into(),
        ..gated.clone()
    };
    assert_ne!(detail(&other, "backend"), detail(&gated, "backend"));

    // The role line names what was seated, so a role that resolved to fewer
    // tools than its name suggests is visible.
    assert!(detail(&gated, "role").contains("write"));
    assert!(detail(&gated, "role").contains("coder"));

    // The monitor line exists only where a monitor could be declared, and it
    // distinguishes a wake from a poll.
    assert!(!subjects(&gated).iter().any(|s| s == "monitor wake"));
    let watching = GateWiring {
        seated: vec!["monitor".into()],
        has_exec_tools: true,
        ..gated.clone()
    };
    assert!(detail(&watching, "monitor wake").contains("poll"));
    let woken = GateWiring {
        monitor_wake: crate::config::MonitorWake::Armed,
        ..watching.clone()
    };
    assert_ne!(
        detail(&woken, "monitor wake"),
        detail(&watching, "monitor wake")
    );
    // And a wake nobody could arm is its own line. Derived from a bool,
    // *the daemon's waiter would not start* and *no daemon armed one* would be one sentence, and a
    // reader told *POLL ONLY* would go looking for a knob that would not help.
    let failed = GateWiring {
        monitor_wake: crate::config::MonitorWake::Failed,
        ..watching.clone()
    };
    let unarmed = detail(&watching, "monitor wake");
    let tried = detail(&failed, "monitor wake");
    assert_ne!(tried, unarmed);
    assert!(
        tried.contains("could not be started") && tried.contains("monitor_wake_not_armed"),
        "the failed line names the attempt and the warning that holds the error: {tried}"
    );
    assert!(
        unarmed.contains("No daemon armed"),
        "and the unarmed line says nobody tried: {unarmed}"
    );
    // And the LINE says it, not only the detail. A banner read at a glance carries the state
    // word; a reader who sees `POLL ONLY` twice cannot tell a knob from a failure without scrolling
    // into prose that both states already had.
    assert_ne!(
        state(&failed, "monitor wake"),
        state(&watching, "monitor wake"),
        "a wake that failed must not read as a session nobody armed"
    );
}

/// Whether a subagent can be placed in a VM is said at start, not found out at spawn -
/// FIRECODE-NOTES item 4: *"Resolve the binary and probe capabilities at daemon start, and
/// publish them in the banner."* Only where a subagent can be started at all.
#[test]
fn the_banner_says_whether_a_subagent_can_be_placed_in_a_vm() {
    let c = Config::for_this_box("/tmp");
    let subjects = |w: &GateWiring| -> Vec<String> {
        c.disclosures(w).into_iter().map(|d| d.subject).collect()
    };
    let detail = |w: &GateWiring, s: &str| -> String {
        c.disclosures(w)
            .into_iter()
            .find(|d| d.subject == s)
            .map(|d| d.detail)
            .unwrap_or_default()
    };
    let base = GateWiring {
        seated: vec!["task".into()],
        ..GateWiring::read_only()
    };
    let found = GateWiring {
        firecode: Some(letibot_tools::firecode::Installed {
            path: "/opt/homebrew/bin/firecode".into(),
            inherit: false,
        }),
        ..base.clone()
    };
    let line = detail(&found, "firecode");
    assert!(line.contains("/opt/homebrew/bin/firecode"), "{line}");
    assert!(line.contains("no layer inheritance"), "{line}");
    let missing = detail(&base, "firecode");
    assert!(missing.contains("refused"), "{missing}");
    assert!(missing.contains("FIRECODE_BIN"), "{missing}");
    // And no line at all where no subagent can be started.
    assert!(
        !subjects(&GateWiring::read_only())
            .iter()
            .any(|s| s == "firecode")
    );
}

#[test]
fn every_thing_that_is_off_is_disclosed() {
    let c = Config::for_this_box("/tmp");
    let all = c
        .disclosures(&GateWiring::read_only())
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in ["spill", "store", "retrieval", "adjudication"] {
        assert!(
            all.contains(expected),
            "{expected} is not disclosed:\n{all}"
        );
    }
}

#[test]
fn the_system_prompt_carries_nothing_volatile() {
    // Rule 1, as a test rather than as a habit. `branch` and `commit` are words of the queue rule
    // now (`working`), not values; the values are what this guards - a home path, a date - and the
    // scratch path, the one value the prompt does carry, is masked out of the fingerprint.
    let f = SeatFacts {
        scratch: None,
        ..seat()
    };
    for system in [default_system(&f), default_system(&SeatFacts::default())] {
        for volatile in ["/home/", "20"] {
            assert!(
                !system.contains(volatile),
                "the stable prefix contains {volatile:?}, which will change and re-prefill"
            );
        }
        assert!(
            system.contains("Answer in English"),
            "§5.2: state the language"
        );
    }
}

/// The daemon's own logs are not in the workspace : a child's workspace is its
/// worktree, and `.letibot/*.jsonl` there made every finished child's tree look dirty. A fixture with
/// no logs root keeps the old place, so no test writes into the box's real data directory.
#[test]
fn the_daemons_logs_go_under_the_logs_root_one_directory_per_session() {
    let mut cfg = Config::for_this_box("/tmp/ws");
    cfg.session_id = "s-7".into();
    assert_eq!(cfg.daemon_logs(), PathBuf::from("/tmp/ws/.letibot"));
    cfg.logs_root = Some(PathBuf::from("/data/letibot/logs"));
    assert_eq!(cfg.daemon_logs(), PathBuf::from("/data/letibot/logs/s-7"));
    // Another session's is its own directory, which is what keeps a closed session's room notes
    // from landing in the daemon's file or in a sibling's.
    assert_eq!(cfg.logs_for("s-9"), PathBuf::from("/data/letibot/logs/s-9"));
    cfg.logs_root = None;
    assert_eq!(cfg.logs_for("s-9"), PathBuf::from("/tmp/ws/.letibot/s-9"));
}

/// Only the session `--vm` placed mounts its workspace - not a child attached to an ancestor's
/// VM (that is the ancestor's mount), not one whose caller named a tree to copy (a reviewer), and
/// not with `--vm-copy`. On a Mac; elsewhere nothing mounts.
#[test]
fn only_the_vm_placed_session_itself_mounts_its_workspace() {
    let mut cfg = Config::for_this_box("/tmp/the-org");
    cfg.placement = letibot_tools::builtins::task::Placement::Firecode;
    cfg.vm_mount = true;
    assert_eq!(
        cfg.vm_mounts_workspace(),
        cfg!(target_os = "macos"),
        "--vm on a Mac"
    );
    let mut attached = cfg.clone();
    attached.vm_attach = Some(PathBuf::from("/tmp/the-org"));
    assert!(
        !attached.vm_mounts_workspace(),
        "an attached child uses its ancestor's VM"
    );
    let mut named = cfg.clone();
    named.vm_source = Some(PathBuf::from("/tmp/the-org/repo"));
    assert!(
        !named.vm_mounts_workspace(),
        "a caller that named a tree gets a copy of it"
    );
    let mut copied = cfg.clone();
    copied.vm_mount = false;
    assert!(!copied.vm_mounts_workspace(), "--vm-copy");
}

/// The notes checkout: a fixture has none, the daemon's default is under the data directory, the
/// user's `[notes] repo` moves it, `$LETIBOT_NOTES_REPO` beats the key, and a project's directory
/// is a keyed subdirectory of it, never a path in the tree.
#[test]
fn the_notes_repo_is_outside_the_tree_and_configurable() {
    let base = std::env::temp_dir().join(format!("config-notes-{}", std::process::id()));
    let ws = base.join("proj");
    std::fs::create_dir_all(&ws).expect("a scratch dir");
    let mut cfg = Config::for_this_box(&ws);
    assert_eq!(cfg.notes_repo(), None, "a fixture has no notes checkout");
    assert_eq!(cfg.project_notes(), PathBuf::new());

    cfg.notes_home = Some(base.join("data/notes"));
    assert_eq!(cfg.notes_repo(), Some(base.join("data/notes")));
    let project = cfg.project_notes();
    assert!(project.starts_with(base.join("data/notes")), "{project:?}");
    assert!(!project.starts_with(&ws), "{project:?}");
    assert!(
        project
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("proj-")),
        "{project:?}"
    );

    cfg.box_dirs.home = base.join("home");
    cfg.leticode.notes.repo = Some("~/my-notes".into());
    cfg.leticode.notes.remote = Some("https://example.invalid/notes.git".into());
    assert_eq!(cfg.notes_repo(), Some(base.join("home/my-notes")));
    assert_eq!(
        cfg.notes_remote().as_deref(),
        Some("https://example.invalid/notes.git")
    );
    cfg.notes_home_env = true;
    assert_eq!(cfg.notes_repo(), Some(base.join("data/notes")));
    let _ = std::fs::remove_dir_all(&base);
}

/// A folder project's repositories are one `listen.NAME` row each: the config-muted one is checked
/// and has no verb, a `/listen` one is checked with one, the rest are unchecked.
#[test]
fn a_folder_projects_repositories_are_listen_rows() {
    let dir = std::env::temp_dir().join(format!("listen-rows-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for n in ["a", "b", "c"] {
        let p = dir.join(n);
        std::fs::create_dir_all(&p).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "remote",
                "add",
                "origin",
                &format!("https://github.com/o/{n}.git"),
            ],
        ] {
            let ok = std::process::Command::new("git")
                .args(&args)
                .current_dir(&p)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
    }
    let file = dir.join("leticode.toml");
    std::fs::write(&file, "[queue]\nmute = [\"c\"]\n").unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.workspace = dir.clone();
    cfg.store = Some(dir.join("s.db"));
    cfg.leticode = crate::leticode_config::LeticodeConfig::load(&file).unwrap();
    let store = letibot_tokencore::store::Store::open(&dir.join("s.db")).unwrap();
    store
        .set_queue_mute(&crate::mergequeue::listen::workspace_key(&dir), "b", true)
        .unwrap();
    let rows = cfg.listen_rows();
    let by = |k: &str| rows.iter().find(|r| r.key == k).expect(k);
    assert_eq!(rows.len(), 3);
    assert_eq!(by("listen.a").value, "[ ] listened");
    assert!(!by("listen.a").editable.is_empty());
    assert_eq!(by("listen.b").value, "[x] muted");
    assert!(!by("listen.b").editable.is_empty());
    assert_eq!(by("listen.c").value, "[x] muted");
    assert!(by("listen.c").editable.is_empty(), "locked");
    assert!(by("listen.c").source.contains("leticode.toml"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scratch providers.toml, so a test never reads the operator's.
fn providers_file(tag: &str, body: &str) -> PathBuf {
    let d = letibot_sessionlog::testing::scratch_root()
        .join(format!("providers-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("providers.toml");
    std::fs::write(&f, body).unwrap();
    f
}

/// A door no catalogue carries is on the card, marked, unpriced and with no window: never free
/// and never some default size.
#[test]
fn a_door_with_no_catalogue_is_unpriced_with_its_window_unknown_on_the_model_card() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.providers_file = Some(providers_file(
        "no-catalogue",
        "[provider.myproxy]\nprotocol = \"chat-completions\"\nurl = \"http://127.0.0.1:9/v1/chat/completions\"\n\
         key_env = \"LETIBOT_TEST_NO_SUCH_KEY\"\nmodel = \"proxy-model\"\n",
    ));
    let rows = cfg.settings("", false, &[]);
    let model = rows
        .iter()
        .find(|r| r.key == "model")
        .expect("the model row");
    let facts = rows
        .iter()
        .find(|r| r.key == letibot_sessionlog::protocol::MODEL_FACTS_KEY)
        .expect("the facts row");
    let at = model
        .choices
        .iter()
        .position(|c| c == "myproxy/proxy-model")
        .unwrap_or_else(|| panic!("the config door is on the card: {:?}", model.choices));
    let line = &facts.choices[at];
    assert!(line.contains("unpriced"), "{line}");
    assert!(line.contains("window unknown"), "{line}");
    assert!(line.contains("providers.toml"), "marked as config: {line}");
    let doors = rows
        .iter()
        .find(|r| r.key == letibot_sessionlog::protocol::MODEL_DOORS_KEY)
        .expect("the doors row");
    assert!(
        doors.choices.iter().any(|c| c == "myproxy own:myproxy"),
        "{:?}",
        doors.choices
    );
}

/// `[nudge]` picks like `[fallback]`: the model, then `provider/*`, then `*`; `--intent-prose`
/// is on for every model.
#[test]
fn the_prose_nudge_is_picked_per_model_and_the_flag_overrides_it() {
    let mut c = Config::for_this_box("/tmp");
    assert_eq!(c.prose_nudge("acme/big"), (false, "default".into()));
    c.nudge = [
        ("acme/*".to_string(), true),
        ("acme/small".to_string(), false),
        ("*".to_string(), false),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        c.prose_nudge("acme/big"),
        (true, "[nudge] \"acme/*\"".into())
    );
    assert_eq!(
        c.prose_nudge("acme/small"),
        (false, "[nudge] \"acme/small\"".into())
    );
    assert_eq!(c.prose_nudge("other/x"), (false, "[nudge] \"*\"".into()));
    c.intent_prose = true;
    assert_eq!(c.prose_nudge("acme/small"), (true, "--intent-prose".into()));
}
