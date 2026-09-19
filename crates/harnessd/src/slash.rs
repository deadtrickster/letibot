//! The daemon's slash verbs: `/flowy …` and `/models …`, as the operator typed
//! them in a head, answered on the session's log.
//!
//! # Wizardy, in a terminal
//!
//! Each verb does what it can with what it was given and, when something is
//! missing, says the exact next command — *"paste it with `/flowy login SEAT
//! --token …`"* — rather than a manual. A step that has a real choice (several
//! seats on the box) lists them. Nothing is guessed: two seats and no name is a
//! list, not a pick, for the reason `creds.rs` gives.
//!
//! # Where it lands
//!
//! `/flowy login` writes the seat's files where the usual path reads them
//! (`~/.config/flowy/agents/<seat>`, `env-<seat>`), opens the seat, and attaches
//! every open root session — the `flowy` tool is always a door, so no session
//! needs reopening. `/models X` switches the provider underneath the session
//! and records it as the standing choice in `~/.config/letibot/providers.toml`,
//! so it sticks for the next daemon too; a key the provider needs is stored
//! there with `--key`, mode 0600.
//!
//! Feedback is `SessionEvent::Warning { code: "slash", .. }` — the one event a
//! head renders at every verbosity, which is right for an answer to something
//! the operator just typed.

use std::path::PathBuf;

use letibot_flowy::creds::{Credentials, Onboarding, discover};
use letibot_flowy::{Seat, SeatState};

use crate::config::ProviderConfig;

/// One verb's outcome: lines for the head, and whether it worked.
pub struct SlashReply {
    pub lines: Vec<String>,
    pub ok: bool,
}

impl SlashReply {
    fn ok(lines: Vec<String>) -> Self {
        SlashReply { lines, ok: true }
    }
}

/// `flowy status`, `flowy login …`, `flowy logout`, `models …` — parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slash {
    FlowyStatus,
    FlowyLogin {
        seat: Option<String>,
        addr: Option<String>,
        token: Option<String>,
        token_file: Option<PathBuf>,
        new_reader: bool,
    },
    FlowyLogout,
    /// `/job` lists this session's background jobs; `/job ID` reads what one
    /// wrote. The pane counted those bytes and could not show them.
    Job { job: Option<String>, offset: u64 },
    /// **What this conversation can call**, and what it only looks like it can.
    Tools,
    Models,
    ModelsSet {
        provider: String,
        model: Option<String>,
        key: Option<String>,
        /// **This conversation only.** `/models X` has always done two things —
        /// switched the running session AND written the standing choice into
        /// `providers.toml` — with no way to have the first without the second.
        /// The operator: *"--once would be nice yes"*. One hard question on a
        /// metered model should not be a default the next daemon inherits.
        once: bool,
    },
    /// **The supervised-labelling verb.** See [`gate`].
    Gate(GateVerb),
    /// Turn the guard model on or off on the running session. `None` reports.
    ///
    /// `at` carries an address the first time somebody names one; it is remembered,
    /// so the second `/supervise` is one word.
    Supervise { want: Option<bool>, at: Option<String> },
    Help(String),
}

/// What the operator is saying about a decision the gate already made.
///
/// > *"literally i'm ready to sit and answer each turn after the model"*
///
/// That is the loop this exists for, and it is not the same thing as answering a
/// question the gate asked. A gate question is answered *before* the call runs and
/// is already recorded as a `human:` verdict. This is the other case: the gate
/// decided by itself, the call has run or been refused, and the operator is saying
/// whether it should have. It is the only source of a label on an *automatic*
/// decision, so it is the only thing that makes auto mode supervisable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerb {
    /// Recent decisions, newest first, so the operator can see what to rule on.
    Recent { limit: usize, only_unlabelled: bool },
    /// How much corpus there is. A count nobody can see is a count nobody keeps.
    Counts,
    /// A ruling on one decision.
    Rule { request_id: String, kind: &'static str, note: String },
}

