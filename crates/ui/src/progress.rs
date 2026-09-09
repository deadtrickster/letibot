//! In-flight turn state: the prefill bar, the decode rate, and a spinner.
//!
//! # This is the one place letibot has more to show than either upstream
//!
//! Both surveyed projects talk to a metered API over someone else's HTTP, so
//! between "request sent" and "first token" they have nothing but a spinner and
//! an elapsed clock. That is why neither has a prefill display: there is no
//! prefill number to display.
//!
//! letibot's server is ours (§11), `return_progress: true` is set on every
//! request (`letibot_turn::completion::CompletionRequest::new`), and the stream
//! carries `prompt_progress {total, cache, processed, time_ms}`. So the dead time
//! before the first token — which on a 40k-token prompt against a local GLM is
//! most of the wait — is fully observable, and showing it as a spinner would be
//! throwing away the one number this architecture bought.
//!
//! # What the three numbers mean, because getting this wrong is easy
//!
//! From `server-task.h`'s `result_prompt_progress`:
//!
//! - `total` — tokens in the prompt.
//! - `cache` — tokens reused from the slot's KV cache. **Free.** This is the
//!   quantity §10 and §11 are about, and it is the reason the bar has three
//!   segments rather than two.
//! - `processed` — tokens now resident in the slot, **including** `cache`. So
//!   the fraction complete is `processed / total`, and the work actually done
//!   this turn is `processed - cache`.
//! - `time_ms` — cumulative prefill milliseconds.
//!
//! Reading `processed` as "processed *since* the cache" would show a 90%-cached
//! prompt as 10% done and then jump to 100%, which is the classic progress-bar
//! lie. [`Prefill::fraction`] and the segment split below are written against the
//! definitions above and tested against them.
//!
//! # Everything here is a pure function of its inputs
//!
//! No clock is read inside this module. Elapsed time is a parameter. A progress
//! display that samples the clock cannot be tested, and a progress display that
//! cannot be tested is exactly the kind that says "99%" for four minutes.
//!
//! # Provenance
//!
//! **Adapted from grok-build** (xAI, Apache-2.0),
//! `crates/codegen/xai-grok-pager/src/views/progress_bar.rs` — the eighth-of-a-cell
//! bar resolution using the LEFT BLOCK glyphs `▏▎▍▌▋▊▉█`, and its `cell_breakdown`
//! split into whole cells plus a remainder in eighths.
//!
//! Changed: their bar is two-valued (filled / not filled) and drives a
//! context-window meter that appears on hover. This one is **three-valued**
//! — reused-from-cache, computed-this-turn, not-yet-processed — because the
//! quantity letibot is optimising for is the first of those and a two-valued bar
//! cannot show it. The sub-cell remainder is therefore applied only to the
//! moving edge (progress) and the cache boundary is rounded to a whole cell;
//! two fractional boundaries cannot both be drawn in one cell, and the cache
//! boundary is the one that barely moves.
//!
//! Also from grok-build, and adopted rather than copied: the spinner frame is
//! chosen by dividing a tick, and `views/turn_status.rs` fixes the divisor at 4
//! ticks of a 30 fps loop (~7.5 frames/s). [`spinner`] takes elapsed
//! milliseconds instead, so it does not assume a frame rate — a letibot head's
//! loop is paced by a 100 ms `VTIME` read, not by a renderer.
//!
//! Their `format_tokens_short` (`turn_status.rs:749`) and this file's
//! [`thousands`] were written to the same brief and land on the same shape.

use crate::style::{Palette, Role};
use crate::width;

/// The server's prefill progress, as it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Prefill {
    pub total: u64,
    pub cache: u64,
    pub processed: u64,
    pub time_ms: u64,
}

