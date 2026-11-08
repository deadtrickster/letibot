//! R114's measurement: what the second tier scores on the cases the take-up logs hold, and
//! what one round costs against the whole notes and skills corpus. Not a pass/fail test.
//!
//! ```text
//! LETIBOT_R114_DATA=<cache dir> LETIBOT_R114_MODEL=<model dir> \
//!   cargo test --release -p letibot-tools --test similarity_measure -- --ignored --nocapture
//! ```
//!
//! The four benches and what each is handed, all of it paths to files this tree does not hold:
//! `LETIBOT_R114_CASES` a case file to score, `LETIBOT_R114_PAIRS` control pairs whose answer is
//! known, `LETIBOT_R114_MISSES` plus `LETIBOT_R114_CORPUS` the missed offers and a notes
//! directory, and `LETIBOT_R114_SUBJECTS` twenty round texts for the cost. A case is one line
//! `{"id","label","corpus","target","subject"}`: `corpus` is a notes directory or `skills`,
//! `target` its path or name, and `label` only groups the report.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_embeddings::{Mode, Settings, Tier};
use letibot_tools::builtins::skill::SkillRegistry;
use letibot_tools::similarity::{Corpus, bounded};

/// The cache directory and the checkpoint, from the environment this bench is run with.
fn places() -> Option<(PathBuf, PathBuf)> {
    let data = PathBuf::from(std::env::var_os("LETIBOT_R114_DATA")?);
    let model = std::env::var_os("LETIBOT_R114_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            data.join("models/all-MiniLM-L6-v2")
                .join(letibot_embeddings::builtin::REVISION)
        });
    Some((data, model))
}

/// The internal tier over a named checkpoint directory, with the cache in `data`.
fn tier(model: &Path, data: &Path) -> Tier {
    let settings = Settings {
        mode: Mode::Internal {
            model: Some(model.to_path_buf()),
        },
        download: false,
        ..Settings::default()
    };
    Tier::open(&settings, data)
        .expect("the tier opens")
        .expect("the mode is internal")
}

fn now() -> i64 {
    letibot_metrics::now_ms() as i64
}

/// One notes directory as the offer builds it: `(path, text)` per `.md`, and no box-wide notes.
fn notes(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md")
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            out.push((path, text));
        }
    }
    out.sort();
    out
}

/// The skills offer's corpus: each live skill's whole frontmatter, keyed by the name every door
/// here keys by.
fn skills(registry: &SkillRegistry) -> Corpus {
    let docs: Vec<(PathBuf, String)> = registry
        .live_skills()
        .iter()
        .map(|s| (PathBuf::from(&s.name), s.document()))
        .collect();
    Corpus::of_texts(&docs)
}

/// One document's score three ways, which is what tells a paraphrase from a coincidence: the
/// words alone, the cosine alone, and the blend a caller would actually see. `None` is a target
/// the corpus does not hold, which is a fact about the box and not a zero score.
fn score(
    corpus: &Corpus,
    target: &Path,
    subject: &str,
    tier: Option<&Tier>,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let find = |ranked: Vec<(&Path, f64)>| {
        ranked
            .into_iter()
            .find(|(p, _)| *p == target)
            .map(|(_, s)| s)
    };
    let lexical = find(corpus.ranked_with(subject, usize::MAX, None));
    let blended = find(corpus.ranked_with(subject, usize::MAX, tier));
    let semantic = tier.and_then(|t| {
        let query = t.query(bounded(subject), now()).ok()?;
        t.cosine(&target.to_string_lossy(), &query).map(f64::from)
    });
    (lexical, semantic, blended)
}

