//! The file tools' half of a boundary: a path a command's view denies is denied to `read`,
//! `write`, `edit`, `grep` and `glob` too, with the same sentence and naming the same key. Built
//! from the seat and the view the shell is threaded with, so there is one list and not two.

use std::path::{Path, PathBuf};

use super::boundary::Origin;
use super::confine::{Grant, HomeView, TmpView, ViewSpec};
use super::seat::SeatView;

/// What a file tool may not touch, and why. Two rules, one list: a path the view masks is denied,
/// and a tight view holds nothing else - the same two the shell obeys. The one exception is this
/// session's own directory in the letibot store, which is the part of it a session writes (its
/// plugins).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFence {
    /// Every path the view masks, read and write: the credential stores, the profiles, `hide`.
    denied: Vec<PathBuf>,
    /// The letibot data directory: `sessions.db` and every session's folder.
    store: Option<PathBuf>,
    /// This session's own directory in the store.
    own: Option<PathBuf>,
    /// The `[boundary] hide` entries by origin, so a denial names the key that would lift it.
    hidden: Vec<(PathBuf, Origin)>,
    /// A tight view's roots, and whether a write may land in each. `None` for a wide view, which
    /// holds the machine: nothing is outside it and only the masks are denied.
    inside: Option<Vec<(PathBuf, bool)>>,
}

impl FileFence {
    /// The fence a seat already draws for a shell. A seat with nothing masked and no view of its
    /// own fences nothing, so a session with no boundary keeps the whole host its tools had.
    pub fn of(seat: &SeatView, view: &ViewSpec) -> FileFence {
        let inside = (!view.wide).then(|| {
            let mut roots: Vec<(PathBuf, bool)> = vec![(view.project.clone(), true)];
            roots.extend(view.system.iter().map(|p| (p.clone(), false)));
            roots.extend(
                view.grants
                    .iter()
                    .map(|g| (g.path().to_path_buf(), matches!(g, Grant::ReadWrite { .. }))),
            );
            if let HomeView::Dir(d) = &view.home {
                roots.push((d.clone(), true));
            }
            if let TmpView::Session(d) = &view.tmp {
                roots.push((d.clone(), true));
            }
            roots.sort();
            roots.dedup();
            roots
        });
        FileFence {
            denied: seat.masked.clone(),
            hidden: seat.hidden.clone(),
            inside,
            ..Default::default()
        }
    }

    /// A directory the session owns and was handed after the view was measured: its scratch, or
    /// the `/tmp` its commands share. Reachable even where a tight view would not hold it.
    pub fn allowing(mut self, path: impl Into<PathBuf>) -> FileFence {
        if let Some(roots) = &mut self.inside {
            roots.push((path.into(), true));
        }
        self
    }

    /// The letibot store, of which this session reaches `sessions/<id>/` and nothing else.
    pub fn owning(mut self, store: &Path, session: &str) -> FileFence {
        self.store = Some(store.to_path_buf());
        self.own = Some(store.join("sessions").join(session));
        self
    }

    /// Is there anything to refuse? A fence with no denied path, no store and no view is the
    /// whole host.
    pub fn is_empty(&self) -> bool {
        self.denied.is_empty() && self.store.is_none() && self.inside.is_none()
    }

