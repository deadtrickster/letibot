//! `~/.config/letibot` as a seat sees it: the one public file, and the files the operator
//! granted, readable inside a mask that covers the directory.
//!
//! The directory is masked whole in every seat — `keys/` (the hosts' identity files), `providers.toml`
//! and its API keys, `permission.json`, `leticode.toml`, `plugin-trust.json`, `hosts.toml` — and this
//! is the rule that says what the mask leaves through. Two things, and nothing else:
//!
//! * `known_hosts`, which is **public data**: other machines' host public keys. `ssh` reads it
//!   before it authenticates, so a view that holds the identity file and not this one refuses the
//!   connection at the host-key check with "Operation not permitted" and never reaches the key.
//! * a path INSIDE the directory the operator granted into this session's view — a host's identity
//!   file answered for on a card, or a `--grant-ro`. The mask is a mask, not a widening ban: what
//!   the operator put in the view is in it.
//!
//! Never the directory itself, and never a file nobody named. A rule that lists what is readable
//! is a rule a file added later cannot fall into; a rule that lists what is *not* would.

use std::path::{Path, PathBuf};

/// The directory, relative to the operator's home.
pub const CONFIG_REL: &str = ".config/letibot";

/// The names inside it that are readable in every seat, whatever any config says: letibot's own
/// known-hosts file, whose contents are public host keys.
pub const PUBLIC: &[&str] = &[crate::remote::KNOWN_HOSTS_FILE];

/// The paths inside `dir` a mask leaves readable: [`PUBLIC`], then every granted path that is
/// `dir` itself or under it. Sorted and deduplicated, so a rendering is deterministic.
pub fn readable<'a, I>(dir: &Path, grants: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = &'a Path>,
{
    let mut out: Vec<PathBuf> = PUBLIC.iter().map(|name| dir.join(name)).collect();
    for g in grants {
        if g.starts_with(dir) {
            out.push(g.to_path_buf());
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/h/.config/letibot")
    }

    /// The public file reads, and nothing else in the directory does — not the identity files, not
    /// the providers file, not the directory itself. This is the whole policy, and it is a list of
    /// names on purpose: anything the rule does not name stays under the mask.
    #[test]
    fn the_public_file_reads_and_the_rest_of_the_directory_does_not() {
        let d = dir();
        let got = readable(&d, std::iter::empty());
        assert_eq!(got, vec![d.join("known_hosts")]);
        for p in [
            d.clone(),
            d.join("keys"),
            d.join("keys/lab2x1"),
            d.join("providers.toml"),
            d.join("permission.json"),
            d.join("leticode.toml"),
            d.join("hosts.toml"),
        ] {
            assert!(
                !got.iter().any(|r| p.starts_with(r)),
                "{} must stay masked",
                p.display()
            );
        }
    }

    /// A granted file inside the directory is readable, because the operator put it in the view;
    /// a grant OUTSIDE the directory adds nothing here, and the public file is always there.
    #[test]
    fn a_grant_inside_the_directory_is_readable_and_a_grant_outside_it_is_not_this_rule() {
        let d = dir();
        let key = d.join("keys/lab2x1");
        let elsewhere = PathBuf::from("/h/datasets");
        let got = readable(&d, [key.as_path(), elsewhere.as_path()]);
        assert_eq!(got, vec![key.clone(), d.join("known_hosts")]);
        // And the inverse: no grant, and the key is not in the set at all.
        let got = readable(&d, [elsewhere.as_path()]);
        assert_eq!(got, vec![d.join("known_hosts")]);
    }
}
