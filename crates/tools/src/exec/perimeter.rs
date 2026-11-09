//! `[perimeter.<name>]`: the two seat levels as tables, not as a branch in code.
//!
//! ```toml
//! [perimeter.subagent]                       # the level a child session gets
//! hide = ["~/datasets"]                      # denied after every grant
//! grants_ro = ["~/shared"]                   # readable inside the view (USER file only)
//! [perimeter.subagent.env]
//! RUST_LOG = "info"                          # a constant
//! TOKEN = { file = "~/.config/x/token" }     # read at open, outside the box: a secret
//! ```
//!
//! A perimeter is one session's answer to *what do my commands see*: the level's own view
//! ([`super::seat`]), what it adds or hides, the env it is handed and how that env is computed,
//! and which secrets it may carry. The built-in tables are [`TOP_LEVEL`] and [`SUBAGENT`], and a
//! session whose config writes neither is the view it always was - byte for byte.
//!
//! A child composes: it starts from its parent's perimeter, at the tight level, and may only
//! narrow it. A widening is not a mode's call, so it is asked of the operator as a card.

use std::path::{Path, PathBuf};

use super::boundary::{self, Entry, Origin, Resolved};
use super::seat::{self, Level, SeatView, SecretEnv};

/// The name of the level a session nobody started is on: the operator's own home, the whole
/// filesystem readable, the deny list the only fence.
pub const TOP_LEVEL: &str = "top-level";

/// The name of the level a child starts at: a throwaway home, the toolchain read-only, no
/// credentials, no push. Every other name is a child's too.
pub const SUBAGENT: &str = "subagent";

/// The level a name is on. The two reserved names decide it, so a file cannot make a child wide
/// by writing a table: a child is always at the tight level, whatever it names.
pub fn level_of(name: &str) -> Level {
    if name == TOP_LEVEL {
        Level::Wide
    } else {
        Level::Tight
    }
}

/// A named secret a perimeter may carry. The value is computed by the daemon OUTSIDE the box, at
/// open, and its `Debug` prints the name and never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Secret {
    /// `gh auth token`, plus the git credential helper that answers with `$GH_TOKEN` on
    /// github.com: what a session pushes with, and what lets a child comment and not push.
    Github,
}

impl Secret {
    pub const ALL: [Secret; 1] = [Secret::Github];

    pub fn name(self) -> &'static str {
        match self {
            Secret::Github => "github",
        }
    }

    pub fn parse(word: &str) -> Option<Secret> {
        Secret::ALL.into_iter().find(|s| s.name() == word.trim())
    }

    /// What this secret is worth, asked of the box the daemon runs on. `Err` says why it answered
    /// nothing, and a perimeter is opened without it rather than refusing the session.
    pub fn value(self) -> Result<SecretEnv, String> {
        match self {
            Secret::Github => seat::gh_auth_token().map(|t| seat::github_env(&t)),
        }
    }

    /// The env names this secret sets, with empty values: a report prints the names and never a
    /// value, and an empty value masks nothing.
    pub fn placeholder(self) -> SecretEnv {
        match self {
            Secret::Github => seat::github_env(""),
        }
    }
}

/// How one env var is computed. A value a file or a command gives is a SECRET: it travels in
/// [`super::seat::SecretEnv`] and is masked in every slice the daemon reads back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvValue {
    /// The literal in the file.
    Constant(String),
    /// Read from this path at open, outside the box.
    File(String),
    /// Run at open, outside the box, and take its first line.
    Command(String),
}

impl EnvValue {
    pub fn is_secret(&self) -> bool {
        !matches!(self, EnvValue::Constant(_))
    }
}

/// One `[perimeter.<name>]` table as a file wrote it, keeping each entry's file and line so a
/// refusal names them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerimeterTable {
    pub name: String,
    pub grants_ro: Vec<Entry<String>>,
    pub grants_rw: Vec<Entry<String>>,
    pub hide: Vec<Entry<String>>,
    pub env: Vec<Entry<(String, EnvValue)>>,
    pub secrets: Vec<Entry<Secret>>,
}

impl PerimeterTable {
    pub fn is_empty(&self) -> bool {
        self.grants_ro.is_empty()
            && self.grants_rw.is_empty()
            && self.hide.is_empty()
            && self.env.is_empty()
            && self.secrets.is_empty()
    }

    /// The project's table over the user's for the same name: both add, `hide` wins where they
    /// disagree, and a grant is only ever in the file that may write one.
    pub fn overridden_by(&self, other: &PerimeterTable) -> PerimeterTable {
        fn union<T: Clone + PartialEq>(a: &[Entry<T>], b: &[Entry<T>]) -> Vec<Entry<T>> {
            let mut out = a.to_vec();
            for e in b {
                if !out.iter().any(|o| o.value == e.value) {
                    out.push(e.clone());
                }
            }
            out
        }
        PerimeterTable {
            name: self.name.clone(),
            grants_ro: union(&self.grants_ro, &other.grants_ro),
            grants_rw: union(&self.grants_rw, &other.grants_rw),
            hide: union(&self.hide, &other.hide),
            env: union(&self.env, &other.env),
            secrets: union(&self.secrets, &other.secrets),
        }
    }
}

