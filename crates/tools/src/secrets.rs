//! The operator's secrets, in one table. Every rule reads it and none keeps a copy: the gate's
//! never-write list, the flow rule's disclosure check, the seat's view mask, the file tools' fence
//! and the shadow's read guard. A second copy is how one falls behind the others.

/// Who refuses an entry. A narrower scope is refused by every reader of a wider one too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    /// Masked in every seat's view and never written.
    Both,
    /// The operator's ruling: a top-level session runs these stores' own CLIs (`gh`, `psql`,
    /// `rclone`, `vault`, ...), so they are masked in a child's view only. A read is still a
    /// disclosure to the flow rule and the file tools, and the gate never writes one.
    TightOnly,
    /// Masked in no view, because a tool the operator runs needs it: a command reading it asks,
    /// and a file tool, which has no card to show, refuses.
    Unmasked,
    /// Refused only to a read the shadow grants with no person asked: the daemon's own files,
    /// other agents' homes and kernel state. The seat handles these by other means.
    Review,
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

const fn unmasked(path: &'static str) -> Store {
    Store {
        path,
        seat: Seat::Unmasked,
    }
}

const fn review(path: &'static str) -> Store {
    Store {
        path,
        seat: Seat::Review,
    }
}

/// Every store, home-relative unless absolute. A browser profile is a credential store that happens to be shaped
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
    // Stores whose CLIs the operator runs from a top-level session: a child's alone.
    tight(".pgpass"),
    tight(".my.cnf"),
    tight(".s3cfg"),
    tight(".vault-token"),
    tight(".config/rclone"),
    tight(".config/sops"),
    tight(".config/hub"),
    tight(".claude/.credentials.json"),
    tight(".codex/auth.json"),
    // `.docker` and not only its config file: a directory of registries is a read-side secret.
    unmasked(".docker"),
    unmasked(".config/flowy"),
    unmasked("/etc/shadow"),
    unmasked("/etc/sudoers"),
    unmasked("/etc/ssl/private"),
    unmasked("/private/etc/ssl/private"),
    review(".config/letibot"),
    review(".local/share/letibot"),
    review(".claude"),
    review(".codex"),
    review(".config/claude"),
    review("bin/fleet"),
    review("/proc"),
    review("/sys"),
    review("/dev"),
];

/// Files that are secret wherever they sit, matched on the final path segment. Exact names and
/// extensions only, because a substring test puts every path with "key" in it in the store.
pub const SECRET_FILE_NAMES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    "credentials",
    ".credentials.json",
    "shadow",
];

pub const SECRET_EXTENSIONS: &[&str] = &[".pem", ".p12", ".pfx", ".jks", ".keystore"];

/// The stores a view masks in some seat: the gate's never-write list.
pub fn stores() -> Vec<&'static str> {
    scoped(&[Seat::Both, Seat::TightOnly])
}

/// Every place whose bytes are a disclosure: the flow rule's read check and the file tools' fence.
pub fn read_secrets() -> Vec<&'static str> {
    scoped(&[Seat::Both, Seat::TightOnly, Seat::Unmasked])
}

/// Every entry: what a read the shadow grants must not reach or contain.
pub fn review_denied() -> Vec<&'static str> {
    STORES.iter().map(|s| s.path).collect()
}

fn scoped(seats: &[Seat]) -> Vec<&'static str> {
    STORES
        .iter()
        .filter(|s| seats.contains(&s.seat))
        .map(|s| s.path)
        .collect()
}

/// What the seat's profile denies after every allow: everything in both seats, and the stores of
/// the operator's own CLIs in a child's only.
pub fn seat_denied(wide: bool) -> Vec<&'static str> {
    match wide {
        true => scoped(&[Seat::Both]),
        false => scoped(&[Seat::Both, Seat::TightOnly]),
    }
}

/// Home-relative files the top-level seat reads and may not write: a commit needs the identity,
/// and a rewritten global git config runs a helper or hook in the operator's own git later.
/// The directory is listed by file so a store inside it stays masked.
pub const SEAT_READ_ONLY: &[&str] = &[
    ".gitconfig",
    ".config/git/config",
    ".config/git/ignore",
    ".config/git/attributes",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The readers of this table, held against each scope. A copy that drifts is the defect this
    /// module exists for: the seat's fence was missing stores the gate already knew.
    #[test]
    fn every_store_is_denied_by_the_gate_the_flow_rule_and_the_seat() {
        let gate = crate::adjudicate::never_write();
        let flow = read_secrets();
        let wide = crate::exec::seatbelt::deny_under_home(true);
        let tight = crate::exec::seatbelt::deny_under_home(false);
        let review = review_denied();
        for s in STORES {
            let p = &s.path;
            assert!(review.contains(p), "{p} is not refused to the shadow");
            match s.seat {
                Seat::Both => assert!(
                    gate.contains(p) && flow.contains(p) && wide.contains(p) && tight.contains(p),
                    "{p} is in every list"
                ),
                // The operator's ruling: a top-level session runs these CLIs, and a child has no
                // credential to spend.
                Seat::TightOnly => assert!(
                    gate.contains(p) && flow.contains(p) && !wide.contains(p) && tight.contains(p),
                    "{p} is a child's alone"
                ),
                Seat::Unmasked => assert!(
                    flow.contains(p) && !gate.contains(p) && !tight.contains(p),
                    "{p} is a read-side secret only"
                ),
                Seat::Review => assert!(!flow.contains(p), "{p} is the shadow's alone"),
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
        let tight = crate::exec::seatbelt::deny_under_home(false);
        for path in [
            ".config/gh",
            ".config/hub",
            ".pgpass",
            ".my.cnf",
            ".s3cfg",
            ".vault-token",
            ".config/rclone",
            ".config/sops",
            ".claude/.credentials.json",
            ".codex/auth.json",
        ] {
            let s = STORES.iter().find(|s| s.path == path);
            assert_eq!(
                s.map(|s| s.seat),
                Some(Seat::TightOnly),
                "{path}: the top-level seat runs its CLI, by the operator's ruling"
            );
            assert!(!seat.contains(&path), "{path} is masked in the wide seat");
            assert!(tight.contains(&path), "{path} is readable in a tight seat");
        }
    }
}
