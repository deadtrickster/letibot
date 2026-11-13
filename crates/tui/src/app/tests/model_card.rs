//! The model card: two levels, a search field of its own, and one line per model with the window
//! and the two rates in columns.

use super::*;

/// The card as this daemon sends it: the choices with a facts line each, which providers hold a key,
/// which need none, the doors' catalogues, and the models the store says were used last. One fixture
/// for every test, so no test reads a row from a frame another test spelled differently.
struct Wire {
    now: &'static str,
    models: Vec<(&'static str, &'static str)>,
    keyed: Vec<&'static str>,
    keyless: Vec<&'static str>,
    doors: Vec<(&'static str, &'static str)>,
    recent: Vec<&'static str>,
}

impl Default for Wire {
    fn default() -> Wire {
        Wire {
            now: "deepseek/deepseek-flash",
            models: vec![
                ("local", "this daemon's own server — no key, no meter"),
                (
                    "deepseek/deepseek-flash",
                    "128k window · $0.15 in / $0.6 out per M",
                ),
                (
                    "deepseek/deepseek-v4-pro",
                    "1M window · $0.6 in / $2.2 out per M",
                ),
                ("glm/glm-5.3", "200k window · $0.4 in / $1.6 out per M"),
                (
                    "gemini/gemini-4-hydrogen",
                    "1M window · $1 in / $4 out per M",
                ),
            ],
            keyed: vec!["deepseek", "glm"],
            keyless: vec!["local"],
            doors: vec![
                ("deepseek", "deepseek"),
                ("glm", "zhipuai"),
                ("gemini", "google"),
            ],
            recent: Vec::new(),
        }
    }
}

impl Wire {
    fn frame(&self) -> ServerFrame {
        let row = |key: &str, value: &str, choices: Vec<String>| {
            letibot_sessionlog::protocol::SettingRow {
                key: key.into(),
                value: value.into(),
                source: String::new(),
                editable: String::new(),
                choices,
                tools: Vec::new(),
            }
        };
        ServerFrame::Settings {
            rows: vec![
                row(
                    "model",
                    self.now,
                    self.models.iter().map(|(n, _)| (*n).to_string()).collect(),
                ),
                row(
                    letibot_sessionlog::protocol::MODEL_FACTS_KEY,
                    "the vendored snapshot",
                    self.models.iter().map(|(_, f)| (*f).to_string()).collect(),
                ),
                row(
                    letibot_sessionlog::protocol::MODEL_DOORS_KEY,
                    "one door per preset",
                    self.doors.iter().map(|(d, c)| format!("{d} {c}")).collect(),
                ),
                row(
                    letibot_sessionlog::protocol::MODEL_KEYS_KEY,
                    &self.keyed.join(","),
                    Vec::new(),
                ),
                row(
                    letibot_sessionlog::protocol::MODEL_KEYLESS_KEY,
                    &self.keyless.join(","),
                    Vec::new(),
                ),
                row(
                    letibot_sessionlog::protocol::MODEL_RECENT_KEY,
                    &self.recent.join(","),
                    Vec::new(),
                ),
            ],
        }
    }
}

/// The card open on a daemon that sent `wire`.
fn card(wire: &Wire) -> App {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(wire.frame());
    assert_eq!(a.command("models"), Some(Action::Settings));
    assert!(a.pick == Some(Pick::Model), "the card did not open");
    a
}

/// The card's own rows. `App::screen` mixes them with the head's header and the conversation, and
/// what these tests are about is the card.
fn rows(a: &App, w: usize) -> Vec<String> {
    a.model_card_layout(w).0
}

/// The index of the first row holding `needle`.
fn row_of(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|l| l.contains(needle))
}

/// The provider rows in the order the cursor walks them: the keys, not the labels, so a provider
/// spelled by its catalogue is comparable.
fn provider_order(a: &App) -> Vec<String> {
    a.model_rows()
        .1
        .iter()
        .filter_map(|t| match t {
            Take::Provider { key, .. } => Some(key.clone()),
            _ => None,
        })
        .collect()
}

/// The cursor on the row that names `name`: the tests count rows the way a reader arrows to them.
fn cursor_on(a: &mut App, name: &str) {
    let (_, takes) = a.model_rows();
    a.mode_sel = takes
        .iter()
        .position(|t| t.name() == name)
        .unwrap_or_else(|| panic!("no row for {name}: {:?}", takes));
}

