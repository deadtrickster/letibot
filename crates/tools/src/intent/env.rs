//! **The environment a session's shell really holds**, as the shell reported it — D14/R95.
//!
//! # Why this exists, and the measurement that shapes it
//!
//! `docs/boundary-and-adjudication.md` §4j.1. The short form: the model's `bash` tool opens
//! no shell — every call is a fresh `/bin/sh -c` with a cleared environment and the pairs the
//! harness sets — so *what the next command will really see* has two honest sources and one
//! dishonest one:
//!
//! | source | true for |
//! |---|---|
//! | the pairs the harness spawns every command with ([`SessionEnv::seeded`]) | every `bash` call, by construction |
//! | a shell's own trailer ([`SessionEnv::observe`]) | that shell's next line, and nothing else |
//! | what an earlier `bash` call exported | **nothing** — it died with the process |
//!
//! So this is not a model of what the shell *would* have if it persisted; it is a record of
//! what a shell said, kept per session and per shell, used only where it is true.
//!
//! # What it keeps, and what it refuses
//!
//! * **Bounded.** [`ENV_PAIRS`] names and [`ENV_BYTES`] of value per report. A report over
//!   either is refused **whole** — a truncated environment is a wrong one, and half an
//!   environment silently resolved is the failure this module exists to avoid.
//! * **Plain words only**, which the shell enforces as it emits (`exec::shell`'s trailer
//!   function) and this module re-checks on the way in: a value the resolver could never
//!   write back is not worth carrying, and accepting one here would make the trailer's
//!   separator ambiguous.
//! * **A credential is kept and never printed.** D14's own words: *"values that look like
//!   secrets are kept for resolution but never printed on a card or in a log"*. Kept, because
//!   a name the shell holds is not a name nobody set; never printed, because a card, a log
//!   line and a corpus row all outlive the moment. [`SessionEnv::masked`] is the one spelling
//!   of a value that may be shown.
//! * **Not spliced into the reading** when it is a credential — see
//!   `intent::parameters::resolve`, which is where that decision actually bites, and §4j.4
//!   for why (the post-substitution text becomes the approval key).

use std::collections::BTreeMap;

use crate::exec::shell::{ENV_BYTES, ENV_PAIRS};

/// The longest single value this will carry, so one name cannot use the whole budget.
pub const ENV_VALUE: usize = 1024;

/// **Where one name's value came from** — carried per name, because the card has to say it.
///
/// Two sources and they are different claims: [`From::Harness`] is a value every command of
/// this session is spawned with (so the reading is about the real spawn), and [`From::Shell`]
/// is a value the shell said it held after a line (so the reading is about that shell's next
/// line). Neither is ever a value inferred from a command's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// One of the pairs the daemon gives every spawn: `HOME`, the pinned `PATH`,
    /// `Config::build_env` (`CARGO_TARGET_DIR`), `LETIBOT_SOCKET`, …
    Harness,
    /// The session's shell, in a trailer after a line.
    Shell,
}

impl From {
    /// How the card says it. A sentence rather than a word, because *"where did this value
    /// come from"* is the whole question a person asks about a reading they did not write.
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

/// **The environment one session's shell holds**, as far as anything observed it.
///
/// `Default` is empty: a session that spawned no shell and was seeded with nothing resolves
/// nothing beyond G3, which is exactly what a one-shot `bash` gets.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SessionEnv {
    values: BTreeMap<String, Entry>,
    /// Set when the shell this environment came from is gone. Nothing new resolves from it —
    /// a dead shell's last report is what its last line saw, and no later command runs there.
    ///
    /// It does **not** empty the map: the values are still the last true thing anybody said,
    /// and a card that forgets them would ask about the same command for ever.
    dead: bool,
}

impl std::fmt::Debug for SessionEnv {
    /// Names and sources, **never values** — see the module header. A `Debug` that printed a
    /// credential would put it in every `dbg!` and every log line that ever formats a
    /// `Surroundings`, which is the same disclosure the card is masked against.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("SessionEnv");
        d.field("names", &self.values.keys().collect::<Vec<_>>());
        d.field("dead", &self.dead);
        d.finish()
    }
}

impl SessionEnv {
    /// **The pairs the harness spawns every command of this session with** — true by
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

    /// **The shell this came from is gone.** Its report dies with it.
    ///
    /// What the harness gives every spawn **stays**, because that is a fact about the next
    /// spawn rather than about the shell — it will still be in the environment of a command
    /// that runs after the shell has exited. Everything the *shell* said goes, because a
    /// command resolved against a value only a dead shell held would be a claim about an
    /// environment nothing holds.
    pub fn mark_dead(&mut self) {
        self.values.retain(|_, e| e.from == From::Harness);
        self.dead = true;
    }

