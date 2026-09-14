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
    Models,
    ModelsSet {
        provider: String,
        model: Option<String>,
        key: Option<String>,
    },
    Help(String),
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
                    Slash::ModelsSet { provider, model, key }
                }
            },
            Some("login") => Slash::Help(
                "/login is two things here: `/flowy login` for the fabric, `/models PROVIDER --key K` \
                 for a cloud model"
                    .into(),
            ),
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
            "  /flowy login            attach the seat on the usual path ($FLOWY_AGENT, or the only".into(),
            "                          token under ~/.config/flowy/agents/)".into(),
            "  /flowy login SEAT       which seat, when there are several".into(),
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
        "  local            the llama.cpp server this daemon was started against   /models local"
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
) -> Result<(Option<ProviderConfig>, Vec<String>), Vec<String>> {
    let mut notes = Vec::new();
    if provider == "local" {
        match letibot_provider::keys::set_default(file, "local", None) {
            Ok(f) => notes.push(format!("standing choice: local, in {}", f.display())),
            Err(e) => notes.push(format!("standing choice not recorded: {e}")),
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
    match letibot_provider::keys::set_default(file, preset.name, model) {
        Ok(f) => notes.push(format!(
            "standing choice: {}/{}, in {} — the next daemon starts on it too",
            preset.name,
            model.unwrap_or(preset.default_model),
            f.display()
        )),
        Err(e) => notes.push(format!("standing choice not recorded: {e}")),
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
            } => {
                assert_eq!(provider, "deepseek");
                assert_eq!(model.as_deref(), Some("deepseek-reasoner"));
                assert_eq!(key.as_deref(), Some("k1"));
            }
            _ => panic!(),
        }
        assert!(matches!(Slash::parse("models"), Slash::Models));
        assert!(matches!(Slash::parse("flowy dance"), Slash::Help(_)));
    }

    #[test]
    fn a_model_choice_stores_the_key_and_the_standing_choice_or_says_what_is_missing() {
        let d = std::env::temp_dir().join(format!("letibot-slash-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("providers.toml");
        // No key anywhere for grok (the env is not set in this test): refused, with the command.
        // (If the developer's shell exports XAI_API_KEY this arm is skipped.)
        if std::env::var("XAI_API_KEY").is_err() && std::env::var("GROK_API_KEY").is_err() {
            let err = models_choice("grok", Some("grok-4-fast"), None, Some(&f)).unwrap_err();
            assert!(
                err.iter()
                    .any(|l| l.contains("/models grok/grok-4-fast --key PASTE")),
                "{err:?}"
            );
        }
        let (choice, notes) =
            models_choice("grok", Some("grok-4-fast"), Some("xai-test"), Some(&f)).unwrap();
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
        let (none, _) = models_choice("local", None, None, Some(&f)).unwrap();
        assert!(none.is_none());
        assert!(letibot_provider::keys::default_choice(Some(&f)).is_none());
        assert!(models_choice("openai", None, None, Some(&f)).unwrap_err()[0].contains("three"));
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
