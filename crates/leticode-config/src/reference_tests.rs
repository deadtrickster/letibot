//! `leticode.toml` against `letibot-config-ref`: every listed key loads, and every key a reader
//! here accepts is listed.

use std::collections::BTreeSet;

use letibot_config_ref::{files::LETICODE, probe};

use super::*;

fn load_text(text: &str) -> Result<LeticodeConfig, String> {
    let dir = std::env::temp_dir().join(format!(
        "letibot-lcref-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("leticode.toml");
    std::fs::write(&path, text).unwrap();
    let out = LeticodeConfig::load_as(&path, Level::User);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn table_keys(path: &str) -> BTreeSet<&'static str> {
    probe::keys_of(
        LETICODE
            .table(path)
            .unwrap_or_else(|| panic!("[{path}] not listed")),
    )
}

fn fields<'de, T: serde::Deserialize<'de>>() -> BTreeSet<&'static str> {
    probe::serde_fields::<T>()
        .expect("a plain struct")
        .iter()
        .copied()
        .collect()
}

#[test]
fn every_listed_key_loads() {
    let paths: Vec<&str> = LETICODE.tables.iter().map(|t| t.path).collect();
    let refused = probe::refused_examples(&LETICODE, &paths, |_, doc| load_text(doc).map(|_| ()));
    assert!(refused.is_empty(), "{refused:#?}");
}

/// The file's top level is exactly the listed top-level keys plus the listed tables' roots.
#[test]
fn the_top_level_is_listed() {
    let mut listed = table_keys("");
    for t in LETICODE.tables.iter().filter(|t| !t.path.is_empty()) {
        listed.insert(t.path.split('.').next().unwrap());
    }
    assert_eq!(fields::<LeticodeConfigFile>(), listed);
}

#[test]
fn serde_tables_are_listed() {
    assert_eq!(fields::<ExecTable>(), table_keys("exec"));
    assert_eq!(fields::<NotesTable>(), table_keys("notes"));
    assert_eq!(fields::<BrowserTable>(), table_keys("browser"));
    assert_eq!(fields::<GithubTable>(), table_keys("github"));
    assert_eq!(fields::<MemoryTable>(), table_keys("memory"));
    let mut skills = table_keys("skills");
    skills.insert("when");
    assert_eq!(fields::<SkillTable>(), skills);
    assert_eq!(
        fields::<letibot_tools::builtins::skill::SkillWhen>(),
        table_keys("skills.when.<name>")
    );
    let samplers: BTreeSet<&str> = SAMPLER_KEYS.iter().copied().chain(["model"]).collect();
    assert_eq!(samplers, table_keys("roles.<name>"));
}

/// The hand-written readers refuse a key they do not know, so a literal in their source that
/// they accept and the table does not list is a key missing from the reference.
#[test]
fn hand_read_keys_are_listed() {
    let mut src = String::from(include_str!("lib.rs"));
    src.push_str(include_str!("workspace.rs"));
    let candidates = probe::literals(&src);
    let mut unlisted = Vec::new();
    for path in [
        "provider",
        "roles.<name>",
        "boundary",
        "perimeter.<name>",
        "ui",
        "warnings",
        "verbosity.<name>",
        "nag",
        "room",
        "project",
    ] {
        let t = LETICODE.table(path).unwrap();
        unlisted.extend(probe::unlisted_accepted(
            &LETICODE,
            t,
            &candidates,
            |doc| match load_text(doc) {
                Ok(_) => true,
                Err(why) => !probe::refuses_name(&why),
            },
        ));
    }
    assert!(
        unlisted.is_empty(),
        "accepted but not in the reference: {unlisted:#?}"
    );
}
