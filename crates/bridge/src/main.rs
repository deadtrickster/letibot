//! `letibot-mcp` - the Claude Code bridge.
//!
//! ```text
//!   letibot-mcp                              MCP over stdio (what Claude Code runs)
//!   letibot-mcp wait <worktree> <session> --until attention [--timeout 2h] [--exit-on-stall]
//!   letibot-mcp room-wait [--attention to_me|mentions|all] [--timeout 2h]
//! ```
//!
//! Three verbs and no flags beyond the waits' own. The first is the server Claude Code
//! registers and keeps for the life of the conversation; the second is the background
//! command whose exit is the notification (`docs/claude-code-bridge.md`), because
//! Claude Code wakes its model when a background command finishes and has no other door.
//!
//! The wait exits:
//!
//! | code | means | on stdout |
//! |---|---|---|
//! | 0 | the turn ended | the last assistant message |
//! | 10 | a permission ask is pending | the ask, as JSON |
//! | 20 | the session failed, or could not be reached | why |
//! | 30 | the timeout passed | nothing (the reason is on stderr) |
//! | 40 | the provider is in trouble (not a stall, unless `--exit-on-stall`) | the event, as JSON |
//!
//! stdout is the MCP wire when serving, and nothing may be printed to it there. The
//! wait's stdout is its own and holds nothing but the payload above.

use std::time::{Duration, Instant};

use letibot_bridge::mcp;
use letibot_bridge::tools::{Bridge, DEFAULT_TIMEOUT, wait_with};