/// A score as a report reads it: three decimals, or `n/a` for a target the corpus does not hold.
fn three(score: Option<f64>) -> String {
    score.map_or("n/a".to_string(), |s| format!("{s:.3}"))
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// Every case in the file, scored three ways, one line per case: a report groups them by the
/// labels the take-up log's reading gave them.
#[test]
#[ignore = "bench: needs LETIBOT_R114_CASES, LETIBOT_R114_DATA and the checkpoint"]
fn every_case_scores_with_the_internal_tier() {
    let (Some(cases), Some((data, model))) = (std::env::var_os("LETIBOT_R114_CASES"), places())
    else {
        eprintln!("SKIPPED every_case_scores: LETIBOT_R114_CASES or LETIBOT_R114_DATA is unset");
        return;
    };
    let tier = tier(&model, &data);
    let text = std::fs::read_to_string(&cases).expect("the case file");
    let mut corpora: BTreeMap<String, Corpus> = BTreeMap::new();
    let registry = SkillRegistry::load_default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let case: serde_json::Value = serde_json::from_str(line).expect("a case");
        let target = PathBuf::from(case["target"].as_str().expect("a target"));
        let subject = case["subject"].as_str().unwrap_or("");
        let name = case["corpus"].as_str().unwrap_or("");
        let corpus = corpora.entry(name.to_string()).or_insert_with(|| {
            if name == "skills" {
                skills(&registry)
            } else {
                Corpus::new(&notes(Path::new(name)))
            }
        });
        corpus.fill(Some(&tier)).expect("the corpus fills");
        let (lexical, semantic, blended) = score(corpus, &target, subject, Some(&tier));
        println!(
            "case {} label={} lexical {} semantic {} blended {} subjects {} target {}",
            case["id"],
            case["label"].as_str().unwrap_or(""),
            three(lexical),
            three(semantic),
            three(blended),
            subject.chars().count(),
            target.display()
        );
    }
    let sizes: Vec<String> = corpora
        .iter()
        .map(|(k, c)| format!("{k}: {} document(s)", c.len()))
        .collect();
    println!("summary corpora: {}", sizes.join(", "));
}

/// What meaning would offer that the words would not: for every round's subject the corpus the
/// offer scores, the note the blend ranks first, and the score the WORDS alone gave that same
/// note. A note the words scored under the floor was never shown and meaning would have shown it.
#[test]
#[ignore = "bench: needs LETIBOT_R114_MISSES, LETIBOT_R114_DATA and the checkpoint"]
fn what_meaning_would_offer_that_words_do_not() {
    let (Some(misses), Some((data, model))) = (std::env::var_os("LETIBOT_R114_MISSES"), places())
    else {
        eprintln!("SKIPPED what_meaning_would_offer: LETIBOT_R114_MISSES or DATA is unset");
        return;
    };
    let tier = tier(&model, &data);
    let text = std::fs::read_to_string(&misses).expect("the miss file");
    let registry = SkillRegistry::load_default();
    let mut corpora: BTreeMap<String, Corpus> = BTreeMap::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let case: serde_json::Value = serde_json::from_str(line).expect("a case");
        let target = PathBuf::from(case["target"].as_str().expect("a target"));
        let subject = case["subject"].as_str().unwrap_or("");
        let name = if case["kind"].as_str() == Some("skill") {
            "skills".to_string()
        } else {
            target
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        if !seen.insert((name.clone(), subject.to_string())) {
            continue;
        }
        let corpus = corpora.entry(name.clone()).or_insert_with(|| {
            if name == "skills" {
                skills(&registry)
            } else {
                Corpus::new(&notes(Path::new(&name)))
            }
        });
        if corpus.is_empty() {
            continue;
        }
        corpus.fill(Some(&tier)).expect("the corpus fills");
        let top = corpus.ranked_with(subject, 1, Some(&tier));
        let Some((path, blended)) = top.first() else {
            continue;
        };
        let lexical = corpus
            .ranked_with(subject, usize::MAX, None)
            .into_iter()
            .find(|(p, _)| p == path)
            .map(|(_, s)| s)
            .unwrap_or(0.0);
        if lexical < 0.18 {
            println!(
                "recall {} words scored the note meaning ranks first: lexical {lexical:.3} \
                 blended {blended:.3} corpus {name} target {}",
                case["id"],
                path.display()
            );
        }
    }
}

/// What the cosines themselves look like, on texts whose answer is known: a note against its own
/// words must be high, two unrelated notes must be low, and a paraphrase must sit with the first
/// rather than the second. The control that says whether the numbers above mean anything.
#[test]
#[ignore = "bench: needs LETIBOT_R114_PAIRS, LETIBOT_R114_DATA and the checkpoint"]
fn cosines_between_two_texts() {
    let (Some(pairs), Some((data, model))) = (std::env::var_os("LETIBOT_R114_PAIRS"), places())
    else {
        eprintln!("SKIPPED cosines_between_two_texts: LETIBOT_R114_PAIRS or DATA is unset");
        return;
    };
    let tier = tier(&model, &data);
    let text = std::fs::read_to_string(&pairs).expect("the pair file");
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let pair: serde_json::Value = serde_json::from_str(line).expect("a pair");
        let a = pair["a"].as_str().unwrap_or("");
        let b = pair["b"].as_str().unwrap_or("");
        let (Ok(va), Ok(vb)) = (tier.query(bounded(a), now()), tier.query(bounded(b), now()))
        else {
            println!("pair {} label={} n/a", pair["id"], pair["label"]);
            continue;
        };
        println!(
            "pair {} label={} cosine {:.3}",
            pair["id"],
            pair["label"].as_str().unwrap_or(""),
            letibot_embeddings::cosine(&va, &vb)
        );
    }
}