    /// Why a file tool may not touch `path`, in the words a command's absence note uses. `path` is
    /// resolved - symlinks followed - because a link is the obvious way round a lexical check.
    /// `write` asks the narrower question: a root the view holds read-only is inside it.
    pub fn refusal(&self, path: &Path, write: bool) -> Option<String> {
        if let Some(own) = &self.own
            && path.starts_with(own)
        {
            return None;
        }
        if self.store.as_ref().is_some_and(|s| path.starts_with(s)) {
            return Some(self.store_sentence(path));
        }
        // The deepest denial wins: a `hide` inside a store is the narrower of the two answers.
        if let Some(hit) = self
            .denied
            .iter()
            .filter(|d| path.starts_with(d))
            .max_by_key(|d| d.components().count())
        {
            return Some(self.sentence(path, hit));
        }
        let roots = self.inside.as_ref()?;
        // A path a root has not got is outside the view; a root that has it decides writability.
        let holder = roots
            .iter()
            .filter(|(r, _)| path.starts_with(r))
            .max_by_key(|(r, _)| r.components().count());
        match holder {
            None => Some(format!(
                "`{}` is not in this session's filesystem view: the file tools see what a \
                 command sees, so a path a command cannot read is not one `read` shows either. \
                 The operator's keys: `[boundary] grants_ro = [\"{}\"]` (`grants_rw` for a write) \
                 in the user `~/.config/letibot/leticode.toml`, where a project cannot widen.",
                path.display(),
                path.display()
            )),
            Some((root, false)) if write => Some(format!(
                "`{}` is inside this session's view and the view holds `{}` read-only, so nothing \
                 was written. Making it writable is `[boundary] grants_rw = [\"{}\"]` in the \
                 operator's user `leticode.toml`: a project file may not widen.",
                path.display(),
                root.display(),
                root.display()
            )),
            Some(_) => None,
        }
    }

    fn store_sentence(&self, path: &Path) -> String {
        let store = self.store.as_ref().map(|s| s.display().to_string());
        match &self.own {
            Some(own) => format!(
                "`{}` is not in this session's filesystem view: it is inside `{}`, the letibot \
                 store, where a session reaches its own `{}` and nothing else — `sessions.db`, \
                 another session's folder and the plugins under it are outside every seat. No \
                 `[boundary]` key grants the store: it is on the never-grant list.",
                path.display(),
                store.unwrap_or_default(),
                own.display()
            ),
            None => format!(
                "`{}` is not in this session's filesystem view: it is inside `{}`, the letibot \
                 store, and this session has no part of it of its own to reach. No `[boundary]` \
                 key grants the store: it is on the never-grant list.",
                path.display(),
                store.unwrap_or_default()
            ),
        }
    }

    fn sentence(&self, path: &Path, denied: &Path) -> String {
        if let Some((_, from)) = self
            .hidden
            .iter()
            .find(|(p, _)| path.starts_with(p) || p.starts_with(path))
        {
            return format!(
                "`{}` is not in this session's filesystem view: `[boundary] hide` at {from} denies \
                 it after every grant, so a command and a file tool lose it in the same place. \
                 Removing that line is what puts it back.",
                path.display()
            );
        }
        match never_granted_rel(denied) {
            Some(rel) => format!(
                "`{}` is not in this session's filesystem view: `~/{rel}` is never granted, so no \
                 `[boundary]` key in leticode.toml grants it or anything under it, and a command's \
                 view masks it in the same place.",
                path.display()
            ),
            None => format!(
                "`{}` is not in this session's filesystem view: the view masks `{}` after every \
                 grant, and no `[boundary]` key lifts a mask.",
                path.display(),
                denied.display()
            ),
        }
    }
}

