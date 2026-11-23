//! `letibot-secret`: a program asks the person for a secret, and the value goes straight to it.
//!
//! ```text
//!   letibot-secret ask --for "sudo on lubuntu1-3" [--key fleet-sudo] [--keep 15m] [--prompt TEXT]
//! ```
//!
//! It blocks, raises a masked card at the head, prints the value on stdout and exits 0. A refusal
//! or a deadline is exit 1 with a sentence on stderr and NOTHING on stdout, so the only thing a
//! caller can ever read is a value.
//!
//! `--key` is what makes a value reusable: with it the daemon may keep the value in its own
//! memory for `--keep` (a minute is the default; an hour is the most), scoped to this session
//! tree, and the next ask with the same key is answered without a card. The person decides
//! whether to keep it - the flag is the permission, the card is the choice.
//!
//! The binary is also what `SUDO_ASKPASS`, `SSH_ASKPASS` and `GIT_ASKPASS` point at, through
//! `letibot-askpass`: those three run a helper with the program's prompt as argv[1] and read the
//! value off its stdout. [`askpass`] turns exactly that into one of these asks.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use letibot_sessionlog::client::{HeadClient, SecretAnswer, askpass_unseated};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::vault::SecretWant;

/// The most a value may be kept: the daemon's own ceiling, restated so the CLI can refuse a
/// number rather than quietly asking for an hour when somebody wrote a week.
const KEEP_MOST: Duration = Duration::from_secs(60 * 60);

/// What one helper invocation is asking for, before it reaches a daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ask {
    pub for_what: String,
    pub key: Option<String>,
    pub keep: Option<Duration>,
    pub prompt: String,
    pub host: String,
    /// The command this came from: `LETIBOT_COMMAND` in a session, which is what the card shows
    /// so a person can see what they are answering.
    pub command: String,
}

impl Ask {
    fn want(&self) -> SecretWant {
        SecretWant {
            prompt: if self.prompt.is_empty() {
                "[letibot] secret:".to_string()
            } else {
                self.prompt.clone()
            },
            command: self.command.clone(),
            for_what: self.for_what.clone(),
            key: self.key.clone(),
            keep: self.keep,
            host: self.host.clone(),
        }
    }
}

/// The `letibot-secret` role: `ask`, or `--help`.
pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("ask") => {
            let ask = match parse(&args[1..]) {
                Ok(a) => a,
                Err(why) => {
                    eprintln!("letibot-secret: {why}");
                    return ExitCode::from(2);
                }
            };
            deliver(&ask)
        }
        Some("--help" | "-h" | "help") | None => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("letibot-secret: no such verb: {other} (there is one: `ask`)");
            ExitCode::from(2)
        }
    }
}

/// `--for TEXT`, `--key NAME`, `--keep SPAN`, `--prompt TEXT`, `--host NAMES`.
fn parse(args: &[String]) -> Result<Ask, String> {
    let mut ask = Ask::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = |what: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs {what}"))
        };
        match flag.as_str() {
            "--for" => ask.for_what = value("the words a person reads on the card")?,
            "--key" => ask.key = Some(value("a name")?),
            "--prompt" => ask.prompt = value("the program's own prompt")?,
            "--host" => ask.host = value("a host name or a list of them")?,
            "--keep" => {
                let span = value("a span - `15m`, `90s`, `1h`")?;
                let d = leticode_config::parse_span(&span)?;
                if d > KEEP_MOST {
                    return Err(format!(
                        "--keep {span} is longer than the {KEEP_MOST:?} a value may be kept: a \
                         password the daemon still holds an hour later is one nobody is \
                         thinking about"
                    ));
                }
                ask.keep = Some(d);
            }
            "--help" | "-h" => {
                return Err("--help is read before this: run `letibot-secret --help`".into());
            }
            other => return Err(format!("no such flag: {other}")),
        }
    }
    if ask.for_what.is_empty() {
        return Err(
            "--for is required: it is the card's first line, and a person answering a masked \
             field has nothing else to go on"
                .into(),
        );
    }
    // A keep with no key is a window nothing can find again: said here rather than silently
    // dropped, because the caller believes it cached something.
    if ask.keep.is_some() && ask.key.is_none() {
        return Err(
            "--keep needs --key: a value with no name cannot be found again, so a window alone \
             caches nothing"
                .into(),
        );
    }
    Ok(ask)
}

/// What `letibot-askpass` is: a program's prompt on argv, the value on stdout. `sudo`, `ssh`
/// and `git` all invoke a helper this way, so one function serves all three.
pub fn askpass(args: &[std::ffi::OsString]) -> ExitCode {
    let prompt = args
        .first()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "[sudo] password:".into());
    let ask = Ask {
        for_what: prompt.clone(),
        prompt,
        command: std::env::var("LETIBOT_COMMAND").unwrap_or_default(),
        // A `sudo` password is not filed under a name: two `sudo` calls an hour apart are two
        // questions, and caching one behind the other would be this helper deciding to stop
        // asking. `--key` is how a caller asks for the opposite, and it is the caller's call.
        ..Ask::default()
    };
    deliver(&ask)
}