/// `/models` opens on what answers now, then what was used, then a row per provider carrying the
/// facts a choice between providers is made on - and the query field above all of it.
#[test]
fn the_card_opens_on_the_model_answering_now_and_a_row_per_provider() {
    let mut wire = Wire::default();
    wire.recent = vec!["glm/glm-5.3"];
    let a = card(&wire);
    let rows = rows(&a, 110);
    let all = rows.join("\n");
    // The search field is the card's own first line, and says which level it is on.
    assert!(rows[0].contains('⌕'), "no search field: {all}");
    assert!(rows[0].contains("4 providers"), "{all}");
    let now = row_of(&rows, "deepseek/deepseek-flash").expect("the model in force");
    assert!(
        rows[now].contains('●'),
        "the current model is not marked: {all}"
    );
    let recent = row_of(&rows, "glm/glm-5.3").expect("the recent group");
    assert!(
        recent > now,
        "the recent rows do not come after the one in force: {all}"
    );
    for provider in ["local", "deepseek", "glm", "gemini"] {
        assert!(
            all.contains(provider),
            "no {provider} row on the card: {all}"
        );
    }
    // A provider this box can take comes before one it cannot, and the daemon's own server leads.
    assert_eq!(
        provider_order(&a),
        vec!["local", "deepseek", "zhipuai", "google"],
        "{all}"
    );
    // Every provider row is under the heading, not above it.
    let heading = row_of(&rows, "providers").expect("the providers heading");
    assert!(
        row_of(&rows, "no key needed").unwrap() > heading,
        "a provider row is above its heading: {all}"
    );
    // The four facts of a provider row.
    let deepseek = &rows[row_of(&rows, "keyed").expect("the deepseek row")];
    assert!(deepseek.contains("deepseek"), "{deepseek}");
    assert!(deepseek.contains("2 models"), "{deepseek}");
    assert!(
        deepseek.contains("$0.15–$0.6 /M"),
        "no price range: {deepseek}"
    );
    let gemini = &rows[row_of(&rows, "not connected").expect("the gemini row")];
    assert!(gemini.contains("gemini"), "{gemini}");
    assert!(gemini.contains("1 model"), "{gemini}");
    // The daemon's own server has no catalogue row, so its price column says what it is.
    let local = &rows[row_of(&rows, "no key needed").expect("the local row")];
    assert!(local.contains("no meter"), "{local}");
}

/// The done-test: whatever the number of models behind them, the card without a query is one screen
/// - the providers are rows, and a provider's models are drawn only once it is opened.
#[test]
fn the_card_without_a_query_fits_one_screen_whatever_the_model_count() {
    let mut wire = Wire::default();
    wire.models = vec![("local", "this daemon's own server — no key, no meter")];
    for provider in ["deepseek", "glm"] {
        for i in 0..18 {
            let name: &'static str =
                Box::leak(format!("{provider}/{provider}-model-{i}").into_boxed_str());
            wire.models
                .push((name, "1M window · $0.15 in / $0.6 out per M"));
        }
    }
    for provider in [
        "gemini",
        "openai",
        "anthropic",
        "kimi-code",
        "grok",
        "kimi-coding",
        "glm-coding-cn",
    ] {
        let name: &'static str = Box::leak(format!("{provider}/default-1").into_boxed_str());
        wire.models.push((name, "1M window · $1 in / $4 out per M"));
    }
    let mut a = card(&wire);
    let screen = a.screen(150, 45);
    let all = screen.join("\n");
    assert!(
        !all.contains("out of view"),
        "the card needed more than a screen: {all}"
    );
    let card_rows = rows(&a, 150);
    for provider in ["deepseek", "glm", "gemini", "openai", "glm-coding-cn"] {
        assert!(
            card_rows
                .iter()
                .any(|l| l.split_whitespace().next().is_some_and(|w| w == provider)),
            "no {provider} row: {}",
            card_rows.join("\n")
        );
    }
    // Nothing behind a provider is drawn until it is opened: 36 models are two rows here.
    assert!(
        !all.contains("deepseek-model-7"),
        "the models are drawn without opening a provider: {all}"
    );
    assert!(
        all.contains("18 models"),
        "no count on a provider row: {all}"
    );
}

