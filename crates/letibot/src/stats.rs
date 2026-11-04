//! `letibot stats` and `letibot fb` - the metrics file from a shell.
//! `stats` opens `metrics.db` read-only, never writes and never starts a daemon. `fb` writes the same
//! `operator.feedback` row the daemon's connection thread writes, straight into the file.

use std::path::PathBuf;
use std::process::ExitCode;

use letibot_metrics::stats;

const STATS_USAGE: &str = "\
letibot stats [VERB] [--since 1d] [--session S] [--db PATH]

  (none) | today          the day at a glance: turns, tool errors, cost, p95 worker wait,
                          deliveries mid-turn, wakes, marks, events dropped
  waits                   worker queue wait per command verb, per build (c12d044)
  deliveries              results reaching the model: rule, door, mid-turn; wakes by cause
  tools                   tool calls: error rate, duration, memory peak against cap
  models                  time to first token, tok/s and cache hit per model
  turns [--limit N]       the turns, newest first, with their marks
  feedback [--bad]        every mark with its turn's numbers
  session ID              one session's timeline
  sql 'SELECT …'          anything, with the per-kind views loaded (read-only)

  --since                 2d, 12h, 30m or all (default 1d)
  --session S             only this session's rows (default: every session)
  --db                    the file (default $LETIBOT_METRICS_DB, else
                          ~/.local/share/letibot/metrics.db)

  example:
    letibot stats turns --since 2d --limit 5";

const FB_USAGE: &str = "\
letibot fb --session S [--turn T] [+|-] [NOTE…]
  marks the session's last finished turn (or T) good (+), bad (-) or with a note;
  #words in the note are tags.

  --session S             the session whose turn is marked (required)
  --turn T                one particular turn (default: the session's last finished one)
  --db                    the file (default $LETIBOT_METRICS_DB, else
                          ~/.local/share/letibot/metrics.db)

  examples:
    letibot fb --session s-1789919514688401228 + \"the retry fix worked\"
    letibot fb --session s-1789919514688401228 - #slow took ages";

struct Args {
    verb: Vec<String>,
    since: u64,
    session: Option<String>,
    db: PathBuf,
    bad: bool,
    limit: u32,
    turn: Option<String>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        verb: Vec::new(),
        since: stats::parse_since("1d")?,
        session: None,
        db: letibot_metrics::store::default_path(),
        bad: false,
        limit: 30,
        turn: None,
    };
    let mut it = args.iter();
    while let Some(w) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match w.as_str() {
            "--since" => a.since = stats::parse_since(&val("--since")?)?,
            "--session" => a.session = Some(val("--session")?),
            "--db" => a.db = PathBuf::from(val("--db")?),
            "--turn" => a.turn = Some(val("--turn")?),
            "--limit" => {
                a.limit = val("--limit")?
                    .parse()
                    .map_err(|_| "--limit is a number".to_string())?
            }
            "--bad" => a.bad = true,
            "-h" | "--help" => return Err(String::new()),
            _ => a.verb.push(w.clone()),
        }
    }
    Ok(a)
}

fn say(r: Result<(), String>, help: &str) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is_empty() => {
            println!("{help}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("letibot: {e}");
            ExitCode::from(2)
        }
    }
}

/// A mistake is two lines, and the second is where that verb is explained. Both verbs printed
/// their whole usage after one wrong word, so a typo was answered with a wall to read.
fn mistake(problem: &str, verb: &str) -> String {
    format!("{problem}\n         run `letibot {verb} --help` for what it takes and one example")
}

/// `letibot stats …`
pub fn run_stats(args: &[String]) -> ExitCode {
    say(stats_text(args).map(|t| print!("{t}")), STATS_USAGE)
}

