//! **The words in a reply that name something become links** — the operator: *"do we render links
//! as links? any chance to infer github branch names and prs?"*, and *"yes i want all 5"*.

use super::*;

fn replied(text: &str) -> App {
    let mut a = app();
    // Links are written for a session with a workspace (`RenderConfig::links`).
    a.wiring.workspace = "/w".into();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.1", "assistant"),
    )));
    a.record_item(
        "r.1",
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    a
}

/// Every URL on the frame, in order.
fn urls(screen: &str) -> Vec<String> {
    screen
        .split("\u{1b}]8;;")
        .skip(1)
        .filter_map(|rest| rest.split_once("\u{1b}\\").map(|(u, _)| u.to_string()))
        .filter(|u| !u.is_empty())
        .collect()
}

fn refs() -> crate::app::ghlinks::GitRefs {
    crate::app::ghlinks::GitRefs {
        slug: "stroppy-io/stroppy".into(),
        branches: ["fix/banking-ydb", "main"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        commits: vec!["68040b5aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()],
    }
}

/// **A markdown link keeps its destination** — the label is the link, where the terminal speaks
/// links; where it does not, the label is plain text and no sequence is written.
#[test]
fn a_markdown_links_label_opens_its_destination_only_where_links_are_spoken() {
    let text = "Read [the guide](https://example.com/guide) first.";
    let mut a = replied(text);
    a.set_features(rano::term::Features::ALL);
    let screen = a.screen(100, 20).join("\n");
    assert!(screen.contains("the guide"), "{screen}");
    assert_eq!(
        urls(&screen),
        vec!["https://example.com/guide"],
        "{screen:?}"
    );

    let mut b = replied(text);
    let plain = b.screen(100, 20).join("\n");
    assert!(plain.contains("the guide"), "{plain}");
    assert!(!plain.contains("\u{1b}]8;;"), "{plain:?}");
}

/// **The repository's own references link, and what it does not hold does not** — `#212`, a full
/// `owner/repo#24`, a commit it has, a branch it has; not a hex word it lacks, not a bare `main`.
#[test]
fn the_sessions_repository_references_link_and_unknown_words_stay_text() {
    let mut a = replied(
        "Opened #212 and stroppy-io/site#24; commit 68040b5 on fix/banking-ydb. \
         Not deadbeef1, not main.",
    );
    a.set_features(rano::term::Features::ALL);
    let before = a.screen(120, 20).join("\n");
    assert!(
        urls(&before).is_empty(),
        "nothing is linked before the refs are read: {before:?}"
    );
    a.gh_refs = Some(std::sync::Arc::new(refs()));
    let screen = a.screen(120, 20).join("\n");
    let gh = "https://github.com";
    assert_eq!(
        urls(&screen),
        vec![
            format!("{gh}/stroppy-io/stroppy/issues/212"),
            format!("{gh}/stroppy-io/site/issues/24"),
            format!("{gh}/stroppy-io/stroppy/commit/68040b5aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            format!("{gh}/stroppy-io/stroppy/tree/fix/banking-ydb"),
        ],
        "{screen:?}"
    );

    // A terminal without links gets none of it.
    let mut b = replied("Opened #212.");
    b.gh_refs = Some(std::sync::Arc::new(refs()));
    assert!(!b.screen(120, 20).join("\n").contains("\u{1b}]8;;"));
}
