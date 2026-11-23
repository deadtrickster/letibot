//! What a catalog has to be true of, in every language that ships: complete, and never a gap the
//! reader sees.

use super::*;

/// Every shipped catalog parses: no line is malformed, no id is said twice.
#[test]
fn every_shipped_catalog_parses() {
    let problems = problems();
    assert!(problems.is_empty(), "{problems:#?}");
    for lang in Lang::ALL {
        assert!(
            !lang.catalog().messages.is_empty(),
            "{} has no messages",
            lang.code()
        );
    }
}

/// No language invents an id: everything it translates is something English says. A plural form
/// is the exception a language owns - Russian has three where English has two - so a form is
/// checked against its base.
#[test]
fn every_key_in_every_language_is_an_english_key() {
    let english = Lang::En.catalog();
    for lang in Lang::ALL {
        for key in lang.catalog().messages.keys() {
            let found = english.messages.contains_key(key)
                || plural_base(key).is_some_and(|base| {
                    english.messages.contains_key(base)
                        || english.messages.contains_key(&format!("{base}.one"))
                });
            assert!(
                found,
                "{}: `{key}` is not an id English has, and translates nothing",
                lang.code()
            );
        }
    }
}

/// Every plural base carries the forms its language distinguishes: English needs `one`/`other`,
/// Russian `one`/`few`/`many`. A base with a missing form would fall back to the base's ungrammatical
/// sentence, which is the kind of thing only a Russian speaker notices.
#[test]
fn every_plural_base_has_the_forms_its_language_needs() {
    for lang in Lang::ALL {
        let catalog = lang.catalog();
        let bases: Vec<&str> = catalog
            .messages
            .keys()
            .filter_map(|k| plural_base(k))
            .collect();
        assert!(!bases.is_empty(), "{} has no plural forms", lang.code());
        for base in bases {
            for form in lang.forms() {
                let key = format!("{base}.{form}");
                assert!(
                    catalog.messages.contains_key(&key),
                    "{}: `{base}` has no `{form}` ({key})",
                    lang.code()
                );
            }
        }
    }
}

/// A translation keeps every placeholder the English names - a dropped `{name}` is a value the
/// reader never sees - and invents none, which would draw the braces themselves.
#[test]
fn every_placeholder_a_translation_carries_is_the_english_one() {
    let english = Lang::En.catalog();
    for lang in Lang::ALL.iter().filter(|l| **l != Lang::En) {
        for (key, text) in &lang.catalog().messages {
            let theirs = placeholders(text);
            if theirs.is_empty() {
                continue;
            }
            let ours = match english.messages.get(key) {
                Some(ours) => placeholders(ours),
                // A form English does not have: the base's forms stand for it.
                None => match plural_base(key) {
                    Some(base) => english
                        .messages
                        .iter()
                        .filter(|(k, _)| k.as_str() == base || (plural_base(k) == Some(base)))
                        .flat_map(|(_, v)| placeholders(v))
                        .collect(),
                    None => Vec::new(),
                },
            };
            for name in &theirs {
                assert!(
                    ours.contains(name),
                    "{}: `{key}` fills `{{{name}}}`, which its English does not name ({ours:?})",
                    lang.code()
                );
            }
            for name in ours.iter().filter(|n| !theirs.contains(n)) {
                assert!(
                    theirs.contains(name),
                    "{}: `{key}` dropped `{{{name}}}`",
                    lang.code()
                );
            }
        }
    }
}

/// The `{name}` slots a message carries, in the order they appear.
fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('{') {
        rest = &rest[at + 1..];
        let Some(end) = rest.find('}') else { break };
        out.push(rest[..end].to_string());
        rest = &rest[end + 1..];
    }
    out
}

