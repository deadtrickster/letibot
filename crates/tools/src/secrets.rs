//! The operator's credential stores, in one list. Three rules read it and none keeps a copy: the
//! gate's never-write list, the flow rule's disclosure check, and the macOS seat's fence. A second
//! copy is how one of the three falls behind the others, which is what happened to the seat's.

/// Where the macOS seat's fence puts a store. The operator's ruling is that a top-level session
/// needs `gh`, so its config is denied only in a child's view; everything else is denied in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    Both,
    TightOnly,
}

/// A store relative to the operator's home, and what the seat does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Store {
    pub path: &'static str,
    pub seat: Seat,
}

const fn both(path: &'static str) -> Store {
    Store {
        path,
        seat: Seat::Both,
    }
}

const fn tight(path: &'static str) -> Store {
    Store {
        path,
        seat: Seat::TightOnly,
    }
}

/// Every store, home-relative. A browser profile is a credential store that happens to be shaped
/// like a directory: a live session cookie is a login. The credential files are named
/// individually because their directory is otherwise the operator's own work.
pub const STORES: &[Store] = &[
    both(".ssh"),
    both(".gnupg"),
    both(".aws"),
    both(".kube"),
    both(".netrc"),
    tight(".config/gh"),
    both(".password-store"),
    both(".docker/config.json"),
    both(".mozilla"),
    both(".thunderbird"),
    both(".config/google-chrome"),
    both(".config/chromium"),
    both(".config/BraveSoftware"),
    both(".config/microsoft-edge"),
    both(".config/vivaldi"),
    both(".config/opera"),
    both("Library/Application Support/Google/Chrome"),
    both("Library/Application Support/Firefox"),
    both("Library/Application Support/Arc"),
    both("Library/Containers/com.apple.Safari"),
    both("Library/Safari"),
    both("AppData/Local/Google/Chrome"),
    both("AppData/Roaming/Mozilla"),
    both("Library/Keychains"),
    both(".local/share/keyrings"),
    both(".gnome2/keyrings"),
    both(".config/gcloud"),
    both(".azure"),
    both(".config/op"),
    both(".config/Bitwarden"),
    both(".git-credentials"),
    both(".config/git/credentials"),
    both(".npmrc"),
    both(".pypirc"),
    both(".cargo/credentials"),
    both(".cargo/credentials.toml"),
];

/// Every store, for the gate's never-write list and the flow rule's read check.
pub fn stores() -> Vec<&'static str> {
    STORES.iter().map(|s| s.path).collect()
}

/// What the seat's profile denies after every allow: everything in both seats, and `gh`'s config
/// in a child's only.
pub fn seat_denied(wide: bool) -> Vec<&'static str> {
    STORES
        .iter()
        .filter(|s| !wide || s.seat == Seat::Both)
        .map(|s| s.path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three readers of this list, held against each other. A copy that drifts is the defect
    /// this module exists for: the seat's fence was missing stores the gate already knew.
    #[test]
    fn every_store_is_denied_by_the_gate_the_flow_rule_and_the_seat() {
        let gate = crate::adjudicate::never_write();
        let flow = crate::intent::secret_dirs();
        let wide = crate::exec::seatbelt::deny_under_home(true);
        let tight = crate::exec::seatbelt::deny_under_home(false);
        for s in STORES {
            assert!(
                gate.contains(&s.path),
                "{} is not in the gate's list",
                s.path
            );
            assert!(flow.contains(&s.path), "{} is not in the flow rule", s.path);
            match s.seat {
                Seat::Both => assert!(
                    wide.contains(&s.path),
                    "{} is missing from the top-level seat's fence",
                    s.path
                ),
                // The operator's ruling: a top-level session needs gh, and a child has no token
                // to spend.
                Seat::TightOnly => assert!(
                    !wide.contains(&s.path) && tight.contains(&s.path),
                    "{} is a child's alone",
                    s.path
                ),
            }
        }
    }

    /// The entries the release review named as missing, one line each so a deletion is a failure
    /// that says which store went.
    #[test]
    fn the_stores_the_review_named_are_fenced() {
        let seat = crate::exec::seatbelt::deny_under_home(true);
        for path in [
            "Library/Application Support/Firefox",
            "Library/Application Support/Arc",
            ".config/BraveSoftware",
            ".config/microsoft-edge",
            "Library/Containers/com.apple.Safari",
            ".git-credentials",
            ".config/git/credentials",
            ".npmrc",
            ".pypirc",
            ".kube",
            ".config/gcloud",
            ".cargo/credentials",
        ] {
            assert!(seat.contains(&path), "{path} is not fenced by the seat");
        }
        let gh = STORES.iter().find(|s| s.path == ".config/gh");
        assert_eq!(
            gh.map(|s| s.seat),
            Some(Seat::TightOnly),
            "the top-level seat needs gh, by the operator's ruling"
        );
    }
}