impl Slash {
    pub fn parse(line: &str) -> Slash {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.first().copied() {
            Some("flowy") => match words.get(1).copied() {
                None | Some("status") => Slash::FlowyStatus,
                Some("logout") => Slash::FlowyLogout,
                Some("login") => {
                    let mut seat = None;
                    let mut addr = None;
                    let mut token = None;
                    let mut token_file = None;
                    let mut new_reader = false;
                    let mut i = 2;
                    while i < words.len() {
                        match words[i] {
                            "--addr" | "--url" => {
                                addr = words.get(i + 1).map(|s| s.to_string());
                                i += 2;
                            }
                            "--token" => {
                                token = words.get(i + 1).map(|s| s.to_string());
                                i += 2;
                            }
                            "--token-file" => {
                                token_file = words.get(i + 1).map(PathBuf::from);
                                i += 2;
                            }
                            "--new-reader" | "--new" => {
                                new_reader = true;
                                i += 1;
                            }
                            w if !w.starts_with("--") && seat.is_none() => {
                                seat = Some(w.to_string());
                                i += 1;
                            }
                            _ => i += 1,
                        }
                    }
                    Slash::FlowyLogin {
                        seat,
                        addr,
                        token,
                        token_file,
                        new_reader,
                    }
                }
                Some(other) => Slash::Help(format!(
                    "/flowy {other}: the verbs are status, login [SEAT] [--addr URL] [--token T | \
                     --token-file PATH] [--new-reader], logout"
                )),
            },
            Some("tools") => Slash::Tools,
            Some("job") | Some("jobs") => {
                let job = words.get(1).filter(|w| !w.starts_with("--")).map(|w| w.to_string());
                // `--offset N` continues a read the ring had more of; the reply
                // names the next offset, so this is a copy rather than a sum.
                let offset = words
                    .iter()
                    .position(|w| *w == "--offset")
                    .and_then(|i| words.get(i + 1))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                Slash::Job { job, offset }
            }
            Some("models") | Some("model") => match words.get(1).copied() {
                None | Some("list") => Slash::Models,
                Some(spec) => {
                    let (provider, model) = match spec.split_once('/') {
                        Some((p, m)) => (p.to_string(), Some(m.to_string())),
                        None => (spec.to_string(), None),
                    };
                    let key = words
                        .iter()
                        .position(|w| *w == "--key" || *w == "--api-key")
                        .and_then(|i| words.get(i + 1))
                        .map(|s| s.to_string());
                    let once = words.iter().any(|w| *w == "--once" || *w == "--here");
                    Slash::ModelsSet { provider, model, key, once }
                }
            },
            Some("login") => Slash::Help(
                "/login is two things here: `/flowy login` for the fabric, `/models PROVIDER --key K` \
                 for a cloud model"
                    .into(),
            ),
            // One word, no arguments, and it lands on the session in front of you.
            // `/supervise off` and `/supervise on` exist for saying it explicitly;
            // bare `/supervise` turns it ON, because that is what somebody typing it
            // wants and asking them to say `on` twice is the kind of ceremony that
            // makes a feature go unused.
            Some("supervise") | Some("supervised") => match words.get(1).copied() {
                None | Some("on") | Some("1") | Some("yes") => {
                    Slash::Supervise { want: Some(true), at: None }
                }
                Some("off") | Some("0") | Some("no") => {
                    Slash::Supervise { want: Some(false), at: None }
                }
                Some("status") | Some("?") => Slash::Supervise { want: None, at: None },
                // **Anything with a colon is an address**, because that is what a
                // person means by `/supervise 192.168.1.76:8090` and refusing it in
                // favour of a separate `--oracle` flag on a daemon they did not start
                // is exactly the ceremony this verb exists to remove.
                Some(other) if other.contains(':') => Slash::Supervise {
                    want: Some(true),
                    at: Some(other.to_string()),
                },
                Some(other) => Slash::Help(format!(
                    "/supervise [on|off|status|HOST:PORT] — `{other}` is none of those"
                )),
            },
            Some("gate") => {
                let note = |from: usize| words[from.min(words.len())..].join(" ");
                match words.get(1).copied() {
                    None | Some("recent") => Slash::Gate(GateVerb::Recent {
                        limit: words.get(2).and_then(|w| w.parse().ok()).unwrap_or(20),
                        only_unlabelled: false,
                    }),
                    // The working queue for somebody labelling as they go: what has
                    // not been ruled on yet.
                    Some("todo") | Some("pending") => Slash::Gate(GateVerb::Recent {
                        limit: words.get(2).and_then(|w| w.parse().ok()).unwrap_or(20),
                        only_unlabelled: true,
                    }),
                    Some("corpus") | Some("counts") => Slash::Gate(GateVerb::Counts),
                    // Three verbs, not two, because "the gate was right" is a label
                    // and not a no-op. A corpus holding only the corrections teaches
                    // that every decision was wrong.
                    Some(k @ ("ok" | "upheld" | "grant" | "granted" | "revoke" | "revoked")) => {
                        let Some(id) = words.get(2) else {
                            return Slash::Help(format!(
                                "/gate {k} REQUEST-ID [note] — `/gate todo` lists what is unruled"
                            ));
                        };
                        let kind = match k {
                            "ok" | "upheld" => "upheld",
                            "grant" | "granted" => "granted",
                            _ => "revoked",
                        };
                        Slash::Gate(GateVerb::Rule {
                            request_id: (*id).to_string(),
                            kind,
                            note: note(3),
                        })
                    }
                    Some(other) => Slash::Help(format!(
                        "/gate {other}: the verbs are recent [N], todo [N], corpus, \
                         and ok|grant|revoke REQUEST-ID [note]"
                    )),
                }
            }
            Some(other) => Slash::Help(format!("/{other} is not a daemon verb; /help lists the head's")),
            None => Slash::Help("/help".into()),
        }
    }
}

