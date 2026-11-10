//! The one admission path: discover a folder's layers, decide them against the trust book, and
//! answer the news a person is owed.
//!
//! The daemon, the head and the bridge each had these three steps spelled out by hand, which is how
//! they came to disagree about a directory and about *how* a moved pin is said. A host supplies
//! the scopes and the engine; everything else is here.

use std::path::Path;
use std::sync::Arc;

use crate::Loader;
use crate::stack::{BrokenPin, Stack};
use crate::trust::Trust;

/// The three scopes a layer can be placed in, as a host knows them: the session's own directory,
/// the folder it is wired to (whose `.letibot/plugins` is the project's) and the user's config
/// directory. Any of them may be absent, which is an empty scope and not an error.
#[derive(Debug, Clone, Copy)]
pub struct Scopes<'a> {
    pub session: Option<&'a Path>,
    pub workspace: &'a Path,
    pub user: Option<&'a Path>,
}

/// What admission came to: the stack, and what has to be said about it.
pub struct Admission {
    /// The layers these scopes hold, with the binary's own not yet at the bottom - a host installs
    /// those at the door it needs them, because a module costs seconds to compile.
    pub stack: Stack,
    /// Pins whose bytes moved: news, said once by the caller. Never dropped silently - a layer that
    /// quietly stopped being yours is what the ladder exists to make impossible to miss.
    pub broken: Vec<BrokenPin>,
}

impl Admission {
    /// Every sentence admission owes, in the one wording this tree refuses a layer with: the pins
    /// whose bytes moved, and the layers that would not load at all. A built-in is left out of the
    /// second - its install is the caller's, and its failure is the caller's to word too.
    pub fn sentences(&self) -> Vec<String> {
        let mut said: Vec<String> = self.broken.iter().map(BrokenPin::sentence).collect();
        said.extend(
            self.stack
                .failures()
                .iter()
                .filter(|f| f.scope != crate::Scope::BuiltIn)
                .map(|f| format!("the plugin layer {f}")),
        );
        said
    }
}

/// Discover `scopes` with `loader`, and decide every placed layer against `book`. `None` leaves
/// every placed layer untrusted, which is the fail-closed direction and what a host that reads its
/// book after the layers (the daemon, which opens it with the session) starts from.
pub fn admit(scopes: Scopes<'_>, loader: Arc<dyn Loader>, book: Option<Arc<Trust>>) -> Admission {
    let mut stack = Stack::discover(scopes.session, Some(scopes.workspace), scopes.user, loader);
    let broken = match book {
        Some(book) => stack.set_trust(book),
        None => Vec::new(),
    };
    Admission { stack, broken }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Scope;
    use crate::testing::{Act, FakeLoader, TempDir, layer};

    /// One path for the three hosts: a layer on disk is discovered and decided here, and the
    /// pins' news comes back as a sentence rather than a `Vec` somebody drops.
    #[test]
    fn admission_discovers_the_scopes_and_answers_the_news() {
        let root = TempDir::new("admit");
        let project = root.path().join("project");
        let user = root.path().join("user");
        std::fs::create_dir_all(project.join(".letibot").join("plugins")).expect("a project");
        std::fs::create_dir_all(user.join("plugins")).expect("a user scope");
        layer(
            &user.join("plugins"),
            "theirs",
            &["on_open"],
            Act::answer(serde_json::json!("theirs")),
        );

        let admitted = admit(
            Scopes {
                session: None,
                workspace: &project,
                user: Some(&user),
            },
            Arc::new(FakeLoader::new()),
            None,
        );
        assert_eq!(
            admitted.stack.layers().len(),
            1,
            "the user scope's one layer"
        );
        assert_eq!(admitted.stack.layers()[0].scope, Scope::User);
        assert!(
            admitted.broken.is_empty(),
            "no book, so nothing is pinned and nothing is broken"
        );

        // A book that pins nothing leaves the layer untrusted and says so in one sentence - the
        // one `/plugin trust` hint every refusal in this tree carries.
        let book = Trust::in_memory();
        let admitted = admit(
            Scopes {
                session: None,
                workspace: &project,
                user: Some(&user),
            },
            Arc::new(FakeLoader::new()),
            Some(book),
        );
        assert!(!admitted.stack.layers()[0].trusted);
        assert!(
            admitted.sentences().is_empty(),
            "nothing was pinned to break"
        );
        assert!(
            admitted.stack.layers()[0]
                .untrusted_sentence()
                .contains("/plugin trust theirs"),
            "the refusal names the verb that fixes it"
        );
    }
}
