//! `/listen` in the head: the mute key on a red notice, the config pane's checkboxes, `this`, and
//! completion of repository names.

use super::redcard::with_notice;
use super::*;
use letibot_sessionlog::protocol::SettingRow;

const NOTICE: &str = include_str!("fixtures/red_main_notice.txt");

fn listen_row(name: &str, value: &str, source: &str, editable: bool) -> SettingRow {
    SettingRow {
        key: format!("listen.{name}"),
        value: value.into(),
        source: source.into(),
        editable: if editable {
            format!("/listen -{name} · /listen +{name}")
        } else {
            String::new()
        },
        choices: Vec::new(),
        tools: Vec::new(),
    }
}

fn rows() -> Vec<SettingRow> {
    vec![
        listen_row("a", "[ ] listened", "default", true),
        listen_row("b", "[x] muted", "/listen", true),
        listen_row("c", "[x] muted", "leticode.toml [queue] mute", false),
    ]
}

fn select(a: &mut App, key: &str) {
    let at = a
        .config_rows()
        .iter()
        .position(|r| r.key == key)
        .expect("the row");
    a.config_sel = at;
}

/// `m` in a red notice's popup mutes that notice's repository, asks for the rows again, and the
/// hint bar names the key.
#[test]
fn m_in_a_red_notice_mutes_its_repository() {
    let mut a = with_notice(NOTICE);
    a.screen(120, 30);
    let (y, _) = a.notice_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    assert!(a.diff_popup.is_some());
    let bar = a.screen(120, 30).join("\n");
    assert!(bar.contains("m mutes firecode"), "{bar}");
    let act = a.key(Key::Char('m'));
    assert_eq!(
        act,
        Some(Action::Slash {
            line: "listen -firecode".into()
        })
    );
    assert!(a.diff_popup.is_none());
    assert!(a.queued.contains(&Action::Settings), "the rows are re-read");
}

/// Other letters stay the popup's, and a popup that is no red notice does not mute.
#[test]
fn m_elsewhere_does_nothing() {
    let mut a = with_notice(NOTICE);
    a.screen(120, 30);
    let (y, _) = a.notice_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    assert_eq!(a.key(Key::Char('x')), None);
    assert!(a.diff_popup.is_some());
}

/// The pane lists every repository with its box; Enter flips an unlocked one through `/listen`
/// and re-reads the rows, and a config-muted one is locked and says which file.
#[test]
fn the_config_pane_toggles_repositories_and_locks_a_files_mute() {
    let mut a = app();
    a.settings = rows();
    let shown: Vec<_> = a
        .config_rows()
        .into_iter()
        .filter(|r| r.key.starts_with("listen."))
        .map(|r| (r.key, r.value, r.source))
        .collect();
    assert_eq!(shown.len(), 3, "{shown:?}");
    assert_eq!(shown[0].1, "[ ] listened");
    assert_eq!(shown[1].1, "[x] muted");
    assert_eq!(shown[2].2, "leticode.toml [queue] mute");

    select(&mut a, "listen.a");
    assert_eq!(
        a.config_change(),
        Some(Action::Slash {
            line: "listen -a".into()
        })
    );
    assert!(a.queued.contains(&Action::Settings));
    select(&mut a, "listen.b");
    assert_eq!(
        a.config_change(),
        Some(Action::Slash {
            line: "listen +b".into()
        })
    );
    a.queued.clear();
    select(&mut a, "listen.c");
    assert_eq!(a.config_change(), None, "locked");
    assert!(a.queued.is_empty());
}

/// `this` is the repository the header names.
#[test]
fn this_is_the_header_repository() {
    let mut a = app();
    a.git_dir = Some(std::path::PathBuf::from("/w/org/stroppy-rust"));
    assert_eq!(
        a.listen_line_with_this("listen only this"),
        "listen only stroppy-rust"
    );
    assert_eq!(
        a.listen_line_with_this("listen -this2 +x"),
        "listen -this2 +x"
    );
}

fn tab(a: &mut App, line: &str) -> String {
    a.set_composer(line);
    a.key(Key::Tab);
    a.input().to_string()
}

/// Tab after `/listen +`, `-` and `only` completes the workspace's repositories; a bare word
/// completes `only`, `all`, `this`.
#[test]
fn tab_completes_listen_words() {
    let mut a = app();
    a.settings = vec![
        listen_row("stroppy", "[ ] listened", "default", true),
        listen_row("stroppy-rust", "[ ] listened", "default", true),
        listen_row("cloud", "[ ] listened", "default", true),
    ];
    assert_eq!(tab(&mut a, "/listen -cl"), "/listen -cloud");
    assert_eq!(tab(&mut a, "/listen +stroppy-r"), "/listen +stroppy-rust");
    assert_eq!(tab(&mut a, "/listen -str"), "/listen -stroppy");
    assert_eq!(tab(&mut a, "/listen on"), "/listen only");
    assert_eq!(tab(&mut a, "/listen only clo"), "/listen only cloud");
    assert_eq!(tab(&mut a, "/listen al"), "/listen all");
    assert_eq!(tab(&mut a, "/listen -zzz"), "/listen -zzz");
}