/// Which socket a helper talks to, and in which session. Read from the environment by
/// [`deliver`], and handed in by a test - a `setenv` beside another thread's `getenv` is
/// undefined behaviour, and `cargo test` is not single-threaded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Door {
    /// `LETIBOT_SOCKET`: the session's own visible socket, which a command's environment carries.
    seated: String,
    /// `LETIBOT_ASKPASS_SOCKET`: the askpass-only socket an `ssh -R` forward reaches, when a
    /// session's tools run on another machine.
    forwarded: String,
    /// `LETIBOT_SESSION`.
    session: String,
}

impl Door {
    fn from_env() -> Door {
        let get = |k: &str| std::env::var(k).unwrap_or_default();
        let forwarded = get("LETIBOT_ASKPASS_SOCKET");
        // Two different names for two different facts, and only one of them is a session this
        // daemon holds: a session's commands carry `LETIBOT_SESSION` (its id), while a placement's
        // commands on another machine carry LETIBOT_SESSION as that placement's process mark and
        // the daemon's session in `LETIBOT_SECRET_SESSION`.
        let session = if forwarded.is_empty() {
            get("LETIBOT_SESSION")
        } else {
            let named = get("LETIBOT_SECRET_SESSION");
            if named.is_empty() {
                get("LETIBOT_SESSION")
            } else {
                named
            }
        };
        Door {
            seated: get("LETIBOT_SOCKET"),
            forwarded,
            session,
        }
    }
}

/// Ask the daemon, print the value, and say what happened when there is none.
pub fn deliver(ask: &Ask) -> ExitCode {
    deliver_at(ask, &Door::from_env())
}

