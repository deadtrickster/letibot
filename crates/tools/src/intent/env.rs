//! The environment a session's shell really holds, as far as anything observed it.
//! `bash` opens no shell, so the only true sources are the pairs the harness spawns every
//! command with ([`SessionEnv::seeded`]) and a shell's own trailer ([`SessionEnv::observe`]).

use std::collections::BTreeMap;

use crate::exec::shell::{ENV_BYTES, ENV_PAIRS};

/// The longest single value this will carry, so one name cannot use the whole budget.
/// A quarter of [`ENV_BYTES`], because a Mac's `PATH` runs past a kilobyte and a `PATH`
/// dropped for its length leaves that machine unable to resolve anything.
pub const ENV_VALUE: usize = 4096;

/// Where one name's value came from, carried per name because the card has to say it:
/// [`From::Harness`] is a value every command of this session is spawned with, and
/// [`From::Shell`] is one the shell said it held after a line. Never inferred from text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// One of the pairs the daemon gives every spawn: `HOME`, the pinned `PATH`,
    /// `Config::build_env` (`CARGO_TARGET_DIR`), `LETIBOT_SOCKET`, and the rest.
    Harness,
    /// The session's shell, in a trailer after a line.
    Shell,
}

impl From {
    /// How the card says it: a sentence rather than a word, because where a value came
    /// from is the whole question a person asks about a reading they did not write.
    pub fn as_phrase(self) -> &'static str {
        match self {
            From::Harness => "from the pairs every command of this session is spawned with",
            From::Shell => "from the environment this session's shell reported",
        }
    }
}

/// One name, one value, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub value: String,
    pub from: From,
}

/// The environment one session's shell holds, as far as anything observed it.
///
/// `Default` is empty, which is exactly what a one-shot `bash` gets.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SessionEnv {
    values: BTreeMap<String, Entry>,
    /// Set when the shell this environment came from is gone; nothing new resolves from it.
    /// The map is not emptied: the values are still the last true thing anybody said, and a
    /// card that forgot them would ask about the same command for ever.
    dead: bool,
}

impl std::fmt::Debug for SessionEnv {
    /// Names and sources, never values: a `Debug` that printed a credential would put it in
    /// every log line that formats a `Surroundings`, the disclosure the card masks against.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("SessionEnv");
        d.field("names", &self.values.keys().collect::<Vec<_>>());
        d.field("dead", &self.dead);
        d.finish()
    }
}

impl SessionEnv {
    /// The pairs the harness spawns every command of this session with - true by
    /// construction, and the only source that needs no observation.
    pub fn seeded(pairs: impl IntoIterator<Item = (String, String)>) -> SessionEnv {
        let mut values = BTreeMap::new();
        for (k, v) in pairs {
            if is_name(&k) && is_plain(&v) && v.len() <= ENV_VALUE {
                values.insert(
                    k,
                    Entry {
                        value: v,
                        from: From::Harness,
                    },
                );
            }
        }
        SessionEnv {
            values,
            dead: false,
        }
    }

