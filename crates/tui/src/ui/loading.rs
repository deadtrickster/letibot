//! **Waiting, drawn**: the cat while a session arrives, and the lines for a transcript still
//! filling or being compacted.

use crate::app::*;
use crate::ui::render::RenderConfig;
use letibot_ui::painter::Sgr;
use letibot_ui::progress;
use rano::style::Role;
use rano::width::text as width;

/// One row, centred horizontally in `w` columns.
///
/// Escape-aware, because the thing being centred may already be painted: `width::width`
/// skips ANSI sequences, so a coloured cat and a plain one land in the same column.
/// A plain `chars().count()` is what puts a painted string two columns left of centre,
/// and the whole point of centring is that the eye finds it in the same place from
/// frame to frame as the string changes length — which the walking cat does.
pub(crate) fn centred_row(cfg: &RenderConfig, text: &str, w: usize) -> String {
    let pad = cfg.palette().painted(Role::Faint, &text.to_string());
    let taken = width::width(&pad);
    if taken >= w {
        return pad;
    }
    let left = (w - taken) / 2;
    format!("{}{pad}", " ".repeat(left))
}

/// **How long a bulk announcement may stay unfilled before the head says it is not
/// coming**, in milliseconds.
///
/// **Measured against the right case, which is why it is seconds and not minutes.** The
/// trigger is a snapshot's bulk announcement ([`Bulk`]), so the R2 slow case — a prompt
/// queued behind a running turn, which can honestly sit body-less for minutes — never
/// sets it. What remains is a carry, and a carry's bodies are published by the daemon in
/// the same loop as its announcements (`Harness::republish`, `Harness::import_opencode`),
/// so the window is one delivery batch rather than a generation.
///
/// Measured on this box's store: rows per turn over 957 turns are median 17, p90 130,
/// p99 290, max 591, and the round cadence — how long a *live* row's body has ever really
/// taken — runs to p99 108 s at the worst. The old 120 s value was calibrated against
/// exactly those, and that was the error: with the trigger fixed they are *excluded*, and
/// waiting two minutes to report a body the operator can see is missing now is the same
/// failure as never reporting it. 5 s clears any delivery lag many times over and fires
/// while the operator is still watching. (The store has no notion of a body-less row, so
/// a carry's own body gap cannot be read off it; leticl measured the live case at 31 ms
/// and chose 5 s for the same reason — the number is bounded by the mechanism, not copied
/// from a clock that measures a different thing.)
pub(crate) const BODY_PATIENCE: u64 = 5_000;

/// **The smallest fill the head draws a bar for**, in the operation's own units.
///
/// A **screen** decision — the daemon stays a pure reporter (it names every operation and
/// counts it; the head decides whether the count is worth a bar with a cat on it).
/// Measured at both ends from this box's own store, because a threshold picked by taste is
/// one the next person re-tunes on their first flash:
///
/// * **An ordinary turn's rows**: 957 turns — median 17, p90 130, p99 290, **max 591**.
/// * **A real carry**: 42 forks — smallest **448 rows**, largest 576,374.
///
/// **The two overlap** (591 > 448), so size cannot separate a turn from a carry — which is
/// precisely why the trigger is the shape of the evidence ([`Bulk`]) and not this number.
/// All this decides is whether an operation the daemon *did* name is worth a bar: 256 sits
/// above the ordinary turn's p90 and below the smallest carry ever seen here. **It gates
/// the bar only** — the sentence below it is not gated, because a three-row batch does not
/// deserve a cat but a three-row batch that never lands is exactly what the sentence is
/// for.
pub(crate) const MIN_FILLING: u64 = 256;

