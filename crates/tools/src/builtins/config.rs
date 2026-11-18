//! `config`: a session proposes a change to letibot's own configuration, and the OPERATOR applies
//! it by answering a card. The config dir stays masked inside a session — a model that edits its
//! own boundary grants itself anything — so this is a door, not a hole: nothing here is written
//! except on the operator's own answer, and no answer but theirs counts.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name, in one place.
pub const TOOL: &str = "config";

/// What a session puts to the operator: to see a config file, or to have a change written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAsk {
    /// `show`: the file itself, redacted, read into the transcript.
    Show { file: String },
    /// `propose`: one change, as the diff a person reads before deciding.
    Propose {
        file: String,
        diff: String,
        why: String,
        /// The two panels a head draws, made once by the desk that had both sides.
        preview: Vec<letibot_transcript::ToolEditExcerpt>,
    },
}

impl ConfigAsk {
    pub fn file(&self) -> &str {
        match self {
            ConfigAsk::Show { file } | ConfigAsk::Propose { file, .. } => file,
        }
    }

    /// The card's headline: one sentence naming what is being asked and of whom.
    pub fn summary(&self) -> String {
        match self {
            ConfigAsk::Show { file } => format!(
                "`config` asks to read `{file}` from the operator's config directory, which is \
                 masked inside this session"
            ),
            ConfigAsk::Propose { file, .. } => format!(
                "`config` proposes a change to `{file}` in the operator's config directory, to \
                 be written only if they say yes"
            ),
        }
    }
}

/// The answer. `Granted` carries who answered, so the result can say whose yes it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAnswer {
    Granted {
        by: String,
    },
    Refused {
        by: String,
        why: String,
    },
    /// Nobody answered, or there was nobody who could: not a refusal, and not a yes.
    NotAsked(String),
}

/// One file, as the desk read it: the bytes, and how a credential was taken out of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub path: String,
    pub text: String,
    /// What redaction did, in one sentence.
    pub note: String,
    /// The mode the file has, as octal (`600`), or `None` for a file that is not there.
    pub mode: Option<u32>,
}

/// A proposal: the bytes a yes would write, the diff a person reads, and what was checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub path: String,
    /// The file's bytes before, so a yes writes only a file that has not moved under the card.
    pub before: String,
    /// The bytes a yes writes.
    pub text: String,
    /// Unified diff, `a/` and `b/` naming the file's own name.
    pub diff: String,
    /// Which reader parsed the result, and any note about a key it could not settle.
    pub check: String,
    /// The card's two panels, as the head draws an edit.
    pub preview: Vec<letibot_transcript::ToolEditExcerpt>,
}

/// What a yes wrote: the path, its size, its mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: String,
    pub bytes: usize,
    pub mode: Option<u32>,
}

/// The daemon's half: the operator's config directory, the readers that judge a file, and the one
/// write. Every method answers a sentence or a refusal in words.
pub trait ConfigDesk: Send + Sync {
    /// Whether there is a config directory behind this tool at all. Asked before a card, so a
    /// session that has none is told so instead of being shown a question nothing can answer.
    fn ready(&self) -> Result<(), String>;
    /// What the files are, what one file decides, or what one key does.
    fn explain(&self, file: Option<&str>, key: Option<&str>) -> Result<String, String>;
    /// One file's bytes with every credential value replaced.
    fn show(&self, file: &str) -> Result<Shown, String>;
    /// Apply `edits` in memory, judge the result with the file's own reader, and describe it.
    fn propose(
        &self,
        file: &str,
        edits: &letibot_config_ref::edit::Edits,
    ) -> Result<Proposal, String>;
    /// Write `text` atomically, keeping the mode — but only over `before`, so a file somebody
    /// changed while the card was up is not overwritten by an answer about the old bytes.
    fn write(&self, file: &str, before: &str, text: &str) -> Result<Written, String>;
}

/// No config dir behind the tool — a registry outside a daemon, or a test. Every verb says so by
/// name rather than the tool being absent, which would read as a capability this build lacks.
pub struct NoConfig;

impl NoConfig {
    const WHY: &'static str =
        "this session's daemon serves no config directory, so there is nothing to read or propose";
}

