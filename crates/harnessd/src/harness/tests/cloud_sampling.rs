//! A switch to a cloud provider sends that model's own sampling, never the local server's.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};

/// A chat-completions server on loopback that answers one word and records every body.
fn stub() -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                let h = h.trim_end().to_ascii_lowercase();
                if h.is_empty() {
                    break;
                }
                if let Some(v) = h.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            recorded
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap_or_default());
            let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n\
                       data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                       data: [DONE]\n\n";
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{sse}",
                sse.len()
            );
        }
    });
    (url, seen)
}

fn one_turn(h: &Harness) {
    let items = [TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: "hi".into() }],
    }];
    let req = letibot_backend::TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    h.provider
        .as_ref()
        .expect("the switch seated a provider")
        .complete(&req, &mut |_| letibot_backend::StreamFlow::Continue)
        .expect("the stub answers");
}

/// The operator's session runs on a local profile with `temperature = 0.7`. `/models` to a
/// chat-completions preset must send no temperature: that number was the local server's, and
/// a cloud model that fixes its own answers it with a 400.
#[test]
fn a_switch_to_a_cloud_preset_sends_no_local_sampling() {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("cloud-samp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let (url, seen) = stub();
    let file = dir.join("providers.toml");
    std::fs::write(
        &file,
        format!(
            "[model.\"local.glm\"]\ntemperature = 0.7\n\n[deepseek]\nkey = \"k\"\nurl = \"{url}\"\n"
        ),
    )
    .unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.session_id = "s-cloud-sampling".into();
    cfg.sampling = serde_json::json!({"temperature": 0.7, "top_k": 40, "seed": 7});
    cfg.providers_file = Some(file.clone());
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no file");
    let mut h =
        Harness::open(&parts, cfg, Hub::new("s-cloud-sampling")).expect("the session opens");
    let pc = crate::config::ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-flash".into()),
        api_key: None,
        thinking: false,
    };
    h.set_provider(Some(pc.clone())).expect("the switch");
    one_turn(&h);
    let body = seen.lock().unwrap().last().cloned().expect("a request");
    for k in ["temperature", "top_k", "seed"] {
        assert!(
            body.get(k).is_none(),
            "the local server's {k} reached the cloud: {body}"
        );
    }

    // The cloud model's own profile is what it gets.
    std::fs::write(
        &file,
        format!(
            "[model.\"local.glm\"]\ntemperature = 0.7\n\n[model.\"deepseek-flash\"]\n\
             temperature = 0.3\n\n[deepseek]\nkey = \"k\"\nurl = \"{url}\"\n"
        ),
    )
    .unwrap();
    h.set_provider(Some(pc)).expect("the rebuild");
    one_turn(&h);
    let body = seen.lock().unwrap().last().cloned().expect("a request");
    assert_eq!(body["temperature"], 0.3, "{body}");
    assert!(body.get("seed").is_none(), "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A key Kimi's plan fixes is dropped from the cloud profile, and the operator is told which
/// one once, through the provider's notes.
#[test]
fn a_fixed_key_in_a_kimi_profile_is_dropped_and_named() {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("cloud-fixed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let file = dir.join("providers.toml");
    std::fs::write(
        &file,
        "[model.\"k3.1-pro\"]\ntemperature = 0.7\nmax_tokens = 4000\n",
    )
    .unwrap();
    let said = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let heard = said.clone();
    let notes: letibot_provider::openai::Notes =
        std::sync::Arc::new(move |s: String| heard.lock().unwrap().push(s));
    let pc = crate::config::ProviderConfig {
        name: "kimi-code-global".into(),
        model: Some("k3.1-pro".into()),
        api_key: Some("k".into()),
        thinking: false,
    };
    let preset = letibot_provider::Preset::parse(&pc.name).unwrap();
    let s = crate::harness::open::cloud_sampling(preset, &pc, Some(&file), &notes);
    assert_eq!(s, serde_json::json!({"max_tokens": 4000}));
    let said = said.lock().unwrap();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("temperature=0.7"), "{}", said[0]);
    let _ = std::fs::remove_dir_all(&dir);
}