/// A language that has no word for an id reads as English, and never as the id: a half-translated
/// release says the sentence in English rather than `card.put_away` on the screen. Russian's three
/// plural forms are a real gap for English's fourth, so this is the shipped catalog's own proof.
#[test]
fn a_gap_reads_as_english_and_never_as_the_id() {
    set_active(Lang::Ru);
    assert!(
        !Lang::Ru
            .catalog()
            .messages
            .contains_key("status.many.job.other"),
        "Russian does not have English's `other` form, or this test proves nothing"
    );
    assert_eq!(
        Lang::En
            .catalog()
            .messages
            .get("status.many.job.other")
            .map(String::as_str),
        Some("{n} jobs")
    );
    assert_eq!(
        t("status.many.job.other"),
        "{n} jobs",
        "a word this language lacks is English's"
    );
    for key in all_ids() {
        assert_ne!(t(key), key, "`{key}` draws its own id");
    }
}

/// English is the source: every id is said there, and every language says it in its own words.
/// A translation that is byte-identical to its English is an id somebody forgot.
#[test]
fn every_id_is_said_in_every_language() {
    let english = Lang::En.catalog();
    for id in all_ids() {
        assert_eq!(t(id), t(id));
        for lang in Lang::ALL {
            let text = lang.catalog().messages.get(id);
            if lang == Lang::En {
                assert!(text.is_some(), "English does not say `{id}`");
                continue;
            }
            let Some(theirs) = text else { continue };
            assert_ne!(
                theirs,
                english.messages.get(id).expect("english"),
                "{}: `{id}` is still the English sentence",
                lang.code()
            );
        }
    }
}

/// The two surfaces the brief names - a permission card and the session list - carry no English
/// left in what was migrated. Latin that survives on purpose: the keys a person presses, the file
/// the rule is written to, and the placeholders.
#[test]
fn the_migrated_surfaces_carry_no_english_words_in_russian() {
    // A word this head may keep in any language: a chord, a path, a format word.
    const KEEP: [&str; 7] = [
        "esc",
        "enter",
        "alt-a",
        "alt-d",
        "ctrl-c",
        "permission.json",
        "json",
    ];
    for lang in Lang::ALL.iter().filter(|l| **l != Lang::En) {
        for (key, text) in &lang.catalog().messages {
            if !(key.starts_with("card.") || key.starts_with("sessions.")) {
                continue;
            }
            // Placeholders are values, not words: `{key}`, `{n}`, `{rule}`.
            let bare = placeholders(text)
                .iter()
                .fold(text.clone(), |acc, name| {
                    acc.replace(&format!("{{{name}}}"), " ")
                })
                .replace('`', " ");
            // A path is a path in any language: `~/.config/letibot/permission.json` is one token
            // and is never translated. Everything else is a word, and a migrated surface has none
            // of English's left.
            for word in bare.split_whitespace() {
                if word.contains('/') || word.len() < 3 || KEEP.contains(&word) {
                    continue;
                }
                if word.chars().all(|c| !c.is_ascii_alphabetic()) {
                    continue;
                }
                panic!("{}: `{key}` still says `{word}`: {text}", lang.code());
            }
        }
    }
}

/// A count takes the language's own plural form: English's two, Russian's three with the teens the
/// exception the language makes.
#[test]
fn a_count_takes_the_form_the_language_uses() {
    set_active(Lang::En);
    assert_eq!(count("status.many.job", 1), "1 job");
    assert_eq!(count("status.many.job", 3), "3 jobs");

    set_active(Lang::Ru);
    assert_eq!(count("status.many.job", 1), "1 задание");
    assert_eq!(count("status.many.job", 3), "3 задания");
    assert_eq!(count("status.many.job", 5), "5 заданий");
    // The teens are `many` whatever their last digit says, and 21 is `one` again.
    assert_eq!(count("status.many.job", 11), "11 заданий");
    assert_eq!(count("status.many.job", 12), "12 заданий");
    assert_eq!(count("status.many.job", 21), "21 задание");
    assert_eq!(count("status.many.job", 22), "22 задания");
    set_active(Lang::En);
}

