//! The `link_<name>` hook: what a word on the screen links to. Asked once per frame, for the words
//! the head's own references did not know, and cached by word - a word answered no is not asked
//! again either. The head keeps the answers a terminal is handed, so only an `http(s)` target lands.

/// The export-name prefix a link answers under: `link_<name>`.
pub const PREFIX: &str = "link_";

/// The hook a layer named `name` answers.
pub fn hook(name: &str) -> String {
    format!("link_{name}")
}

/// How many words one frame may hand one layer. A frame's links are the words a screen shows
/// that name something; past this a layer is being asked to index a language.
pub const MAX_WORDS: usize = 256;

/// The repository a word may be about: enough for a layer to link a branch or a pull request
/// without a look at the disk, which a frame cannot afford to wait for.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Repo {
    /// `owner/repo` on GitHub, empty when the session's repository is not one.
    #[serde(default)]
    pub slug: String,
    /// The branches it has, so a word with a `/` in it can be told from prose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
}

/// What a link hook is handed: this frame's words, in the order the answer must match.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// At most [`MAX_WORDS`], the words the head's own references did not resolve.
    pub words: Vec<String>,
    #[serde(default)]
    pub repo: Repo,
}

impl Input {
    /// One frame's question.
    pub fn new(words: Vec<String>, repo: Repo) -> Input {
        Input {
            v: crate::V,
            words,
            repo,
        }
    }

    /// The input `plugin test` and the shadow gate use when a session has none recorded.
    pub fn synthetic() -> Input {
        Input::new(
            vec!["#212".to_string(), "stroppy-io/stroppy#24".to_string()],
            Repo {
                slug: "stroppy-io/stroppy".to_string(),
                branches: vec!["main".to_string(), "agent/fix".to_string()],
            },
        )
    }
}

/// The answer: one target per word, in the same order, `null` for a word this layer does not link.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Out {
    #[serde(default)]
    pub urls: Vec<Option<String>>,
}

impl Out {
    /// The targets this answer means, checked against the words it was asked about. A refusal is
    /// the whole answer: a frame drawn from half of it would link words the layer never saw.
    pub fn checked(&self, words: &[String]) -> Result<Vec<Option<String>>, String> {
        if self.urls.len() != words.len() {
            return Err(format!(
                "it answered {} url(s) for {} word(s), and the answer is one entry per word in \
                 the order it was asked: a shorter or longer list would link the wrong word",
                self.urls.len(),
                words.len()
            ));
        }
        for (word, url) in words.iter().zip(&self.urls) {
            let Some(url) = url else { continue };
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!(
                    "it answered `{url}` for `{word}`, and the only schemes this head draws are \
                     `http` and `https`: what a terminal does with another one is not something \
                     the harness can predict"
                ));
            }
        }
        Ok(self.urls.clone())
    }

    /// How many words this answer links - the one line `plugin test` reports.
    pub fn linked(&self) -> usize {
        self.urls.iter().filter(|u| u.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn words() -> Vec<String> {
        vec!["#212".to_string(), "a/fix".to_string(), "prose".to_string()]
    }

    /// The batched shape: `{"words":[…]} → {"urls":[…]}`.
    #[test]
    fn the_plans_batched_shape_is_this_type() {
        let input = Input::new(words(), Repo::default());
        assert_eq!(input.words.len(), 3);
        let out: Out =
            serde_json::from_str(r#"{"urls":["https://github.com/o/r/issues/212",null,null]}"#)
                .expect("an answer");
        let checked = out.checked(&words()).expect("aligned");
        assert_eq!(
            checked[0].as_deref(),
            Some("https://github.com/o/r/issues/212")
        );
        assert_eq!(checked[1], None);
        assert_eq!(out.linked(), 1);
    }

    /// An answer that is not one entry per word is refused, because it would link the wrong one.
    #[test]
    fn an_answer_of_the_wrong_length_is_refused() {
        let short = Out {
            urls: vec![Some("https://x".into())],
        };
        let why = short.checked(&words()).expect_err("short");
        assert!(why.contains("one entry per word"), "{why}");
    }

    /// A scheme this head will not draw is refused, and the refusal names the word.
    #[test]
    fn only_http_targets_are_drawn() {
        let out = Out {
            urls: vec![Some("javascript:alert(1)".into()), None, None],
        };
        let why = out.checked(&words()).expect_err("scheme");
        assert!(why.contains("#212"), "{why}");
        assert!(why.contains("http"), "{why}");
    }

    /// The synthetic input carries a word this layer's own refs know and one they do not.
    #[test]
    fn the_synthetic_input_is_the_hosts_own_shape() {
        let input = serde_json::to_value(Input::synthetic()).expect("serialises");
        assert_eq!(input["v"], json!(crate::V));
        assert_eq!(input["words"][0], json!("#212"));
        assert_eq!(input["repo"]["slug"], json!("stroppy-io/stroppy"));
        assert_eq!(hook("gh"), "link_gh");
    }
}
