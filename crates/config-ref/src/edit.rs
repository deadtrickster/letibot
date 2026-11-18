//! A proposal's changes, applied to a file's own text. `toml_edit` is the reason this module
//! exists: the operator's comments, key order and spacing survive a write, so a file they
//! hand-edited stays theirs.

use std::collections::BTreeMap;

/// What a proposal asks for: key paths to set, and key paths to take out.
///
/// A path is dotted, as the reference spells it: `shadow.answer_reads`, `provider.<name>.key`.
/// A value is TOML source, parsed here rather than guessed at, so `"45m"` and `45m` cannot both
/// look like the string they differ on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edits {
    pub set: BTreeMap<String, String>,
    pub remove: Vec<String>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty()
    }

    pub fn len(&self) -> usize {
        self.set.len() + self.remove.len()
    }

    /// Every path named, set or removed, in one list: what a caller checks against the reference.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.set.keys().chain(self.remove.iter())
    }
}

/// `text` with `edits` applied. Comments, order and spacing outside the edited values are kept.
pub fn apply(text: &str, edits: &Edits) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("{e}"))?;
    for (path, value) in &edits.set {
        let segments = segments(path)?;
        let new = value
            .parse::<toml_edit::Value>()
            .map_err(|e| format!("`{path}`: {value} is not a TOML value: {e}"))?;
        let item = at(&mut doc, &segments, true)
            .ok_or_else(|| format!("`{path}`: `{path}` cannot be set here"))?;
        // The old value's own decorations carry the trailing comment and the spacing before
        // it; a set that dropped them would delete the operator's sentence beside the key.
        let new = match item.as_value() {
            Some(old) => {
                let mut new = new;
                *new.decor_mut() = old.decor().clone();
                new
            }
            None => new,
        };
        *item = toml_edit::Item::Value(new);
    }
    for path in &edits.remove {
        let segments = segments(path)?;
        let item = at(&mut doc, &segments, false)
            .ok_or_else(|| format!("`{path}` is not in the file, so there is nothing to remove"))?;
        if item.is_none() {
            return Err(format!(
                "`{path}` is not in the file, so there is nothing to remove"
            ));
        }
        *item = toml_edit::Item::None;
        sweep_none(doc.as_table_mut());
        drop_emptied(&mut doc, &segments);
    }
    Ok(doc.to_string())
}

/// A table whose last key a removal took is taken out with it: a `[queue]` header over nothing
/// is a suggestion that the table is still there. Only along the chain the removal emptied, so a
/// table the operator wrote empty stays theirs.
fn drop_emptied(doc: &mut toml_edit::DocumentMut, removed: &[&str]) {
    for depth in (1..removed.len()).rev() {
        let path = &removed[..depth];
        let empty = table_at(doc, path).is_some_and(|t| t.is_empty());
        if !empty {
            return;
        }
        let (parent, leaf) = path.split_at(path.len() - 1);
        if let Some(p) = table_at_mut(doc, parent) {
            p.remove(leaf[0]);
        }
    }
}

fn table_at<'d>(doc: &'d toml_edit::DocumentMut, path: &[&str]) -> Option<&'d toml_edit::Table> {
    let mut table = doc.as_table();
    for seg in path {
        table = table.get(seg)?.as_table()?;
    }
    Some(table)
}

fn table_at_mut<'d>(
    doc: &'d mut toml_edit::DocumentMut,
    path: &[&str],
) -> Option<&'d mut toml_edit::Table> {
    let mut table = doc.as_table_mut();
    for seg in path {
        table = table.get_mut(seg)?.as_table_mut()?;
    }
    Some(table)
}

fn segments(path: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "`{path}` is not a key path: write it dotted, as `shadow.answer_reads`"
        ));
    }
    Ok(parts)
}