// ------------------------------------------------------------- flowy

pub fn flowy_status(seat: Option<&Seat>) -> SlashReply {
    let Some(seat) = seat else {
        return SlashReply::ok(vec![
            "flowy: no seat is attached to this daemon.".into(),
            "  /flowy login attach the seat on the usual path ($FLOWY_AGENT, or the only".into(),
            "                          token under ~/.config/flowy/agents/)".into(),
            "  /flowy login SEAT which seat, when there are several".into(),
            "  /flowy login SEAT --token T [--addr URL]   a seat this box has never held".into(),
        ]);
    };
    let st = seat.state();
    let stats = seat.stats();
    let mut lines = vec![
        format!("flowy: seat `{}` — {}", seat.name(), st.word()),
        format!("  {}", seat.credentials().describe()),
        match seat.reader() {
            Ok(Some(r)) => format!("  reader `{}` at cursor {}", r.reader, r.cursor),
            Ok(None) => {
                "  reader NOT DECLARED on the node (/flowy login --new-reader, after reading why)"
                    .into()
            }
            Err(e) => format!("  reader: could not ask ({e})"),
        },
        format!(
            "  polls {}, delivered {}, node-filtered {}, acks {} (failed {}), last poll {}",
            stats.polls,
            stats.delivered,
            stats.server_skipped,
            stats.acks,
            stats.ack_failures,
            stats.last_poll.as_deref().unwrap_or("never")
        ),
        format!("  sessions attached: {}", seat.attached_names()),
    ];
    if let SeatState::Stopped { .. } | SeatState::Stalled { .. } = st {
        lines.push(
            "  (a stalled seat keeps trying; a stopped one needs /flowy logout and /flowy login)"
                .into(),
        );
    }
    SlashReply::ok(lines)
}