impl ConfigDesk for NoConfig {
    fn ready(&self) -> Result<(), String> {
        Err(Self::WHY.into())
    }
    fn explain(&self, _file: Option<&str>, _key: Option<&str>) -> Result<String, String> {
        Err(Self::WHY.into())
    }
    fn show(&self, _file: &str) -> Result<Shown, String> {
        Err(Self::WHY.into())
    }
    fn propose(
        &self,
        _file: &str,
        _edits: &letibot_config_ref::edit::Edits,
    ) -> Result<Proposal, String> {
        Err(Self::WHY.into())
    }
    fn write(&self, _file: &str, _before: &str, _text: &str) -> Result<Written, String> {
        Err(Self::WHY.into())
    }
}

/// The tool.
pub struct ConfigTool {
    desk: Arc<dyn ConfigDesk>,
}

impl ConfigTool {
    pub fn new(desk: Arc<dyn ConfigDesk>) -> Self {
        ConfigTool { desk }
    }
}

fn text<'a>(args: &'a Value, field: &str) -> Option<&'a str> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The file a call names, refused by name unless it is one the reference describes.
///
/// This runs before any card is raised, and for a better reason than cost: a card asking the
/// operator to approve a path a tool should never have offered teaches them to answer without
/// reading.
pub fn file_named(args: &Value) -> Result<&'static letibot_config_ref::File, String> {
    let Some(name) = text(args, "file") else {
        return Err(format!(
            "`file` is required: one of {}",
            known_files().join(", ")
        ));
    };
    if name.starts_with("keys") || name.contains('/') || name.contains("..") {
        return Err(format!(
            "`{name}` is not a config file this tool opens. `keys/` holds ssh identities and is \
             never read or written by a tool, and every file here is named by its own name under \
             the config dir — {}",
            known_files().join(", ")
        ));
    }
    if name == "theme.toml" {
        // A palette is `themes/NAME.toml`, chosen by `head.toml`'s `theme`, so there is no one
        // file this could mean — said here rather than after a card nobody can answer with a yes.
        return Err(
            "`theme.toml` is a name and not a file: palettes are `themes/NAME.toml` beside \
             `head.toml`, chosen by `theme = \"NAME\"`. Edit the palette you mean, or propose a \
             change to `head.toml`'s `theme` to pick another."
                .to_string(),
        );
    }
    let known = known_files();
    match letibot_config_ref::find(name) {
        Some(f) => Ok(f),
        None => Err(format!(
            "`{name}` is not one of letibot's config files: {}. `config explain` with no name \
             lists them, with what each one decides",
            known.join(", ")
        )),
    }
}

/// The names, from the reference, so the tool and the docs cannot drift.
pub fn known_files() -> Vec<&'static str> {
    letibot_config_ref::ALL.iter().map(|f| f.name).collect()
}

/// Whether `path` sets or changes a credential, as the refusal's words, or `None` for a key that
/// is an ordinary setting.
///
/// The whole path is walked, not the leaf: `perimeter.p.env.GH_TOKEN` is a secret whatever the
/// last segment is called, and a session that could write one could hand itself a token the
/// operator never granted.
pub fn credential_path(path: &str) -> Option<String> {
    for segment in path.split('.') {
        let leaf = segment.trim();
        if matches!(leaf, "headers" | "env") || letibot_config_ref::lookup::is_credential(leaf) {
            return Some(format!(
                "`{path}` sets a credential (`{leaf}`): a key, a token, a password, or an env or \
                 headers table that can carry one. A session may not write one at all — not with \
                 a card, not with a reason — so this is refused before the operator is asked. \
                 Put it in the file yourself."
            ));
        }
    }
    None
}

/// `edit`, as the proposal's changes: each key is a dotted path, each value a TOML value, and
/// `null` means take the key out.
pub fn edits_of(args: &Value) -> Result<letibot_config_ref::edit::Edits, String> {
    let Some(edit) = args.get("edit") else {
        return Err(
            "`edit` is required: an object of dotted key paths to values, e.g. \
             {\"shadow.answer_reads\": true}; a null value removes the key"
                .into(),
        );
    };
    let Some(obj) = edit.as_object() else {
        return Err(format!(
            "`edit` is an object of key paths to values, and `{edit}` is not one"
        ));
    };
    if obj.is_empty() {
        return Err("`edit` names no key, so there is nothing to propose".into());
    }
    let mut edits = letibot_config_ref::edit::Edits::default();
    for (path, value) in obj {
        if value.is_null() {
            edits.remove.push(path.clone());
        } else {
            edits.set.insert(
                path.clone(),
                json_to_toml(value).map_err(|why| format!("`{path}`: {why}"))?,
            );
        }
    }
    Ok(edits)
}