/// The setting and the environment: `auto` is LC_ALL, then LC_MESSAGES, then LANG; a locale names
/// its language by its head; a word nothing has is said and English is drawn.
#[test]
fn the_language_is_chosen_by_the_setting_then_the_environment() {
    assert_eq!(choose(None, Some("ru_RU.UTF-8")).0, Lang::Ru);
    assert_eq!(choose(Some("auto"), Some("ru_RU.UTF-8")).0, Lang::Ru);
    assert_eq!(choose(Some("auto"), Some("en_GB.UTF-8")).0, Lang::En);
    assert_eq!(choose(Some("auto"), None).0, Lang::En);
    // A setting wins over the environment, and is read by its head too.
    assert_eq!(choose(Some("ru"), Some("en_US.UTF-8")).0, Lang::Ru);
    assert_eq!(choose(Some("ru_RU.UTF-8"), None).0, Lang::Ru);
    assert_eq!(Lang::parse("ru-RU"), Some(Lang::Ru));
    assert_eq!(Lang::parse("RU"), Some(Lang::Ru));
    assert_eq!(Lang::parse("de_DE.UTF-8"), None);

    let (lang, why) = choose(Some("de"), None);
    assert_eq!(lang, Lang::En, "a word nothing has is English");
    let why = why.expect("and it is said");
    assert!(why.contains("de"), "{why}");
    assert!(why.contains("ru") && why.contains("auto"), "{why}");
    assert!(unknown("auto").is_none() && unknown("").is_none());
}

/// A message's own escapes, and a line that cannot be read is a problem rather than a silent drop.
#[test]
fn the_catalog_reads_escapes_and_says_what_it_cannot() {
    let catalog = parse(
        Lang::En,
        "# a comment\n\nsay = \"a \\\"quoted\\\" word\\nnext line\"\n",
    );
    assert!(catalog.problems.is_empty(), "{:?}", catalog.problems);
    assert_eq!(
        catalog.messages.get("say").map(String::as_str),
        Some("a \"quoted\" word\nnext line")
    );
    for (text, why) in [
        ("no equals sign\n", "not `id = \"text\"`"),
        ("k = unquoted\n", "is not a quoted string"),
        ("k = \"a\"\nk = \"b\"\n", "is said twice"),
        ("k = \"a \\q\"\n", "is not an escape"),
    ] {
        let catalog = parse(Lang::En, text);
        assert!(
            catalog.problems.iter().any(|p| p.contains(why)),
            "{text:?}: {:?}",
            catalog.problems
        );
    }
}

/// A message with a slot filled keeps the rest of its sentence, including a slot said twice.
#[test]
fn a_message_fills_its_slots() {
    set_active(Lang::En);
    let said = tf(
        "card.behind",
        &[("n", "2"), ("clock", " — one has a deadline")],
    );
    assert_eq!(
        said,
        "+2 more waiting behind this card — one has a deadline"
    );
    // A slot the caller does not supply stays as written: a visible placeholder beats a wrong value.
    assert!(tf("card.diff.more", &[("n", "4")]).contains("{key}"));
}

/// The ids a guest pane looks up are the head's own: the built-in session list names every word it
/// draws by id, and the head hands it a map of them. A literal in that layer with no catalog key
/// would draw English under any language, so it is a test and not a hope.
#[test]
fn the_session_lists_own_ids_are_in_the_catalog() {
    const LAYER: &str = include_str!("../../../../plugins/builtin/sessions/src/lib.rs");
    let english = Lang::En.catalog();
    let mut found = 0;
    for quote in LAYER.split('"').skip(1).step_by(2) {
        let Some(rest) = quote.strip_prefix("sessions.") else {
            continue;
        };
        // An id built at run time (`sessions.system.{group}`) is a template, not a lookup.
        if rest.contains('{') {
            continue;
        }
        let id = format!("sessions.{rest}");
        assert!(
            english.messages.contains_key(&id),
            "the session list looks up `{id}`, which no catalog says"
        );
        found += 1;
    }
    assert!(found >= 15, "only {found} ids were found in the layer");
}