/// The seats this box holds tokens for.
fn seats_on_disk() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let dir = PathBuf::from(home).join(".config/flowy/agents");
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Resolve credentials for a login, writing the seat's files when a token was
/// pasted. Every refusal names the next command.
pub fn flowy_login_credentials(
    seat: Option<&str>,
    addr: Option<&str>,
    token: Option<&str>,
    token_file: Option<&PathBuf>,
) -> Result<(Credentials, Vec<String>), Vec<String>> {
    let mut notes = Vec::new();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| vec!["$HOME is not set".to_string()])?;
    let cfg_dir = home.join(".config/flowy");
    // A pasted token is written where the usual path reads it, so the next
    // daemon needs no flag. Needs a seat name to file it under.
    if let Some(t) = token {
        let Some(name) = seat else {
            return Err(vec![
                "a pasted token needs the seat's name to be filed under:".into(),
                "  /flowy login SEAT --token …".into(),
            ]);
        };
        let agents = cfg_dir.join("agents");
        std::fs::create_dir_all(&agents).map_err(|e| vec![format!("{}: {e}", agents.display())])?;
        let path = agents.join(name);
        std::fs::write(&path, format!("{}\n", t.trim()))
            .map_err(|e| vec![format!("{}: {e}", path.display())])?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        notes.push(format!("wrote the token to {} (mode 600)", path.display()));
        let env = cfg_dir.join(format!("env-{name}"));
        if !env.is_file() {
            let node = addr.unwrap_or(letibot_flowy::creds::DEFAULT_ADDR);
            let body = format!(
                "export FLOWY_ADDR={node}\nexport FLOWY_AGENT={name}\nexport FLOWY_TOKEN=$(cat ~/.config/flowy/agents/{name})\n"
            );
            std::fs::write(&env, body).map_err(|e| vec![format!("{}: {e}", env.display())])?;
            notes.push(format!("wrote {} (node {node})", env.display()));
        }
    }
    let onboarding = Onboarding {
        addr: addr.map(str::to_string),
        agent: seat.map(str::to_string),
        token: None,
        token_file: token_file.cloned(),
        config_dir: None,
        read_env: true,
    };
    match discover(&onboarding) {
        Ok(c) => Ok((c, notes)),
        Err(e) => {
            let mut lines = vec![format!("flowy login: {e}")];
            let seats = seats_on_disk();
            if seat.is_none() && seats.len() > 1 {
                lines.push("  seats on this box:".into());
                for s in &seats {
                    lines.push(format!("    /flowy login {s}"));
                }
            } else if seats.is_empty() {
                lines.push("  no seat token on this box. The operator mints one on the node (`flowy mint`)".into());
                lines.push(
                    "  and then:  /flowy login SEAT --token PASTE [--addr http://node:8787]".into(),
                );
            }
            Err(lines)
        }
    }
}

// ------------------------------------------------------------- models

/// Every provider this build knows, with its auth state, and the local server.
pub fn models_listing(current: &str) -> Vec<String> {
    let mut lines = vec![format!("models — now answering: {current}")];
    lines.push(
        "  local the llama.cpp server this daemon was started against   /models local"
            .into(),
    );
    for p in letibot_provider::presets::ALL {
        let auth = match letibot_provider::keys::resolve(p, None, None) {
            Ok(c) => format!("key from {}", c.from),
            Err(_) => format!(
                "NO KEY — /models {} --key PASTE, or export ${}",
                p.name, p.key_env
            ),
        };
        lines.push(format!(
            "  {:<16} default model {:<18} {auth}   /models {}/MODEL",
            p.name, p.default_model, p.name
        ));
    }
    lines.push(format!(
        "  the standing choice is in {} ([default]); /models writes it",
        letibot_provider::keys::config_file().display()
    ));
    lines
}