/// The item at `segments`, created along the way when `create` is set: a proposal may name a
/// table the file does not have yet, and refusing that would refuse the ordinary edit.
fn at<'d>(
    doc: &'d mut toml_edit::DocumentMut,
    segments: &[&str],
    create: bool,
) -> Option<&'d mut toml_edit::Item> {
    let (first, rest) = segments.split_first()?;
    let mut item = doc.as_table_mut().entry(first).or_insert_with(|| {
        if rest.is_empty() {
            toml_edit::Item::None
        } else {
            toml_edit::Item::Table(toml_edit::Table::new())
        }
    });
    for seg in rest {
        // Only a table is descended into: a dotted path through an inline table is refused
        // rather than re-spelled, because the operator wrote that value as one line.
        let table = item.as_table_mut()?;
        item = table.entry(seg).or_insert_with(|| {
            if create {
                toml_edit::Item::Table(toml_edit::Table::new())
            } else {
                toml_edit::Item::None
            }
        });
    }
    Some(item)
}

/// A removed key leaves an empty table behind; one that was not in the file before is not a
/// table the operator wrote, so it is taken out rather than left as a header with nothing under it.
fn sweep_none(table: &mut toml_edit::Table) {
    let empty: Vec<String> = table
        .iter()
        .filter(|(_, i)| i.is_none())
        .map(|(k, _)| k.to_string())
        .collect();
    for k in empty {
        table.remove(&k);
    }
    for (_, item) in table.iter_mut() {
        if let Some(t) = item.as_table_mut() {
            sweep_none(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAND: &str = "\
# the operator's own note
[shadow]
enabled = [\"turn_end\"]   # only the reminder
mode = \"persistent\"

[exec]
foreground_seconds = 60
";

    fn edits(set: &[(&str, &str)], remove: &[&str]) -> Edits {
        Edits {
            set: set
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            remove: remove.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A set changes one value and leaves every other byte, comments included, where it was.
    #[test]
    fn a_set_keeps_the_comments_and_the_order() {
        let out = apply(HAND, &edits(&[("shadow.answer_reads", "true")], &[])).unwrap();
        assert!(
            out.starts_with("# the operator's own note\n[shadow]\n"),
            "{out}"
        );
        assert!(
            out.contains("enabled = [\"turn_end\"]   # only the reminder"),
            "{out}"
        );
        assert!(out.contains("answer_reads = true"), "{out}");
        assert!(
            out.find("answer_reads").unwrap() > out.find("mode").unwrap(),
            "a new key goes at the end of its table: {out}"
        );
    }

    /// Setting a key that already has a trailing comment keeps the comment.
    #[test]
    fn a_set_keeps_the_comment_beside_the_key() {
        let out = apply(HAND, &edits(&[("exec.foreground_seconds", "90")], &[])).unwrap();
        assert!(out.contains("foreground_seconds = 90"), "{out}");
        // The comment is the value's own decoration, so a set on a commented line keeps it.
        let out = apply(
            "foreground_seconds = 60  # the operator's\n",
            &edits(&[("foreground_seconds", "90")], &[]),
        )
        .unwrap();
        assert_eq!(out, "foreground_seconds = 90  # the operator's\n");
    }

    /// A path through a table the file does not have creates it, and removes leave nothing behind.
    #[test]
    fn a_new_table_and_a_removal() {
        let out = apply(HAND, &edits(&[("queue.gate_train", "4")], &[])).unwrap();
        assert!(out.contains("[queue]\ngate_train = 4"), "{out}");
        let out = apply(HAND, &edits(&[], &["exec.foreground_seconds"])).unwrap();
        assert!(!out.contains("foreground_seconds"), "{out}");
        // The table the file wrote for nothing is gone with the key that was under it.
        assert!(!out.contains("[exec]"), "{out}");
    }

    /// A value that is not TOML, a path that is not dotted, and a removal of nothing are refused.
    #[test]
    fn the_refusals() {
        let e = apply(HAND, &edits(&[("shadow.mode", "persistent")], &[])).unwrap_err();
        assert!(e.contains("not a TOML value"), "{e}");
        let e = apply(HAND, &edits(&[("shadow..mode", "1")], &[])).unwrap_err();
        assert!(e.contains("not a key path"), "{e}");
        let e = apply(HAND, &edits(&[], &["shadow.nope"])).unwrap_err();
        assert_eq!(
            e,
            "`shadow.nope` is not in the file, so there is nothing to remove"
        );
        let e = apply("not toml = = =", &edits(&[], &[])).unwrap_err();
        assert!(!e.is_empty());
    }
}