/// The table's own entries checked: a project file may not grant and may not carry a secret,
/// because a project file is one a session can edit and this is the widening it would buy.
fn widening_refusal(table: &PerimeterTable) -> Result<(), String> {
    for (key, e) in [
        ("grants_ro", table.grants_ro.first()),
        ("grants_rw", table.grants_rw.first()),
    ] {
        if let Some(e) = e
            && !e.from.user
        {
            return Err(format!(
                "{}: [perimeter.{}] {key} = \"{}\": a project file may not grant a path. \
                 Widening is the operator's call, so write it in the user file, or ask for it at \
                 the spawn, which puts the question to the operator",
                e.from, table.name, e.value
            ));
        }
    }
    if let Some(e) = table.secrets.iter().find(|e| !e.from.user) {
        return Err(format!(
            "{}: [perimeter.{}] secrets = [\"{}\"]: a project file may not hand a session a \
             secret. Write it in the user file",
            e.from,
            table.name,
            e.value.name()
        ));
    }
    Ok(())
}

/// What one session's commands see, as the config decided it: the level, and what this session
/// adds to or takes off that level's own view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Perimeter {
    pub name: String,
    pub level: Level,
    pub grants_ro: Vec<Entry<String>>,
    pub grants_rw: Vec<Entry<String>>,
    pub hide: Vec<Entry<String>>,
    pub env: Vec<Entry<(String, EnvValue)>>,
    pub secrets: Vec<Secret>,
    /// What a spawn had to be GRANTED for this perimeter, and the reason it gave: the operator's
    /// answer, recorded on this session only.
    pub widened: Vec<WidenItem>,
    pub widened_because: Option<String>,
}

impl Perimeter {
    /// The built-in table for a level. A config that writes nothing uses these and nothing else.
    pub fn default_for(level: Level) -> Perimeter {
        let name = match level {
            Level::Wide => TOP_LEVEL,
            Level::Tight => SUBAGENT,
        };
        Perimeter {
            name: name.to_string(),
            level,
            grants_ro: Vec::new(),
            grants_rw: Vec::new(),
            hide: Vec::new(),
            env: Vec::new(),
            secrets: match level {
                // The wide seat is the operator's own session: it pushes, so it carries the
                // token. A child never does until somebody says so.
                Level::Wide => vec![Secret::Github],
                Level::Tight => Vec::new(),
            },
            widened: Vec::new(),
            widened_because: None,
        }
    }

    /// The built-in table `name` for, narrowed by the tables a config wrote for that name.
    pub fn named(name: &str, table: Option<&PerimeterTable>) -> Result<Perimeter, String> {
        let mut p = Perimeter::default_for(level_of(name));
        p.name = name.trim().to_string();
        let Some(t) = table else {
            return Ok(p);
        };
        widening_refusal(t)?;
        p.grants_ro.extend(t.grants_ro.iter().cloned());
        p.grants_rw.extend(t.grants_rw.iter().cloned());
        p.hide.extend(t.hide.iter().cloned());
        p.env.extend(t.env.iter().cloned());
        for e in &t.secrets {
            if !p.secrets.contains(&e.value) {
                p.secrets.push(e.value);
            }
        }
        Ok(p)
    }

    /// The key this perimeter's entries were written under, for a refusal or a report.
    pub fn label(&self) -> String {
        format!("[perimeter.{}]", self.name)
    }

    pub fn carries(&self, secret: Secret) -> bool {
        self.secrets.contains(&secret)
    }

    /// The perimeter's own entries resolved against a home: which paths, and where they came
    /// from. The rules are `[boundary]`'s, because the view they build is the same one.
    pub fn resolved(&self, home: &Path) -> Result<Resolved, String> {
        let label = self.label();
        let mut r = Resolved {
            label: label.clone(),
            ..Resolved::default()
        };
        for e in &self.grants_ro {
            let key = format!("{label} grants_ro");
            r.grants_ro.push((
                boundary::checked_path(&key, &e.value, &e.from, home, true)?,
                e.from.clone(),
            ));
        }
        for e in &self.grants_rw {
            if !e.from.user {
                return Err(widening_refusal(&PerimeterTable {
                    name: self.name.clone(),
                    grants_rw: vec![e.clone()],
                    ..PerimeterTable::default()
                })
                .unwrap_err());
            }
            let key = format!("{label} grants_rw");
            r.grants_rw.push((
                boundary::checked_path(&key, &e.value, &e.from, home, true)?,
                e.from.clone(),
            ));
        }
        for e in &self.hide {
            let key = format!("{label} hide");
            r.hide.push((
                boundary::checked_path(&key, &e.value, &e.from, home, false)?,
                e.from.clone(),
            ));
        }
        for e in &self.env {
            let (k, v) = &e.value;
            if !a_variable_name(k) {
                return Err(format!(
                    "{}: {label} env: `{k}` is not a variable name",
                    e.from
                ));
            }
            if let Some(why) = match v {
                EnvValue::Constant(_) => boundary::env_refusal(k),
                // A value a file or a command gives is a secret, and a secret's name is the
                // point of it; the shape check still applies, because a loader variable runs
                // code in every command whatever its value was.
                _ => boundary::env_shape_refusal(k),
            } {
                return Err(format!("{}: {label} env: `{k}` is refused, {why}", e.from));
            }
            if let EnvValue::Constant(v) = v {
                r.env.push((k.clone(), v.clone(), e.from.clone()));
            }
        }
        Ok(r)
    }

