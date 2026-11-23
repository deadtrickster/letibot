//! What a shipped theme has to be true of: every role answers, a file can override one line of a
//! built-in, `auto` is the terminal's background, and the text roles can be read on the page they
//! are drawn on.

use super::*;
use rano::style::{Hue, Palette};

/// A directory of this test's own. The name is not the process id: every test binary in a seat's
/// own pid namespace is pid 14, so a directory keyed by the pid is another test's.
fn tmp(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("letibot-theme-{name}-{nanos}-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// WCAG relative luminance of an exact colour, so "reads on that background" is a number and not
/// an opinion. `None` for a slot or a cube index: the terminal's own palette is the reader's, and
/// this test cannot know its twelve values.
fn luminance(h: Hue) -> Option<f64> {
    let Hue::Rgb(r, g, b) = h else {
        return None;
    };
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b))
}

/// The ratio the WCAG calls contrast: 1.0 is invisible, 21.0 is black on white.
fn contrast(a: f64, b: f64) -> f64 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// The look a role is drawn with under a theme: the theme's own line, else rano's default table -
/// the two halves of what a reader sees, in the order `Palette::look` consults them.
fn look_of(theme: &Theme, r: Role) -> Look {
    match theme.roles.iter().rev().find(|(role, _)| *role == r) {
        Some((_, look)) => *look,
        None => Palette::Colour.base_look(r),
    }
}

/// Every shipped theme parses, answers for every role (Info included), and the five severities are
/// five different looks - the defect [`crate::ui::severity`] exists to end, checked from the other
/// side: a palette that drew two of them alike would make that table a lie.
#[test]
fn every_shipped_theme_answers_for_every_role() {
    for (name, text) in BUILTIN {
        let (theme, problems) = Theme::parse(name, text);
        assert!(problems.is_empty(), "{name}: {problems:?}");
        let resolved = resolve(Path::new("/nonexistent"), Some(name), &[], None);
        assert!(
            resolved.1.is_empty(),
            "{name}: {name} is shipped and must resolve: {:?}",
            resolved.1
        );
        if name == "terminal" {
            // It names nothing on purpose: the terminal's own palette is the point, so every role
            // is rano's default table and there is no theme to install.
            assert!(theme.roles.is_empty(), "terminal must name no role");
            assert!(resolved.0.is_none(), "terminal installs no theme");
            for r in Role::ALL {
                let l = Palette::Colour.base_look(r);
                assert_eq!(look_of(&theme, r), l, "{name}: {r:?}");
            }
            // Info is a role with a colour of its own, and this palette is the one it is defined in.
            assert_eq!(
                Palette::Colour.base_look(Role::Info).fg,
                Some(Hue::Slot(6)),
                "information is the cyan slot"
            );
            continue;
        }
        for r in Role::ALL {
            assert!(
                theme.roles.iter().any(|(role, _)| *role == r),
                "{name} does not name {r:?}, so it would fall back to the terminal's slot"
            );
            assert_ne!(look_of(&theme, r), Look::PLAIN, "{name}: {r:?} is plain");
        }
        let five = [
            Role::Info,
            Role::Pending,
            Role::Warning,
            Role::Attention,
            Role::Failure,
        ];
        for (i, a) in five.iter().enumerate() {
            for b in &five[i + 1..] {
                assert_ne!(
                    look_of(&theme, *a),
                    look_of(&theme, *b),
                    "{name}: {a:?} and {b:?} draw alike"
                );
            }
        }
    }
}

/// A user file of a built-in's name is the whole of that name's roles, not a patch on the
/// built-in: the file wins, and `/theme` says so on the row rather than in a footnote.
#[test]
fn a_user_file_overrides_a_built_in_of_the_same_name() {
    let dir = tmp("override");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("dark.toml"), "plain = \"fg:#ff0000\"\n").unwrap();

    let (theme, problems) = resolve(&dir, Some("dark"), &[], None);
    let theme = theme.expect("the file's own theme");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        theme.roles.len(),
        1,
        "the file's lines, and not the built-in's"
    );
    assert_eq!(look_of(&theme, Role::Plain).fg, Some(Hue::Rgb(255, 0, 0)));
    // A role the file does not name is rano's table, which is what a partial theme is for.
    assert_eq!(
        look_of(&theme, Role::Info),
        Palette::Colour.base_look(Role::Info)
    );

    // Without the file the same name is this binary's dark palette.
    let (builtin, problems) = resolve(Path::new("/nonexistent"), Some("dark"), &[], None);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(builtin.expect("the built-in").roles.len(), Role::ALL.len());

    // And the listing says where the bytes are: the reader's, in the built-in's place.
    let names: Vec<(String, Source)> = listing(&dir);
    let dark = names.iter().find(|(n, _)| n == "dark").expect("a dark row");
    assert_eq!(dark.1, Source::User, "{names:?}");
    assert_eq!(names[0].0, "auto");
    let fixed: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        &fixed[..4],
        ["auto", "terminal", "dark", "light"],
        "{fixed:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `auto` is not a palette: it is `dark` or `light`, chosen by the one fact the head already has
