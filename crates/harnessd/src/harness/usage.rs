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
        // The poll's reading serves when it shows the spent window, so one park is one request.
        let url = letibot_provider::usage::url(preset, &creds)?;
        let at = letibot_provider::usage_poll::spent_reset_ms(
            &url,
            &creds,
            letibot_provider::usage_poll::EVERY,
        )?;
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
        let (w, at_ms) = letibot_provider::usage_poll::reading(&url)?;
        let th = self.cfg.leticode.usage_warn(preset.name);
        usage_setting(preset.name, &w, &th, now_ms(), at_ms)
    }

    /// Write the threshold notices the poll left for this session, one row each. Called at a
    /// round boundary and on the idle pass, never between a call and its result.
    pub(crate) fn flush_usage_notices(&mut self) -> Result<(), HarnessError> {
        let lines = std::mem::take(
            &mut self
                .usage_said
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending,
        );
        for line in lines {
            self.record_notice(&line)?;
        }
        Ok(())
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
        let th = self.cfg.leticode.usage_warn(door);
        let said = self.usage_said.clone();
        let reg = self.session_registry.clone();
        let still = {
            let (reg, sid) = (reg.clone(), sid.clone());
            move || on_door(&reg.settings(&sid), door)
        };
        let watcher = usage_poll::Watcher {
            still: Box::new(still),
            tell: teller(reg, sid.clone(), door, th.clone(), said.clone()),
        };
        // A session joining a running poll hears its cached reading now, not a period on.
        if let Some((w, _)) = usage_poll::watch(preset, &creds, &sid, usage_poll::EVERY, watcher) {
            said.lock()
                .unwrap_or_else(|e| e.into_inner())
                .hear(door, &w, &th, now_ms());
        }
    }
}

/// What the poll hands this session each round: the row is replaced on every reading, crossing or
/// not, and only a crossing queues a notice.
fn teller(
    reg: std::sync::Arc<letibot_sessionlog::registry::Registry>,
    sid: String,
    door: &'static str,
    th: letibot_provider::usage_warn::Thresholds,
    said: std::sync::Arc<std::sync::Mutex<UsageSaid>>,
) -> letibot_provider::usage_poll::Tell {
    Box::new(move |w, at_ms| {
        let now = now_ms();
        if let Some(row) = usage_setting(door, w, &th, now, at_ms) {
            reg.put_setting(&sid, row);
        }
        said.lock()
            .unwrap_or_else(|e| e.into_inner())
            .hear(door, w, &th, now);
    })
}

/// The thresholds one session has been told of, per door, and the notices not yet written.
#[derive(Debug, Default)]
pub(crate) struct UsageSaid {
    said: std::collections::BTreeMap<String, letibot_provider::usage_warn::Said>,
    pub(crate) pending: Vec<String>,
}

impl UsageSaid {
    /// Queue one notice per threshold this reading crossed that was not yet said for its window.
    pub(crate) fn hear(
        &mut self,
        door: &str,
        windows: &[letibot_provider::usage_poll::Window],
        th: &letibot_provider::usage_warn::Thresholds,
        now_ms: u64,
    ) {
        let said = self.said.entry(door.to_string()).or_default();
        for w in said.crossings(windows, th) {
            let offset = local_offset_s(w.reset_ms.unwrap_or(now_ms));
            self.pending.push(format!(
                "[usage] {}",
                letibot_provider::usage_warn::notice(door, w, now_ms, offset)
            ));
        }
    }
}