    /// The grants this table asks for on a PLACEMENT's view, as the file spelled them: a `~/` path is
    /// that machine's home, which is not this one's, so nothing is expanded here. `resolved` runs
    /// first, so a table this box refuses refuses the session rather than opening an unchecked view.
    pub fn host_grants(&self, home: &Path) -> Result<Vec<boundary::HostGrant>, String> {
        self.resolved(home)?;
        let label = self.label();
        let mut out = Vec::new();
        for (write, entries) in [(false, &self.grants_ro), (true, &self.grants_rw)] {
            for e in entries.iter() {
                out.push(boundary::HostGrant {
                    path: e.value.clone(),
                    write,
                    label: label.clone(),
                    from: e.from.clone(),
                });
            }
        }
        Ok(out)
    }

    /// The `[perimeter.<name>.env]` pairs, as a placement hands them to another machine: the
    /// literals, which may be spelled, and the values a file or a command gave, which are secrets
    /// and may not. Only `env`: a named `secrets` grant is the seat's own injection and is not a
    /// placement's business. Every entry is checked by the table's own rule again, and one that
    /// cannot be read comes back as a line rather than a silent absence.
    pub fn env_pairs(
        &self,
        home: &Path,
    ) -> (Vec<(String, String)>, Vec<(String, String)>, Vec<String>) {
        let label = self.label();
        let mut spelled = Vec::new();
        let mut secret = Vec::new();
        let mut said = Vec::new();
        for e in &self.env {
            let (k, value) = &e.value;
            if !a_variable_name(k) {
                said.push(format!(
                    "{}: {label} env: `{k}` is not a variable name",
                    e.from
                ));
                continue;
            }
            let refused = match value {
                EnvValue::Constant(_) => boundary::env_refusal(k),
                _ => boundary::env_shape_refusal(k),
            };
            if let Some(why) = refused {
                said.push(format!(
                    "{}: {label} env: `{k}` is refused, {why}; it did not travel",
                    e.from
                ));
                continue;
            }
            let got = match value {
                EnvValue::Constant(v) => Ok(v.clone()),
                EnvValue::File(path) => read_one(home, path),
                EnvValue::Command(cmd) => run_one(cmd),
            };
            match got {
                Ok(v) => match value {
                    EnvValue::Constant(_) => spelled.push((k.clone(), v)),
                    _ => secret.push((k.clone(), v)),
                },
                Err(why) => said.push(format!("{label} carries no `{k}`: {why}")),
            }
        }
        (spelled, secret, said)
    }

    /// What this perimeter's computed env is worth: every value a file or a command gives, and
    /// every named secret, asked of the box at open. Each answer is a secret, so it is masked in
    /// every slice the daemon reads back.
    pub fn secret_values(&self, home: &Path) -> (SecretEnv, Vec<String>) {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut said: Vec<String> = Vec::new();
        for e in &self.env {
            let (name, value) = (&e.value.0, &e.value.1);
            let got = match value {
                EnvValue::Constant(_) => continue,
                EnvValue::File(path) => read_one(home, path),
                EnvValue::Command(cmd) => run_one(cmd),
            };
            match got {
                Ok(v) => out.push((name.clone(), v)),
                Err(why) => said.push(format!("{} carries no `{name}`: {why}", self.label())),
            }
        }
        for s in &self.secrets {
            match s.value() {
                Ok(env) => out.extend(env.0),
                Err(why) => said.push(format!(
                    "this session carries no `{}` secret, so what needs it will not authenticate: \
                     {why}",
                    s.name()
                )),
            }
        }
        let mut seen: Vec<(String, String)> = Vec::new();
        for (k, v) in out {
            match seen.iter_mut().find(|(n, _)| *n == k) {
                Some(slot) => slot.1 = v,
                None => seen.push((k, v)),
            }
        }
        (SecretEnv(seen), said)
    }

    /// The values this perimeter's secrets WOULD set, with nothing in them: for a report, where
    /// the names print and no value may.
    pub fn placeholder_values(&self) -> SecretEnv {
        let mut out = Vec::new();
        for s in &self.secrets {
            out.extend(s.placeholder().0);
        }
        SecretEnv(out)
    }