/// Resolve a `/models PROVIDER[/MODEL] [--key K]` into a config, storing the
/// key and the standing choice. `local` clears both.
pub fn models_choice(
    provider: &str,
    model: Option<&str>,
    key: Option<&str>,
    file: Option<&std::path::Path>,
    once: bool,
) -> Result<(Option<ProviderConfig>, Vec<String>), Vec<String>> {
    let mut notes = Vec::new();
    // `--once` skips the standing choice and nothing else. The key, if one was
    // pasted, is still stored: a key is a credential and not a preference, and
    // making somebody paste it again next time would be the wrong half to forget.
    if provider == "local" {
        if once {
            notes.push(
                "this conversation only — the standing choice in providers.toml is \
                 untouched"
                    .into(),
            );
        } else {
            match letibot_provider::keys::set_default(file, "local", None) {
                Ok(f) => notes.push(format!("standing choice: local, in {}", f.display())),
                Err(e) => notes.push(format!("standing choice not recorded: {e}")),
            }
        }
        return Ok((None, notes));
    }
    let preset = letibot_provider::Preset::parse(provider).map_err(|e| vec![e])?;
    if let Some(k) = key {
        match letibot_provider::keys::store_key(file, preset.name, k) {
            Ok(f) => notes.push(format!(
                "stored the {} key in {} (mode 600)",
                preset.name,
                f.display()
            )),
            Err(e) => return Err(vec![format!("could not store the key: {e}")]),
        }
    }
    if let Err(e) = letibot_provider::keys::resolve(preset, None, file) {
        return Err(vec![
            format!("{e}"),
            format!(
                "  /models {}{} --key PASTE",
                preset.name,
                model.map(|m| format!("/{m}")).unwrap_or_default()
            ),
        ]);
    }
    if once {
        notes.push(
            "this conversation only — the standing choice in providers.toml is \
             untouched, so the next daemon starts where it did before"
                .into(),
        );
    } else {
        match letibot_provider::keys::set_default(file, preset.name, model) {
            Ok(f) => notes.push(format!(
                "standing choice: {}/{}, in {} — the next daemon starts on it too",
                preset.name,
                model.unwrap_or(preset.default_model),
                f.display()
            )),
            Err(e) => notes.push(format!("standing choice not recorded: {e}")),
        }
    }
    Ok((
        Some(ProviderConfig {
            name: preset.name.to_string(),
            model: model.map(str::to_string),
            api_key: None,
            thinking: false,
        }),
        notes,
    ))
}

#[cfg(test)]
mod tests {

    /// **`/supervise` is one word.**
    ///
    /// > *"I want to start leticode, do /supervise, and move on."*
    ///
    /// So a bare `/supervise` turns it ON rather than printing usage: asking somebody
    /// to say `on` after a verb that has one obvious direction is the ceremony that
    /// makes a feature go unused. `status` reports without changing anything, and it
    /// is a different word on purpose.
    #[test]
    fn supervise_takes_no_arguments_in_the_case_that_matters() {
        use super::{GateVerb, Slash};
        assert_eq!(
            Slash::parse("supervise"),
            Slash::Supervise { want: Some(true), at: None }
        );
        assert_eq!(
            Slash::parse("supervise on"),
            Slash::Supervise { want: Some(true), at: None }
        );
        assert_eq!(
            Slash::parse("supervise off"),
            Slash::Supervise { want: Some(false), at: None }
        );
        assert_eq!(
            Slash::parse("supervise status"),
            Slash::Supervise { want: None, at: None }
        );
        // An address is the one-off override, and anything with a colon is one —
        // refusing it in favour of a daemon flag is the ceremony this verb removes.
        assert_eq!(
            Slash::parse("supervise 192.168.1.76:8090"),
            Slash::Supervise { want: Some(true), at: Some("192.168.1.76:8090".into()) }
        );
        // A typo is named, never silently treated as `on`: turning a guard on by
        // accident and turning it on deliberately must not be the same keystroke.
        assert!(matches!(Slash::parse("supervise yesss"), Slash::Help(_)));

        // And the labelling verb still parses beside it.
        assert_eq!(
            Slash::parse("gate ok adj-7 looks right"),
            Slash::Gate(GateVerb::Rule {
                request_id: "adj-7".into(),
                kind: "upheld",
                note: "looks right".into()
            })
        );
    }
    use super::*;