/// The text `letibot stats` prints - apart from the printing, so a test can read it.
pub fn stats_text(args: &[String]) -> Result<String, String> {
    let a = parse(args)?;
    let since = stats::floor(letibot_metrics::now_ms(), a.since);
    let verb: Vec<&str> = a.verb.iter().map(String::as_str).collect();
    // The verb is a fact about the command line, so it is answered before the file is opened:
    // on a box with no metrics file the old order blamed the file for a typo.
    if !known_verb(&verb) {
        let problem = format!("stats: unknown verb {:?}", verb.join(" "));
        return Err(mistake(&problem, "stats"));
    }
    let c = stats::open(&a.db)?;
    let tables = match verb.as_slice() {
        [] | ["today"] => vec![stats::summary(&c, since)?],
        ["waits"] => vec![stats::waits(&c, since)?],
        ["deliveries"] => stats::deliveries(&c, since)?,
        ["tools"] => vec![stats::tools(&c, since)?],
        ["models"] => vec![stats::models(&c, since)?],
        ["turns"] => vec![stats::turns(&c, since, a.session.as_deref(), a.limit)?],
        ["feedback"] => vec![stats::feedback(&c, since, a.session.as_deref(), a.bad)?],
        ["session", id] => vec![stats::session(&c, id)?],
        ["sql", q @ ..] if !q.is_empty() => vec![stats::query(&c, "", &q.join(" "), &[])?],
        _ => {
            return Err(mistake(
                &format!("stats: unknown verb {:?}", verb.join(" ")),
                "stats",
            ));
        }
    };
    Ok(tables
        .iter()
        .map(stats::Table::render)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The verbs [`stats_text`] answers. One list, so *is this a verb* is asked once and the
/// answer cannot drift from the match below it.
fn known_verb(verb: &[&str]) -> bool {
    const ONE_WORD: &[&str] = &[
        "today",
        "waits",
        "deliveries",
        "tools",
        "models",
        "turns",
        "feedback",
    ];
    match verb {
        [] => true,
        [word] => ONE_WORD.contains(word),
        ["session", _] => true,
        ["sql", q @ ..] => !q.is_empty(),
        _ => false,
    }
}

/// `letibot fb …`
pub fn run_fb(args: &[String]) -> ExitCode {
    say(fb(args).map(|said| println!("{said}")), FB_USAGE)
}

/// Write one mark; the sentence says where it landed.
pub fn fb(args: &[String]) -> Result<String, String> {
    let a = parse(args)?;
    let session = a
        .session
        .clone()
        .ok_or_else(|| mistake("fb: --session S is required", "fb"))?;
    let mut words = a.verb.clone();
    let polarity = match words.first().map(String::as_str) {
        Some("+") => {
            words.remove(0);
            "good"
        }
        Some("-") => {
            words.remove(0);
            "bad"
        }
        _ => "note",
    };
    let note = words.join(" ");
    if polarity == "note" && note.trim().is_empty() {
        return Err(mistake("fb: a note, or + / -", "fb"));
    }
    if !a.db.exists() {
        return Err(format!(
            "no metrics file at {} — no daemon with metrics on has written one yet",
            a.db.display()
        ));
    }
    let conn = letibot_metrics::store::open(&a.db).map_err(|e| e.to_string())?;
    // The turn: as given, or the session's last finished one.
    let turn: Option<String> = match a.turn.clone() {
        Some(t) => Some(t),
        None => conn
            .query_row(
                "SELECT turn_id FROM event WHERE kind = 'turn.end' AND session_id = ?1 \
                 ORDER BY ts_ms DESC, id DESC LIMIT 1",
                [&session],
                |r| r.get(0),
            )
            .ok(),
    };
    let mut w = letibot_metrics::store::Writer::new(
        conn,
        letibot_metrics::store::Identity {
            daemon: format!("letibot-fb/{}", std::process::id()),
            build: format!("letibot {}", env!("CARGO_PKG_VERSION")),
        },
        letibot_metrics::store::Settings::default(),
    );
    let tags = letibot_metrics::tags(&note);
    let ev = letibot_metrics::Event::new(letibot_metrics::kind::FEEDBACK)
        .session(&session)
        .turn(turn.as_deref().unwrap_or(""), None)
        .name(polarity)
        .attr("note", note.as_str())
        .attr("tags", tags)
        .attr("head", "shell")
        .attr(
            "identity",
            std::env::var("USER").unwrap_or_else(|_| "operator".into()),
        );
    w.write_batch(&[ev]).map_err(|e| e.to_string())?;
    Ok(match turn {
        Some(t) => format!("marked {polarity} · {session} turn {t}"),
        None => {
            format!("marked {polarity} · {session} (no finished turn recorded — kept with no turn)")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-cli-stats-{}-{}",
            std::process::id(),
            letibot_metrics::now_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("metrics.db");
        let mut w = letibot_metrics::store::Writer::new(
            letibot_metrics::store::open(&p).unwrap(),
            letibot_metrics::store::Identity {
                daemon: "box/1/0".into(),
                build: "b".into(),
            },
            letibot_metrics::store::Settings::default(),
        );
        w.write_batch(
            &[letibot_metrics::Event::new(letibot_metrics::kind::TURN_END)
                .session("s-9")
                .turn("tr#4", None)
                .name("glm")
                .outcome("eos")],
        )
        .unwrap();
        p
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_shell_mark_lands_on_the_sessions_last_turn_and_stats_shows_it() {
        let db = temp_db();
        let dbs = db.to_str().unwrap();
        let said = fb(&s(&[
            "--db",
            dbs,
            "--session",
            "s-9",
            "-",
            "#slow",
            "took",
            "ages",
        ]))
        .unwrap();
        assert_eq!(said, "marked bad · s-9 turn tr#4");
        let out = stats_text(&s(&["feedback", "--db", dbs])).unwrap();
        assert!(
            out.contains("bad") && out.contains("#slow took ages"),
            "{out}"
        );
        let today = stats_text(&s(&["--db", dbs])).unwrap();
        assert!(
            today.contains("marks good / bad / note  0 / 1 / 0"),
            "{today}"
        );
        for verb in ["waits", "deliveries", "tools", "models", "turns"] {
            stats_text(&s(&[verb, "--db", dbs])).unwrap_or_else(|e| panic!("{verb}: {e}"));
        }
        let q = stats_text(&s(&[
            "sql",
            "SELECT COUNT(*) AS n FROM feedback",
            "--db",
            dbs,
        ]))
        .unwrap();
        assert!(q.contains("\n1\n"), "{q}");
        assert!(stats_text(&s(&["nonsense", "--db", dbs])).is_err());
        assert!(
            fb(&s(&["--db", dbs, "+"])).is_err(),
            "a mark needs its session"
        );
    }
}