    /// The seat a command in this perimeter gets: the level's own view, then this perimeter's
    /// grants, hides, env and secrets. `boundary` is the `[boundary]` table, applied by the caller
    /// exactly as it was before perimeters existed.
    pub fn render<S: AsRef<str>>(
        &self,
        mode: &crate::mode::Mode,
        place: &str,
        present: &[S],
        home: &Path,
        secrets: &SecretEnv,
    ) -> Result<SeatView, String> {
        let r = self.resolved(home)?;
        let view = seat::seat_for_level(self.level, mode, place, present, home, None);
        let mut view = boundary::apply(view, &r);
        view.secret_env = secrets.clone();
        Ok(view)
    }

    /// What `want` asks for that this perimeter does not already hold: empty means the request
    /// only narrows, and anything else is a widening the operator has to answer.
    pub fn widening(&self, want: &Widen, home: &Path) -> Result<Vec<WidenItem>, String> {
        let label = format!("{} widen", self.label());
        let mut items: Vec<WidenItem> = Vec::new();
        for (raw, rw) in want
            .grants_ro
            .iter()
            .map(|p| (p, false))
            .chain(want.grants_rw.iter().map(|p| (p, true)))
        {
            let at = want.from.clone().unwrap_or_else(|| Origin {
                file: PathBuf::from("the spawn's `widen`"),
                line: 0,
                user: true,
            });
            let path = boundary::checked_path(&label, raw, &at, home, true)?;
            if self.holds(&WidenItem::of(path.clone(), rw), home) {
                continue;
            }
            items.push(WidenItem::of(path, rw));
        }
        for s in &want.secrets {
            if !self.carries(*s) {
                items.push(WidenItem::Secret(*s));
            }
        }
        Ok(items)
    }

    /// Does this perimeter already hold `item`? A wide view reads the whole filesystem and writes
    /// its own home, so a grant inside it is not a widening; a tight view holds its own grants and
    /// the toolchain the level reads; and a path anything hides is never held.
    pub fn holds(&self, item: &WidenItem, home: &Path) -> bool {
        let hidden = |p: &Path| {
            self.hide.iter().any(|e| {
                boundary::checked_path("hide", &e.value, &e.from, home, false)
                    .is_ok_and(|h| p.starts_with(h))
            })
        };
        match item {
            WidenItem::ReadOnly(p) | WidenItem::ReadWrite(p) if hidden(p) => false,
            WidenItem::ReadOnly(p) => match self.level {
                Level::Wide => true,
                Level::Tight => self.granted(p, false, home) || toolchain(p, home),
            },
            WidenItem::ReadWrite(p) => match self.level {
                Level::Wide => p.starts_with(home) || self.granted(p, true, home),
                Level::Tight => self.granted(p, true, home),
            },
            WidenItem::Secret(s) => self.carries(*s),
        }
    }

    /// Is `path` under one of this perimeter's own grants, at `rw` or (when `rw` is false) at all?
    fn granted(&self, path: &Path, rw: bool, home: &Path) -> bool {
        let has = |entries: &[Entry<String>]| {
            entries.iter().any(|e| {
                boundary::checked_path("grant", &e.value, &e.from, home, true)
                    .is_ok_and(|g| path.starts_with(g))
            })
        };
        has(&self.grants_rw) || (!rw && has(&self.grants_ro))
    }

    /// The perimeter a child of this one gets: this one's at the tight level, narrowed by the table
    /// it names, plus what the operator GRANTED for this spawn - [`Self::widening`]'s answer once a
    /// card allowed it, and empty when the request only narrowed.
    pub fn child_of(
        &self,
        named: Option<&PerimeterTable>,
        granted: &[WidenItem],
        because: Option<&str>,
    ) -> Result<Perimeter, String> {
        let mut child = Perimeter {
            name: match named {
                Some(t) => t.name.clone(),
                None => SUBAGENT.to_string(),
            },
            level: Level::Tight,
            grants_ro: self.grants_ro.clone(),
            grants_rw: self.grants_rw.clone(),
            hide: self.hide.clone(),
            // Its parent's env and no `[perimeter]` table's: an env var is a value every command
            // reads, and a child handed a new one reaches further than the parent that started it.
            env: self.env.clone(),
            // The level's own, never the parent's: a child of the wide seat is not the wide seat.
            secrets: Perimeter::default_for(Level::Tight).secrets,
            widened: Vec::new(),
            widened_because: None,
        };
        if let Some(t) = named {
            widening_refusal(t)?;
            child.grants_ro.extend(t.grants_ro.iter().cloned());
            child.grants_rw.extend(t.grants_rw.iter().cloned());
            for e in &t.hide {
                if !child.hide.iter().any(|h| h.value == e.value) {
                    child.hide.push(e.clone());
                }
            }
            for s in &t.secrets {
                // Narrow only: a table may hand down a secret its parent already carries, never a
                // new one. A new one is a widening, and a widening is a card.
                if self.carries(s.value) && !child.secrets.contains(&s.value) {
                    child.secrets.push(s.value);
                }
            }
        }
        for item in granted {
            match item {
                WidenItem::ReadOnly(p) => child.grants_ro.push(Entry {
                    value: p.display().to_string(),
                    from: granted_origin(),
                }),
                WidenItem::ReadWrite(p) => child.grants_rw.push(Entry {
                    value: p.display().to_string(),
                    from: granted_origin(),
                }),
                WidenItem::Secret(s) => {
                    if !child.secrets.contains(s) {
                        child.secrets.push(*s);
                    }
                }
            }
        }
        if !granted.is_empty() {
            child.widened = granted.to_vec();
            child.widened_because = because.map(str::to_string);
        }
        Ok(child)
    }

