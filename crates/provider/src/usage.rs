//! A plan's own report of its windows ([`Preset::usage_url`]). A coding plan's `429` can name the
//! window it closed and not the instant it lifts; the plan says the instant here.

use std::time::Duration;

use serde_json::Value;

use crate::keys::Credentials;
use crate::presets::Preset;

/// One request on a failure path that already waited out a ladder: a few seconds, then the window.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// The usage endpoint for this door: the preset's whole URL, or its path on the origin the session
/// actually posts to, so a door moved by a `url` override is asked on its new host.
pub fn url(preset: &Preset, creds: &Credentials) -> Option<String> {
    let usage = preset.usage_url?;
    if usage.starts_with("http://") || usage.starts_with("https://") {
        return Some(usage.to_string());
    }
    let door = creds.url.as_deref().unwrap_or(preset.url);
    let (scheme, rest) = door.split_once("://")?;
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host}{usage}"))
}

/// When the plan's limit lifts, Unix millis: the latest reset among its exhausted windows, since
/// nothing lifts until every one of them has. `None` when the door declares no endpoint, the
/// request fails, or no window is exhausted - the caller then reads the refusal's own prose.
pub fn exhausted_reset_ms(preset: &Preset, creds: &Credentials) -> Option<u64> {
    let url = url(preset, creds)?;
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .build(),
    );
    let body: Value = agent
        .get(&url)
        .header("Authorization", &format!("Bearer {}", creds.key))
        .header("User-Agent", crate::kimi::USER_AGENT)
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    reset_of(&body)
}

/// The reading itself: `usages.<window>.used_ratio >= 1` beside its `reset_time`. `limits[].detail`
/// (`used >= limit`, `resetTime`) is the same fact a second later, read only when `usages` is silent.
pub fn reset_of(body: &Value) -> Option<u64> {
    let named = body["usages"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, w)| {
            let spent = w["used_ratio"].as_f64()? >= 1.0;
            spent.then(|| letibot_http::reset::rfc3339_ms(w["reset_time"].as_str()?))?
        });
    let listed = body["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let d = &l["detail"];
            let count = |k: &str| -> Option<f64> {
                d[k].as_f64()
                    .or_else(|| d[k].as_str().and_then(|s| s.parse().ok()))
            };
            let spent = count("used")? >= count("limit")?;
            spent.then(|| letibot_http::reset::rfc3339_ms(d["resetTime"].as_str()?))?
        });
    named.max().or_else(|| listed.max())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan's answer verbatim, as Kimi gave it with the 5-hour window spent.
    const SPENT_5H: &str = r#"{"limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},"detail":{"limit":"100","used":"100","resetTime":"2026-11-14T01:07:08.146515Z"}}],"usages":{"limit_5h":{"used_ratio":1,"reset_time":"2026-11-14T01:07:07Z"},"limit_month_total":{"used_ratio":0.1688,"reset_time":"2026-12-13T00:07:40Z"},"limit_month_code":{"used_ratio":0.1688,"reset_time":"2026-12-13T00:07:40Z"}}}"#;

    fn ms(s: &str) -> u64 {
        letibot_http::reset::rfc3339_ms(s).unwrap()
    }

    #[test]
    fn the_spent_window_is_read_and_the_unspent_month_is_not() {
        let body: Value = serde_json::from_str(SPENT_5H).unwrap();
        assert_eq!(reset_of(&body), Some(ms("2026-11-14T01:07:07Z")));
        let limits_only = serde_json::json!({"limits": body["limits"]});
        assert_eq!(
            reset_of(&limits_only),
            Some(ms("2026-11-14T01:07:08.146515Z"))
        );
    }

    #[test]
    fn a_spent_month_outlasts_the_5_hour_window() {
        let body = serde_json::json!({"usages": {
            "limit_5h": {"used_ratio": 1, "reset_time": "2026-11-14T01:07:07Z"},
            "limit_month_total": {"used_ratio": 1.0, "reset_time": "2026-12-13T00:07:40Z"},
        }});
        assert_eq!(reset_of(&body), Some(ms("2026-12-13T00:07:40Z")));
    }

    #[test]
    fn nothing_spent_is_no_instant() {
        let body = serde_json::json!({"usages": {
            "limit_5h": {"used_ratio": 0.4, "reset_time": "2026-11-14T01:07:07Z"},
        }, "limits": [{"detail": {"limit": "100", "used": "40", "resetTime": "2026-11-14T01:07:07Z"}}]});
        assert_eq!(reset_of(&body), None);
        assert_eq!(reset_of(&serde_json::json!({"error": "nope"})), None);
    }

    #[test]
    fn a_path_is_asked_on_the_host_the_session_posts_to() {
        let p = crate::presets::KIMI_CODE;
        let mut creds = Credentials {
            key: "k".into(),
            from: "test".into(),
            prices: Default::default(),
            url: None,
        };
        assert_eq!(
            url(&p, &creds).as_deref(),
            Some("https://api.kimi.com/coding/v1/usages")
        );
        creds.url = Some("http://127.0.0.1:9/coding/v1/chat/completions".into());
        assert_eq!(
            url(&p, &creds).as_deref(),
            Some("http://127.0.0.1:9/coding/v1/usages")
        );
        assert_eq!(url(&crate::presets::DEEPSEEK, &creds), None);
    }
}