/// from the terminal (OSC 11), and the terminal's own table when the terminal has not said.
#[test]
fn auto_is_the_terminal_background_and_a_terminal_that_says_nothing_keeps_its_own() {
    assert_eq!(effective(Some("auto"), Some(false)), "dark");
    assert_eq!(effective(Some("auto"), Some(true)), "light");
    assert_eq!(
        effective(Some("auto"), None),
        "terminal",
        "a palette chosen from a fact nobody has is the terminal's own"
    );
    assert_eq!(effective(None, Some(true)), "terminal");
    assert_eq!(
        effective(Some("  dark "), Some(true)),
        "dark",
        "an explicit name wins"
    );

    // Through `resolve`, a light background picks the light file and nothing else.
    let (theme, problems) = resolve(Path::new("/nonexistent"), Some("auto"), &[], Some(true));
    assert!(problems.is_empty(), "{problems:?}");
    let theme = theme.expect("a palette");
    let (light, _) = Theme::parse("light", BUILTIN[2].1);
    assert_eq!(theme.roles, light.roles);
}

/// A name nothing has is said, with the names that exist in the sentence, and installs nothing:
/// a typo must not draw as a deliberate default.
#[test]
fn a_name_nothing_has_is_said_and_the_head_keeps_the_terminal() {
    let dir = tmp("absent");
    let (theme, problems) = resolve(&dir, Some("drak"), &[], None);
    assert!(theme.is_none());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("theme `drak`"), "{problems:?}");
    for name in ["terminal", "dark", "light", "auto"] {
        assert!(problems[0].contains(name), "{}", problems[0]);
    }
}

/// `head.toml`'s own `color.ROLE` lines still land on top of a shipped theme, and a line naming no
/// role is still said - the acceptance suite's sentence, from the built-in side.
#[test]
fn colour_lines_ride_on_top_of_a_shipped_theme() {
    let (theme, problems) = resolve(
        Path::new("/nonexistent"),
        Some("dark"),
        &[
            ("plain".into(), "fg:#00ff00".into()),
            ("wat".into(), "bold".into()),
        ],
        None,
    );
    let theme = theme.expect("dark");
    assert_eq!(problems, vec!["color.wat: `wat` is not a role".to_string()]);
    assert_eq!(
        look_of(&theme, Role::Plain).fg,
        Some(Hue::Rgb(0, 255, 0)),
        "the config's line wins over the theme's"
    );
}

/// A theme a reader can read: every text role's foreground has real contrast with the page it is
/// drawn on, and every band's background is not the page's. `faint` and `comment` are de-emphasised
/// on purpose and held to the 3:1 the WCAG asks of large text; the rest to 4.5:1.
#[test]
fn every_text_role_reads_on_the_page_it_is_drawn_on() {
    // A role that paints a band rather than text: what matters is that the band is visible.
    let bands = [
        Role::Added,
        Role::Removed,
        Role::UserBlock,
        Role::Selection,
        Role::Match,
        Role::Bar,
        Role::Selected,
    ];
    // `emphasis` is an attribute on top of whatever run it marks (the changed span inside a diff
    // line), so it names no colour of its own and has nothing to measure.
    let attribute_only = [Role::Emphasis];
    for (name, text) in BUILTIN.iter().filter(|(n, _)| *n != "terminal") {
        let (theme, problems) = Theme::parse(name, text);
        assert!(problems.is_empty(), "{name}: {problems:?}");
        let page = look_of(&theme, Role::Plain)
            .bg
            .and_then(luminance)
            .unwrap_or_else(|| panic!("{name}: `plain` must carry the page's background"));
        let mut checked = 0;
        for r in Role::ALL {
            if bands.contains(&r) || attribute_only.contains(&r) || r == Role::Plain {
                continue;
            }
            let look = look_of(&theme, r);
            let Some(fg) = look.fg.and_then(luminance) else {
                panic!("{name}: {r:?} names no exact colour, so nothing can read it");
            };
            let ratio = contrast(fg, page);
            let floor = if matches!(r, Role::Faint | Role::Comment) {
                3.0
            } else {
                4.5
            };
            assert!(
                ratio >= floor,
                "{name}: {r:?} is {ratio:.1}:1 on the page ({floor}:1 needed)"
            );
            checked += 1;
        }
        assert!(checked >= 20, "{name}: only {checked} roles were measured");
        for r in bands {
            let look = look_of(&theme, r);
            let band = match r {
                // Reverse video: the terminal's own foreground becomes the band, which is legible
                // on any page by construction.
                Role::Bar | Role::Selected => continue,
                _ => look.bg.and_then(luminance),
            };
            let Some(band) = band else {
                panic!("{name}: {r:?} names no exact band colour");
            };
            assert!(
                contrast(band, page) > 1.2,
                "{name}: {r:?}'s band is the page's own colour"
            );
        }
    }
}