/// The session's published `model` row names a model of `door`.
fn on_door(rows: &[letibot_sessionlog::protocol::SettingRow], door: &str) -> bool {
    rows.iter()
        .find(|r| r.key == "model")
        .and_then(|r| r.value.strip_prefix(door))
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The row for one reading taken at `at_ms`: the window past most thresholds, else the busiest, its
/// reset in this box's zone, and its age once older than the poll period. `choices[1]` is how many
/// of its thresholds it has reached, which the head colours by.
pub(crate) fn usage_setting(
    door: &str,
    windows: &[letibot_provider::usage_poll::Window],
    th: &letibot_provider::usage_warn::Thresholds,
    now_ms: u64,
    at_ms: u64,
) -> Option<letibot_sessionlog::protocol::SettingRow> {
    use letibot_provider::usage_poll;
    let (w, level) = letibot_provider::usage_warn::shown(windows, th)?;
    let offset = local_offset_s(w.reset_ms.unwrap_or(now_ms));
    Some(letibot_sessionlog::protocol::SettingRow {
        key: letibot_sessionlog::protocol::MODEL_USAGE_KEY.into(),
        value: usage_poll::label(w, now_ms, offset)
            + &usage_poll::age(at_ms, now_ms, usage_poll::EVERY),
        source: format!("{door} usage endpoint, polled every few minutes"),
        editable: String::new(),
        choices: vec![door.to_string(), level.to_string()],
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
        let th = letibot_provider::usage_warn::Thresholds::default();
        let now = 1_000_000_000_000 - 3_600_000;
        let r = usage_setting("kimi-code", &w, &th, now, now).unwrap();
        assert_eq!(r.key, letibot_sessionlog::protocol::MODEL_USAGE_KEY);
        assert!(r.value.starts_with("5h 44% · resets "), "{}", r.value);
        assert_eq!(r.choices, ["kimi-code", "0"]);
        assert!(usage_setting("kimi-code", &[], &th, 0, 0).is_none());
    }

    fn five_h(used: f64, reset_ms: u64) -> Vec<letibot_provider::usage_poll::Window> {
        vec![letibot_provider::usage_poll::Window {
            name: "5h".into(),
            used,
            reset_ms: Some(reset_ms),
        }]
    }

    /// 80% is said once, 95% once more, nothing until the window resets, then 80% again; the row's
    /// level follows.
    #[test]
    fn each_threshold_is_said_once_per_window() {
        let th = letibot_provider::usage_warn::Thresholds::default();
        let mut s = UsageSaid::default();
        let now = 1_000_000_000_000;
        let (r1, r2) = (now + 3_600_000, now + 5 * 3_600_000);
        let mut heard = |used, reset| {
            s.hear("kimi-code", &five_h(used, reset), &th, now);
            std::mem::take(&mut s.pending)
        };
        assert!(heard(0.5, r1).is_empty());
        let first = heard(0.82, r1);
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(
            first[0].starts_with("[usage] kimi-code 5h at 82% · resets "),
            "{first:?}"
        );
        assert!(heard(0.84, r1).is_empty(), "80% is said once");
        assert_eq!(heard(0.96, r1).len(), 1, "95% once more");
        assert!(heard(0.99, r1).is_empty());
        assert!(heard(0.02, r2).is_empty(), "a reset alone says nothing");
        assert_eq!(heard(0.81, r2).len(), 1, "the new window says 80% again");
        let level = |used| usage_setting("kimi-code", &five_h(used, r1), &th, now, now).unwrap();
        assert_eq!(level(0.5).choices[1], "0");
        assert_eq!(level(0.82).choices[1], "1");
        assert_eq!(level(0.96).choices[1], "2");
    }

    /// rano's 10:03-11:08: a window held at 100% for an hour of polls, its reset reported a few
    /// seconds either side of the same instant each time, is one notice.
    #[test]
    fn a_window_held_at_full_for_an_hour_of_polls_is_said_once() {
        let th = letibot_provider::usage_warn::Thresholds::default();
        let mut s = UsageSaid::default();
        let start = 1_794_834_180_000;
        let reset = start + 4 * 3_600_000;
        let mut said = Vec::new();
        for poll in 0..33u64 {
            let jitter = (poll % 7) * 900;
            let at = if poll % 2 == 0 {
                reset + jitter
            } else {
                reset - jitter
            };
            s.hear(
                "kimi-code-global",
                &five_h(1.0, at),
                &th,
                start + poll * 120_000,
            );
            said.append(&mut s.pending);
        }
        assert_eq!(said.len(), 1, "{said:?}");
    }

    /// Every reading replaces the head's row, with no threshold crossed; one the poll could not
    /// refresh carries its age.
    #[test]
    fn each_reading_redraws_the_row_without_a_crossing() {
        let reg = letibot_sessionlog::registry::Registry::new();
        reg.create("s-1", "", Default::default()).unwrap();
        reg.set_settings("s-1", vec![row("model", "kimi-code/k3")]);
        let said = std::sync::Arc::new(std::sync::Mutex::new(UsageSaid::default()));
        let th = letibot_provider::usage_warn::Thresholds::default();
        let tell = teller(reg.clone(), "s-1".into(), "kimi-code", th, said.clone());
        let shown = || {
            reg.settings("s-1")
                .into_iter()
                .find(|r| r.key == letibot_sessionlog::protocol::MODEL_USAGE_KEY)
                .map(|r| r.value)
                .unwrap_or_default()
        };
        let now = now_ms();
        let reset = now + 3_600_000;
        tell(&five_h(0.47, reset), now);
        assert!(shown().starts_with("5h 47% · resets "), "{}", shown());
        tell(&five_h(0.57, reset), now);
        assert!(shown().starts_with("5h 57% · resets "), "{}", shown());
        assert!(!shown().contains("ago"), "{}", shown());
        assert!(
            said.lock().unwrap().pending.is_empty(),
            "no threshold was crossed"
        );
        tell(&five_h(0.57, reset), now - 4 * 60_000);
        assert!(shown().ends_with(" (4m ago)"), "{}", shown());
    }

    /// A per-preset list replaces the default for that preset only.
    #[test]
    fn a_presets_own_thresholds_apply() {
        let th = letibot_provider::usage_warn::Thresholds {
            five_h: vec![90],
            ..Default::default()
        };
        let mut s = UsageSaid::default();
        s.hear("kimi-code", &five_h(0.85, 1), &th, 0);
        assert!(s.pending.is_empty(), "85% is under the preset's 90");
        s.hear("kimi-code", &five_h(0.91, 1), &th, 0);
        assert_eq!(s.pending.len(), 1);
    }
}
