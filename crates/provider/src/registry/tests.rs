use super::*;
use crate::presets::{ANTHROPIC, DEEPSEEK};

fn file(tag: &str, body: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-registry-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("providers.toml");
    std::fs::write(&f, body).unwrap();
    f
}

fn no_env(_: &str) -> Option<String> {
    None
}

/// An entry with a built-in's name and only `url` moves the URL and leaves every other fact.
#[test]
fn a_deepseek_entry_with_only_a_url_changes_the_url_and_nothing_else() {
    let f = file(
        "deepseek-url",
        "[provider.deepseek]\nurl = \"http://127.0.0.1:9/v1/chat/completions\"\n",
    );
    let r = Registry::read_with(Some(&f), &no_env);
    let p = r.find("deepseek").unwrap();
    assert_eq!(p.url, "http://127.0.0.1:9/v1/chat/completions");
    assert_eq!(p.origin, Origin::Changed);
    let same = Preset {
        url: p.url,
        origin: Origin::Changed,
        ..DEEPSEEK
    };
    assert_eq!(*p, same);
    // The built-in's aliases still reach it, and the table's own constant is untouched.
    assert_eq!(r.find("DeepSeek").unwrap().url, p.url);
    assert_eq!(DEEPSEEK.url, "https://api.deepseek.com/chat/completions");
    assert!(r.faults().is_empty(), "{:?}", r.faults());
}

/// A wire this build does not speak is refused by name, with the file and the key, and the
/// other doors still answer.
#[test]
fn an_unknown_protocol_is_refused_naming_the_file_and_the_key() {
    let f = file(
        "bad-protocol",
        "[provider.grpcproxy]\nprotocol = \"grpc\"\nurl = \"http://127.0.0.1:9\"\nmodel = \"m\"\n",
    );
    let r = Registry::read_with(Some(&f), &no_env);
    let why = r.find("grpcproxy").unwrap_err();
    assert!(why.contains(&f.display().to_string()), "{why}");
    assert!(why.contains("[provider.grpcproxy] protocol"), "{why}");
    assert!(why.contains("chat-completions"), "{why}");
    assert_eq!(r.faults(), vec![why]);
    assert_eq!(r.find("deepseek").unwrap(), &DEEPSEEK);
}

/// A new door takes the four facts a request needs, and a typo'd key is refused rather than ignored.
#[test]
fn a_new_door_is_built_from_its_entry_and_a_stray_key_is_refused() {
    let f = file(
        "new-door",
        "[provider.myproxy]\nprotocol = \"anthropic-messages\"\nurl = \"https://proxy.lan/v1/messages\"\n\
         model = \"k3\"\nfixed = [\"temperature\", \"top_p\"]\naliases = [\"mp\"]\nwindow = 262144\n\n\
         [provider.typo]\nprotocol = \"responses\"\nurl = \"https://x\"\nmodle = \"m\"\n",
    );
    let r = Registry::read_with(Some(&f), &no_env);
    let p = r.find("mp").unwrap();
    assert!(
        std::ptr::eq(p, r.find("MyProxy").unwrap()),
        "a name is matched without case"
    );
    assert_eq!(p.name, "myproxy");
    assert_eq!(p.protocol, Protocol::AnthropicMessages);
    assert_eq!(p.key_env, "MYPROXY_API_KEY");
    assert_eq!(p.fixed_sampling, ["temperature", "top_p"]);
    assert_eq!(p.catalogue_id, "");
    assert_eq!(p.origin, Origin::Config);
    assert_eq!(
        p.window(None, &crate::catalogue::Catalogue::default()),
        Some(262144)
    );
    let why = r.find("typo").unwrap_err();
    assert!(why.contains("[provider.typo] modle"), "{why}");
    // Read again, the same facts are the same door: interning, not a second leak.
    let again = Registry::read_with(Some(&f), &no_env)
        .find("myproxy")
        .unwrap();
    assert!(std::ptr::eq(p, again));
}

/// A base URL defines a door of its own; without one, the built-in is what it was.
#[test]
fn a_base_url_variable_defines_an_env_door_and_its_absence_defines_none() {
    let env = |v: &str| match v {
        "ANTHROPIC_BASE_URL" => Some("https://api.kimi.ai/coding/".to_string()),
        "ANTHROPIC_MODEL" => Some("k3.1-pro".to_string()),
        "OPENAI_BASE_URL" => Some("http://127.0.0.1:4000/v1".to_string()),
        "LETIBOT_OPENAI_PROTOCOL" => Some("responses".to_string()),
        _ => None,
    };
    let r = Registry::read_with(None, &env);
    let a = r.find("env-anthropic").unwrap();
    assert_eq!(a.url, "https://api.kimi.ai/coding/v1/messages");
    assert_eq!(a.fallback_model, "k3.1-pro");
    assert_eq!(a.key_env, "ANTHROPIC_API_KEY");
    assert_eq!(a.alt_envs, ["ANTHROPIC_AUTH_TOKEN"]);
    assert_eq!(a.origin, Origin::Env("ANTHROPIC_BASE_URL"));
    let o = r.find("env-openai").unwrap();
    assert_eq!(o.protocol, Protocol::Responses);
    assert_eq!(o.url, "http://127.0.0.1:4000/v1/responses");
    assert_eq!(r.find("anthropic").unwrap(), &ANTHROPIC);

    let none = Registry::read_with(None, &no_env);
    assert!(none.find("env-anthropic").is_err());
    assert_eq!(none.find("anthropic").unwrap(), &ANTHROPIC);
}

