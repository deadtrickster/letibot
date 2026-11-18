//! `~/.config/letibot` as a seat sees it: the two halves of an ssh call, and the files the operator
//! granted, readable inside a mask that covers the directory.
//!
//! The directory is masked whole in every seat — `providers.toml` and its API keys,
//! `permission.json`, `leticode.toml`, `plugin-trust.json`, `hosts.toml` — and this is the rule that
//! says what the mask leaves through. Two things, and nothing else:
//!
//! * `known_hosts` and `keys/`, which are one ssh call. `ssh` opens the host keys *before* it
//!   authenticates and loads the identity file after, so a view holding one half and not the other
//!   fails with "Operation not permitted" at whichever it opened first and never reaches the far
//!   machine. The keys are in the hole because the operator put them there for this: `keys/<host>`
//!   is a fleet key letibot's own ssh presents ([`crate::remote::Paths::default_key`]), not
//!   `~/.ssh`, which is a deny of its own and stays masked from every seat.
//! * a path INSIDE the directory the operator granted into this session's view — a host's identity
//!   file answered for on a card, or a `--grant-ro`. The mask is a mask, not a widening ban: what
//!   the operator put in the view is in it.
//!
//! Never the directory itself, and never a file nobody named. A rule that lists what is readable
//! is a rule a file added later cannot fall into; a rule that lists what is *not* would.

use std::path::{Path, PathBuf};

/// The directory, relative to the operator's home.
pub const CONFIG_REL: &str = ".config/letibot";

/// The names inside it that are readable in every seat, whatever any config says: letibot's
/// known-hosts file and the directory of hosts' identity files. One ssh call needs both, and
/// neither is the operator's own secret — the host keys are public data and `keys/<host>` is the key
/// letibot's ssh presents to a declared host. `providers.toml`, `permission.json`, `leticode.toml`
/// and `hosts.toml` are not here, and a name added to the directory later does not fall in.
pub const ALWAYS_READABLE: &[&str] = &[crate::remote::KNOWN_HOSTS_FILE, crate::remote::KEYS_DIR];

/// The paths inside `dir` a mask leaves readable: [`ALWAYS_READABLE`], then every granted path that
/// is `dir` itself or under it. Sorted and deduplicated, so a rendering is deterministic.
pub fn readable<'a, I>(dir: &Path, grants: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = &'a Path>,
{
    let mut out: Vec<PathBuf> = ALWAYS_READABLE.iter().map(|name| dir.join(name)).collect();
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

    /// The ssh pair reads — the host keys and the identity keys — and nothing else in the directory
    /// does: not the providers file, not the seed, not the directory itself. This is the whole
    /// policy, and it is a list of names on purpose: anything the rule does not name stays under the
    /// mask.
    ///
    /// The identity files are here because the operator put them there for sessions to use, and
    /// `ssh` needs both halves: it opens the host keys before it authenticates and loads the key
    /// after, so `known_hosts` alone dies at `Load key … Operation not permitted`.
    #[test]
    fn the_ssh_pair_reads_and_the_rest_of_the_directory_does_not() {
        let d = dir();
        let got = readable(&d, std::iter::empty());
        assert_eq!(got, vec![d.join("keys"), d.join("known_hosts")]);
        for p in [
            d.clone(),
            d.join("providers.toml"),
            d.join("permission.json"),
            d.join("leticode.toml"),
            d.join("hosts.toml"),
            d.join("plugin-trust.json"),
        ] {
            assert!(
                !got.iter().any(|r| p.starts_with(r)),
                "{} must stay masked",
                p.display()
            );
        }
        // A host's key reads without any grant: it is under the hole, not granted into it.
        assert!(got.iter().any(|r| d.join("keys/lab2x1").starts_with(r)));
    }

    /// A granted file inside the directory is readable, because the operator put it in the view;
    /// a grant OUTSIDE the directory adds nothing here, and the ssh pair is always there.
    #[test]
    fn a_grant_inside_the_directory_is_readable_and_a_grant_outside_it_is_not_this_rule() {
        let d = dir();
        let key = d.join("keys/lab2x1");
        let elsewhere = PathBuf::from("/h/datasets");
        let got = readable(&d, [key.clone().as_path(), elsewhere.as_path()]);
        assert_eq!(
            got,
            vec![d.join("keys"), key, d.join("known_hosts")],
            "the pair, then the granted file inside it"
        );
        // And a grant outside it changes nothing: the pair is the whole of it.
        let got = readable(&d, [elsewhere.as_path()]);
        assert_eq!(got, vec![d.join("keys"), d.join("known_hosts")]);
    }
}