/// A JSON value as TOML source. Strings are quoted here rather than by the caller: `"45m"` and
/// `45m` are different values, and a proposal that guessed would write one for the other.
pub fn json_to_toml(v: &Value) -> Result<String, String> {
    Ok(match v {
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("{s:?}"),
        Value::Array(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if item.is_null() {
                    return Err(
                        "null is how a whole key is removed, not what an array item is".into(),
                    );
                }
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&json_to_toml(item)?);
            }
            out.push(']');
            out
        }
        Value::Object(map) => {
            let mut out = String::from("{ ");
            for (i, (k, item)) in map.iter().enumerate() {
                if item.is_null() {
                    return Err(
                        "null is how a whole key is removed, not what a table entry is".into(),
                    );
                }
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("{} = {}", toml_key(k), json_to_toml(item)?));
            }
            out.push_str(" }");
            out
        }
        Value::Null => return Err("null is how a whole key is removed".into()),
    })
}

fn toml_key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        format!("{k:?}")
    }
}

impl Tool for ConfigTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "Letibot's own configuration, in the operator's config directory. `explain` says what \
             each file is and what a key does, from the reference the daemon reads (no file is \
             opened). `show` prints one file with every credential value redacted. `propose` \
             applies `edit` in memory — dotted key paths to TOML values, `null` removes a key — \
             keeps the file's comments and layout, checks the result with the reader the daemon \
             itself uses, and puts the diff and `why` to the operator as a card. ONLY the \
             operator's yes writes it: no mode, no session or always grant and no model answers \
             this card. A path under `keys/`, a file the reference does not list, and any edit \
             that sets a credential are refused before anybody is asked. The result says what \
             changed and what a restart means for it.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["explain", "show", "propose"],
                        "description": "`explain` — what the files are, what one file decides (with `file`), or what one key does (`key`, a dotted path such as `shadow.answer_reads`); `show` — one file's current content (`file`); `propose` — a change to one file (`file`, `edit`, `why`), which the operator answers."
                    },
                    "file": {
                        "type": "string",
                        "description": "A file name under the config directory, exactly as `explain` lists it: `leticode.toml`, `providers.toml`, `hosts.toml`, `prompts.toml`, `permission.json`, `plugin-trust.json`, `modes.tsv`. Not a path, and never under `keys/`."
                    },
                    "key": {
                        "type": "string",
                        "description": "For `explain`: a dotted key path (`shadow.answer_reads`) or one word (`answer_reads`) to find every key it names, with its type, default and meaning."
                    },
                    "edit": {
                        "type": "object",
                        "description": "For `propose`: dotted key paths to new values, e.g. `{\"shadow.answer_reads\": true, \"exec.foreground_seconds\": 90}`. A JSON string is a TOML string, so quote a span: `\"45m\"`. A `null` value removes the key."
                    },
                    "why": {
                        "type": "string",
                        "description": "For `propose`: the problem the change solves, in your words. It is the card's own line and the operator reads it before deciding."
                    }
                },
                "required": ["action"]
            }),
            // Read at the gate, and the write is behind a card of its own: what this tool writes
            // is answered by the operator and by nobody else, so a second, weaker class here
            // would be a second question about one file — and at a mode that admits writes
            // unasked it would be no question at all.
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        match text(args, "action").unwrap_or("") {
            "explain" => match self.explain(args) {
                Ok(said) => Invocation::ok(said),
                Err(why) => Invocation::failed("`config explain` could not answer", why),
            },
            "show" => match self.show(ctx, args) {
                Ok(said) => Invocation::ok(said),
                Err(why) => Invocation::failed("nothing was shown", why),
            },
            "propose" => match self.propose(ctx, args) {
                Ok(said) => Invocation::ok(said),
                Err(why) => Invocation::failed("nothing was written", why),
            },
            other => Invocation::failed(
                format!("`{other}` is not an action of the `config` tool"),
                "the actions are \"explain\", \"show\" and \"propose\"".to_string(),
            ),
        }
    }
}

