//! This box's peering, as the coordinator's owner sets it up: is there a cookie, is peering
//! switched on, where does a project's repository live, and what are this box's seats on.
//!
//! The room crate links the boxes and knows nothing of git or of a config file, and this is the
//! half that does. Nothing here runs unless the operator wrote `~/.config/letibot/cookie`: a box
//! with no cookie is a box whose coordinator is exactly what it was before peering existed.

use std::path::Path;
use std::time::Duration;

use letibot_room::Server;
use letibot_room::peer::cookie;
use letibot_room::peer::{Cookie, Peer, PeerConfig, ProjectProfile, RepoIdentity, SeatProfile};

/// How often this box re-reads what its seats are on and what its projects are. A seat's task line
/// changes when it says so, and the peer sends a list only when it differs, so this is a poll and
/// not a line per edit.
pub const REFRESH: Duration = Duration::from_secs(10);

/// Where the cookie lives: `~/.config/letibot/cookie`, beside the file that names the model.
pub fn cookie_path() -> std::path::PathBuf {
    cookie::path_in(
        &letibot_provider::keys::config_file()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| Path::new(".").to_path_buf()),
    )
}

/// Open this box's peering and give it to `server`, or say why this box does not peer and answer
/// `None` - which is the ordinary case and not an error.
///
/// The refusal is by name and goes to the log: a cookie at a mode anybody could read is the one
/// thing here that must not be silent, because the operator believes the box is private.
pub fn start(server: &Server) -> Option<Peer> {
    let user = match crate::leticode_config::load_user() {
        Ok(config) => config,
        Err(why) => {
            eprintln!("harnessd: leticode.toml: {why} - this box does not peer");
            return None;
        }
    };
    if !user.peering.peers() {
        eprintln!("harnessd: [peering] enabled = false - this box does not peer");
        return None;
    }
    let path = cookie_path();
    let cookie = match Cookie::load(&path) {
        Ok(cookie) => cookie,
        Err(cookie::CookieError::Absent) => return None,
        Err(why) => {
            eprintln!("harnessd: {why}");
            return None;
        }
    };
    let mut config = PeerConfig::new(server.seat_host(), cookie);
    if let Some(port) = user.peering.port {
        config = config.with_port(port);
    }
    if let Some(port) = user.peering.group_port {
        let group = user
            .peering
            .group
            .as_deref()
            .and_then(|g| g.parse().ok())
            .unwrap_or(config.group);
        config = config.at_group(group, port);
    }
    let (peer, events) = match Peer::open(config) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!(
                "harnessd: this box could not open a link listener for peering ({e}) - it does not \
                 peer, and its own room is unaffected"
            );
            return None;
        }
    };
    server.with_peer(peer.clone(), events);
    publish(server, &peer);
    eprintln!(
        "harnessd: peering as {} on port {} - a box with this LAN's cookie may link to it",
        peer.host(),
        peer.port()
    );
    let writing = peer.clone();
    let serving = server.clone();
    std::thread::spawn(move || {
        // The coordinator's stop takes the peer out of the server, and that is the end of this.
        while serving.peer().is_some() {
            std::thread::sleep(REFRESH);
            publish(&serving, &writing);
        }
    });
    Some(peer)
}

/// Say what this box's seats are on, and what its projects are: one call, so a seat cannot be
/// published against a project list that is a poll older.
pub fn publish(server: &Server, peer: &Peer) {
    let projects: Vec<ProjectProfile> = server
        .projects()
        .into_iter()
        .filter(|key| !key.is_empty())
        .map(|key| {
            let root = server.project_root(&key);
            ProjectProfile {
                identity: identity_of(Path::new(&root)).unwrap_or_default(),
                introduce: introduce_here(Path::new(&root)),
                key,
            }
        })
        .collect();
    let seats: Vec<SeatProfile> = server
        .seat_rows()
        .into_iter()
        .map(|row| {
            let identity = projects
                .iter()
                .find(|p| p.key == row.project)
                .map(|p| p.identity.clone())
                .unwrap_or_default();
            SeatProfile::new(row.id, &identity, &row.branch, &row.task).with_nick(&row.nick)
        })
        .collect();
    peer.publish_projects(projects);
    peer.publish(seats);
}

/// A repository by its identity: the first commit of its history and where its remote says it
/// lives - never the path, because the other box checked it out somewhere else.
///
/// `None` when the folder is not a git checkout with a commit, which is a project whose seats are
/// matched with nobody and introduced to nobody.
pub fn identity_of(root: &Path) -> Option<RepoIdentity> {
    if root.as_os_str().is_empty() || !root.is_dir() {
        return None;
    }
    let commit = git(root, &["rev-list", "--max-parents=0", "HEAD"])?;
    let remote = git(root, &["config", "--get", "remote.origin.url"]).unwrap_or_default();
    Some(RepoIdentity::of(&commit, &remote))
}

/// Does this project's own `leticode.toml` opt out of introductions? A file that will not parse is
/// the project's own business and the session already says so: here it means *in*.
fn introduce_here(root: &Path) -> bool {
    if root.as_os_str().is_empty() {
        return true;
    }
    match crate::leticode_config::LeticodeConfig::discover(root) {
        Some(path) => crate::leticode_config::LeticodeConfig::load(&path)
            .map(|c| c.peering.introduces())
            .unwrap_or(true),
        None => true,
    }
}

/// One `git` question, answered on one line. A repository with no commits answers nothing for
/// `rev-list`, which is the `None` this returns rather than a fabricated identity.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_known_by_its_first_commit_and_its_remote() {
        // The tree this test runs in is a checkout, so the real reader has something to read.
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let identity = identity_of(here).expect("this crate is in a git checkout");
        assert!(
            identity.as_str().starts_with("local@") || identity.as_str().contains('@'),
            "{identity:?}"
        );
        assert!(!identity.as_str().contains(here.to_str().unwrap_or("")));
        // A folder that is not a repository is nobody, and not a guess.
        let tmp = std::env::temp_dir().join(format!("lb-peer-id-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(identity_of(&tmp), None);
        assert_eq!(identity_of(Path::new("")), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_cookie_is_the_one_beside_the_file_that_names_the_model() {
        let path = cookie_path();
        assert_eq!(path.file_name().unwrap(), "cookie");
        assert_eq!(
            path.parent(),
            letibot_provider::keys::config_file().parent()
        );
    }
}
