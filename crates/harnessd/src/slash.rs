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

/// **The verbs this daemon answers** — the first word of every arm of [`Slash::parse`].
///
/// # Why this is a list at all, when the parser is a `match`
///
/// A head completes `/`-commands from a table, and **a head that does not recognise a verb
/// forwards it here**. So the daemon's verb table is the authority for its half of the
/// namespace, and a head that enumerated its own guess at it would be holding a copy of the
/// other half's knowledge — the `head-run.tools` mistake exactly.
///
/// Measured 2026-09-23, and this is the defect the list exists to end: the head's completion
/// table offered 27 verbs and **five working daemon verbs were not among them** — `/flowy`,
/// `/gate`, `/job`, `/login`, `/supervise`. Every one of them runs. The operator's words,
/// via B's audit of its own tree: *"i want tab completion for /<commands"*, and the cause was
/// not that completion is missing but that **it completes from a different list than the one
/// that dispatches**. `docs/evidence/slash-completion-2026-09-23.py` is that measurement.
///
/// # The shape, and why not one list
///
/// Three registries on one key would be worse than one wrong one, so there are two and they
/// are joined rather than duplicated: the head completes **its own** verbs from its own
/// table (tied to its dispatcher by a test that reads the source, see
/// `crates/tui/src/app.rs`), and completes **these** from a `SettingRow` the daemon
/// publishes. See [`crate::protocol::DAEMON_VERBS_KEY`].
pub const VERBS: &[&str] = &[
    "default-model",
    "flowy",
    "gate",
    "job",
    "login",
    "models",
    "supervise",
    "tools",
];

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
    Job {
        job: Option<String>,
        offset: u64,
    },
    /// **What this conversation can call**, and what it only looks like it can.
    Tools,
    Models,
    /// **Switch what answers THIS conversation.** Nothing else: the standing
    /// choice belongs to [`Slash::DefaultModel`], and it took three goes to get
    /// here.
    ///
    /// `/models X` used to do both at once, which is why — reading its own
    /// listing — the operator asked *"so how do i switch a model for the
    /// conversation?"* about a verb that had been doing exactly that all along:
    /// the sentence it ended on was about `providers.toml`. A `--once` flag was
    /// added to opt out of the half nobody asked for; then a scripted run wrote a
    /// broken model name into the operator's box, and the flag turned out to be
    /// the wrong shape for the problem: *"so /models is sticking to session, if I
    /// need to set default model i will do it how? give me /default-model"*.
    ///
    /// One verb, one effect. `--once` went with the conflation it was patching.
    ModelsSet {
        provider: String,
        model: Option<String>,
        /// A key pasted while switching is still stored — a credential is not a
        /// preference, and making somebody paste it twice is the wrong half to
        /// forget.
        key: Option<String>,
    },
    /// **What a NEW session starts on**, in `providers.toml`. `None` reports it.
    DefaultModel(Option<String>),
    /// **The supervised-labelling verb.** See [`gate`].
    Gate(GateVerb),
    /// Turn the guard model on or off on the running session. `None` reports.
    ///
    /// `at` carries an address the first time somebody names one; it is remembered,
    /// so the second `/supervise` is one word.
    Supervise {
        want: Option<bool>,
        at: Option<String>,
    },
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
    Rule {
        request_id: String,
        kind: &'static str,
        note: String,
    },
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
            // Three spellings, because all three are what somebody reaches for.
            Some("default-model") | Some("default_model") | Some("default") => {
                Slash::DefaultModel(words.get(1).map(|w| w.to_string()))
            }
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
                    Slash::ModelsSet { provider, model, key }
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
    // Read once for the whole listing rather than per provider: it is a 4MB file.
    let cat = letibot_provider::catalogue::Catalogue::load();
    let mut lines = vec![format!("models — now answering: {current}")];
    lines.push(
        "  local the llama.cpp server this daemon was started against   /models local".into(),
    );
    // **This fleet's own models, above the presets**, because they cost nothing and
    // a listing that buries them under five metered providers is a listing that
    // reads as "the choices are cloud".
    for m in letibot_provider::keys::local_models(None) {
        let sampling = if m.profile.sampling.is_empty() {
            "the built-in sampling".to_string()
        } else {
            m.profile
                .sampling
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        lines.push(format!(
            "  {:<16} {:<18} at {}   {sampling}   /models {}",
            m.name, m.model, m.url, m.name
        ));
    }
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
            p.name,
            p.default_model(&cat),
            p.name
        ));
    }
    lines.push(format!(
        "  the standing choice is in {} ([default]); /models writes it",
        letibot_provider::keys::config_file().display()
    ));
    lines
}