/// What one round costs on this box: opening the model, filling the corpus, and the round's own
/// embed and cosines, with the cache cold and warm. Medians, because one mean measures the box.
#[test]
#[ignore = "bench: needs LETIBOT_R114_CORPUS, LETIBOT_R114_SUBJECTS, LETIBOT_R114_DATA"]
fn a_round_costs_this_much() {
    let (Some(dir), Some(subjects), Some((data, model))) = (
        std::env::var_os("LETIBOT_R114_CORPUS"),
        std::env::var_os("LETIBOT_R114_SUBJECTS"),
        places(),
    ) else {
        eprintln!(
            "SKIPPED a_round_costs: LETIBOT_R114_CORPUS, LETIBOT_R114_SUBJECTS or DATA unset"
        );
        return;
    };
    let files = notes(Path::new(&dir));
    let skills = skills(&SkillRegistry::load_default());
    let texts: Vec<String> = std::fs::read_to_string(&subjects)
        .expect("the subject file")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    // One round, as the loop spends it: both offers score the same subject.
    let round = |corpus: &Corpus, tier: &Tier, subject: &str| {
        corpus.ranked_with(subject, 2, Some(tier));
        skills.ranked_with(subject, 2, Some(tier));
    };
    let (mut opens, mut cold_fill, mut cold_round) = (Vec::new(), Vec::new(), Vec::new());
    let (mut warm_fill, mut warm_round) = (Vec::new(), Vec::new());
    let mut embedded = 0;
    for i in 0..20 {
        // A cache directory of its own per cold run: nothing a previous run embedded is in it.
        let fresh = data.join(format!("cold-{i}"));
        let _ = std::fs::remove_dir_all(&fresh);
        let start = Instant::now();
        let cold = tier(&model, &fresh);
        let corpus = Corpus::new(&files);
        opens.push(start.elapsed());
        let before = cold.embedder().spent();
        let start = Instant::now();
        corpus.fill(Some(&cold)).expect("fill");
        skills.fill(Some(&cold)).expect("fill");
        cold_fill.push(start.elapsed());
        embedded = (cold.embedder().spent().embedded - before.embedded) as usize;
        let start = Instant::now();
        round(&corpus, &cold, &texts[i % texts.len()]);
        cold_round.push(start.elapsed());

        let start = Instant::now();
        let warm = tier(&model, &data);
        let corpus = Corpus::new(&files);
        opens.push(start.elapsed());
        let start = Instant::now();
        corpus.fill(Some(&warm)).expect("fill");
        skills.fill(Some(&warm)).expect("fill");
        warm_fill.push(start.elapsed());
        let start = Instant::now();
        round(&corpus, &warm, &texts[i % texts.len()]);
        warm_round.push(start.elapsed());
    }
    // And a round inside a live process, on subjects no earlier round has embedded: the cache
    // used here is its own, so every subject below is a text this box has never seen.
    let steady_dir = data.join("steady");
    let _ = std::fs::remove_dir_all(&steady_dir);
    let live = tier(&model, &steady_dir);
    let corpus = Corpus::new(&files);
    corpus.fill(Some(&live)).expect("fill");
    skills.fill(Some(&live)).expect("fill");
    let mut steady = Vec::new();
    for subject in texts.iter().take(20) {
        let start = Instant::now();
        round(&corpus, &live, subject);
        steady.push(start.elapsed());
    }
    // And the same twenty subjects a second time: the memo and the cache answer, so this is a
    // round whose text this process has already embedded.
    let mut repeats = Vec::new();
    for subject in texts.iter().take(20) {
        let start = Instant::now();
        round(&corpus, &live, subject);
        repeats.push(start.elapsed());
    }
    println!(
        "cost corpora: {} note(s) and {} skill(s); one cold fill embeds {embedded} text(s)",
        files.len(),
        skills.len(),
    );
    println!("cost open, the model loaded: {:?}", median(opens));
    println!("cost fill, cold cache: {:?}", median(cold_fill));
    println!(
        "cost round, cold (fill + one score): {:?}",
        median(cold_round)
    );
    println!("cost fill, warm cache: {:?}", median(warm_fill));
    println!("cost round, warm restart: {:?}", median(warm_round));
    println!("cost round, steady, unseen subject: {:?}", median(steady));
    println!(
        "cost round, steady, same subject again: {:?}",
        median(repeats)
    );
}