const USAGE: &str = "\
letibot-mcp — the Claude Code bridge to leticode

  letibot-mcp
      MCP over stdio. This is what Claude Code runs; it is not meant to be typed.

  letibot-mcp wait <worktree> <session> --until attention [--timeout 2h] [--exit-on-stall]
      Attach as a head and exit when the session needs you:
        0   the turn ended        (the last assistant message is on stdout)
        10  an ask is pending     (the ask is on stdout, as JSON)
        20  the session failed    (why is on stdout)
        30  the timeout passed    (nothing on stdout)
        40  the provider is stuck (a rate limit, a quota or a timeout: the event is on
                                  stdout, as JSON). A stall, a slow round with the request
                                  still open, does not end the wait unless --exit-on-stall.

      --timeout takes s, m or h (2h, 90s, 15m). Default 2h.
      --exit-on-stall makes a stall exit 40 too.

  letibot-mcp room-wait [--attention to_me|mentions|all] [--timeout 2h]
      Connect to the host's chat rooms as `cc@<folder>` ($LETIBOT_ROOM_NAME names it) and exit when a message arrives
      for it (default `to_me`: what names it, the operator's unaddressed lines, its DMs).
        0   a message arrived     (`#seq [room] author -> to: text`, one line each, on
                                  stdout; the inbox cursor is moved past them)
        20  the coordinator could not be reached (why is on stderr)
        30  the timeout passed    (nothing on stdout)

The server is registered with `claude mcp add` and is never run by hand for that job.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None => serve(),
        Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            0
        }
        Some("--version") => {
            println!("letibot-mcp {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("wait") => match parse_wait(&args[1..]) {
            Ok((worktree, session, deadline, exit_on_stall)) => {
                let (code, said) = wait_with(&worktree, &session, deadline, exit_on_stall);
                match code {
                    // The payload is the message or the ask: a person reading the
                    // notification sees the words, and the orchestrator that only wants the
                    // code ignores it.
                    0 | 10 | 40 => println!("{said}"),
                    20 => {
                        eprintln!("letibot-mcp: {said}");
                        println!("{said}");
                    }
                    _ => eprintln!("letibot-mcp: nothing needed attention before the timeout"),
                }
                code
            }
            Err(e) => {
                eprintln!("letibot-mcp: {e}\n\n{USAGE}");
                2
            }
        },
        Some("room-wait") => match parse_room_wait(&args[1..]) {
            Ok((attention, deadline)) => {
                let link = letibot_bridge::rooms::RoomLink::default();
                let (code, said) = letibot_bridge::rooms::wait(&link, attention, deadline);
                match code {
                    0 => println!("{said}"),
                    20 => eprintln!("letibot-mcp: {said}"),
                    _ => eprintln!("letibot-mcp: no room message before the timeout"),
                }
                code
            }
            Err(e) => {
                eprintln!("letibot-mcp: {e}\n\n{USAGE}");
                2
            }
        },
        Some(other) => {
            eprintln!("letibot-mcp: unknown command `{other}`\n\n{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

/// Serve MCP on stdin/stdout until the client closes it.
fn serve() -> i32 {
    // The folder's plugin layers (X25): a `tool_` layer becomes a tool this client can call, and
    // `push_channel` decides what wakes it. The server's door is the only one that reads them.
    let mut bridge = Bridge::new().streaming_rooms().with_layers_from_disk();
    // Off unless the user's file says `[bridge] permission_relay = true`: the relay approves tool
    // calls in somebody's session, which is not a thing to find switched on.
    if letibot_bridge::settings::BridgeSettings::load_user().permission_relay {
        bridge = bridge.relaying();
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match mcp::serve(stdin.lock(), stdout, &bridge) {
        Ok(()) => 0,
        Err(e) => {
            // A writer error is the client leaving; a reader error is the pipe. Neither is
            // a bug in the bridge, and stdout is the wire - the sentence goes to stderr.
            eprintln!("letibot-mcp: {e}");
            1
        }
    }
}

/// `wait <worktree> <session> --until attention [--timeout DUR] [--exit-on-stall]`. `--until attention` is the
/// only value this build understands, and it is required rather than defaulted: a future
/// `--until` that means something else must not be read as this one by a caller who typed nothing.
fn parse_wait(args: &[String]) -> Result<(String, String, Instant, bool), String> {
    let mut exit_on_stall = false;
    let mut worktree: Option<String> = None;
    let mut session: Option<String> = None;
    let mut until: Option<String> = None;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--until" => {
                until = Some(
                    it.next()
                        .ok_or("--until wants a value (`attention`)")?
                        .to_string(),
                )
            }
            "--timeout" => {
                let raw = it.next().ok_or("--timeout wants a duration (2h)")?;
                timeout = duration(raw)?;
            }
            "--exit-on-stall" => exit_on_stall = true,
            other if other.starts_with('-') => return Err(format!("unknown flag `{other}`")),
            other => {
                if worktree.is_none() {
                    worktree = Some(other.to_string());
                } else if session.is_none() {
                    session = Some(other.to_string());
                } else {
                    return Err(format!("unexpected argument `{other}`"));
                }
            }
        }
    }
    let until = until.ok_or("`--until attention` is required")?;
    if until != "attention" {
        return Err(format!(
            "`--until {until}` is not a value this build has; it is `attention`"
        ));
    }
    let worktree = worktree.ok_or("wait wants a worktree")?;
    let session = session.ok_or("wait wants a session id")?;
    Ok((worktree, session, Instant::now() + timeout, exit_on_stall))
}

/// `room-wait [--attention A] [--timeout DUR]`.
fn parse_room_wait(args: &[String]) -> Result<(letibot_bridge::rooms::Attention, Instant), String> {
    let mut attention = letibot_bridge::rooms::Attention::ToMe;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attention" => {
                let raw = it.next().ok_or("--attention wants a value (`to_me`)")?;
                attention = letibot_bridge::rooms::Attention::parse(raw)?;
            }
            "--timeout" => {
                let raw = it.next().ok_or("--timeout wants a duration (2h)")?;
                timeout = duration(raw)?;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    Ok((attention, Instant::now() + timeout))
}

/// A year. `Instant::now() + timeout` panics on a duration the clock cannot hold.
const LONGEST_TIMEOUT_SECS: u64 = 365 * 24 * 3600;

/// A duration as a person writes it: `90s`, `15m`, `2h`, or a bare number of seconds.
fn duration(raw: &str) -> Result<Duration, String> {
    let raw = raw.trim();
    let (number, unit) = match raw.char_indices().last() {
        Some((at, c)) if c.is_ascii_alphabetic() => (&raw[..at], Some(c.to_ascii_lowercase())),
        _ => (raw, None),
    };
    let n: u64 = number
        .parse()
        .map_err(|_| format!("`{raw}` is not a duration; write 90s, 15m or 2h"))?;
    let secs = match unit {
        None | Some('s') => n,
        Some('m') => n.saturating_mul(60),
        Some('h') => n.saturating_mul(3600),
        Some(other) => return Err(format!("`{other}` is not a unit; use s, m or h")),
    };
    Ok(Duration::from_secs(secs.min(LONGEST_TIMEOUT_SECS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The verb the orchestrator type-checks: two positionals, `--until attention`, and a
    /// deadline that is where the timeout says.
    #[test]
    fn the_wait_verb_parses_and_bounds_its_deadline() {
        let before = Instant::now();
        let (worktree, session, deadline, exit_on_stall) = parse_wait(&args(&[
            "/tmp/wt",
            "s-1",
            "--until",
            "attention",
            "--timeout",
            "90s",
        ]))
        .expect("parses");
        assert_eq!(worktree, "/tmp/wt");
        assert_eq!(session, "s-1");
        assert!(!exit_on_stall, "a stall is ridden out unless asked");
        let left = deadline.saturating_duration_since(before);
        assert!(
            left > Duration::from_secs(89) && left < Duration::from_secs(91),
            "{left:?}"
        );
        // The flags may come in any order, and a bare number is seconds.
        let (_, _, deadline, exit_on_stall) = parse_wait(&args(&[
            "--exit-on-stall",
            "--timeout",
            "120",
            "--until",
            "attention",
            "/tmp/wt",
            "s-1",
        ]))
        .expect("parses");
        assert!(deadline > Instant::now() + Duration::from_secs(110));
        assert!(exit_on_stall);
    }

    /// `--until` is required and `attention` is the only value: a `wait` that silently
    /// defaulted would be waiting for something nobody named.
    #[test]
    fn until_is_required_and_names_one_thing() {
        let e = parse_wait(&args(&["/tmp/wt", "s-1"])).expect_err("no --until");
        assert!(e.contains("--until"), "{e}");
        let e = parse_wait(&args(&["/tmp/wt", "s-1", "--until", "completion"]))
            .expect_err("unknown until");
        assert!(e.contains("completion") && e.contains("attention"), "{e}");
        let e = parse_wait(&args(&["/tmp/wt", "--until", "attention"])).expect_err("no session");
        assert!(e.contains("session"), "{e}");
    }

    #[test]
    fn room_wait_defaults_to_to_me_and_takes_both_flags() {
        let (a, _) = parse_room_wait(&args(&[])).expect("parses");
        assert_eq!(a, letibot_bridge::rooms::Attention::ToMe);
        let (a, d) =
            parse_room_wait(&args(&["--timeout", "90s", "--attention", "all"])).expect("parses");
        assert_eq!(a, letibot_bridge::rooms::Attention::All);
        assert!(d < Instant::now() + Duration::from_secs(91));
        assert!(parse_room_wait(&args(&["--attention", "some"])).is_err());
        assert!(parse_room_wait(&args(&["x"])).is_err());
    }

    /// A duration in the three units, and a refusal that says what the units are.
    #[test]
    fn durations_are_s_m_and_h() {
        assert_eq!(duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(duration("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(duration("45").unwrap(), Duration::from_secs(45));
        assert!(
            Instant::now()
                .checked_add(duration("18446744073709551615h").unwrap())
                .is_some()
        );
        let e = duration("2d").expect_err("not a unit");
        assert!(e.contains('d'), "{e}");
        let e = duration("soon").expect_err("not a number");
        assert!(e.contains("soon"), "{e}");
    }
}