impl Prefill {
    /// Fraction of the prompt resident, 0.0 to 1.0.
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.processed.min(self.total) as f64) / (self.total as f64)
    }

    /// Fraction of the prompt that cost nothing. The headline number for this
    /// project: it is `f_keep` observed live rather than reconstructed after the
    /// turn.
    pub fn cached_fraction(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.cache.min(self.total) as f64) / (self.total as f64)
    }

    /// Tokens actually computed this turn.
    pub fn computed(&self) -> u64 {
        self.processed.saturating_sub(self.cache)
    }

    /// Prefill throughput in tokens per second, or `None` before there is
    /// enough to divide by.
    ///
    /// Measured over `computed()`, not `processed`: dividing the cache hit by
    /// the wall clock produces a number in the hundreds of thousands and it is
    /// not a speed, it is an artefact.
    pub fn rate(&self) -> Option<f64> {
        if self.time_ms < 50 || self.computed() == 0 {
            return None;
        }
        Some(self.computed() as f64 * 1000.0 / self.time_ms as f64)
    }

    /// Estimated milliseconds to the end of the prefill.
    ///
    /// `None` when there is no rate yet. The estimate assumes the remaining
    /// tokens all have to be computed, which they do — everything past
    /// `processed` is by definition not in the cache.
    pub fn eta_ms(&self) -> Option<u64> {
        let r = self.rate()?;
        let left = self.total.saturating_sub(self.processed);
        if left == 0 {
            return Some(0);
        }
        Some((left as f64 * 1000.0 / r) as u64)
    }
}

/// Eighth-of-a-cell fill, from empty to full. Index is the number of eighths.
///
/// From grok-build's `progress_bar.rs`. The reason to have them at all: a
/// 20-column bar over a 40,000-token prompt advances one cell per 2,000 tokens,
/// so at whole-cell resolution a bar that is genuinely moving looks frozen for
/// seconds at a time — which is the exact impression a progress display exists
/// to prevent.
const EIGHTHS: [&str; 9] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

/// Draw a three-segment bar `cols` columns wide.
///
/// ```text
/// ▐████████▓▓▓▓▍░░░░░▌
///  ^cached  ^computed ^remaining
/// ```
///
/// The cached run is drawn differently from the computed run on purpose. A
/// two-colour bar answers "how far along"; this one also answers "how much of
/// this did the prefix cache save me", which is the question §10 says the whole
/// prompt pipeline is optimising for. In a terminal with no colour the three
/// runs are still three different glyphs, so the information survives a pipe to
/// a file.
pub fn bar(p: &Prefill, cols: usize, palette: Palette) -> String {
    let cols = cols.max(4);
    let inner = cols.saturating_sub(2);
    if p.total == 0 || inner == 0 {
        return format!("▐{}▌", "░".repeat(inner));
    }
    let frac = |n: u64| (n.min(p.total) as f64) / (p.total as f64);
    // The moving edge, in eighths of a cell.
    let done_e = (frac(p.processed) * (inner * 8) as f64).round() as usize;
    let done_e = done_e.min(inner * 8);
    // The cache boundary, in whole cells, never past the moving edge.
    let cached = ((frac(p.cache) * inner as f64).floor() as usize).min(done_e / 8);

    let full = done_e / 8 - cached;
    let rem = done_e % 8;
    let partial = usize::from(rem > 0);
    let rest = inner - cached - full - partial;

    let mut s = String::with_capacity(cols * 8 + 48);
    s.push('▐');
    if cached > 0 {
        s.push_str(&palette.paint(Role::Success, &"█".repeat(cached)));
    }
    if full > 0 {
        // A different *glyph*, not just a different colour: `Palette::None` is
        // the replay and CI case and the cache split has to survive it.
        s.push_str(&palette.paint(Role::Pending, &"▓".repeat(full)));
    }
    if partial == 1 {
        s.push_str(&palette.paint(Role::Pending, EIGHTHS[rem]));
    }
    if rest > 0 {
        s.push_str(&palette.paint(Role::Faint, &"░".repeat(rest)));
    }
    s.push('▌');
    s
}