    #[test]
    fn the_verbs_parse_with_their_flags_and_unknowns_say_so() {
        match Slash::parse(
            "flowy login lab2x1-leticode --token abc --addr http://n:8787 --new-reader",
        ) {
            Slash::FlowyLogin {
                seat,
                addr,
                token,
                new_reader,
                ..
            } => {
                assert_eq!(seat.as_deref(), Some("lab2x1-leticode"));
                assert_eq!(addr.as_deref(), Some("http://n:8787"));
                assert_eq!(token.as_deref(), Some("abc"));
                assert!(new_reader);
            }
            _ => panic!(),
        }
        assert!(matches!(Slash::parse("flowy"), Slash::FlowyStatus));
        assert!(matches!(Slash::parse("flowy logout"), Slash::FlowyLogout));
        match Slash::parse("models deepseek/deepseek-reasoner --key k1") {
            Slash::ModelsSet {
                provider,
                model,
                key,
                once,
            } => {
                assert!(!once, "no `--once` in this line");
                assert_eq!(provider, "deepseek");
                assert_eq!(model.as_deref(), Some("deepseek-reasoner"));
                assert_eq!(key.as_deref(), Some("k1"));
            }
            _ => panic!(),
        }
        assert!(matches!(Slash::parse("models"), Slash::Models));
        assert!(matches!(Slash::parse("flowy dance"), Slash::Help(_)));
    }

    /// `/job` with no argument is the listing, not a refusal, and `--offset`
    /// is not mistaken for a job id — the operator reaching a second page types
    /// `/job ID --offset N`, but a flag alone must still list.
    #[test]
    fn job_parses_bare_with_an_id_and_with_an_offset() {
        assert!(matches!(
            Slash::parse("job"),
            Slash::Job { job: None, offset: 0 }
        ));
        assert!(matches!(
            Slash::parse("jobs"),
            Slash::Job { job: None, offset: 0 }
        ));
        match Slash::parse("job j-3 --offset 4096") {
            Slash::Job { job, offset } => {
                assert_eq!(job.as_deref(), Some("j-3"));
                assert_eq!(offset, 4096);
            }
            _ => panic!(),
        }
        assert!(matches!(
            Slash::parse("job --offset 10"),
            Slash::Job { job: None, offset: 10 }
        ));
    }

    /// `/tools` is the listing, not the fold. The fold kept ctrl-t and `/t`.
    #[test]
    fn tools_is_a_daemon_verb_now() {
        assert!(matches!(Slash::parse("tools"), Slash::Tools));
    }

    /// `--once` switches the conversation without writing the standing choice.
    #[test]
    fn once_is_parsed_and_is_off_by_default() {
        match Slash::parse("models deepseek/deepseek-chat --once") {
            Slash::ModelsSet { provider, model, once, .. } => {
                assert_eq!(provider, "deepseek");
                assert_eq!(model.as_deref(), Some("deepseek-chat"));
                assert!(once);
            }
            other => panic!("{other:?}"),
        }
        // `--here` says the same thing; both read naturally at a prompt.
        assert!(matches!(
            Slash::parse("models glm --here"),
            Slash::ModelsSet { once: true, .. }
        ));
        // And the old spelling still sticks, because that is what it always did.
        assert!(matches!(
            Slash::parse("models glm"),
            Slash::ModelsSet { once: false, .. }
        ));
    }