/// **One line for a fill the DAEMON named**: the cat, the bar, and the count.
///
/// The numbers are the daemon's — `what` in its own words, and `done` of `total` in the
/// `unit` it named — so this draws the fact rather than a rendering of it. The head used
/// to draw this line from the rows still lacking a body, which meant inferring the
/// *operation* from the *symptom*: four things produce body-less rows (an ordinary
/// reply, a reseat, a compaction, an import) and only some are a carry, so the line said
/// *"carrying the conversation onto the new prompt"* over every ordinary message. Only
/// the layer doing the operation knows which one it is; that is `SessionEvent::Filling`,
/// and this is the one renderer for all of them.
///
/// A free function for the same reason `filling_line` is: by the time the tail is
/// assembled, `screen` has already borrowed `self` mutably.
pub(crate) fn filling_line(
    what: &str,
    unit: &str,
    done: u64,
    total: u64,
    now_ms: u64,
    cfg: &RenderConfig,
) -> Vec<String> {
    // `cache == processed == done`, the same three-valued bar the prefill draws, so the
    // landed span reads green rather than as an expense.
    let p = progress::Prefill {
        total,
        cache: done,
        processed: done,
        time_ms: 0,
    };
    let total_s = progress::thousands(total);
    let counts = format!(
        "{:>w$} of {total_s} {unit}",
        progress::thousands(done),
        w = total_s.chars().count()
    );
    let used = CAT_SLOT + counts.chars().count() + 6;
    let bar_cols = cfg.width.saturating_sub(used).clamp(8, 40);
    vec![
        String::new(),
        format!(
            "  {} {}  {}",
            progress::bar(&p, bar_cols, cfg.palette()),
            cfg.palette().painted(Role::Faint, &counts),
            cfg.palette().painted(
                Role::Faint,
                &format!("{cat:<CAT_SLOT$}", cat = cat_frame(now_ms))
            )
        ),
        cfg.palette().painted(Role::Faint, &format!("  {what}")),
    ]
}

/// **A fold, in the compaction's own units** — the renderer for
/// [`SessionEvent::CompactionProgress`](letibot_sessionlog::SessionEvent::CompactionProgress).
///
/// A free function for `filling_line`'s reason: by the time the tail is assembled,
/// `screen` has already borrowed `self` mutably.
///
/// **Two phases, because a fold has two and they are minutes apart.** While the server
/// is still reading the half's prompt it draws the prefill bar the ordinary turn draws —
/// the same three-valued bar, so a reader who has watched one recognises this one — and
/// the count is *read*. Once it starts writing, the count is *written* and the bar is
/// gone: there is no total to draw a fraction of, which is why that number is a count and
/// not a percentage. A bar that invented a total would be an indicator that is not the
/// fact.
pub(crate) fn compacting_line(c: &CompactionLine, now_ms: u64, cfg: &RenderConfig) -> Vec<String> {
    let p = cfg.palette();
    let where_ = format!("half {} of {}", c.half, c.halves);
    // Reading, and there is something to show progress against.
    if c.processed > 0 && c.processed < c.prompt_tokens {
        let pre = progress::Prefill {
            total: c.prompt_tokens,
            // Nothing of a scratch prompt is cached: it is a slice of history under a
            // prefix the server may hold, but the slice itself has never been sent.
            cache: 0,
            processed: c.processed,
            time_ms: 0,
        };
        let total_s = progress::thousands(c.prompt_tokens);
        let counts = format!(
            "{:>w$} of {total_s} tokens read",
            progress::thousands(c.processed),
            w = total_s.chars().count()
        );
        let used = CAT_SLOT + counts.chars().count() + 6;
        let bar_cols = cfg.width.saturating_sub(used).clamp(8, 40);
        return vec![
            String::new(),
            format!(
                "  {} {}  {}",
                progress::bar(&pre, bar_cols, p),
                p.painted(Role::Faint, &counts),
                p.painted(
                    Role::Faint,
                    &format!("{cat:<CAT_SLOT$}", cat = cat_frame(now_ms))
                )
            ),
            p.painted(
                Role::Faint,
                &format!(
                    "  compacting {where_} — nothing shows on the \
                 transcript until it lands, and the conversation is kept either way"
                ),
            ),
        ];
    }
    // Writing: a count and no fraction. `unit` is the daemon's word for what it counts.
    vec![
        String::new(),
        format!(
            "  {} {}",
            p.painted(
                Role::Faint,
                &format!("{cat:<CAT_SLOT$}", cat = cat_frame(now_ms))
            ),
            p.painted(
                Role::Faint,
                &format!(
                    "compacting {where_} — {} {} written so far",
                    progress::thousands(c.written),
                    c.unit
                )
            )
        ),
    ]
}