/// **What `/models NAME` resolved to.** Three kinds, because there are three,
/// and an `Option<ProviderConfig>` could only say two.
///
/// It was `Option<ProviderConfig>`: `Some` a metered provider, `None` the daemon's
/// own server. A declared local model is neither — it takes the local path and moves
/// where that path points — and squeezing it into `None` would have made "back to
/// this daemon's model" and "over to the 27B on .78" the same value.
///
/// **Two of the three are local**, and that is the axis this tree already turns on:
/// local means the prefix is reusable, the tokens are counted here against this
/// session's own vocabulary, and nothing is billed. `metered` is the other side of
/// that line. A box on the LAN is on the local side of it, so it is `Local` rather
/// than a third category named for the fleet.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelChoice {
    /// `/models local` — this daemon's own server, exactly as it was started.
    OwnServer,
    /// **A local model this fleet declares** — a `[model."…"]` block with a url.
    /// No key, no meter, tokenized here like any other local model.
    Local(letibot_provider::keys::LocalModel),
    /// A cloud preset, metered.
    Metered(ProviderConfig),
}

/// Resolve a `/models PROVIDER[/MODEL] [--key K]` into a config for THIS session.
///
/// It stores a pasted key and nothing else. The standing choice is
/// [`default_model`]'s job — see [`Slash::ModelsSet`] for why the two were split.
pub fn models_choice(
    provider: &str,
    model: Option<&str>,
    key: Option<&str>,
    file: Option<&std::path::Path>,
) -> Result<(ModelChoice, Vec<String>), Vec<String>> {
    let mut notes = Vec::new();
    if provider == "local" {
        return Ok((ModelChoice::OwnServer, notes));
    }
    // **The operator's own names first.** A fleet block is something they wrote in
    // their own file; a preset is a name this binary ships. If the two ever collide,
    // the file wins, because the person who typed the name also typed the block.
    if let Some(m) = letibot_provider::keys::local_models(file)
        .into_iter()
        .find(|m| m.name == provider)
    {
        if !m.profile.unknown.is_empty() {
            notes.push(format!(
                "[model.\"{}\"] has keys this does not read: {}",
                m.name,
                m.profile.unknown.join(", ")
            ));
        }
        return Ok((ModelChoice::Local(m), notes));
    }
    let preset = letibot_provider::Preset::parse(provider).map_err(|e| {
        let fleet = letibot_provider::keys::local_models(file);
        if fleet.is_empty() {
            vec![e]
        } else {
            vec![
                e,
                format!(
                    "  this fleet declares: {}",
                    fleet
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ]
        }
    })?;
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
    Ok((
        ModelChoice::Metered(ProviderConfig {
            name: preset.name.to_string(),
            model: model.map(str::to_string),
            api_key: None,
            thinking: false,
        }),
        notes,
    ))
}

/// **`/default-model` — what a NEW session starts on.**
///
/// The other half of what `/models` used to do in one breath. Separated because
/// one verb doing two things is what made the operator ask how to do either:
/// *"so how do i switch a model for the conversation?"* about the verb that
/// switched it, and then *"if I need to set default model i will do it how?"*
/// about the verb that set it.
///
/// `None` reports. `local` clears the standing choice, which is the honest way to
/// say "new sessions use this daemon's own model" — there is no `[default]` that
/// means local, so the row is removed rather than written with a name.
pub fn default_model(want: Option<&str>, file: Option<&std::path::Path>) -> SlashReply {
    let Some(want) = want else {
        let now = letibot_provider::keys::default_choice(file);
        let path = file
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| letibot_provider::keys::config_file().display().to_string());
        return SlashReply {
            lines: vec![
                match now {
                    Ok(Some(d)) => format!(
                        "new sessions start on {}{}, from [default] in {path}",
                        d.provider,
                        d.model.map(|m| format!("/{m}")).unwrap_or_default()
                    ),
                    Ok(None) => format!(
                        "no standing choice in {path}, so a new session starts on the \
                         local server its daemon was launched against"
                    ),
                    // The file is there and could not be read: a fault, not an
                    // absence, so the absence sentence must not be printed. The
                    // path and the parser's own message are the fix.
                    Err(u) => format!(
                        "{u} — the standing choice could not be read, so a new \
                         session starts on the local server its daemon was launched \
                         against"
                    ),
                },
                "`/default-model PROVIDER[/MODEL]` sets it; `/default-model local` \
                 clears it. This conversation is unaffected either way — `/models` is \
                 the verb for that."
                    .into(),
            ],
            ok: true,
        };
    };
    if want == "local" {
        return match letibot_provider::keys::set_default(file, "local", None) {
            Ok(f) => SlashReply {
                lines: vec![format!(
                    "new sessions start on their daemon's own local model; the standing \
                     choice in {} is cleared. This conversation is unchanged.",
                    f.display()
                )],
                ok: true,
            },
            Err(e) => SlashReply {
                lines: vec![e],
                ok: false,
            },
        };
    }
    let (provider, model) = match want.split_once('/') {
        Some((p, m)) => (p, Some(m)),
        None => (want, None),
    };
    let preset = match letibot_provider::Preset::parse(provider) {
        Ok(p) => p,
        Err(e) => {
            return SlashReply {
                lines: vec![e],
                ok: false,
            };
        }
    };
    // The key is checked before the choice is written, so a default nothing can
    // authenticate is never left for the next daemon to discover at its first turn.
    if let Err(e) = letibot_provider::keys::resolve(preset, None, file) {
        return SlashReply {
            lines: vec![
                format!("{e}"),
                format!("  /models {} --key PASTE stores it", preset.name),
                "Nothing was written: a standing choice nothing can authenticate would \
                 refuse at the first turn of every session that inherited it."
                    .into(),
            ],
            ok: false,
        };
    }
    match letibot_provider::keys::set_default(file, preset.name, model) {
        Ok(f) => SlashReply {
            lines: vec![
                format!(
                    "new sessions start on {}/{}, written to [default] in {}",
                    preset.name,
                    model
                        .map(str::to_string)
                        .unwrap_or_else(|| preset
                            .default_model(&letibot_provider::catalogue::Catalogue::load())),
                    f.display()
                ),
                "This conversation is unchanged — `/models` switches the one you are in.".into(),
            ],
            ok: true,
        },
        Err(e) => SlashReply {
            lines: vec![e],
            ok: false,
        },
    }
}