/// The whole prefill line: bar, percentage, counts, rate, estimate.
///
/// Degrades by dropping the *least* useful field first as the terminal narrows:
/// the estimate, then the rate, then the raw counts, then the bar — leaving
/// `prefill 61%`, which is still true. It never wraps: a status line that wraps
/// scrolls the transcript by a row every frame, and that reads as flicker.
pub fn prefill_line(p: &Prefill, cols: usize, palette: Palette) -> String {
    let pct = (p.fraction() * 100.0).round() as u64;
    let cachepct = (p.cached_fraction() * 100.0).round() as u64;
    let head = format!("prefill {pct}%");
    let counts = format!(
        "{}/{} tok · {} cached ({cachepct}%)",
        thousands(p.processed),
        thousands(p.total),
        thousands(p.cache),
    );
    let rate = p
        .rate()
        .map(|r| format!("{} tok/s", thousands(r as u64)))
        .unwrap_or_default();
    let eta = p
        .eta_ms()
        .filter(|_| p.processed < p.total)
        .map(|ms| format!("~{} left", duration(ms)))
        .unwrap_or_default();

    // Widest form first, then progressively less.
    let barw = 20usize.min(cols / 3);
    let candidates = [
        format!(
            "{head} {} {counts} · {rate} · {eta}",
            bar(p, barw, palette)
        ),
        format!("{head} {} {counts} · {rate}", bar(p, barw, palette)),
        format!("{head} {} {counts}", bar(p, barw, palette)),
        format!("{head} {counts}"),
        head.clone(),
    ];
    for c in candidates {
        let c = c.replace(" ·  · ", " · ").replace(" · \u{0}", "");
        let c = c.trim_end_matches(" · ").to_string();
        if width::width(&c) <= cols {
            return c;
        }
    }
    width::truncate(&head, cols)
}

/// The decode-phase counterpart: how fast tokens are coming out.
///
/// Kept beside the prefill because the two phases are one wait to the person
/// doing the waiting, and a head that shows a bar and then nothing has just told
/// them the work stopped.
pub fn decode_line(predicted: u64, elapsed_ms: u64, cols: usize, palette: Palette) -> String {
    let rate = if elapsed_ms > 0 {
        predicted as f64 * 1000.0 / elapsed_ms as f64
    } else {
        0.0
    };
    let s = format!(
        "{} {} tok · {:.1} tok/s · {}",
        palette.paint(Role::Pending, "generating"),
        thousands(predicted),
        rate,
        duration(elapsed_ms)
    );
    width::truncate(&s, cols)
}

/// A spinner frame chosen from elapsed time rather than from a counter.
///
/// Driving a spinner from a frame counter makes it a liveness indicator for
/// *the render loop*, which is always alive. Driving it from the clock makes it
/// a liveness indicator for the clock, which is also always alive. Neither is
/// evidence the turn is progressing — so this returns a frame, and the caller is
/// expected to stop calling it when the turn stops, rather than to read the
/// spinner as proof of anything.
pub fn spinner(elapsed_ms: u64) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[((elapsed_ms / 80) % FRAMES.len() as u64) as usize]
}

/// `1234567` becomes `1.23M`, `12345` becomes `12.3k`.
///
/// Not a thousands separator: a status line has no room for one, and `40.1k` is
/// read faster than `40,132` when the digits past the first three are noise.
pub fn thousands(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.2}M", n as f64 / 1_000_000.0),
    }
}

