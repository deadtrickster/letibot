//! The daemon's half of the `config` tool: the operator's config directory, the readers that judge
//! a proposed file, the diff a person decides on, and the one atomic write.
//!
//! The directory itself stays masked inside a session — that is the defence this door does not
//! remove. What the daemon holds is the ability to read and write it, which is only ever spent on
//! the operator's own answer: [`SessionConfig`] never asks anybody anything, it just does what the
//! tool has already been allowed to ask for.

use std::path::{Path, PathBuf};

use letibot_config_ref as cref;
use letibot_tools::builtins::config::{ConfigDesk, Proposal, Shown, Written};
use std::os::unix::fs::PermissionsExt;

/// Lines of context either side of a change, as any diff a person reads carries.
const CONTEXT: usize = 3;

/// The config dir this session's `config` tool speaks for.
pub struct SessionConfig {
    dir: PathBuf,
}

impl SessionConfig {
    /// The process's own config dir: what the launcher, the daemon and the head all read.
    pub fn here() -> SessionConfig {
        SessionConfig::at(
            letibot_paths::config_dir().unwrap_or_else(|| PathBuf::from("/.config/letibot")),
        )
    }

    /// A directory of the caller's choosing, which is what a test uses: no fixture of this tree
    /// ever reads or writes the operator's real one.
    pub fn at(dir: impl Into<PathBuf>) -> SessionConfig {
        SessionConfig { dir: dir.into() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// The file a name addresses, or a refusal naming why it addresses none. The tool refuses an
/// unknown name before this; the checks are repeated here because this is also a public door.
fn file_for(name: &str) -> Result<&'static cref::File, String> {
    if name == "theme.toml" {
        return Err(
            "`theme.toml` is a name and not a file: palettes are `themes/NAME.toml` beside \
             `head.toml`, chosen by `theme = \"NAME\"`, so there is no one file to read or write. \
             Edit the palette you mean, or change `head.toml`'s `theme` to pick another."
                .to_string(),
        );
    }
    cref::find(name).ok_or_else(|| {
        format!(
            "`{name}` is not one of letibot's config files: {}",
            cref::ALL
                .iter()
                .map(|f| f.name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// The reader that judges `text` for this file, and a sentence naming what was checked.
///
/// The readers are the daemon's own, reached through the door each parser grew for bytes: a file
/// the daemon would refuse is refused here, in the words it would have used. A file whose reader
/// lives in another process — the head's `head.toml`, a palette — is only held to its syntax, and
/// the sentence says so rather than implying a check nobody ran.
fn check(f: &cref::File, text: &str, path: &Path) -> Result<String, String> {
    let by = match f.name {
        "leticode.toml" => {
            leticode_config::LeticodeConfig::parse_text(text, path, leticode_config::Level::User)?;
            "letibot's own leticode.toml reader"
        }
        "providers.toml" => {
            letibot_provider::keys::validate_text(text)?;
            "letibot's own providers.toml reader"
        }
        "hosts.toml" => {
            letibot_tools::remote::Hosts::parse(text)?;
            "letibot's own hosts.toml reader"
        }
        "prompts.toml" => {
            crate::config::Prompts::parse_text(text, path)?;
            "the daemon's own prompts.toml reader"
        }
        "permission.json" => {
            letibot_tools::permission::parse_text(text, path)?;
            "the gate's own permission.json reader"
        }
        "plugin-trust.json" => {
            serde_json::from_str::<serde_json::Value>(text)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            "reads as JSON; the trust book's own reader is the plugins crate's and is not run here"
        }
        "modes.tsv" => {
            "reads as a TSV table; this file was imported once and is not read any more"
        }
        _ => {
            if f.format == cref::Format::Toml {
                toml::from_str::<toml::Value>(text)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                "reads as TOML; this file has no reader in the daemon, so a key its own reader \
                 would refuse is not caught here"
            } else {
                "no reader for this file's syntax is wired here"
            }
        }
    };
    Ok(format!("Read back by {by}."))
}

/// A unified diff of one file, with context either side of the change.
///
/// One hunk from the first differing line to the last, which is what a proposal is: a handful of
/// keys in one file. A differ with an edit script would draw several hunks for the same change and
/// nobody answering a card needs them.
fn unified(name: &str, before: &str, after: &str) -> String {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let span = letibot_tools::edit::changed_span(before, after);
    // 0-based: the hunk starts `CONTEXT` lines above the first change, and the region the two
    // files share either side of it is drawn once.
    let lo = span.first.saturating_sub(1 + CONTEXT);
    let b_hi = (span.last_before + CONTEXT).min(b.len());
    let a_hi = (span.last_after + CONTEXT).min(a.len());
    let b_from = span.first.saturating_sub(1).min(b.len());
    let b_to = span.last_before.min(b.len());
    let a_to = span.last_after.min(a.len()).max(b_from.min(a.len()));
    let a_from = b_from.min(a_to);
    let mut out = format!("--- a/{name}\n+++ b/{name}\n");
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        lo + 1,
        b_hi.saturating_sub(lo),
        lo + 1,
        a_hi.saturating_sub(lo)
    ));
    for line in &b[lo..b_from] {
        out.push_str(&format!(" {line}\n"));
    }
    for line in &b[b_from..b_to] {
        out.push_str(&format!("-{line}\n"));
    }
    for line in &a[a_from..a_to] {
        out.push_str(&format!("+{line}\n"));
    }
    for line in &b[b_to..b_hi] {
        out.push_str(&format!(" {line}\n"));
    }
    out
}

fn read_or_empty(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

impl ConfigDesk for SessionConfig {
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }

    fn explain(&self, file: Option<&str>, key: Option<&str>) -> Result<String, String> {
        let Some(word) = key else {
            let f = file_for(file.unwrap_or_default())?;
            return Ok(cref::text(f));
        };
        let mut hits: Vec<cref::lookup::Hit> = Vec::new();
        match file {
            Some(name) => {
                let f = file_for(name)?;
                if let Some((table, k)) = cref::lookup::describe(f, word) {
                    hits.push(cref::lookup::Hit {
                        file: f,
                        table,
                        key: k,
                    });
                }
            }
            None => {
                // A word first: a key is asked for by its own name far more often than by its
                // path, and `shadow.answer_reads` is a path, so the other order is tried second.
                hits = cref::lookup::search(word);
                if hits.is_empty() {
                    hits = cref::lookup::hits(word);
                }
            }
        }
        if hits.is_empty() {
            return Err(format!(
                "no key of {} is called `{word}`. `config explain` with no arguments lists the \
                 files; `config explain` with a `file` lists every key that file has",
                file.unwrap_or("any letibot config file")
            ));
        }
        let mut out = String::new();
        for h in &hits {
            let level = if h.key.level.is_empty() {
                String::new()
            } else {
                format!("  [{}]", h.key.level)
            };
            out.push_str(&format!(
                "{} [{}] {} = {}  (when it is absent: {}){level}\n    {}\n",
                h.file.name, h.table.path, h.key.key, h.key.ty, h.key.default, h.key.meaning
            ));
        }
        out.push_str(&format!(
            "\n{} key{}. To change one: `config propose` with file \"{}\" and edit \
             {{\"{}\": <value>}}.",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" },
            hits[0].file.name,
            hits[0].key.key
        ));
        Ok(out)
    }

    fn show(&self, file: &str) -> Result<Shown, String> {
        let f = file_for(file)?;
        let path = self.path(f.name);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "{} does not exist, so there is nothing to show: letibot runs on its built-in \
                     answers for it. `config propose` would create it.",
                    path.display()
                ));
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let redacted = cref::redact::redact(&text, f.format)?;
        let mode = std::fs::metadata(&path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777);
        Ok(Shown {
            path: path.display().to_string(),
            text: redacted.text,
            note: redacted.note,
            mode,
        })
    }

    fn propose(
        &self,
        file: &str,
        edits: &cref::edit::Edits,
    ) -> Result<Proposal, String> {
        let f = file_for(file)?;
        if f.format != cref::Format::Toml {
            return Err(format!(
                "`{}` is {:?} and this tool proposes TOML edits only. `config show {}` prints it, \
                 and it is a file to edit yourself.",
                f.name, f.format, f.name
            ));
        }
        let path = self.path(f.name);
        let before = read_or_empty(&path)?;
        let text = cref::edit::apply(&before, edits)
            .map_err(|why| format!("{}: {why}", path.display()))?;
        if text == before {
            return Err(format!(
                "{} would be unchanged by that edit, so there is nothing to put to the operator.",
                path.display()
            ));
        }
        let check = check(f, &text, &path)?;
        let preview = letibot_tools::edit::FileEdit {
            path: f.name.to_string(),
            before: before.clone(),
            after: text.clone(),
            created: before.is_empty(),
            before_digest: letibot_tools::spill::content_hash(before.as_bytes()),
            after_digest: letibot_tools::spill::content_hash(text.as_bytes()),
            replacements: 1,
            changed: letibot_tools::edit::changed_span(&before, &text),
        }
        .excerpt(CONTEXT, 400);
        Ok(Proposal {
            path: path.display().to_string(),
            diff: unified(f.name, &before, &text),
            before,
            text,
            check,
            preview: vec![preview],
        })
    }

    fn write(&self, file: &str, before: &str, text: &str) -> Result<Written, String> {
        let f = file_for(file)?;
        let path = self.path(f.name);
        let now = read_or_empty(&path)?;
        if now != before {
            return Err(format!(
                "{} changed while the card was up, so the answer was about bytes that are no \
                 longer there. Nothing was written; propose it again against what the file says \
                 now.",
                path.display()
            ));
        }
        let mode = std::fs::metadata(&path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
            // A config file can hold a key, so a new one is private from its first byte rather
            // than from the first operator who notices.
            .unwrap_or(0o600);
        let tmp = path.with_file_name(format!(".{}.tmp", f.name));
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })?;
        Ok(Written {
            path: path.display().to_string(),
            bytes: text.len(),
            mode: Some(mode),
        })
    }
}

#[cfg(test)]
#[path = "config_desk/tests.rs"]
mod tests;
