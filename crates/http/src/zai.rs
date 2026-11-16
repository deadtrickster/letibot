//! Z.ai / Zhipu (GLM) business codes on a refusal, read off the `(NNNN)` the provider crate appends.
//! Every limit is a `429`, so the status alone cannot tell a rate limit from a spent window.
//! Table: <https://docs.z.ai/api-reference/api-code>.

/// What a GLM limit code means to a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Not now, and soon: `1302` rate or concurrency, `1305` overloaded. Never a window.
    Transient,
    /// A spent usage window with its reset in the prose: `1308` (5 h), `1310` (weekly/monthly),
    /// `1316`-`1321` (5 h / 7 d with extra usage unavailable).
    Window,
    /// No wait lifts it: `1113` no balance, `1309` plan expired, `1311` model not in the plan,
    /// `1313` fair-use restriction, `1314` enterprise plan expired, `1315` wrong key type.
    Refused,
}

/// `next_flush_time` is written in China Standard Time (UTC+8) with no zone.
pub const FLUSH_TIME_OFFSET_MS: u64 = 8 * 3_600_000;

/// The GLM business code in `body`, when it carries one from the table.
pub fn code(body: &str) -> Option<u16> {
    let b = body.as_bytes();
    (0..b.len().saturating_sub(5)).find_map(|i| {
        let digits = b.get(i + 1..i + 5)?;
        if b[i] != b'(' || b.get(i + 5) != Some(&b')') || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let n: u16 = std::str::from_utf8(digits).ok()?.parse().ok()?;
        limit_of(n).map(|_| n)
    })
}

/// The meaning of one GLM limit code.
pub fn limit_of(code: u16) -> Option<Limit> {
    match code {
        1302 | 1305 => Some(Limit::Transient),
        1308 | 1310 | 1316..=1321 => Some(Limit::Window),
        1113 | 1309 | 1311 | 1313 | 1314 | 1315 => Some(Limit::Refused),
        _ => None,
    }
}

/// What the GLM code in `body` says, if it names one.
pub fn limit(body: &str) -> Option<Limit> {
    code(body).and_then(limit_of)
}

/// Is this a GLM refusal, whose wall clock is China time? Only the code says so: the prose
/// template alone is not proof of the vendor.
pub fn is_glm(body: &str) -> bool {
    code(body).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_is_read_off_the_appended_parenthesis() {
        assert_eq!(code("Rate limit reached for requests (1302)"), Some(1302));
        assert_eq!(
            limit("Rate limit reached for requests (1302)"),
            Some(Limit::Transient)
        );
        assert_eq!(
            limit("Usage limit reached for 5 hour. Your limit will reset at x (1308)"),
            Some(Limit::Window)
        );
        assert_eq!(limit("plan expired (1309)"), Some(Limit::Refused));
        // A number in parens that is not in the table is not a code.
        assert_eq!(code("took (2024) ms"), None);
        assert_eq!(code("(130"), None);
        assert_eq!(code(""), None);
    }
}
