//! Where a limit's instant comes from when the refusal named only its window: the door's own
//! usage endpoint, for a preset that declares one ([`letibot_provider::Preset::usage_url`]).

use super::*;

impl Harness {
    /// `e`, carrying the plan's reset as its `Retry-After` when the refusal is a rate limit that
    /// named no instant and the door reports one. One bounded request; any failure leaves `e`
    /// as it was, so the window in its prose is still the reading.
    pub(super) fn with_the_plans_reset(
        &self,
        e: letibot_turn::HttpError,
    ) -> letibot_turn::HttpError {
        use letibot_http::retry::{Policy, Trouble};
        let letibot_turn::HttpError::Status {
            code,
            body,
            retry_after: None,
        } = &e
        else {
            return e;
        };
        if !matches!(Policy::trouble(&e), Some(Trouble::RateLimit))
            || letibot_http::reset::names_a_wall_clock(body)
        {
            return e;
        }
        let Some(secs) = self.plans_reset_secs() else {
            return e;
        };
        letibot_turn::HttpError::Status {
            code: *code,
            body: body.clone(),
            retry_after: Some(secs),
        }
    }

    /// Seconds from the limit clock to the latest reset among the plan's spent windows.
    fn plans_reset_secs(&self) -> Option<u64> {
        let pc = self.cfg.provider.as_ref()?;
        let file = self.cfg.providers_file.as_deref();
        let preset = letibot_provider::registry::lookup(&pc.name, file).ok()?;
        preset.usage_url?;
        let creds = letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), file).ok()?;
        let at = letibot_provider::usage::exhausted_reset_ms(preset, &creds)?;
        let now = self.limit_now_ms();
        // A reset already past is a stale report: the window in the prose is the better reading.
        (at > now).then(|| (at - now).div_ceil(1000))
    }
}

impl Harness {
    /// This session's preset and key when the preset declares a usage endpoint.
    fn usage_door(
        &self,
    ) -> Option<(
        &'static letibot_provider::Preset,
        letibot_provider::keys::Credentials,
    )> {
        let pc = self.cfg.provider.as_ref()?;
        let file = self.cfg.providers_file.as_deref();
        let preset = letibot_provider::registry::lookup(&pc.name, file).ok()?;
        preset.usage_url?;
        let creds = letibot_provider::keys::resolve(preset, pc.api_key.as_deref(), file).ok()?;
        Some((preset, creds))
    }

    /// The `models.usage` row from the last poll of this session's preset, if there was one.
    pub(super) fn usage_row(&self) -> Option<letibot_sessionlog::protocol::SettingRow> {
        let (preset, creds) = self.usage_door()?;
        let url = letibot_provider::usage::url(preset, &creds)?;
        let w = letibot_provider::usage_poll::reading(&url)?;
        usage_setting(preset.name, &w, now_ms())
    }

    /// Keep the row fresh: each reading is put straight into the published rows. Called after the
    /// publish, so a first reading cannot land before the rows it joins. No poll for this session
    /// while its preset declares no endpoint.
    pub(super) fn watch_usage(&self) {
        use letibot_provider::usage_poll;
        let sid = self.cfg.session_id.clone();
        let Some((preset, creds)) = self.usage_door() else {
            usage_poll::unwatch(&sid);
            return;
        };
        let door = preset.name;
        let reg = self.session_registry.clone();
        let still = {
            let (reg, sid) = (reg.clone(), sid.clone());
            move || on_door(&reg.settings(&sid), door)
        };
        let tell = {
            let sid = sid.clone();
            move |w: &[usage_poll::Window]| {
                if let Some(row) = usage_setting(door, w, now_ms()) {
                    reg.put_setting(&sid, row);
                }
            }
        };
        let watcher = usage_poll::Watcher {
            still: Box::new(still),
            tell: Box::new(tell),
        };
        usage_poll::watch(preset, &creds, &sid, usage_poll::EVERY, watcher);
    }
}

/// The session's published `model` row names a model of `door`.
fn on_door(rows: &[letibot_sessionlog::protocol::SettingRow], door: &str) -> bool {
    rows.iter()
        .find(|r| r.key == "model")
        .and_then(|r| r.value.strip_prefix(door))
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The row for one reading: the busiest window, its reset in this box's zone.
pub(crate) fn usage_setting(
    door: &str,
    windows: &[letibot_provider::usage_poll::Window],
    now_ms: u64,
) -> Option<letibot_sessionlog::protocol::SettingRow> {
    let w = letibot_provider::usage_poll::busiest(windows)?;
    let offset = local_offset_s(w.reset_ms.unwrap_or(now_ms));
    Some(letibot_sessionlog::protocol::SettingRow {
        key: letibot_sessionlog::protocol::MODEL_USAGE_KEY.into(),
        value: letibot_provider::usage_poll::label(w, now_ms, offset),
        source: format!("{door} usage endpoint, polled every few minutes"),
        editable: String::new(),
        choices: vec![door.to_string()],
        tools: Vec::new(),
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Seconds east of UTC at `ms` in this box's zone, 0 when the zone cannot be read.
fn local_offset_s(ms: u64) -> i64 {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`, both owned here; the `_r` form keeps
    // no shared static, and the daemon runs many threads.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if ok { tm.tm_gmtoff } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, value: &str) -> letibot_sessionlog::protocol::SettingRow {
        letibot_sessionlog::protocol::SettingRow {
            key: key.into(),
            value: value.into(),
            source: String::new(),
            editable: String::new(),
            choices: Vec::new(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn a_session_is_on_a_door_only_by_its_model_row() {
        let rows = vec![row("model", "kimi-code/kimi-for-coding")];
        assert!(on_door(&rows, "kimi-code"));
        assert!(
            !on_door(&rows, "kimi"),
            "a prefix of the name is another door"
        );
        assert!(!on_door(&[row("model", "local (x)")], "kimi-code"));
        assert!(!on_door(&[], "kimi-code"), "a closed session has no rows");
    }

    #[test]
    fn the_row_names_the_busiest_window_and_its_door() {
        use letibot_provider::usage_poll::Window;
        let w = [
            Window {
                name: "5h".into(),
                used: 0.44,
                reset_ms: Some(1_000_000_000_000),
            },
            Window {
                name: "month".into(),
                used: 0.17,
                reset_ms: Some(2_000_000_000_000),
            },
        ];
        let r = usage_setting("kimi-code", &w, 1_000_000_000_000 - 3_600_000).unwrap();
        assert_eq!(r.key, letibot_sessionlog::protocol::MODEL_USAGE_KEY);
        assert!(r.value.starts_with("5h 44% · resets "), "{}", r.value);
        assert_eq!(r.choices, vec!["kimi-code".to_string()]);
        assert!(usage_setting("kimi-code", &[], 0).is_none());
    }
}