    /// The value in force for `name`, with its source, or `None` when nothing fixed it.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.values.get(name)
    }

    /// How many names are held, for a report a person may read.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Whether the shell that reported this is gone.
    pub fn dead(&self) -> bool {
        self.dead
    }

    /// The shell this came from is gone, and everything it said goes with it.
    /// What the harness gives every spawn stays, because that is a fact about the next
    /// spawn: a value only a dead shell held would be a claim about an environment nobody holds.
    pub fn mark_dead(&mut self) {
        self.values.retain(|_, e| e.from == From::Harness);
        self.dead = true;
    }

    /// Take one report from the shell, whole or not at all: `NAME=value;` pairs, which the
    /// trailer emits unescaped because it drops every value that is not one plain word. A
    /// report replaces the last one, and a pair the harness gives every spawn wins over it.
    pub fn observe(&mut self, report: &str) -> Result<(), String> {
        let mut values: BTreeMap<String, Entry> = self
            .values
            .iter()
            .filter(|(_, e)| e.from == From::Harness)
            .map(|(k, e)| (k.clone(), e.clone()))
            .collect();
        let mut reported = 0usize;
        let mut held = 0usize;
        let mut value_bytes = 0usize;
        for pair in report.split(';') {
            if pair.is_empty() {
                continue;
            }
            let Some((name, value)) = pair.split_once('=') else {
                return Err(format!("`{pair}` is not `NAME=value`"));
            };
            if !is_name(name) {
                return Err(format!("`{name}` is not a variable name"));
            }
            if !is_plain(value) {
                return Err(format!("`{name}` is not one plain word"));
            }
            reported += 1;
            // Denied: reported and believable, never carried. Counting it keeps a report
            // of nothing but denied names from reading as an empty one.
            if denied(name) {
                continue;
            }
            // Too big, or the budget is spent: not held.
            if value.len() > ENV_VALUE || held >= ENV_PAIRS {
                continue;
            }
            if value_bytes + value.len() > ENV_BYTES {
                continue;
            }
            held += 1;
            value_bytes += value.len();
            // The harness's pair wins.
            if values.get(name).is_some_and(|e| e.from == From::Harness) {
                continue;
            }
            values.insert(
                name.to_string(),
                Entry {
                    value: value.to_string(),
                    from: From::Shell,
                },
            );
        }
        if reported == 0 {
            return Err("an empty report".to_string());
        }
        self.values = values;
        self.dead = false;
        Ok(())
    }

    /// The one spelling of a value that may be shown: masked when it looks like a
    /// credential, verbatim when it does not. `None` when the name is not held at all,
    /// which is a different sentence on the card.
    pub fn masked(&self, name: &str) -> Option<String> {
        self.get(name).map(|e| {
            if is_secret(name, &e.value) {
                mask(&e.value)
            } else {
                e.value.clone()
            }
        })
    }

    /// Whether this name is a credential this module will not write into a reading.
    pub fn is_secret(&self, name: &str) -> bool {
        self.get(name).is_some_and(|e| is_secret(name, &e.value))
    }

    /// The pairs a shell reported and nothing else, in name order: what the next call's
    /// spawn is given on top of the pairs the harness always gives. The harness's own
    /// names are absent on purpose, so the two sources cannot disagree on a value.
    pub fn shell_pairs(&self) -> Vec<(String, String)> {
        self.values
            .iter()
            .filter(|(_, e)| e.from == From::Shell)
            .map(|(k, e)| (k.clone(), e.value.clone()))
            .collect()
    }
}

/// The names a shell's report is never carried for (R95's §4j.5 ruling): a value here changes
/// what a later command does with no text saying so - the loaders' namespace, the files and
/// options a shell reads unasked, the `PATH` (which file a bare name runs), and the shell's
/// machinery about itself. The narrowest safe default; widening it is the operator's open
/// ruling, and one list serves the resolver and the spawn alike.
pub fn denied(name: &str) -> bool {
    matches!(
        name,
        "PATH" | "BASH_ENV" | "ENV" | "SHELLOPTS" | "BASHOPTS" | "PWD" | "OLDPWD" | "SHLVL" | "_"
    ) || name.starts_with("LD_")
        || name.starts_with("DYLD_")
}

/// Whether a name is a shell variable name: `[A-Za-z_][A-Za-z0-9_]*`, the trailer's
/// grammar and the resolver's alike. The value is a different question, [`is_plain`].
fn is_name(name: &str) -> bool {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether the shell would deliver this value as exactly one plain word: the set
/// [`crate::exec::shell::PLAIN`] names for a bracket expression, restated as characters a
/// `contains` can test, so this and `intent::parameters::as_one_word` cannot drift apart.
fn is_plain(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%,".contains(c))
}

/// Whether this name and value look like a credential: a name match rather than a guess at
/// entropy, plus the value prefixes that are unambiguous on their own. Deliberately nothing
/// that would catch a path (`SSH_AUTH_SOCK`, `KUBECONFIG`), which is the reading worth having.
pub fn is_secret(name: &str, value: &str) -> bool {
    const IN_NAME: &[&str] = &[
        "PASSWORD",
        "PASSWD",
        "SECRET",
        "TOKEN",
        "CREDENTIAL",
        "APIKEY",
        "API_KEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
    ];
    let upper = name.to_ascii_uppercase();
    if IN_NAME.iter().any(|k| upper.contains(k)) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "sk-",
        "sk_live_",
        "AKIA",
        "ASIA",
        "xoxb-",
        "xoxp-",
        "glpat-",
        "-----BEGIN",
    ];
    PREFIXES.iter().any(|p| value.starts_with(p))
}