#[cfg(test)]
mod tests {

    /// **A fleet model is selectable by name**, and it is not a provider: no key is
    /// looked for and none is needed.
    #[test]
    fn a_fleet_block_is_a_choice_models_can_resolve() {
        let f = fleet_file("a_fleet_block_is_a_choice");
        let (choice, notes) = models_choice("dense78", None, None, Some(&f)).expect("resolving");
        match choice {
            ModelChoice::Local(m) => {
                assert_eq!(m.name, "dense78");
                assert_eq!(m.model, "qwen-3.8-27b");
                assert_eq!(m.url, "http://192.168.1.78:8082");
                assert_eq!(
                    m.profile.sampling.get("temperature").unwrap().as_f64(),
                    Some(0.7)
                );
                assert_eq!(
                    m.profile
                        .sampling
                        .get("thinking_budget_tokens")
                        .unwrap()
                        .as_i64(),
                    Some(1024),
                    "a llama.cpp knob survives: this is not the OpenAI shape"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// **The operator's own names come first.** The file is theirs; the preset list is
    /// this binary's. A collision resolves to the thing the person typed a block for.
    #[test]
    fn a_fleet_name_wins_over_a_preset_of_the_same_name() {
        let f = tmp_file(
            "a_fleet_name_wins",
            "[model.\"grok\"]\nurl = \"http://192.168.1.78:8082\"\nmodel = \"qwen-3.8-27b\"\n",
        );
        let (choice, _) = models_choice("grok", None, None, Some(&f)).expect("resolving");
        assert!(
            matches!(&choice, ModelChoice::Local(m) if m.model == "qwen-3.8-27b"),
            "{choice:?}"
        );
    }

    /// **A name nobody declared lists what is declared.** The old refusal named the
    /// five presets and stopped, which on a box whose models are all in that file
    /// answers a question the operator was not asking.
    #[test]
    fn an_unknown_name_names_this_fleets_own_models() {
        let f = fleet_file("an_unknown_name_names");
        let e = models_choice("dense97", None, None, Some(&f)).unwrap_err();
        assert!(
            e.iter().any(|l| l.contains("dense78")),
            "the refusal shows what this fleet has: {e:?}"
        );
    }

    /// A key the reader does not know is reported when the choice is made, not
    /// swallowed into a profile that silently lacks it.
    #[test]
    fn a_misspelled_sampling_key_is_reported_on_the_switch() {
        let f = tmp_file(
            "a_misspelled_key_on_switch",
            "[model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\ntemperatuer = 0.7\n",
        );
        let (_, notes) = models_choice("dense78", None, None, Some(&f)).expect("resolving");
        assert!(notes.iter().any(|n| n.contains("temperatuer")), "{notes:?}");
    }

    fn tmp_file(tag: &str, body: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-slash-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).expect("scratch");
        let f = d.join("providers.toml");
        std::fs::write(&f, body).expect("write");
        f
    }

    fn fleet_file(tag: &str) -> std::path::PathBuf {
        tmp_file(
            tag,
            "[model.qwen]\neffort = \"low\"\n\n\
             [model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\n\
             model = \"qwen-3.8-27b\"\ntemperature = 0.7\ntop_p = 0.8\ntop_k = 20\n\
             min_p = 0\npresence_penalty = 1.5\nmax_tokens = 2048\n\
             thinking_budget_tokens = 1024\n",
        )
    }

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
            Slash::Supervise {
                want: Some(true),
                at: None
            }
        );
        assert_eq!(
            Slash::parse("supervise on"),
            Slash::Supervise {
                want: Some(true),
                at: None
            }
        );
        assert_eq!(
            Slash::parse("supervise off"),
            Slash::Supervise {
                want: Some(false),
                at: None
            }
        );
        assert_eq!(
            Slash::parse("supervise status"),
            Slash::Supervise {
                want: None,
                at: None
            }
        );
        // An address is the one-off override, and anything with a colon is one —
        // refusing it in favour of a daemon flag is the ceremony this verb removes.
        assert_eq!(
            Slash::parse("supervise 192.168.1.76:8090"),
            Slash::Supervise {
                want: Some(true),
                at: Some("192.168.1.76:8090".into())
            }
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

    /// `/job` with no argument is the listing, not a refusal, and `--offset`
    /// is not mistaken for a job id — the operator reaching a second page types
    /// `/job ID --offset N`, but a flag alone must still list.
    #[test]
    fn job_parses_bare_with_an_id_and_with_an_offset() {
        assert!(matches!(
            Slash::parse("job"),
            Slash::Job {
                job: None,
                offset: 0
            }
        ));
        assert!(matches!(
            Slash::parse("jobs"),
            Slash::Job {
                job: None,
                offset: 0
            }
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
            Slash::Job {
                job: None,
                offset: 10
            }
        ));
    }

    /// `/tools` is the listing, not the fold. The fold kept ctrl-t and `/t`.
    #[test]
    fn tools_is_a_daemon_verb_now() {
        assert!(matches!(Slash::parse("tools"), Slash::Tools));
    }

    /// **One verb, one effect.** `/models` switches this conversation;
    /// `/default-model` says what a new one starts on. They were the same verb,
    /// and that is why the operator had to ask how to do each of them.
    #[test]
    fn models_switches_the_session_and_default_model_is_its_own_verb() {
        match Slash::parse("models deepseek/deepseek-flash") {
            Slash::ModelsSet {
                provider,
                model,
                key,
            } => {
                assert_eq!(provider, "deepseek");
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
                assert!(key.is_none());
            }
            other => panic!("{other:?}"),
        }
        // All three spellings reach the standing choice, because all three are
        // what somebody reaches for.
        for line in ["default-model", "default_model", "default"] {
            assert!(
                matches!(Slash::parse(line), Slash::DefaultModel(None)),
                "{line}"
            );
        }
        assert!(matches!(
            Slash::parse("default-model deepseek/deepseek-flash"),
            Slash::DefaultModel(Some(ref m)) if m == "deepseek/deepseek-flash"
        ));
    }

    /// Switching a session writes no standing choice — the bug that put a model
    /// name deepseek rejects into the operator's box, from a scripted test run.
    #[test]
    fn switching_a_session_leaves_providers_toml_alone() {
        let d = std::env::temp_dir().join(format!(
            "letibot-split-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&d).expect("scratch");
        let f = d.join("providers.toml");
        let (choice, notes) =
            models_choice("grok", None, Some("xai-test"), Some(&f)).expect("choosing");
        assert!(
            matches!(choice, ModelChoice::Metered(_)),
            "the session switches: {choice:?}"
        );
        assert!(
            notes.join("\n").contains("stored the grok key"),
            "{notes:?}"
        );
        let on_disk = std::fs::read_to_string(&f).unwrap_or_default();
        assert!(
            on_disk.contains("xai-test"),
            "the key is written: {on_disk}"
        );
        assert!(
            !on_disk.contains("[default]"),
            "and the standing choice is not: {on_disk}"
        );

        // `/default-model` is what writes it, and it reports before it is set.
        let before = default_model(None, Some(&f));
        assert!(
            before.lines[0].contains("no standing choice"),
            "{:?}",
            before.lines
        );
        let set = default_model(Some("grok"), Some(&f));
        assert!(set.ok, "{:?}", set.lines);
        assert!(
            set.lines[0].contains("new sessions start on grok/"),
            "{:?}",
            set.lines
        );
        assert!(
            set.lines[1].contains("This conversation is unchanged"),
            "{:?}",
            set.lines
        );
        assert!(std::fs::read_to_string(&f).unwrap().contains("[default]"));

        // And a default nothing can authenticate is refused rather than left for
        // the next daemon to discover at its first turn.
        let bad = default_model(Some("glm"), Some(&f));
        assert!(!bad.ok);
        assert!(
            bad.lines.last().unwrap().contains("Nothing was written"),
            "{:?}",
            bad.lines
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_model_choice_stores_the_key_or_says_what_is_missing() {
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
        assert!(
            matches!(&choice, ModelChoice::Metered(pc) if pc.name == "grok"),
            "{choice:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("stored the grok key")),
            "{notes:?}"
        );
        // **And no standing choice.** This used to write one, which is the
        // conflation `/default-model` was split out of: switching a session is
        // not a statement about every session this box opens afterwards.
        assert!(
            !notes.iter().any(|n| n.contains("standing choice")),
            "{notes:?}"
        );
        assert!(letibot_provider::keys::default_choice(Some(&f)).unwrap().is_none());
        let (local, _) = models_choice("local", None, None, Some(&f)).unwrap();
        assert_eq!(local, ModelChoice::OwnServer);
        assert!(models_choice("openai", None, None, Some(&f)).unwrap_err()[0].contains("five"));
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
            return SlashReply {
                lines: vec![format!("opening {}: {e}", path.display())],
                ok: false,
            };
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
                    format!("  {} where an oracle was actually consulted", c.measured),
                    format!(
                        "  {} where you and the model differ — the rows a fine-tune is for",
                        c.disagreements
                    ),
                    format!("  {} decided by a model, not by a rule", c.model_decided),
                    format!(
                        "  {} decided by a rule or a mode with nobody asked",
                        c.total.saturating_sub(c.decided_by_operator)
                    ),
                    // **The two columns nothing writes** (R11). Named here rather than
                    // left for a reader to discover: `p_allow` has no producer because
                    // the oracle this build ships answers with a hard label and the
                    // logprob encoding that would give it a number is not built, and
                    // `oracle_model` is empty because the model is already named in
                    // full on `verdict_by` (`model oracle \`qwen-3.8-27b\` at …`).
                    "  p_allow and oracle_model are NULL on every row: neither has a \
                     producer — see `CorpusRow` in `letibot-tools`. A reader must not \
                     read their NULL as a measurement."
                        .to_string(),
                ],
                ok: true,
            },
            Err(e) => SlashReply {
                lines: vec![e.to_string()],
                ok: false,
            },
        },

        GateVerb::Recent {
            limit,
            only_unlabelled,
        } => {
            // `corpus(only_labelled)` narrows the other way, so the unlabelled queue
            // is filtered here rather than by asking for a set that excludes itself.
            let rows = match store.corpus(false, if *only_unlabelled { limit * 8 } else { *limit })
            {
                Ok(r) => r,
                Err(e) => {
                    return SlashReply {
                        lines: vec![e.to_string()],
                        ok: false,
                    };
                }
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

        GateVerb::Rule {
            request_id,
            kind,
            note,
        } => {
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
                Err(e) => SlashReply {
                    lines: vec![e.to_string()],
                    ok: false,
                },
            }
        }
    }
}

#[cfg(test)]
mod the_verb_table_is_the_parser {
    use super::*;

    /// **Every arm's first word is in [`VERBS`], and every name there has an arm.**
    ///
    /// The list is what a head offers and the `match` is what runs, so the two drifting
    /// apart is the whole defect this pair exists against — and the failure mode is not a
    /// crash: a verb in the table with no arm reaches `Some(other) => Slash::Help`, which
    /// answers *"is not a daemon verb"* about something the head just completed. A test that
    /// reads the source is the only mechanism available, because match arms are not
    /// reflectable and a hand-maintained second list is the thing being fixed.
    #[test]
    fn the_table_and_the_parser_agree() {
        let src = include_str!("slash.rs");
        // The `match words.first()` block, by brace balance from `pub fn parse`.
        let start = src
            .find("    pub fn parse(line: &str) -> Slash {")
            .expect("the parser");
        let mut depth = 0usize;
        let mut end = start;
        for (i, c) in src[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        // The verb-deciding arms sit two braces deep: the fn, then the `match`.
        let mut arms: Vec<&str> = Vec::new();
        let mut d = 0i32;
        for line in src[start..end].split('\n') {
            let t = line.trim();
            if d == 2
                && let Some(rest) = t.strip_prefix("Some(\"")
                && let Some((name, _)) = rest.split_once('"')
            {
                arms.push(name);
            }
            d += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        }
        assert!(!arms.is_empty(), "no arms found — the parser moved");

        // Aliases collapsed to the spelling [`VERBS`] carries: the table is what a head
        // OFFERS, and offering `/default` and `/default_model` beside `/default-model`
        // teaches one verb three times.
        fn canon(v: &str) -> &str {
            match v {
                "default" | "default_model" => "default-model",
                "model" => "models",
                "supervised" => "supervise",
                other => other,
            }
        }
        for a in &arms {
            let c = canon(a);
            assert!(
                VERBS.contains(&c),
                "`/{a}` has an arm and is not in VERBS, so no head will ever offer it"
            );
        }
        for v in VERBS {
            assert!(
                arms.iter().any(|a| canon(a) == *v),
                "`/{v}` is offered and the parser has no arm for it — it would answer \
                 `is not a daemon verb` about a verb the head just completed"
            );
        }
    }
}