impl ConfigTool {
    fn explain(&self, args: &Value) -> Result<String, String> {
        let file = text(args, "file");
        let key = text(args, "key");
        if file.is_some() {
            // A name the reference does not carry is refused here, before the desk is asked to
            // describe a list of keys that does not exist.
            file_named(args)?;
        }
        if file.is_none() && key.is_none() {
            return Ok(format!(
                "{}\n\nEvery setting `propose` may name is here, and `show` prints a file with \
                 its credentials redacted. Nothing is read from the disk to answer this.",
                letibot_config_ref::overview().trim_end()
            ));
        }
        self.desk.explain(file, key)
    }

    fn show(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Result<String, String> {
        let f = file_named(args)?;
        self.desk.ready()?;
        let by = operator_only(
            ctx,
            &ConfigAsk::Show {
                file: f.name.to_string(),
            },
        )?;
        let shown = self.desk.show(f.name)?;
        let mode = shown
            .mode
            .map(|m| format!(", mode {m:03o}"))
            .unwrap_or_default();
        Ok(format!(
            "{} ({}{mode}) — {}. Read into this transcript on {by}'s word.\n\n{}",
            shown.path, f.name, shown.note, shown.text
        ))
    }

    fn propose(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Result<String, String> {
        let f = file_named(args)?;
        self.desk.ready()?;
        let edits = edits_of(args)?;
        let why = text(args, "why").ok_or_else(|| {
            "`why` is required: it is the line the operator reads before deciding".to_string()
        })?;
        // Before any card: the reference must know every key, and no key it names may be a
        // credential.
        let mut unknown = Vec::new();
        for path in edits.paths() {
            if let Some(why) = credential_path(path) {
                return Err(why);
            }
            if letibot_config_ref::lookup::describe(f, path).is_none() {
                unknown.push(path.clone());
            }
        }
        if !unknown.is_empty() {
            return Err(format!(
                "`{}` has no {}: the reference lists every key it reads, and a key it does not \
                 name would be refused by the reader or ignored. `config explain` with file \
                 \"{}\" lists them; `key` takes one word to find the closest.",
                f.name,
                if unknown.len() == 1 {
                    format!("key `{}`", unknown[0])
                } else {
                    format!("keys {}", unknown.join(", "))
                },
                f.name
            ));
        }
        let proposal = self.desk.propose(f.name, &edits)?;
        let by = operator_only(
            ctx,
            &ConfigAsk::Propose {
                file: f.name.to_string(),
                diff: proposal.diff.clone(),
                why: why.to_string(),
                preview: proposal.preview.clone(),
            },
        )?;
        let written = self.desk.write(f.name, &proposal.before, &proposal.text)?;
        let changed = edits
            .paths()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "{changed} written to {} ({} bytes{}), on {by}'s word.\n{}\n\n{}",
            written.path,
            written.bytes,
            written
                .mode
                .map(|m| format!(", mode {m:03o}"))
                .unwrap_or_default(),
            proposal.check,
            f.reload
        ))
    }
}

/// Put the card to the operator and refuse everything that is not their own answer. The rule
/// lives here as well as in the gate, because this is where a caller can see it: a session, a
/// bridge seat or a guard model answering in their place is not the operator, and its words say so.
fn operator_only(ctx: &InvokeCtx<'_>, ask: &ConfigAsk) -> Result<String, String> {
    match ctx.ask_config(ask) {
        ConfigAnswer::Granted { by } if by.starts_with("human:") => Ok(by),
        ConfigAnswer::Granted { by } => Err(format!(
            "`{by}` answered, and only the operator may: a mode, a session or always grant, a \
             bridge seat and a model are all refused here, because a change to letibot's own \
             configuration is a change to what may run unasked. Nothing was written or shown."
        )),
        ConfigAnswer::Refused { by, why } => {
            if by.starts_with("human:") {
                Err(format!("refused by {by}: {why}. Nothing was written."))
            } else {
                Err(format!(
                    "`{by}` answered and was not the operator, so nothing was written: only the \
                     operator's own answer applies a config change. Their words: {why}"
                ))
            }
        }
        ConfigAnswer::NotAsked(why) => Err(format!(
            "nobody could be asked, so nothing was written: {why}. A config change happens on \
             the operator's own answer and on nothing else."
        )),
    }
}

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