/// Enter on a provider opens its models, one line each with the window and the two rates in
/// columns; Enter on one of them switches the session - an `Action::Slash`, which is a verb the
/// composer never sees.
#[test]
fn a_provider_opens_onto_its_models_and_enter_switches_to_one() {
    let mut a = card(&Wire::default());
    cursor_on(&mut a, "deepseek");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "opening a provider is not a switch"
    );
    assert_eq!(a.model_open.as_deref(), Some("deepseek"));
    let opened = rows(&a, 120);
    let all = opened.join("\n");
    assert!(
        all.contains("deepseek-flash"),
        "the provider did not open: {all}"
    );
    assert!(all.contains("window"), "no column labels: {all}");
    assert!(all.contains("in /M"), "no rate labels: {all}");
    assert!(
        all.contains("$0.15"),
        "the rates are not on the rows: {all}"
    );
    assert!(
        !all.contains("glm-5.3"),
        "another provider's models are drawn too: {all}"
    );
    assert!(
        opened[row_of(&opened, "deepseek-flash").expect("the row")].contains('●'),
        "the model in force is not marked: {all}"
    );
    // The rows are the bare model names: the provider is the heading.
    assert!(
        !all.contains("deepseek/deepseek-flash"),
        "the switch name is drawn under a heading that already names the provider: {all}"
    );
    // Enter on a model that is not the one in force switches to it.
    cursor_on(&mut a, "deepseek/deepseek-v4-pro");
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => assert_eq!(line, "models deepseek/deepseek-v4-pro"),
        other => panic!("a model row left as {other:?}"),
    }
    assert!(a.pick.is_none(), "taking a model closes the card");
    assert!(
        a.pending_prompts.is_empty(),
        "the switch reached the transcript as a message: {:?}",
        a.pending_prompts
    );
    assert!(
        a.editor.text().is_empty(),
        "the switch went through the composer: {:?}",
        a.editor.text()
    );
}

/// A provider this box holds no key for does not open: taking it asks for the key, which is the one
/// thing that would make its models reachable.
#[test]
fn an_unkeyed_provider_asks_for_its_key_instead_of_opening() {
    let mut a = card(&Wire::default());
    cursor_on(&mut a, "gemini");
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.model_open.is_none(), "an unkeyed provider opened");
    let ask = a.key_ask.clone().expect("no key card");
    assert_eq!(ask.provider, "gemini");
    assert_eq!(ask.choice, "gemini/gemini-4-hydrogen");
    assert!(
        a.notice.clone().unwrap_or_default().contains("no key held"),
        "{:?}",
        a.notice
    );
}

/// Esc and left step back one level at a time - the query, then the provider, then the card - so a
/// reader who typed something never loses the card on the first press.
#[test]
fn esc_steps_back_a_level_at_a_time_and_clearing_the_query_keeps_the_level() {
    let mut a = card(&Wire::default());
    cursor_on(&mut a, "deepseek");
    a.key(Key::Enter);
    assert_eq!(a.model_open.as_deref(), Some("deepseek"));
    typed(&mut a, "flash");
    assert_eq!(a.model_query, "flash");
    assert!(!a.model_rows().0.is_empty());
    a.key(Key::Esc);
    assert_eq!(a.model_query, "", "esc did not clear the query first");
    assert_eq!(
        a.model_open.as_deref(),
        Some("deepseek"),
        "the level was lost"
    );
    // Left is the same step.
    a.key(Key::Left);
    assert_eq!(a.model_open, None, "left did not go back");
    assert!(
        a.pick == Some(Pick::Model),
        "the card closed from level one"
    );
    a.key(Key::Esc);
    assert!(a.pick.is_none(), "esc at the top did not close the card");
}

/// Typing at either level is one ranked list across every provider, each row carrying its provider;
/// a model name is full of digits, so a digit narrows and never marks a row.
#[test]
fn a_query_is_one_ranked_list_across_every_provider() {
    let mut a = card(&Wire::default());
    typed(&mut a, "pro");
    let rows = rows(&a, 120);
    let all = rows.join("\n");
    assert!(all.contains("deepseek/deepseek-v4-pro"), "{all}");
    assert!(
        row_of(&rows, "deepseek/deepseek-v4-pro").unwrap() > 0,
        "the hit replaced the search field: {all}"
    );
    assert!(
        !all.contains("providers"),
        "the levels are not replaced by the list: {all}"
    );
    // A digit is a query character: it narrows, and Enter takes the row it leaves.
    for _ in 0..3 {
        a.key(Key::Backspace);
    }
    typed(&mut a, "5.3");
    assert!(a.pick == Some(Pick::Model), "a digit answered the card");
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => assert_eq!(line, "models glm/glm-5.3"),
        other => panic!("the digit took {other:?}"),
    }
}