/// The most of a credential that may be shown: the first four and last two characters, or
/// an ellipsis for a value too short for that to hide anything. Enough to tell which value
/// decided the reading, since two tokens for one service share a prefix, and not enough to use.
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 12 {
        return "…".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 2..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> SessionEnv {
        SessionEnv::seeded([
            ("HOME".to_string(), "/home/dead".to_string()),
            ("CARGO_TARGET_DIR".to_string(), "/w/target".to_string()),
        ])
    }

    #[test]
    fn the_seed_is_what_every_spawn_gets() {
        let e = seeded();
        assert_eq!(
            e.get("CARGO_TARGET_DIR").map(|x| x.value.as_str()),
            Some("/w/target")
        );
        assert_eq!(
            e.get("CARGO_TARGET_DIR").map(|x| x.from),
            Some(From::Harness)
        );
        assert!(e.get("NOPE").is_none());
    }

    #[test]
    fn a_report_drops_the_names_the_deny_list_refuses() {
        let mut e = seeded();
        e.observe(
            "PATH=/tmp/x:/bin;LD_PRELOAD=/tmp/evil.so;LD_LIBRARY_PATH=/tmp;DYLD_INSERT_LIBRARIES=/l;\
             BASH_ENV=/tmp/rc;ENV=/tmp/rc;SHELLOPTS=xtrace;BASHOPTS=x;PWD=/elsewhere;OLDPWD=/gone;SHLVL=9;_=x;",
        )
        .expect("a clean report");
        for name in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "BASH_ENV",
            "ENV",
            "SHELLOPTS",
            "BASHOPTS",
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
        ] {
            assert!(e.get(name).is_none(), "{name} must not be held");
        }
        // `PATH` the harness did not seed here: dropped, not remembered as the shell's own.
        assert!(e.get("PATH").is_none(), "a denied name is never held");
    }

    #[test]
    fn a_report_of_nothing_but_denied_names_is_believed() {
        let mut e = seeded();
        e.observe("PATH=/tmp/x:/bin;SHLVL=2;")
            .expect("a real report");
        assert_eq!(e.len(), 2, "the seed is untouched: {:?}", e);
        assert_eq!(e.shell_pairs(), Vec::<(String, String)>::new());
    }

    #[test]
    fn shell_pairs_carry_only_what_the_shell_said() {
        let mut e = seeded();
        e.observe("p=/w;q=/q;HOME=/elsewhere;").expect("a report");
        assert_eq!(
            e.shell_pairs(),
            vec![
                ("p".to_string(), "/w".to_string()),
                ("q".to_string(), "/q".to_string())
            ],
            "harness pairs stay off the carry: {:?}",
            e
        );
    }

    #[test]
    fn a_report_replaces_rather_than_accumulates() {
        let mut e = seeded();
        e.observe("HOME=/home/dead;p=/w;").expect("a clean report");
        assert_eq!(e.get("p").map(|x| x.value.as_str()), Some("/w"));
        assert_eq!(e.get("p").map(|x| x.from), Some(From::Shell));
        // A name the harness gives every spawn is its own, whatever the shell says about it:
        // the host puts it on the child after `env_clear()`, so it is what the next `bash`
        // call really gets.
        assert_eq!(e.get("HOME").map(|x| x.from), Some(From::Harness));
        assert_eq!(e.get("HOME").map(|x| x.value.as_str()), Some("/home/dead"));
        // A name NEITHER source has is not held: the second report replaced the first.
        let mut e = SessionEnv::default();
        e.observe("p=/w;q=/q;").expect("a clean report");
        e.observe("p=/w;").expect("a clean report");
        assert!(
            e.get("q").is_none(),
            "an unset did not take: {:?}",
            e.get("q")
        );
    }

    #[test]
    fn a_report_that_is_not_believable_changes_nothing() {
        let mut e = seeded();
        for bad in ["no_equals_here;", "1BAD=x;", "p=/w/x y;", "p=/w\n;", ""] {
            assert!(e.observe(bad).is_err(), "{bad:?} must be refused");
            assert_eq!(e.len(), 2, "{bad:?} must change nothing");
        }
    }

    #[test]
    fn a_value_too_big_to_hold_is_dropped_and_takes_nothing_else_with_it() {
        // Not a refusal: `cargo test` sets a 2 KiB `LD_LIBRARY_PATH`, and a refused report
        // would take the session's whole environment, `PATH` included, with it.
        let mut e = seeded();
        let big = format!("LD_LIBRARY_PATH={};", "/x".repeat(ENV_VALUE));
        e.observe(&format!("p=/w;{big}q=/q;")).expect("a report");
        assert_eq!(e.get("p").map(|x| x.value.as_str()), Some("/w"));
        assert_eq!(e.get("q").map(|x| x.value.as_str()), Some("/q"));
        assert!(e.get("LD_LIBRARY_PATH").is_none());
    }

    #[test]
    fn a_value_that_is_not_one_word_is_refused_rather_than_trimmed() {
        let mut e = seeded();
        // A space, a glob, a `;` and a `$` are each a way the shell would deliver
        // something other than these bytes - the trailer's filter drops them at the
        // source and this is the second door.
        for bad in ["p=a b;", "p=*;", "p=a;b;"] {
            assert!(e.observe(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_credential_is_held_and_masked() {
        let mut e = SessionEnv::default();
        e.observe("GITHUB_TOKEN=ghp_0123456789abcdef4c;")
            .expect("a clean report");
        assert!(e.is_secret("GITHUB_TOKEN"));
        assert_eq!(e.masked("GITHUB_TOKEN").as_deref(), Some("ghp_…4c"));
        // And the value itself is still there for resolution: kept, not dropped.
        assert_eq!(
            e.get("GITHUB_TOKEN").map(|x| x.value.as_str()),
            Some("ghp_0123456789abcdef4c")
        );
    }

    #[test]
    fn a_plainly_named_credential_is_caught_by_its_prefix() {
        assert!(is_secret("X", "sk-abcdefghijklmnop"));
        assert!(is_secret("X", "AKIAIOSFODNN7EXAMPLE"));
        assert!(!is_secret("X", "/home/dead/.ssh/authorized"));
        // A path in an AUTH-sounding name is a path. `SSH_AUTH_SOCK` is the most
        // commonly exported socket there is, and masking it would cost the reading this
        // feature exists for, which is why `AUTH` is not one of the name words.
        assert!(!is_secret("SSH_AUTH_SOCK", "/run/user/1000/keyring/ssh"));
        assert!(is_secret(
            "SSH_PRIVATE_KEY_PATH",
            "/home/dead/.ssh/id_ed25519"
        ));
    }

    #[test]
    fn a_short_value_is_masked_whole() {
        assert_eq!(mask("abcdef"), "…");
        assert_eq!(mask("abcdefghijklmn"), "abcd…mn");
    }

    #[test]
    fn debug_prints_names_and_never_values() {
        let mut e = SessionEnv::default();
        e.observe("GITHUB_TOKEN=ghp_0123456789abcdef4c;p=/w;")
            .expect("a clean report");
        let text = format!("{e:?}");
        assert!(text.contains("GITHUB_TOKEN"), "{text}");
        assert!(!text.contains("ghp_0123456789abcdef4c"), "{text}");
    }

    #[test]
    fn a_dead_shell_takes_its_report_with_it_and_leaves_the_harness_pairs() {
        let mut e = seeded();
        e.observe("p=/w;HOME=/home/dead;").expect("a clean report");
        assert_eq!(e.get("p").map(|x| x.value.as_str()), Some("/w"));
        e.mark_dead();
        assert!(e.dead());
        // The shell's own values are gone: nothing holds them any more.
        assert!(
            e.get("p").is_none(),
            "a dead shell's value: {:?}",
            e.get("p")
        );
        // And the pairs every command of this session is spawned with are untouched,
        // because they are a fact about the NEXT spawn, not about the shell that exited.
        assert_eq!(
            e.get("CARGO_TARGET_DIR").map(|x| x.value.as_str()),
            Some("/w/target")
        );
    }
}
