//! The themes this build ships, and the one rule that picks between them and the reader's own.
//!
//! Three palettes are compiled into the binary - `terminal` (the terminal's own sixteen slots),
//! `dark` and `light` - plus `auto`, which is not a palette but a choice between two of them. A
//! file in `themes/NAME.toml` beside `head.toml` wins over a built-in of the same name, so a
//! reader can adjust `dark` without giving up the shipped one's other thirty-three roles.
//!
//! The parsing, the role names and the look grammar are rano's (`rano::theme`); what is here is
//! which files exist, and what a name means when it is `auto`.

use std::path::Path;

use rano::style::{Look, Role};
use rano::theme::Theme;

/// The built-in themes, in the order `/theme` lists them: the terminal's own table, then the two
/// palettes this build chose.
pub const BUILTIN: [(&str, &str); 3] = [
    ("terminal", include_str!("../themes/terminal.toml")),
    ("dark", include_str!("../themes/dark.toml")),
    ("light", include_str!("../themes/light.toml")),
];

/// `auto` is not a file: it is the two palettes above, chosen by the terminal's background.
pub const AUTO: &str = "auto";

/// Where a theme's roles come from, for `/theme`'s listing and for what a name resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The terminal's own palette: a theme that names no role at all.
    Terminal,
    /// `auto`, which is not a palette: it is `dark` or `light`, once the terminal has said.
    Auto,
    /// This binary's own copy.
    Builtin,
    /// A file beside `head.toml`, over a built-in of the same name or on its own.
    User,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Terminal => "the terminal's own palette",
            Source::Auto => "dark or light, by the terminal's own background",
            Source::Builtin => "built in",
            Source::User => "~/.config/letibot/themes",
        }
    }
}

/// The theme a name means once the terminal has said how light its background is. `auto` needs
/// that answer - and it is the answer the head already has (one OSC 11 query at start-up, read as
/// `Key::Background`), never a second query: `None` is a terminal that has not said, or cannot,
/// and the safe reading of a palette chosen from a fact nobody has is the terminal's own.
pub fn effective(name: Option<&str>, light_background: Option<bool>) -> String {
    match name.unwrap_or("").trim() {
        "" | "terminal" => "terminal".to_string(),
        AUTO => match light_background {
            Some(true) => "light".to_string(),
            Some(false) => "dark".to_string(),
            None => "terminal".to_string(),
        },
        other => other.to_string(),
    }
}

/// The theme a name resolves to: the reader's file of that name first, then this binary's copy,
/// then the terminal's own table. Problems are sentences, and a name nothing has is one of them -
/// a typo must be said, never drawn as a deliberate default.
pub fn resolve(
    dir: &Path,
    name: Option<&str>,
    colors: &[(String, String)],
    light_background: Option<bool>,
) -> (Option<Theme>, Vec<String>) {
    let asked = name.unwrap_or("").trim();
    let at = effective(name, light_background);
    let mut problems = Vec::new();
    let mut theme = Theme {
        name: asked.to_string(),
        roles: Vec::new(),
    };
    if !at.is_empty() && at != "terminal" {
        match load(dir, &at) {
            Ok((t, p)) => {
                theme.roles = t.roles;
                problems.extend(p);
            }
            Err(e) => problems.push(e),
        }
    }
    // `head.toml`'s own `color.ROLE` lines, on top of the theme, in file order - rano's rule and
    // rano's words for a line it cannot use.
    for (role, spec) in colors {
        let Some(r) = Role::from_name(role) else {
            problems.push(format!("color.{role}: `{role}` is not a role"));
            continue;
        };
        match Look::parse(spec) {
            Ok(l) => theme.roles.push((r, l)),
            Err(e) => problems.push(format!("color.{role}: {e}")),
        }
    }
    // No role at all is no override: `terminal`, and a file that only comments, both leave rano's
    // default table in charge rather than a theme that paints nothing.
    ((!theme.roles.is_empty()).then_some(theme), problems)
}

/// The file `dir/NAME.toml` if there is one, else the built-in of that name, else why neither.
/// A parse problem is prefixed with the file it came from, so a bad line is one the reader can
/// find.
fn load(dir: &Path, name: &str) -> Result<(Theme, Vec<String>), String> {
    let path = rano::theme::theme_path(dir, name);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let (theme, problems) = Theme::parse(name, &text);
        return Ok((
            theme,
            problems
                .into_iter()
                .map(|p| format!("{}: {p}", path.display()))
                .collect(),
        ));
    }
    let builtin = BUILTIN.iter().find(|(n, _)| *n == name);
    match builtin {
        Some((_, text)) => Ok(Theme::parse(name, text)),
        None => Err(format!(
            "theme `{name}`: {} is not there, and this build ships {}",
            path.display(),
            shipped()
        )),
    }
}

/// The names this build ships, as one sentence for a refusal or a listing - `terminal, dark, light
/// and auto`.
pub fn shipped() -> String {
    let mut names: Vec<&str> = BUILTIN.iter().map(|(n, _)| *n).collect();
    names.push(AUTO);
    match names.split_last() {
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// Every name `/theme` offers, in the order it offers them: `Auto` first (the default nobody has
/// to understand), then the three built-ins, then the reader's own files by name. A user file that
/// shadows a built-in keeps the built-in's place and says it is the reader's.
pub fn listing(dir: &Path) -> Vec<(String, Source)> {
    let mut out: Vec<(String, Source)> = vec![(AUTO.to_string(), Source::Auto)];
    let files = files_in(dir);
    for (name, _) in BUILTIN.iter() {
        // A user file wins over the built-in it is named after, so the row says where the bytes
        // actually come from - that is the one fact a reader of this list is looking for.
        let source = if files.iter().any(|f| f == name) {
            Source::User
        } else if *name == "terminal" {
            Source::Terminal
        } else {
            Source::Builtin
        };
        out.push(((*name).to_string(), source));
    }
    for name in files {
        if !BUILTIN.iter().any(|(n, _)| *n == name) {
            out.push((name, Source::User));
        }
    }
    out
}

/// The `*.toml` files in `dir`, by name, without the extension. A name that is not plain
/// alphanumerics or `-`/`_` is skipped: `rano::theme::theme_path` would rewrite it, so listing a
/// name that cannot be loaded would offer a reader a row that does nothing.
pub fn files_in(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension()? != "toml" {
                    return None;
                }
                let name = path.file_stem()?.to_str()?.to_string();
                let plain = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                plain.then_some(name)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// The directory `head.toml`'s themes live in: `themes/` beside it. Empty for a head that has
/// nowhere to look, which `resolve` reads as no user file at all.
pub fn dir_of(head_toml: &Path) -> std::path::PathBuf {
    head_toml
        .parent()
        .map(|d| d.join("themes"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