/// The ranking table: exact before prefix, prefix before a word's start, a word's start before
/// anywhere, anywhere before a scattering.
#[test]
fn the_ranking_is_a_table_from_exact_to_a_scattering() {
    let names = [
        "deepseek/deepseek-v4-pro",
        "gemini/lyria-3-pro-preview",
        "glm/glm-approach",
        "glm/pro",
        "glm/prodigy",
    ];
    let mut tiers: Vec<(u8, &str)> = names
        .iter()
        .filter_map(|n| rank(n, "pro").map(|t| (t, *n)))
        .collect();
    tiers.sort();
    // `glm/pro` is the model part exactly, `prodigy` starts with it, `v4-pro` and
    // `lyria-3-pro-preview` each start a word, and `approach` merely contains it. Nothing here
    // needs the scattering.
    assert_eq!(tiers[0], (0, "glm/pro"), "{tiers:?}");
    assert_eq!(tiers[1], (1, "glm/prodigy"), "{tiers:?}");
    assert_eq!(tiers.last().map(|t| t.0), Some(3), "{tiers:?}");
    assert!(
        !tiers.iter().any(|t| t.0 == crate::app::SUBSEQUENCE),
        "{tiers:?}"
    );
    // `flash` starts a word; `dflash` is only its letters in order.
    assert_eq!(rank("deepseek/deepseek-flash", "flash"), Some(2));
    assert_eq!(rank("deepseek/deepseek-flash", "dflash"), Some(4));
    // A name that begins with what was typed beats one that only contains it.
    assert!(rank("deepseek/deepseek", "deep") < rank("glm/glm-xdeep", "deep"));
    // Nothing that is not there at all.
    assert_eq!(rank("glm/glm-5.3", "zzz"), None);
}

/// The done-tests as behaviour: `pro` puts `deepseek-v4-pro` first, and `flash` shows only the rows
/// whose name contains it - a scattering is a fallback and never a companion.
#[test]
fn pro_comes_first_and_flash_shows_only_what_contains_it() {
    let mut wire = Wire::default();
    wire.models = vec![
        (
            "gemini/lyria-3-pro-preview",
            "1M window · $1 in / $4 out per M",
        ),
        (
            "deepseek-anthropic/deepseek-flash",
            "128k window · $0.15 in / $0.6 out per M",
        ),
        (
            "deepseek/deepseek-v4-pro",
            "1M window · $0.6 in / $2.2 out per M",
        ),
        (
            "deepseek/deepseek-flash",
            "128k window · $0.15 in / $0.6 out per M",
        ),
        ("glm/glm-5.3", "200k window · $0.4 in / $1.6 out per M"),
    ];
    wire.doors = vec![
        ("deepseek", "deepseek"),
        ("deepseek-anthropic", "deepseek"),
        ("gemini", "google"),
        ("glm", "zhipuai"),
    ];
    wire.now = "deepseek/deepseek-flash";
    let mut a = card(&wire);
    typed(&mut a, "pro");
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => {
            assert_eq!(line, "models deepseek/deepseek-v4-pro", "the top hit")
        }
        other => panic!("`pro` took {other:?}"),
    }

    let mut a = card(&wire);
    typed(&mut a, "flash");
    let (_, takes) = a.model_rows();
    let shown: Vec<String> = takes.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(
        shown,
        vec!["deepseek/deepseek-flash".to_string()],
        "`flash` shows more than the names that contain it"
    );
    assert!(shown[0].contains("flash"));
}