/// The frame of the walking cat, from **elapsed milliseconds**.
///
/// Same rule as `letibot_ui::progress::spinner`, and for the same reason: a frame
/// chosen from a counter is a fact about the render loop rather than about the wait,
/// and a counter here would also break the property this whole path exists for — the
/// `screen()` call has to be the same for the same elapsed time, or the frame is not
/// a function of the clock and the pre-attach draw cannot be driven at all.
/// **Every frame is the same width, and the face is in the same columns in all of
/// them.** Only what is *meant* to change changes.
///
/// The first set was not: `(=^.^=)` is seven columns and `(=^.-.=)` is eight,
/// with a `~` on half of them, so the widths ran 7, 8, 7, 8, 8, 9, 8, 9. Padding
/// each frame to the widest fixed the block's LEFT edge, which is where the
/// previous attempt stopped — but the face inside the block still grew and shrank
/// by a column, so the thing the eye actually tracks kept moving. The operator,
/// having watched it: *"why dont you fix cats center during animation"*.
///
/// So the layout is fixed by construction rather than by padding afterwards:
///
/// ```text
///   col  0 1 2 3 4 5 6 7
///        ( = ^ X ^ = )  T
///              ^        ^
///              |        the tail, ~ or blank
///              the expression, the one glyph that carries the animation
/// ```
///
/// The expression sits at column 3, which is the face's own centre, so it changes
/// **in place**; the ears and cheeks never move; the tail flicks in a column of
/// its own past the face. `cat_frames_are_one_width_with_the_face_in_one_place`
/// asserts all of that, because it is the kind of thing a later edit breaks by
/// adding one nice-looking frame.
///
/// ASCII only, deliberately: `CAT_SLOT` measures with `str::len` and the centring
/// arithmetic is in columns, and those are the same number only while every glyph
/// is one byte and one column.
pub(crate) const CAT_FRAMES: [&str; 8] = [
    // A cat blinking, with its tail flicking behind it.
    "(=^.^=) ", "(=^-^=) ", "(=^o^=) ", "(=^-^=) ", "(=^.^=)~", "(=^-^=)~", "(=^o^=)~", "(=^-^=)~",
];

/// When the waiting frame starts naming the way out, in milliseconds.
///
/// Under it the cat is a cat and the wait is usually over in a few hundred
/// milliseconds; over it something is wrong, and the operator should be told the escape
/// hatch exists rather than having to discover it. The wait loop in `letibot-tui`'s
/// `main` is what makes the keys live — the first version of that screen did not read
/// them at all, so the hint bar under it named a key that did nothing.
pub(crate) const ATTACH_IMPATIENT: u64 = 2_000;

/// The width of the **slot** the cat walks in: the widest frame.
///
/// Every frame is now that width — see [`CAT_FRAMES`], where a constant width is
/// the point and not a coincidence — so the padding this feeds is a no-op and is
/// kept as the guard rather than the fix. Measured rather than written down, so a
/// frame added in a hurry widens the slot instead of silently overflowing the
/// arithmetic that assumes it.
pub(crate) const CAT_SLOT: usize = {
    let mut w = 0;
    let mut i = 0;
    while i < CAT_FRAMES.len() {
        let n = CAT_FRAMES[i].len();
        if n > w {
            w = n;
        }
        i += 1;
    }
    w
};

/// The cat's frame at `elapsed_ms`.
pub(crate) fn cat_frame(elapsed_ms: u64) -> &'static str {
    CAT_FRAMES[((elapsed_ms / 120) % CAT_FRAMES.len() as u64) as usize]
}