/// A door no catalogue carries is unpriced with no known window, never free or some default size.
#[test]
fn a_door_with_no_catalogue_says_unpriced_and_window_unknown() {
    let f = file(
        "facts",
        "[provider.myproxy]\nprotocol = \"chat-completions\"\nurl = \"http://127.0.0.1:9\"\nmodel = \"m\"\n",
    );
    let p = Registry::read_with(Some(&f), &no_env)
        .find("myproxy")
        .unwrap();
    let cat = crate::catalogue::Catalogue::default();
    let line = p.facts_line("m", &cat, &BTreeMap::new());
    assert!(line.contains("unpriced"), "{line}");
    assert!(line.contains("window unknown"), "{line}");
    assert!(line.contains("providers.toml"), "{line}");
}

/// The endpoint's own listing, read off a loopback stub with the door's key.
#[test]
fn a_door_lists_its_models_from_its_own_endpoint() {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(Mutex::new(String::new()));
    let saw = seen.clone();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut l = String::new();
            if reader.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
                break;
            }
            head.push_str(&l);
        }
        *saw.lock().unwrap() = head;
        let body = r#"{"data":[{"id":"zeta"},{"id":"alpha"}]}"#;
        let _ = write!(
            conn,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
    });
    let f = file(
        "listing",
        &format!(
            "[provider.myproxy]\nprotocol = \"chat-completions\"\n\
             url = \"http://{addr}/v1/chat/completions\"\nmodel = \"m\"\n"
        ),
    );
    let p = Registry::read_with(Some(&f), &no_env)
        .find("myproxy")
        .unwrap();
    let creds = crate::keys::Credentials {
        key: "sk-stub".into(),
        from: "test".into(),
        prices: BTreeMap::new(),
        url: None,
    };
    assert_eq!(fetch_models(p, &creds).unwrap(), ["alpha", "zeta"]);
    let head = seen.lock().unwrap().clone();
    assert!(head.starts_with("GET /v1/models "), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer sk-stub"),
        "{head}"
    );
}

/// `usage` is preset data like any other: set on a new door, cleared on a built-in by an empty
/// value, and refused when it is neither a path nor a URL.
#[test]
fn a_usage_endpoint_is_declared_cleared_or_refused() {
    let f = file(
        "usage",
        "[provider.myplan]\nprotocol = \"chat-completions\"\n\
         url = \"http://127.0.0.1:9/v1/chat/completions\"\nmodel = \"m\"\nusage = \"/v1/usages\"\n\n\
         [provider.kimi-code]\nusage = \"\"\n\n\
         [provider.badplan]\nprotocol = \"chat-completions\"\nurl = \"http://127.0.0.1:9\"\n\
         model = \"m\"\nusage = \"usages\"\n",
    );
    let r = Registry::read_with(Some(&f), &no_env);
    assert_eq!(r.find("myplan").unwrap().usage_url, Some("/v1/usages"));
    assert_eq!(r.find("kimi-code").unwrap().usage_url, None);
    assert!(crate::presets::KIMI_CODE.usage_url.is_some());
    let why = r.find("badplan").unwrap_err();
    assert!(why.contains("[provider.badplan] usage"), "{why}");
}

/// `[provider.<name>]` in the reference is exactly `KEYS`, and an entry setting every listed
/// key is a door.
#[test]
fn the_provider_table_is_listed() {
    use letibot_config_ref::{files::PROVIDERS, probe};
    let t = PROVIDERS.table("provider.<name>").unwrap();
    let keys: std::collections::BTreeSet<&str> = KEYS.iter().copied().collect();
    assert_eq!(keys, probe::keys_of(t));
    let mut body = String::from("[provider.probe]\n");
    for k in t.keys {
        body.push_str(&format!("{} = {}\n", k.key, k.example));
    }
    let f = file("reference", &body);
    assert!(
        lookup("probe", Some(&f)).is_ok(),
        "{:?}",
        lookup("probe", Some(&f)).err()
    );
}