/// A duration a person can read at a glance.
pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let s = ms / 1000;
    if s < 60 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let (m, s) = (s / 60, s % 60);
    if m < 60 {
        return format!("{m}m{s:02}s");
    }
    format!("{}h{:02}m", m / 60, m % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mostly_cached_prompt_reads_as_nearly_done_not_nearly_undone() {
        // The lie this module exists to avoid. 18,000 of 20,000 tokens came from
        // the cache and 1,000 more have been computed: that is 95% resident.
        let p = Prefill {
            total: 20_000,
            cache: 18_000,
            processed: 19_000,
            time_ms: 400,
        };
        assert_eq!((p.fraction() * 100.0).round() as u64, 95);
        assert_eq!(p.computed(), 1_000);
        assert_eq!((p.cached_fraction() * 100.0).round() as u64, 90);
    }

    #[test]
    fn the_rate_is_computed_over_work_done_not_over_the_cache_hit() {
        let p = Prefill {
            total: 20_000,
            cache: 18_000,
            processed: 19_000,
            time_ms: 500,
        };
        // 1000 tokens in half a second.
        assert_eq!(p.rate().unwrap().round() as u64, 2000);
        // Reading `processed` instead would claim 38,000 tok/s.
        assert!(p.rate().unwrap() < 5000.0);
    }

    #[test]
    fn there_is_no_rate_before_there_is_evidence_for_one() {
        assert!(
            Prefill {
                total: 100,
                cache: 100,
                processed: 100,
                time_ms: 0
            }
            .rate()
            .is_none()
        );
        assert!(
            Prefill {
                total: 100,
                cache: 0,
                processed: 4,
                time_ms: 10
            }
            .rate()
            .is_none()
        );
    }

    #[test]
    fn the_bar_shows_three_segments_and_the_cache_is_one_of_them() {
        let p = Prefill {
            total: 100,
            cache: 50,
            processed: 75,
            time_ms: 100,
        };
        let b = bar(&p, 22, Palette::None);
        assert_eq!(width::width(&b), 22);
        assert_eq!(b.matches('█').count(), 10, "cached run");
        assert_eq!(b.matches('▓').count(), 5, "computed run");
        assert_eq!(b.matches('░').count(), 5, "remaining run");
    }

    #[test]
    fn the_moving_edge_has_sub_cell_resolution() {
        // A 20-cell bar over a 40k prompt moves one cell per 2k tokens. At
        // whole-cell resolution a bar that is genuinely advancing looks frozen.
        let mk = |processed| Prefill {
            total: 40_000,
            cache: 0,
            processed,
            time_ms: 1_000,
        };
        let a = bar(&mk(10_000), 22, Palette::None);
        let b = bar(&mk(10_300), 22, Palette::None);
        assert_ne!(a, b, "300 tokens of a 40k prompt must move the bar");
        assert_eq!(width::width(&a), 22);
        assert_eq!(width::width(&b), 22);
    }

    #[test]
    fn the_bar_is_exactly_the_requested_width_at_every_fraction() {
        for cache in [0u64, 1, 37, 99, 100] {
            for processed in cache..=100 {
                for w in [6usize, 10, 21, 40] {
                    let p = Prefill {
                        total: 100,
                        cache,
                        processed,
                        time_ms: 100,
                    };
                    let b = bar(&p, w, Palette::Colour);
                    assert_eq!(width::width(&b), w, "cache {cache} processed {processed} w {w}");
                }
            }
        }
    }

    #[test]
    fn the_status_line_never_exceeds_its_width() {
        let p = Prefill {
            total: 41_233,
            cache: 38_100,
            processed: 39_900,
            time_ms: 1_240,
        };
        for w in [12usize, 20, 30, 50, 80, 120, 200] {
            let l = prefill_line(&p, w, Palette::Colour);
            assert!(width::width(&l) <= w, "{w}: {} cols {l:?}", width::width(&l));
            assert!(l.contains("prefill"), "{w}: {l:?}");
        }
    }

    #[test]
    fn a_narrow_terminal_drops_the_estimate_before_the_percentage() {
        let p = Prefill {
            total: 41_233,
            cache: 3_100,
            processed: 20_000,
            time_ms: 5_000,
        };
        let wide = prefill_line(&p, 120, Palette::None);
        let narrow = prefill_line(&p, 24, Palette::None);
        assert!(wide.contains("left"), "{wide:?}");
        assert!(!narrow.contains("left"), "{narrow:?}");
        assert!(narrow.contains('%'), "{narrow:?}");
    }

    #[test]
    fn compact_numbers_and_durations_read_at_a_glance() {
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(41_233), "41.2k");
        assert_eq!(thousands(1_234_567), "1.23M");
        assert_eq!(duration(340), "340ms");
        assert_eq!(duration(3_400), "3.4s");
        assert_eq!(duration(95_000), "1m35s");
        assert_eq!(duration(3_700_000), "1h01m");
    }
}
