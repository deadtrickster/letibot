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