    /// What this perimeter asks the operator for, in the words a card shows.
    pub fn ask(&self, child: &str, items: &[WidenItem], because: Option<&str>) -> PerimeterAsk {
        PerimeterAsk {
            parent: self.name.clone(),
            child: child.to_string(),
            items: items.to_vec(),
            because: because.map(str::to_string),
        }
    }

    /// The perimeter as lines, for `/boundary` and for the sentence a spawn is told.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!(
            "perimeter: `{}` ({} level)",
            self.name,
            match self.level {
                Level::Wide => "top",
                Level::Tight => "subagent",
            }
        )];
        for (key, entries) in [
            ("grants_ro", &self.grants_ro),
            ("grants_rw", &self.grants_rw),
            ("hide", &self.hide),
        ] {
            for e in entries {
                out.push(format!("  {key} {} - {}", e.value, e.from));
            }
        }
        for e in &self.env {
            let how = match &e.value.1 {
                EnvValue::Constant(v) => format!("= {v}"),
                EnvValue::File(f) => format!("from the file {f}, masked"),
                EnvValue::Command(c) => format!("from `{c}` run outside the box, masked"),
            };
            out.push(format!("  env {} {how}", e.value.0));
        }
        if !self.secrets.is_empty() {
            out.push(format!(
                "  secrets: {} (injected outside the box, values never printed)",
                self.secrets
                    .iter()
                    .map(|s| s.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.widened.is_empty() {
            out.push(format!(
                "  WIDENED at its spawn, at the operator's word: {} - {}",
                self.widened
                    .iter()
                    .map(|i| i.label())
                    .collect::<Vec<_>>()
                    .join(", "),
                self.widened_because.as_deref().unwrap_or("no reason given")
            ));
        }
        out
    }
}

/// The origin entry a granted widening writes: it came from a person's answer, not from a file.
fn granted_origin() -> Origin {
    Origin {
        file: PathBuf::from("an operator's answer at the spawn"),
        line: 0,
        user: true,
    }
}

/// Is `path` one of the toolchain paths the tight level reads anyway? A child asking for what it
/// already has is not a widening.
fn toolchain(path: &Path, home: &Path) -> bool {
    super::confine::GRANT_CANDIDATES
        .iter()
        .any(|c| path.starts_with(home.join(c)))
}

fn a_variable_name(k: &str) -> bool {
    !k.is_empty()
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn read_one(home: &Path, raw: &str) -> Result<String, String> {
    let p = match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(raw),
    };
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(text.trim().to_string())
}

fn run_one(cmd: &str) -> Result<String, String> {
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("`{cmd}` could not run: {e}"))?;
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if out.status.success() && !line.is_empty() {
        Ok(line)
    } else {
        Err(format!(
            "`{cmd}` answered nothing (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A grant or a secret a spawn asks for beyond the perimeter it starts from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Widen {
    pub grants_ro: Vec<String>,
    pub grants_rw: Vec<String>,
    pub secrets: Vec<Secret>,
    /// The reason, in the caller's words: a card without one is a card nobody can answer.
    pub because: Option<String>,
    /// Where the request was written, when a file wrote it.
    pub from: Option<Origin>,
}

impl Widen {
    pub fn is_empty(&self) -> bool {
        self.grants_ro.is_empty() && self.grants_rw.is_empty() && self.secrets.is_empty()
    }
}

/// One thing a widening adds, as the card lists it and as the granted set is carried.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum WidenItem {
    ReadOnly(PathBuf),
    ReadWrite(PathBuf),
    Secret(Secret),
}

impl WidenItem {
    pub fn of(path: PathBuf, rw: bool) -> WidenItem {
        if rw {
            WidenItem::ReadWrite(path)
        } else {
            WidenItem::ReadOnly(path)
        }
    }

    pub fn label(&self) -> String {
        match self {
            WidenItem::ReadOnly(p) => format!("ro {}", p.display()),
            WidenItem::ReadWrite(p) => format!("rw {}", p.display()),
            WidenItem::Secret(s) => format!("the `{}` secret", s.name()),
        }
    }

    /// The arg spelling a caller may type: `ro:/path`, `rw:/path`, `secret:github`. Written
    /// down in a child's record too, so a resumed child keeps what it was granted.
    pub fn arg(&self) -> String {
        match self {
            WidenItem::ReadOnly(p) => format!("ro:{}", p.display()),
            WidenItem::ReadWrite(p) => format!("rw:{}", p.display()),
            WidenItem::Secret(s) => format!("secret:{}", s.name()),
        }
    }

    /// The arg spelling back, or why it is not one.
    pub fn parse(word: &str) -> Result<WidenItem, String> {
        let word = word.trim();
        let (kind, rest) = word.split_once(':').ok_or_else(|| {
            format!("`{word}` is not `ro:<path>`, `rw:<path>` or `secret:<name>`")
        })?;
        match kind {
            "ro" | "rw" if !rest.is_empty() => Ok(WidenItem::of(PathBuf::from(rest), kind == "rw")),
            "secret" => Secret::parse(rest).map(WidenItem::Secret).ok_or_else(|| {
                format!(
                    "`{rest}` is not a secret this build knows; the names are: {}",
                    Secret::ALL
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
            _ => Err(format!(
                "`{word}` is not `ro:<path>`, `rw:<path>` or `secret:<name>`"
            )),
        }
    }
}

/// A widening a spawn puts to the operator. `Tier::AlwaysAsk`, so no mode decides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerimeterAsk {
    /// The perimeter the child starts from.
    pub parent: String,
    /// The perimeter the child asks for.
    pub child: String,
    pub items: Vec<WidenItem>,
    pub because: Option<String>,
}

impl PerimeterAsk {
    /// The one line the card's subject is.
    pub fn summary(&self) -> String {
        format!(
            "a child of this session (`{}`) asks for a WIDER perimeter than `{}`: {}",
            self.child,
            self.parent,
            self.items
                .iter()
                .map(|i| i.label())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// What the operator said about a widening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerimeterGrant {
    /// Carried by that child and by nothing else: its own children inherit at most this set.
    Granted,
    Refused(String),
    /// Nobody was there to ask. Treated as a refusal by every caller: a widening the mode could
    /// have decided would be the model granting itself the box.
    NotAsked,
}

impl PerimeterGrant {
    pub fn granted(&self) -> bool {
        matches!(self, PerimeterGrant::Granted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;

    fn entry<T>(value: T, user: bool) -> Entry<T> {
        Entry {
            value,
            from: Origin {
                file: PathBuf::from(if user {
                    "/h/.config/letibot/leticode.toml"
                } else {
                    "/p/leticode.toml"
                }),
                line: 4,
                user,
            },
        }
    }

    fn table(name: &str, body: PerimeterTable) -> PerimeterTable {
        PerimeterTable {
            name: name.to_string(),
            ..body
        }
    }

    /// No file, no change: the two defaults render the two views the code used to hardcode, and
    /// this is the test that keeps "a file that says nothing is byte-identical" true.
    #[test]
    fn the_default_perimeters_are_the_views_they_were() {
        let home = Path::new("/h");
        let present = ["/h/.gitconfig", "/h/.cargo"];
        for (level, name) in [(Level::Wide, TOP_LEVEL), (Level::Tight, SUBAGENT)] {
            let p = Perimeter::default_for(level);
            assert_eq!(p.name, name);
            let got = p
                .render(
                    &Mode::WRITES_ALLOWED,
                    "here",
                    &present,
                    home,
                    &SecretEnv::default(),
                )
                .expect("the default perimeter resolves");
            let want =
                seat::seat_for_level(level, &Mode::WRITES_ALLOWED, "here", &present, home, None);
            assert_eq!(got, want, "{name} is not the view it was");
        }
        assert_eq!(level_of(TOP_LEVEL), Level::Wide);
        assert_eq!(level_of("reviewer"), Level::Tight);
    }

    /// Which secrets a perimeter carries is the perimeter's, not the code's: the top level has the
    /// token, a child has none, and a value a file or a command gives is a secret too.
    #[test]
    fn secrets_follow_the_perimeter() {
        let home = std::env::temp_dir().join(format!("letibot-perimeter-s-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("a fixture home");
        std::fs::write(home.join("token"), "s3cr3t-from-a-file\n").expect("a token");
        let mut p = Perimeter::default_for(Level::Tight);
        assert!(!p.carries(Secret::Github));
        assert!(Perimeter::default_for(Level::Wide).carries(Secret::Github));
        p.env.push(entry(
            ("DEPLOY_TOKEN".to_string(), EnvValue::File("~/token".into())),
            true,
        ));
        p.env.push(entry(
            ("RUST_LOG".to_string(), EnvValue::Constant("info".into())),
            true,
        ));
        let (values, said) = p.secret_values(&home);
        assert!(said.is_empty(), "{said:?}");
        assert!(
            values
                .0
                .iter()
                .any(|(k, v)| k == "DEPLOY_TOKEN" && v == "s3cr3t-from-a-file"),
            "{values:?}"
        );
        assert!(
            !values.0.iter().any(|(k, _)| k == "RUST_LOG"),
            "a constant is not a secret"
        );
        let view = p
            .render(
                &Mode::WRITES_ALLOWED,
                "here",
                &[] as &[&str],
                &home,
                &values,
            )
            .expect("renders");
        assert!(view.env.contains(&("RUST_LOG".into(), "info".into())));
        assert!(!format!("{view:?}").contains("s3cr3t-from-a-file"));
        let said = p.lines().join("\n");
        assert!(
            said.contains("DEPLOY_TOKEN from the file ~/token, masked"),
            "{said}"
        );
        assert!(!said.contains("s3cr3t"), "{said}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The pairs a PLACEMENT hands over: a literal is spellable, a value a file or a command gave is
    /// a secret whatever it is called, and one that cannot be read is a line rather than a silent
    /// absence. A named `secrets` grant is in neither list - it is the seat's injection.
    #[test]
    fn a_placements_pairs_are_split_by_provenance_and_a_refusal_is_said() {
        let home = std::env::temp_dir().join(format!("letibot-perimeter-e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("a fixture home");
        std::fs::write(home.join("token"), "s3cr3t-from-a-file\n").expect("a token");
        let mut p = Perimeter::default_for(Level::Wide);
        p.env.push(entry(
            ("RUST_LOG".to_string(), EnvValue::Constant("info".into())),
            true,
        ));
        p.env.push(entry(
            (
                "A_PLAINLY_NAMED".to_string(),
                EnvValue::File("~/token".into()),
            ),
            true,
        ));
        p.env.push(entry(
            (
                "A_PLAINLY_NAMED_TOO".to_string(),
                EnvValue::File("~/nothing-here".into()),
            ),
            true,
        ));
        p.env.push(entry(
            ("BASH_ENV".to_string(), EnvValue::Constant("/tmp/no".into())),
            true,
        ));
        let (spelled, secret, said) = p.env_pairs(&home);
        assert_eq!(spelled, vec![("RUST_LOG".to_string(), "info".to_string())]);
        assert_eq!(
            secret,
            vec![(
                "A_PLAINLY_NAMED".to_string(),
                "s3cr3t-from-a-file".to_string()
            )],
            "a value a file gave is a secret whatever it is called"
        );
        assert!(
            said.iter()
                .any(|s| s.contains("BASH_ENV") && s.contains("refused")),
            "{said:?}"
        );
        assert!(
            said.iter().any(|s| s.contains("A_PLAINLY_NAMED_TOO")),
            "an unreadable value is said, not dropped: {said:?}"
        );
        // The named grant is not a placement's business, even at a level that carries it.
        assert!(
            !spelled
                .iter()
                .chain(secret.iter())
                .any(|(k, _)| k == "GH_TOKEN")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A child starts from its parent's perimeter at the tight level and adds what it names: the
    /// hide list grows, a grant inside the parent's is kept, and the parent's secrets are the only
    /// ones it can carry.
    #[test]
    fn a_child_starts_from_its_parents_perimeter_and_only_narrows() {
        let home = Path::new("/h");
        let mut parent = Perimeter::default_for(Level::Wide);
        parent
            .hide
            .push(entry("~/datasets/private".to_string(), true));
        let named = table(
            "reviewer",
            PerimeterTable {
                name: String::new(),
                grants_ro: vec![entry("~/.cargo".to_string(), true)],
                grants_rw: Vec::new(),
                hide: vec![entry("~/models".to_string(), false)],
                env: vec![entry(
                    ("RUST_LOG".to_string(), EnvValue::Constant("warn".into())),
                    false,
                )],
                secrets: vec![entry(Secret::Github, true)],
            },
        );
        let child = parent.child_of(Some(&named), &[], None).expect("a child");
        assert_eq!(child.level, Level::Tight);
        assert_eq!(child.name, "reviewer");
        let hides: Vec<&str> = child.hide.iter().map(|e| e.value.as_str()).collect();
        assert!(
            hides.contains(&"~/datasets/private") && hides.contains(&"~/models"),
            "the child's hide list is its parent's and its own: {hides:?}"
        );
        assert!(child.grants_ro.iter().any(|e| e.value == "~/.cargo"));
        assert!(
            child.carries(Secret::Github),
            "a table may hand down a secret its parent carries"
        );
        // And a table that names a secret the parent does not carry hands down nothing.
        let tight = Perimeter::default_for(Level::Tight);
        let child = tight.child_of(Some(&named), &[], None).expect("a child");
        assert!(!child.carries(Secret::Github), "narrow only");
        // A child's env is its parent's: a table's constant is not handed to a child.
        assert!(child.env.is_empty(), "{child:?}");
        let _ = home;
    }

    /// What a child asks for that its parent does not hold is a widening, and only that: a path
    /// inside a grant, the toolchain and the level's own reads are not.
    #[test]
    fn what_a_child_asks_beyond_its_parent_is_a_widening() {
        let home = Path::new("/h");
        let mut parent = Perimeter::default_for(Level::Tight);
        parent.grants_ro.push(entry("~/datasets".to_string(), true));
        let want = Widen {
            grants_ro: vec![
                "~/datasets/cifar".to_string(),
                "~/.cargo/registry".to_string(),
                "~/.local/state".to_string(),
            ],
            grants_rw: vec!["~/.local/state/placements".to_string()],
            secrets: vec![Secret::Github],
            because: Some("the child removes placements".into()),
            from: None,
        };
        let items = parent.widening(&want, home).expect("a widening");
        assert_eq!(
            items,
            vec![
                WidenItem::ReadOnly(PathBuf::from("/h/.local/state")),
                WidenItem::ReadWrite(PathBuf::from("/h/.local/state/placements")),
                WidenItem::Secret(Secret::Github),
            ]
        );
        assert!(items[0].label().contains("/h/.local/state"));
        // A narrowing request asks for nothing.
        let narrow = Widen {
            grants_ro: vec!["~/datasets/cifar".to_string()],
            ..Widen::default()
        };
        assert!(parent.widening(&narrow, home).expect("narrow").is_empty());
    }

    /// A widened child's own child inherits at most the widened set: what the operator granted is
    /// carried down, and anything further is another card.
    #[test]
    fn a_widened_childs_own_child_cannot_exceed_it() {
        let home = Path::new("/h");
        let parent = Perimeter::default_for(Level::Tight);
        let granted = vec![WidenItem::ReadOnly(PathBuf::from("/h/.local/state"))];
        let child = parent
            .child_of(None, &granted, Some("the child removes placements"))
            .expect("a widened child");
        assert!(
            child.lines().join("\n").contains("WIDENED at its spawn"),
            "{:?}",
            child.lines()
        );
        assert!(child.lines().join("\n").contains("removes placements"));
        let grandchild = child.child_of(None, &[], None).expect("a grandchild");
        assert!(
            grandchild
                .grants_ro
                .iter()
                .any(|e| e.value == "/h/.local/state"),
            "the granted path is inherited: {grandchild:?}"
        );
        let same = Widen {
            grants_ro: vec!["/h/.local/state".to_string()],
            ..Widen::default()
        };
        assert!(
            grandchild.widening(&same, home).expect("held").is_empty(),
            "the grandchild already holds what its parent was granted"
        );
        let more = Widen {
            grants_ro: vec!["/h/elsewhere".to_string()],
            ..Widen::default()
        };
        assert_eq!(
            grandchild.widening(&more, home).expect("a widening").len(),
            1,
            "anything past what its parent was granted is a card again"
        );
    }

    /// The never-grant list holds at a spawn too, and the refusal names the file, the line and the
    /// key the operator could edit.
    #[test]
    fn a_widening_cannot_ask_for_what_no_key_grants() {
        let home = Path::new("/h");
        let parent = Perimeter::default_for(Level::Tight);
        for bad in ["~/.ssh", "~/.ssh/id_ed25519", "/h", "/"] {
            let want = Widen {
                grants_ro: vec![bad.to_string()],
                ..Widen::default()
            };
            let err = parent.widening(&want, home).expect_err(bad);
            assert!(err.contains(bad), "{bad}: {err}");
            assert!(
                err.contains("never-grant list") || err.contains("whole home"),
                "{err}"
            );
        }
    }

    /// A project file may narrow a perimeter and may not grant one: it is a file the sessions it
    /// governs can edit.
    #[test]
    fn a_project_table_may_not_widen() {
        let p = table(
            "reviewer",
            PerimeterTable {
                name: String::new(),
                grants_ro: vec![entry("~/datasets".to_string(), false)],
                ..PerimeterTable::default()
            },
        );
        let err = Perimeter::named("reviewer", Some(&p)).expect_err("a project grant");
        assert!(err.contains("a project file may not grant"), "{err}");
        assert!(err.contains("/p/leticode.toml:4:"), "{err}");
        let s = table(
            "reviewer",
            PerimeterTable {
                name: String::new(),
                secrets: vec![entry(Secret::Github, false)],
                ..PerimeterTable::default()
            },
        );
        let err = Perimeter::named("reviewer", Some(&s)).expect_err("a project secret");
        assert!(err.contains("may not hand a session a secret"), "{err}");
        assert!(Perimeter::named("reviewer", None).is_ok());
    }

    /// A named table is the default for its level plus what the file adds, which is the whole of
    /// "the two seat levels are config": the project's table narrows the default and nothing else.
    #[test]
    fn a_named_table_narrows_its_levels_default() {
        let home = Path::new("/h");
        let project = table(
            "subagent",
            PerimeterTable {
                name: String::new(),
                hide: vec![entry("~/models".to_string(), false)],
                env: vec![entry(
                    ("RUST_LOG".to_string(), EnvValue::Constant("warn".into())),
                    false,
                )],
                ..PerimeterTable::default()
            },
        );
        let child = Perimeter::named(SUBAGENT, Some(&project)).expect("a narrowed level");
        assert_eq!(child.level, Level::Tight);
        let view = child
            .render(
                &Mode::WRITES_ALLOWED,
                "here",
                &[] as &[&str],
                home,
                &SecretEnv::default(),
            )
            .expect("renders");
        assert!(
            view.masked.contains(&PathBuf::from("/h/models")),
            "{view:?}"
        );
        assert!(view.env.contains(&("RUST_LOG".into(), "warn".into())));
        let said = child.lines().join("\n");
        assert!(said.contains("hide ~/models"), "{said}");
    }
}