/// A model the daemon offers behind two doors is one row: both levels count models, and the door is
/// chosen inside the provider that serves them.
#[test]
fn a_model_behind_two_doors_is_one_row_and_tab_chooses_the_door() {
    let mut wire = Wire::default();
    wire.models = vec![
        (
            "deepseek/deepseek-flash",
            "128k window · $0.15 in / $0.6 out per M",
        ),
        (
            "deepseek-anthropic/deepseek-flash",
            "128k window · $0.15 in / $0.6 out per M",
        ),
        (
            "deepseek/deepseek-v4-pro",
            "1M window · $0.6 in / $2.2 out per M",
        ),
        (
            "deepseek-anthropic/deepseek-v4-pro",
            "1M window · $0.6 in / $2.2 out per M",
        ),
    ];
    wire.doors = vec![("deepseek", "deepseek"), ("deepseek-anthropic", "deepseek")];
    wire.now = "deepseek/deepseek-flash";
    let mut a = card(&wire);
    let providers = a.model_card().providers;
    assert_eq!(providers.len(), 1, "{providers:?}");
    assert_eq!(providers[0].models.len(), 2, "{:?}", providers[0].models);
    assert_eq!(providers[0].doors, vec!["deepseek", "deepseek-anthropic"]);
    let all = rows(&a, 120).join("\n");
    assert!(all.contains("2 models"), "{all}");
    // Opening it names both doors and the key that moves between them.
    cursor_on(&mut a, "deepseek");
    a.key(Key::Enter);
    let all = rows(&a, 120).join("\n");
    assert!(all.contains("2 doors"), "{all}");
    assert!(all.contains("tab"), "the key is not named: {all}");
    a.key(Key::Tab);
    let (_, takes) = a.model_rows();
    let named: Vec<&str> = takes.iter().map(|t| t.name()).collect();
    assert_eq!(
        named,
        vec![
            "deepseek-anthropic/deepseek-flash",
            "deepseek-anthropic/deepseek-v4-pro"
        ],
        "{named:?}"
    );
}

/// A daemon that sent no door row is drawn by the names it did send: one provider per prefix, so an
/// older daemon loses the folding and nothing else.
#[test]
fn without_the_doors_row_every_prefix_is_its_own_provider() {
    let mut wire = Wire::default();
    wire.doors = Vec::new();
    let a = card(&wire);
    let providers = a.model_card().providers;
    let labels: Vec<&str> = providers.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["local", "deepseek", "glm", "gemini"],
        "{labels:?}"
    );
}

/// At most five models are named as recent, the model in force is not one of them (it has a row of
/// its own above), and a model the daemon no longer offers is not named at all.
#[test]
fn the_recent_group_holds_five_and_never_the_model_in_force() {
    let mut wire = Wire::default();
    wire.recent = vec![
        "deepseek/deepseek-flash",
        "glm/glm-5.3",
        "gemini/gemini-4-hydrogen",
        "local",
        "deepseek/deepseek-v4-pro",
        "gemini/gemini-4-hydrogen",
    ];
    let a = card(&wire);
    let first = a.model_card();
    assert_eq!(first.recent.len(), 4, "{:?}", first.recent);
    assert!(
        !first.recent.iter().any(|r| r == "deepseek/deepseek-flash"),
        "the model in force is in the recent group: {:?}",
        first.recent
    );

    // A daemon that names a model the catalogue has dropped leaves the group short rather than
    // drawing a switch it would refuse.
    wire.recent = vec!["gone/retired"];
    let b = card(&wire);
    assert!(
        b.model_card().recent.is_empty(),
        "{:?}",
        b.model_card().recent
    );
}

/// One line per model, with the window and the two rates as columns that line up, and the labels
/// that say which column is which.
#[test]
fn a_models_window_and_rates_are_columns_that_line_up() {
    let mut a = card(&Wire::default());
    a.model_open = Some("deepseek".into());
    let rows = rows(&a, 120);
    let all = rows.join("\n");
    let at = |needle: &str| {
        rows.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle}: {all}"))
    };
    let flash = &rows[at("deepseek-flash")];
    let pro = &rows[at("deepseek-v4-pro")];
    assert!(flash.contains("128k"), "{flash}");
    assert!(pro.contains("1M"), "{pro}");
    // Both windows END in one column, and so do both input rates: that is what makes two rows
    // comparable without reading them.
    let end_of = |line: &str, needle: &str| {
        visible_width(&line[..line.find(needle).expect("the needle") + needle.len()])
    };
    assert_eq!(
        end_of(flash, "128k"),
        end_of(pro, "1M"),
        "the windows do not line up:\n{flash}\n{pro}"
    );
    assert_eq!(
        end_of(flash, "$0.15"),
        end_of(pro, "$0.6"),
        "the input rates do not line up:\n{flash}\n{pro}"
    );
    assert!(
        at("in /M") < at("deepseek-flash"),
        "the labels are not above the rows: {all}"
    );
}

/// The characters that matched are tinted, which is how a reader sees why a row is in the list.
#[test]
fn the_matched_characters_are_highlighted() {
    let mut a = card(&Wire::default());
    a.cfg.color = true;
    typed(&mut a, "pro");
    let all = rows(&a, 120).join("\n");
    assert!(
        all.contains("\u{1b}[30;43mpro"),
        "the match is not tinted: {all:?}"
    );
}
