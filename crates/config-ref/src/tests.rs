use super::*;

const ODD: &str = "# my own top comment\r\n\
\t[default]   # trailing\n\
provider='glm'   \n\
\n\
  # indented comment\n\
[ model . \"a.b\" ]\n\
temperature=0.6\n\
key = \"\"\"\nmulti\nline\"\"\"\n\
# last line has no newline";

fn head() -> &'static File {
    find("head.toml").unwrap()
}

/// Inserting the block changes nothing after it; replacing it changes nothing outside it.
#[test]
fn splice_round_trips_an_odd_file() {
    let once = splice(ODD, head()).unwrap();
    assert!(once.starts_with(BEGIN));
    assert!(
        once.ends_with(ODD),
        "the original is not kept verbatim after the block"
    );
    assert_eq!(strip(&once), ODD);
    let twice = splice(&once, head()).unwrap();
    assert_eq!(once, twice, "a second splice is not a no-op");

    // A stale block in the middle is replaced in place.
    let (a, b) = ODD.split_at(ODD.find("[ model").unwrap());
    let stale = format!("{a}{BEGIN}\n# old\n#   gone: x\n{END}\n{b}");
    let fresh = splice(&stale, head()).unwrap();
    assert_eq!(fresh, format!("{a}{}{b}", header(head())));
    assert_eq!(strip(&fresh), ODD);
}

#[test]
fn a_block_with_no_end_is_refused() {
    let broken = format!("x = 1\n{BEGIN}\n# no end\n");
    assert!(splice(&broken, head()).is_err());
}

/// The block is comments only: a valid TOML file stays valid and means the same.
#[test]
fn the_block_is_toml_comments() {
    for f in ALL.iter().filter(|f| f.format == Format::Toml) {
        let h = header(f);
        assert!(h.lines().all(|l| l.starts_with('#')), "{}", f.name);
        let doc = splice("a = 1\n", f).unwrap();
        let v: toml::Table = toml::from_str(&doc).unwrap();
        assert_eq!(v.len(), 1);
    }
}

/// The checked-in docs are the generated ones. `LETIBOT_BLESS=1` rewrites them.
#[test]
fn docs_match_the_tables() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut stale = Vec::new();
    for f in ALL {
        let path = root.join(f.doc_path());
        let want = markdown(f);
        if std::env::var_os("LETIBOT_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &want).unwrap();
            continue;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(want.as_str()) {
            stale.push(f.doc_path());
        }
    }
    assert!(
        stale.is_empty(),
        "regenerate with LETIBOT_BLESS=1: {stale:?}"
    );
}

/// Names are unique and every TOML key has an example.
#[test]
fn tables_are_well_formed() {
    let mut names = std::collections::BTreeSet::new();
    for f in ALL {
        assert!(names.insert(f.name), "{} twice", f.name);
        let mut seen = std::collections::BTreeSet::new();
        for (t, k) in f.keys() {
            assert!(
                seen.insert((t.path, k.key)),
                "{}: [{}] {} twice",
                f.name,
                t.path,
                k.key
            );
            assert!(
                !k.meaning.is_empty(),
                "{}: {} has no meaning",
                f.name,
                k.key
            );
            assert!(
                !k.example.is_empty(),
                "{}: {} has no example",
                f.name,
                k.key
            );
        }
    }
}

#[test]
fn patterns_match() {
    assert!(probe::matches("color.<role>", "color.user_block"));
    assert!(probe::matches("<name>", "glm"));
    assert!(!probe::matches("color.<role>", "colour.x"));
    assert_eq!(probe::concrete("provider.<name>"), "provider.probe");
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct Two {
    a: u8,
    #[serde(rename = "b-b")]
    b: u8,
}

#[test]
fn serde_fields_reads_the_derive() {
    assert_eq!(probe::serde_fields::<Two>(), Some(&["a", "b-b"][..]));
}