    /// The standing choice is the ONLY thing `--once` skips. A pasted key is a
    /// credential, not a preference, and forgetting it would make somebody paste
    /// it again next time.
    #[test]
    fn once_skips_the_standing_choice_and_still_stores_a_key() {
        let d = std::env::temp_dir().join(format!("letibot-once-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("scratch");
        let f = d.join("providers.toml");
        let (choice, notes) =
            models_choice("grok", None, Some("xai-test"), Some(&f), true).expect("choosing");
        assert!(choice.is_some(), "the session still switches");
        let said = notes.join("\n");
        assert!(said.contains("stored the grok key"), "{said}");
        assert!(said.contains("this conversation only"), "{said}");
        assert!(!said.contains("standing choice:"), "{said}");
        let on_disk = std::fs::read_to_string(&f).unwrap_or_default();
        assert!(on_disk.contains("xai-test"), "the key was written: {on_disk}");
        assert!(
            !on_disk.contains("[default]"),
            "and the default was not: {on_disk}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_model_choice_stores_the_key_and_the_standing_choice_or_says_what_is_missing() {
        let d = std::env::temp_dir().join(format!("letibot-slash-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("providers.toml");
        // No key anywhere for grok (the env is not set in this test): refused, with the command.
        // (If the developer's shell exports XAI_API_KEY this arm is skipped.)
        if std::env::var("XAI_API_KEY").is_err() && std::env::var("GROK_API_KEY").is_err() {
            let err = models_choice("grok", Some("grok-4-fast"), None, Some(&f), false).unwrap_err();
            assert!(
                err.iter()
                    .any(|l| l.contains("/models grok/grok-4-fast --key PASTE")),
                "{err:?}"
            );
        }
        let (choice, notes) =
            models_choice("grok", Some("grok-4-fast"), Some("xai-test"), Some(&f), false).unwrap();
        assert_eq!(choice.as_ref().map(|c| c.name.as_str()), Some("grok"));
        assert!(
            notes.iter().any(|n| n.contains("stored the grok key")),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("standing choice: grok/grok-4-fast")),
            "{notes:?}"
        );
        assert_eq!(
            letibot_provider::keys::default_choice(Some(&f)).map(|d| d.provider),
            Some("grok".into())
        );
        let (none, _) = models_choice("local", None, None, Some(&f), false).unwrap();
        assert!(none.is_none());
        assert!(letibot_provider::keys::default_choice(Some(&f)).is_none());
        assert!(models_choice("openai", None, None, Some(&f), false).unwrap_err()[0].contains("three"));
    }

    #[test]
    fn the_models_listing_names_every_provider_and_how_to_authenticate() {
        let l = models_listing("local — x").join("\n");
        for p in ["deepseek", "glm", "grok"] {
            assert!(l.contains(&format!("/models {p}/MODEL")), "{l}");
        }
        assert!(l.contains("/models local"));
    }
}

/// **The supervised-labelling loop, served from the store.**
///
/// Deliberately *not* routed through the live gate, and the reason is the deadlock
/// `answers` is shaped around: the daemon has one worker, and a slash verb arrives
/// on the socket reader's thread while the worker may be inside a turn. The ruling
/// is a row in the store; the store is reachable from here and the gate is not.
///
/// **What that costs, stated rather than hidden:** `AdjudicatedGate::record_override`
/// also resets the consecutive-denial breaker for that direction, because a human
/// answering is the only thing that closes an open one. A ruling written through
/// this path records the label and does **not** lift the breaker — so an operator
/// clearing a run of denials with `/gate grant` will still meet the breaker on the
/// next call. Lifting it needs the same channel `answers` uses, and that is not
/// built here.
pub fn gate(store_path: Option<&std::path::Path>, verb: &GateVerb) -> SlashReply {
    let Some(path) = store_path else {
        return SlashReply {
            lines: vec![
                "no session store is configured (`--store`), so no decision has ever been \
                 recorded and there is nothing to rule on. This is not an empty corpus — \
                 it is no corpus."
                    .into(),
            ],
            ok: false,
        };
    };
    let store = match letibot_tokencore::store::Store::open(path) {
        Ok(s) => s,
        Err(e) => {
            return SlashReply { lines: vec![format!("opening {}: {e}", path.display())], ok: false };
        }
    };

    match verb {
        GateVerb::Counts => match store.corpus_counts() {
            Ok(c) => SlashReply {
                lines: vec![
                    format!("{} decisions recorded", c.total),
                    // Who decided and what it was measured against are independent,
                    // and reporting one number for both is how a session where the
                    // operator personally answered four hundred calls read as
                    // "0 ruled on".
                    format!("  {} you answered yourself", c.decided_by_operator),
                    format!("  {} carry a model verdict", c.measured),
                    format!(
                        "  {} where you and the model differ — the rows a fine-tune is for",
                        c.disagreements
                    ),
                    format!(
                        "  {} decided by a rule or a mode with nobody asked",
                        c.total.saturating_sub(c.decided_by_operator)
                    ),
                ],
                ok: true,
            },
            Err(e) => SlashReply { lines: vec![e.to_string()], ok: false },
        },

        GateVerb::Recent { limit, only_unlabelled } => {
            // `corpus(only_labelled)` narrows the other way, so the unlabelled queue
            // is filtered here rather than by asking for a set that excludes itself.
            let rows = match store.corpus(false, if *only_unlabelled { limit * 8 } else { *limit }) {
                Ok(r) => r,
                Err(e) => return SlashReply { lines: vec![e.to_string()], ok: false },
            };
            let mut lines = Vec::new();
            // **"unruled" is not "nobody answered it".** A call the operator was put
            // in front of already carries their judgement in `verdict_by`; listing it
            // as work would ask the same question twice and get a worse answer the
            // second time. The queue is the calls a rule or a mode settled with
            // nobody asked, and which nobody has ruled on since.
            for r in rows
                .iter()
                .filter(|r| !*only_unlabelled || (!r.asked && r.operator_kind.is_none()))
                .take(*limit)
            {
                let label = match (&r.operator_kind, r.asked) {
                    (Some(k), _) => format!("[{k}]"),
                    // An unruled decision the operator was never shown is the one
                    // auto mode produces, and the one worth ruling on. Marked apart
                    // from one they answered live, which already carries their
                    // judgement in the verdict.
                    (None, false) => "[UNRULED]".to_string(),
                    (None, true) => "[answered live]".to_string(),
                };
                lines.push(format!(
                    "{label} {} · {} · {} → {}",
                    r.request_id, r.tool, r.action, r.effect
                ));
                if let Some(v) = &r.model_verdict {
                    lines.push(format!("        {v}"));
                }
            }
            if lines.is_empty() {
                lines.push(if *only_unlabelled {
                    "nothing unruled — every decision was either answered by you or \
                     already ruled on"
                        .into()
                } else {
                    "no decisions recorded".into()
                });
            } else {
                lines.push("`/gate ok|grant|revoke REQUEST-ID [note]`".into());
            }
            SlashReply { lines, ok: true }
        }

        GateVerb::Rule { request_id, kind, note } => {
            match store.record_operator_ruling(request_id, kind, note) {
                Ok(true) => SlashReply {
                    lines: vec![format!("{request_id}: {kind}")],
                    ok: true,
                },
                // Two reasons this returns false and they are different facts, so
                // both are named rather than collapsed into "not found".
                Ok(false) => SlashReply {
                    lines: vec![format!(
                        "{request_id} was not ruled: either no decision by that id is in \
                         this store, or it already carries a ruling — the first one \
                         stands, because the session acted on it. `/gate recent` shows \
                         which."
                    )],
                    ok: false,
                },
                Err(e) => SlashReply { lines: vec![e.to_string()], ok: false },
            }
        }
    }
}