/// [`deliver`] at a door that was handed in rather than read.
fn deliver_at(ask: &Ask, door: &Door) -> ExitCode {
    let want = ask.want();
    // Two doors, and which one is set IS the answer: a session's own commands are given the
    // seated socket, and a program on another machine is given the askpass-only one, which it
    // reaches through an `ssh -R` forward.
    let answer = if !door.forwarded.is_empty() {
        if door.session.is_empty() {
            eprintln!(
                "letibot-secret: LETIBOT_ASKPASS_SOCKET is set but LETIBOT_SESSION is not, so \
                 there is no session to ask in"
            );
            return ExitCode::FAILURE;
        }
        askpass_unseated(&door.forwarded, &door.session, &want)
    } else if door.seated.is_empty() || door.session.is_empty() {
        eprintln!(
            "letibot-secret: not inside a letibot session (LETIBOT_SOCKET / LETIBOT_SESSION \
             unset), so there is nobody to ask. Nothing was shown and no value was printed."
        );
        return ExitCode::FAILURE;
    } else {
        ask_seated(&door.seated, &door.session, &want)
    };
    match answer {
        Ok(SecretAnswer {
            secret: Some(value),
            ..
        }) => {
            let mut out = std::io::stdout().lock();
            // The value and a newline, nothing else: every caller of a helper - `sudo`, `ssh`,
            // `git`, a shell's `$(...)` - reads this stream as the secret and nothing but it.
            let _ = out.write_all(value.as_bytes());
            let _ = out.write_all(b"\n");
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        Ok(SecretAnswer { secret: None, why }) => {
            match why {
                Some(why) => eprintln!("letibot-secret: no secret was given — {why}"),
                None => eprintln!(
                    "letibot-secret: no secret was given — nobody answered before the deadline, \
                     or the person refused"
                ),
            }
            ExitCode::FAILURE
        }
        Err(why) => {
            eprintln!("letibot-secret: {why}");
            ExitCode::FAILURE
        }
    }
}

/// The seated door: attach as the helper (`askpass` kind, no vote), send the one frame, and read
/// until the answer. Every door but this one is refused to a tool call, which is why the attach
/// is worth making rather than reaching for a second socket.
fn ask_seated(socket: &str, session: &str, want: &SecretWant) -> Result<SecretAnswer, String> {
    let caps = Caps {
        queue: 8,
        can_decide: false,
        ..Caps::default()
    };
    let (mut client, _hello, mut reader) =
        HeadClient::attach(socket, session, u64::MAX, "askpass", "secret", caps)
            .map_err(|e| format!("the session's daemon did not take the question: {e}"))?;
    client.askpass(want, "").map_err(|e| format!("{e}"))?;
    // The daemon may deliver a snapshot or events before the answer; only the `Secret` frame is
    // for this connection, and it comes on this connection or not at all.
    loop {
        match reader.read::<ServerFrame>() {
            Ok(ServerFrame::Secret { secret, why }) => {
                let _ = client.detach();
                return Ok(SecretAnswer { secret, why });
            }
            Ok(ServerFrame::Bye { reason }) => {
                return Err(format!("the daemon closed the connection: {reason}"));
            }
            Ok(_) => continue,
            Err(e) => return Err(format!("{e}")),
        }
    }
}

fn print_usage() {
    println!(
        "\
letibot-secret — ask the person for a secret, and keep it out of the conversation

USAGE
  letibot-secret ask --for TEXT [--key NAME] [--keep SPAN] [--prompt TEXT] [--host NAMES]

  --for TEXT     what will use it, in your words: the card's first line (required)
  --key NAME     the name the value is filed under, so a second ask can be answered from
                 the daemon's memory instead of a second card
  --keep SPAN    how long the value may be kept when --key is set: `15m`, `90s`, `1h`; an
                 hour is the most. The person chooses on the card, and may say this
                 command only
  --prompt TEXT  the program's own prompt, shown above the field
  --host NAMES   the machine(s) it is going to, shown on the card

  The value goes to this command's stdout and nowhere else: not the transcript, not a tool
  result, not a log. A refusal or a deadline is exit 1 with a sentence on stderr and an
  empty stdout. Set from a session's own environment: LETIBOT_SOCKET, LETIBOT_SESSION.
  `LETIBOT_ASKPASS_SOCKET` names the askpass-only socket an `ssh -R` forward reaches.

  letibot-askpass PROMPT     what SUDO_ASKPASS/SSH_ASKPASS/GIT_ASKPASS run (same binary).

EXAMPLES
  letibot-secret ask --for 'sudo on lubuntu1-3' --key fleet-sudo --keep 15m \\
    | ssh lubuntu1 'sudo -S -p \"\" apt-get -y upgrade'
  secret GITHUB_TOKEN 'pushing the release branch'   # the shell helper on a session's PATH
"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    /// `--for` is the card's first line, and a masked field with no sentence above it is a person
    /// typing a password for nobody. A keep with no key is the same kind of mistake one step
    /// later: a window nothing can find again.
    #[test]
    fn the_cli_refuses_an_ask_with_nothing_to_say_or_nowhere_to_keep_it() {
        let why = parse(&argv(&[])).expect_err("no --for");
        assert!(why.contains("--for is required"), "{why}");
        let why = parse(&argv(&["--for", "x", "--keep", "15m"])).expect_err("no key");
        assert!(why.contains("--keep needs --key"), "{why}");
        let why =
            parse(&argv(&["--for", "x", "--key", "k", "--keep", "24h"])).expect_err("too long");
        assert!(why.contains("longer than"), "{why}");
        let ask = parse(&argv(&[
            "--for",
            "sudo on lubuntu1-3",
            "--key",
            "fleet-sudo",
            "--keep",
            "15m",
        ]))
        .expect("a whole ask");
        assert_eq!(ask.keep, Some(Duration::from_secs(15 * 60)));
        assert_eq!(ask.key.as_deref(), Some("fleet-sudo"));
    }

    /// A span is parsed the way the config files parse one, and a bare number is NOT a span:
    /// minutes and seconds both read `900`, so the refusal is the honest answer.
    #[test]
    fn a_keep_is_a_span_and_a_bare_number_is_refused() {
        let ask =
            parse(&argv(&["--for", "x", "--key", "k", "--keep", "90s"])).expect("90s is a span");
        assert_eq!(ask.keep, Some(Duration::from_secs(90)));
        assert!(parse(&argv(&["--for", "x", "--key", "k", "--keep", "900"])).is_err());
    }

    fn one_ask() -> Ask {
        Ask {
            for_what: "sudo on lubuntu1-3".into(),
            ..Ask::default()
        }
    }

    /// Outside a session there is nobody to ask, and the honest answer is a refusal that says so
    /// rather than a wait: `sudo` has a deadline, and a person watching a stalled terminal does
    /// not know what it is waiting for.
    #[test]
    fn outside_a_session_the_helper_refuses_rather_than_waiting() {
        assert_eq!(
            deliver_at(&one_ask(), &Door::default()),
            ExitCode::FAILURE,
            "no socket and no session is exit 1, so sudo reports that no password was given"
        );
    }

    /// A socket with no daemon behind it is a refusal too, and it comes back at once: a helper
    /// that waited on a path nothing listens on would be `sudo` hanging on a card nobody sees.
    #[test]
    fn a_socket_with_no_daemon_behind_it_is_a_refusal() {
        let nowhere = std::env::temp_dir().join(format!(
            "letibot-secret-no-daemon-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let door = Door {
            seated: nowhere.display().to_string(),
            session: "s-nobody".into(),
            ..Door::default()
        };
        assert_eq!(deliver_at(&one_ask(), &door), ExitCode::FAILURE);
    }
}
