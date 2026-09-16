//! **The head's own preferences**, on disk: `~/.config/letibot/head.toml`.
//!
//! What a head chooses about how it draws — the diff shape, the folds, raw
//! tool calls — used to live in the process and die with it, and was moved by
//! slash commands nobody remembered. The operator's ask (2026-09-16): *"i'd
//! prefer a config option and a pane with runtime-able configurations
//! editable"*. So the choices are a file beside `modes.tsv`, `permission.json`
//! and `providers.toml`, read at start, and written whenever the config pane
//! changes one.
//!
//! The format is the same flat `key = "value"` subset `providers.toml` already
//! uses, parsed by hand for the same reason: four keys do not earn a
//! dependency, and a file a person edits with `vi` must survive a comment and
//! a key this build does not know. Unknown lines are kept on write.

use std::path::{Path, PathBuf};

/// Everything the head persists. Each field is a runtime-editable row in the
/// config pane; adding one here is adding a row there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadPrefs {
    /// `split` — two panels — or `unified` always. The toggle is the whole
    /// choice; the width is the renderer's business.
    pub diff: DiffPref,
    /// `open` or `folded`.
    pub thinking: String,
    /// `open` or `folded`.
    pub tools: String,
    /// Show the model's `<function=…>` markup under each call.
    pub raw_calls: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffPref {
    Split,
    Unified,
}

impl DiffPref {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffPref::Split => "split",
            DiffPref::Unified => "unified",
        }
    }
    pub fn parse(s: &str) -> Option<DiffPref> {
        match s.trim() {
            "split" | "side-by-side" | "auto" => Some(DiffPref::Split),
            "unified" | "single" => Some(DiffPref::Unified),
            _ => None,
        }
    }
    pub fn flip(self) -> DiffPref {
        match self {
            DiffPref::Split => DiffPref::Unified,
            DiffPref::Unified => DiffPref::Split,
        }
    }
}

impl Default for HeadPrefs {
    fn default() -> Self {
        HeadPrefs {
            diff: DiffPref::Split,
            thinking: "folded".into(),
            tools: "folded".into(),
            raw_calls: false,
        }
    }
}

/// `$XDG_CONFIG_HOME/letibot/head.toml`, or `~/.config/letibot/head.toml`.
/// `None` when neither variable is set, which is a head with nowhere to write
/// and says so rather than writing into the working directory.
pub fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("letibot").join("head.toml"))
}

/// What one line of the file says.
enum Line {
    Pair(String, String),
    Other(String),
}

fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .map(|raw| {
            let l = raw.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with('[') {
                return Line::Other(raw.to_string());
            }
            match l.split_once('=') {
                Some((k, v)) => Line::Pair(
                    k.trim().to_string(),
                    v.trim().trim_matches('"').trim_matches('\'').to_string(),
                ),
                None => Line::Other(raw.to_string()),
            }
        })
        .collect()
}

/// Read the file. A missing file is the defaults; a line this build does not
/// understand is reported by name and otherwise ignored, never a refusal to
/// start the head.
pub fn load(path: &Path) -> (HeadPrefs, Vec<String>) {
    let mut p = HeadPrefs::default();
    let mut notes = Vec::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return (p, notes);
    };
    for line in parse(&text) {
        let Line::Pair(k, v) = line else { continue };
        match k.as_str() {
            "diff" => match DiffPref::parse(&v) {
                Some(d) => p.diff = d,
                None => notes.push(format!("head.toml: diff = {v:?} is not split or unified")),
            },
            "thinking" | "tools" => match v.as_str() {
                "open" | "folded" => {
                    if k == "thinking" {
                        p.thinking = v;
                    } else {
                        p.tools = v;
                    }
                }
                _ => notes.push(format!("head.toml: {k} = {v:?} is not open or folded")),
            },
            "raw_calls" => match v.as_str() {
                "true" | "yes" | "on" => p.raw_calls = true,
                "false" | "no" | "off" => p.raw_calls = false,
                _ => notes.push(format!("head.toml: raw_calls = {v:?} is not true or false")),
            },
            other => notes.push(format!("head.toml: `{other}` is not a key this head knows")),
        }
    }
    (p, notes)
}

/// Write the file, keeping every line that is not one of ours — a comment, a
/// key from a newer build — where it was. Creates the directory.
pub fn save(path: &Path, p: &HeadPrefs) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let ours: [(&str, String); 4] = [
        ("diff", format!("\"{}\"", p.diff.as_str())),
        ("thinking", format!("\"{}\"", p.thinking)),
        ("tools", format!("\"{}\"", p.tools)),
        ("raw_calls", p.raw_calls.to_string()),
    ];
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in parse(&existing) {
        match line {
            Line::Pair(k, _) if ours.iter().any(|(n, _)| *n == k) => {
                let (n, v) = ours.iter().find(|(n, _)| *n == k).unwrap();
                out.push(format!("{n} = {v}"));
                written.insert(n);
            }
            Line::Pair(k, v) => out.push(format!("{k} = \"{v}\"")),
            Line::Other(raw) => out.push(raw),
        }
    }
    if out.is_empty() {
        out.push("# letibot head preferences — edited by the /config pane, or by hand".into());
    }
    for (n, v) in &ours {
        if !written.contains(n) {
            out.push(format!("{n} = {v}"));
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut body = out.join("\n");
    body.push('\n');
    std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-prefs-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("head.toml")
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_a_save_round_trips() {
        let p = tmp("rt");
        let (prefs, notes) = load(&p);
        assert_eq!(prefs, HeadPrefs::default());
        assert!(notes.is_empty());
        let changed = HeadPrefs { diff: DiffPref::Unified, thinking: "open".into(), tools: "folded".into(), raw_calls: true };
        save(&p, &changed).unwrap();
        let (back, notes) = load(&p);
        assert_eq!(back, changed);
        assert!(notes.is_empty(), "{notes:?}");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A person's comment and a key from a newer build survive a save; a value
    /// this build cannot read is named, not swallowed.
    #[test]
    fn unknown_lines_are_kept_and_bad_values_are_named() {
        let p = tmp("keep");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "# mine\ndiff = \"unified\"\nfuture_key = \"x\"\nthinking = \"sideways\"\n").unwrap();
        let (prefs, notes) = load(&p);
        assert_eq!(prefs.diff, DiffPref::Unified);
        assert_eq!(prefs.thinking, "folded", "a bad value fell back to the default");
        assert!(notes.iter().any(|n| n.contains("future_key")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("sideways")), "{notes:?}");
        save(&p, &HeadPrefs { diff: DiffPref::Split, ..prefs }).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(text.contains("diff = \"split\""), "{text}");
        assert!(text.contains("future_key = \"x\""), "{text}");
        assert_eq!(text.matches("diff =").count(), 1, "{text}");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