/// The never-grant entry a masked path came from, if it came from one. Matched on the tail,
/// because the masked list is that list joined to a home this side does not hold.
fn never_granted_rel(masked: &Path) -> Option<&'static str> {
    let spelled = masked.to_string_lossy().replace('\\', "/");
    super::boundary::never_grant()
        .into_iter()
        .find(|rel| spelled == *rel || spelled.ends_with(&format!("/{rel}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::seat::{self, Level};
    use crate::mode::Mode;

    fn seat(home: &Path, level: Level) -> SeatView {
        seat::seat_for_level(
            level,
            &Mode::WRITES_ALLOWED,
            "here",
            &[] as &[&str],
            home,
            None,
        )
    }

    fn wide_view(project: &Path, home: &Path) -> ViewSpec {
        ViewSpec::project_only(project)
            .with_home(HomeView::Dir(home.into()))
            .wide()
    }

    /// The two rules this exists for: the store is reachable only where the session's own folder
    /// is, and a store is denied by name rather than read as a path that does not exist.
    #[test]
    fn the_store_is_reachable_only_in_the_sessions_own_folder() {
        let home = Path::new("/h");
        let store = home.join(".local/share/letibot");
        let view = wide_view(Path::new("/p"), home);
        let fence = FileFence::of(&seat(home, Level::Wide), &view).owning(&store, "s-1");

        assert!(
            fence
                .refusal(&store.join("sessions/s-1/plugins/x.toml"), false)
                .is_none()
        );
        assert!(
            fence
                .refusal(&store.join("sessions/s-1/plugins/x.toml"), true)
                .is_none()
        );
        for outside in [
            store.join("sessions.db"),
            store.join("sessions/s-2/plugins/x.toml"),
            store.join("sessions/s-1x/db"),
        ] {
            let why = fence.refusal(&outside, false).unwrap_or_default();
            assert!(why.contains("the letibot store"), "{why}");
            assert!(
                why.contains("/h/.local/share/letibot/sessions/s-1"),
                "{why}"
            );
            assert!(why.contains("never-grant"), "{why}");
        }
    }

    /// A credential store is denied with the never-grant sentence and no key; `[boundary] hide`
    /// names its own key and the file and line that wrote it.
    #[test]
    fn a_store_and_a_hidden_path_name_their_own_reason() {
        let home = Path::new("/h");
        let mut s = seat(home, Level::Wide);
        let hidden = home.join(".local/bin/tool");
        s.masked.push(hidden.clone());
        s.hidden.push((
            hidden.clone(),
            Origin {
                file: PathBuf::from("/h/.config/letibot/leticode.toml"),
                line: 4,
                user: true,
            },
        ));
        let fence = FileFence::of(&s, &wide_view(Path::new("/p"), home));

        let key = fence
            .refusal(&home.join(".ssh/id_ed25519"), false)
            .expect("denied");
        assert!(key.contains("`~/.ssh` is never granted"), "{key}");
        let hid = fence.refusal(&hidden, false).expect("denied");
        assert!(
            hid.contains("`[boundary] hide`") && hid.contains("leticode.toml:4"),
            "{hid}"
        );
        assert!(fence.refusal(&home.join("note.txt"), false).is_none());
    }

    /// A tight view is the project and its grants: a path outside it is refused naming
    /// `grants_ro`, and a write under a read-only grant names `grants_rw`.
    #[test]
    fn a_tight_view_refuses_outside_itself_and_names_the_key() {
        let home = Path::new("/h");
        let view = ViewSpec::project_only("/p")
            .granting(Grant::ReadOnly {
                path: PathBuf::from("/h/.cargo"),
                why: "the toolchain".into(),
            })
            .with_home(HomeView::Dir("/p/home".into()));
        let fence = FileFence::of(&seat(home, Level::Tight), &view);
        assert!(fence.refusal(Path::new("/p/src/main.rs"), true).is_none());
        let out = fence
            .refusal(Path::new("/h/datasets/x"), false)
            .expect("denied");
        assert!(out.contains("`[boundary] grants_ro"), "{out}");
        assert!(
            fence
                .refusal(Path::new("/h/.cargo/registry"), false)
                .is_none()
        );
        let ro = fence
            .refusal(Path::new("/h/.cargo/registry/x"), true)
            .expect("denied");
        assert!(ro.contains("`[boundary] grants_rw"), "{ro}");
    }

    /// A fence built from a seat with nothing masked and a wide view refuses nothing: where there
    /// is no boundary there is no list, and a refusal there would claim a boundary that is not.
    #[test]
    fn a_seat_with_nothing_masked_fences_nothing() {
        let s = SeatView {
            grants: Vec::new(),
            egress: crate::exec::confine::Egress::Denied,
            env: Vec::new(),
            home: None,
            masked: Vec::new(),
            hidden: Vec::new(),
            secret_env: Default::default(),
        };
        let fence = FileFence::of(&s, &wide_view(Path::new("/p"), Path::new("/h")));
        assert!(fence.is_empty());
        assert!(fence.refusal(Path::new("/etc/passwd"), false).is_none());
    }
}