    /// **Take one report from the shell, whole or not at all.**
    ///
    /// The report is `NAME=value;` pairs, which is what `exec::shell`'s trailer function
    /// emits: the shell drops every value that is not already one plain word, so a pair
    /// cannot contain the separator and this needs no escaping.
    ///
    /// **A pair the harness already gives every spawn wins**, and that is the one precedence
    /// rule in this module: `PATH`, `HOME` and `Config::build_env` are put on the child after
    /// `env_clear()` by the host itself, so they are what the next `bash` call really gets
    /// whatever a shell did to its own copy. Everything else the shell reports is added, and
    /// a name neither source has is simply not held.
    ///
    /// **A report REPLACES the last one** — the shell's environment is the shell's, so a name
    /// it stopped exporting stops resolving no matter what an earlier report said. That is the
    /// only way an `unset` can be observed at all.
    ///
    /// `Err` is a report this build will not believe: a pair that is not `NAME=value`, a name
    /// that is not an identifier, a value that is not one plain word. All three mean the
    /// trailer was not written by this build, and an environment half-updated from a mangled
    /// one would be exactly the wrong claim this module is built to avoid.
    ///
    /// **A value that is merely too big is SKIPPED, not refused**, and the difference is what
    /// a refusal would cost: one 2 KiB `LD_LIBRARY_PATH` — which is what `cargo test` sets on
    /// this box — would otherwise take the whole environment with it, including the `PATH` and
    /// every name the session really uses. A skipped name is simply not held, which is the
    /// same answer as one nobody set: an expansion of it stays unresolved.
    pub fn observe(&mut self, report: &str) -> Result<(), String> {
        let mut values: BTreeMap<String, Entry> = self
            .values
            .iter()
            .filter(|(_, e)| e.from == From::Harness)
            .map(|(k, e)| (k.clone(), e.clone()))
            .collect();
        let mut reported = 0usize;
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
            // Too big, or the budget is spent: not held. See this method's own docs.
            if value.len() > ENV_VALUE || reported >= ENV_PAIRS {
                continue;
            }
            if value_bytes + value.len() > ENV_BYTES {
                continue;
            }
            reported += 1;
            value_bytes += value.len();
            // **The harness's pair wins**, see this method's own docs.
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

    /// **The one spelling of a value that may be shown** — masked when it looks like a
    /// credential, verbatim when it does not.
    ///
    /// `None` when the name is not held at all, which is a different sentence on the card.
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
}

/// **Whether a name is a shell variable name.**
///
/// The trailer's grammar and the resolver's are the same one: `[A-Za-z_][A-Za-z0-9_]*`.
/// Deliberately not a character class over the value — see [`is_plain`].
fn is_name(name: &str) -> bool {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// **Whether the shell would deliver this value as exactly one plain word** — the same set
/// `intent::parameters::as_one_word` splices back, restated here because this module is what
/// the trailer's filter is built from and the two must not be able to drift silently.
///
/// [`crate::exec::shell::PLAIN`] is that set in the spelling a shell bracket expression
/// needs (`A-Za-z0-9_./:@%,-`, the hyphen last); this is the same characters as a set a
/// `contains` can test. `a_plain_value_is_the_same_set_as_the_shell_filter` in `exec::shell`
/// is the test that keeps the two spellings one set.
fn is_plain(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%,".contains(c))
}

/// **Whether this name and value look like a credential.**
///
/// A name match rather than a guess at entropy: `$RANDOM` is not a secret and `$HOME` is not,
/// while `GITHUB_TOKEN` and `AWS_SECRET_ACCESS_KEY` are, and a person reading the card knows
/// which of those they exported. The value prefixes are the ones that are unambiguous in
/// practice — a token that starts `ghp_` is a GitHub token whether or not the name says so,
/// because a session that read it out of a file into a plainly-named variable has still put a
/// credential in one. Deliberately **not** included: anything that would catch a path
/// (`SSH_AUTH_SOCK`, `KUBECONFIG`) — those are the values this feature is most useful for,
/// and masking a path costs the reading for no gain.
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

/// **The most of a credential that may be shown**: the first four and last two characters,
/// or `…` for a value too short for that to hide anything.
///
/// Enough for a person to tell *which* value decided the reading — two tokens for the same
/// service share a prefix, the tail is what differs — and not enough to use.
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
        // **Not a refusal.** `cargo test` on this box sets a 2 KiB `LD_LIBRARY_PATH`, and a
        // refused report would take the session's whole environment — `PATH` included —
        // with it. The name is simply not held.
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
        // something other than these bytes — the trailer's filter drops them at the
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
        // …and the value itself is still there for resolution: kept, not dropped.
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
        // feature exists for — which is why `AUTH` is not one of the name words.
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
        // And the pairs every command of this session is spawned with are untouched —
        // they are a fact about the NEXT spawn, not about the shell that exited.
        assert_eq!(
            e.get("CARGO_TARGET_DIR").map(|x| x.value.as_str()),
            Some("/w/target")
        );
    }
}
