//! **The quit card**: what leaving would stop, and the choices for it.

use crate::app::*;
use crate::ui::render::{sgr, trim_to, wrap};
use crate::ui::*;
use letibot_ui::painter::Sgr;
use rano::style::Role;

impl App {
    /// The mode card: the daemon's own mode names, in the ask card's slot at
    /// the bottom of the screen — the transcript stays visible above it, the
    /// way approvals sit, instead of the card taking the whole body the way
    /// the session picker does.
    ///
    /// The names are `SettingRow::choices` verbatim — the head keeps no list
    /// of its own, because a second copy of a list is a copy that drifts. A
    /// daemon that sent none gets one dim line saying so, and `/mode NAME`
    /// keeps working for an operator who knows the name anyway.
    ///
    /// The card's shape is load-bearing: the first line is the title and the
    /// second is the first choice, because the click arithmetic in `screen`
    /// counts on it. No blank between them.
    /// The quit card's rows: what Enter does, and the consequence of it.
    ///
    /// The consequence is on the row rather than in a footnote because it is
    /// the whole reason the card exists — one of these two is cheap and the
    /// other is not, and a card that made them look alike would be a card that
    /// answered for the operator.
    pub(crate) fn quit_choices(&self) -> [(&'static str, String); 2] {
        let others = self.heads.saturating_sub(1);
        [
            (
                "leave this head",
                "the daemon keeps running: the session stays warm and `letibot` \
                 reattaches to it"
                    .to_string(),
            ),
            ("leave and stop the daemon", {
                // **AND THE WORK THAT DIES WITH IT, SAID FIRST** — the operator's
                // ask, 2026-10-05: stopping the daemon stops the jobs and the
                // subagents with it, and a card that named only the cold prefill
                // made the cheap row and the killing row read alike. A running
                // job is `cargo test --release` an hour in; a running subagent is
                // a session mid-task. Neither survives the stop, and the
                // consequence is the one fact that decides the answer.
                //
                // Only the RUNNING count, on both: a settled job is history the
                // store keeps, and `opening` is a subagent that has not started —
                // nothing that dies. The first row needs no such clause: leaving
                // the head stops nothing (its own text says the daemon keeps
                // running).
                let running_jobs = self.jobs.iter().filter(|j| j.running).count();
                let running_subs = self
                    .subagents
                    .iter()
                    .filter(|s| s.state == "running")
                    .count();
                let mut dies = String::new();
                if running_jobs > 0 && running_subs > 0 {
                    dies = format!(
                        "{running_jobs} job{} and {running_subs} subagent{} are \
                             running and stop with the daemon. ",
                        if running_jobs == 1 { "" } else { "s" },
                        if running_subs == 1 { "" } else { "s" },
                    );
                } else if running_jobs > 0 {
                    dies = format!(
                        "{running_jobs} job{} running — {} stop with the daemon. ",
                        if running_jobs == 1 { "is" } else { "s are" },
                        if running_jobs == 1 {
                            "it stops"
                        } else {
                            "they stop"
                        },
                    );
                } else if running_subs > 0 {
                    dies = format!(
                        "{running_subs} subagent{} running — {} stop with the daemon. ",
                        if running_subs == 1 { "is" } else { "s are" },
                        if running_subs == 1 {
                            "it stops"
                        } else {
                            "they stop"
                        },
                    );
                }
                match others {
                    0 => format!(
                        "{dies}the session is written to disk and `letibot --continue` \
                             reopens it — but its prompt leaves the model server's cache, \
                             so the next turn prefills cold"
                    ),
                    1 => format!(
                        "{dies}one other head is attached and will be told. The session \
                             is on disk; the next turn after reopening prefills cold"
                    ),
                    n => format!(
                        "{dies}{n} other heads are attached and will be told. The session \
                             is on disk; the next turn after reopening prefills cold"
                    ),
                }
            }),
        ]
    }

    pub(crate) fn quit_card_lines(&self, w: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let mut out = vec![colour(
            &self.cfg,
            sgr::BOLD,
            "leave — and what happens to the daemon",
        )];
        for (i, (name, why)) in self.quit_choices().iter().enumerate() {
            let picked = i == self.quit_sel.min(1);
            let mark = if picked { "▸" } else { " " };
            out.push(trim_to(
                &format!(
                    "{mark} {:>2}  {}",
                    i + 1,
                    p.painted(if picked { Role::Strong } else { Role::Plain }, name)
                ),
                w,
            ));
            for l in wrap(why, w.saturating_sub(8)) {
                out.push(dim(&self.cfg, &format!("       {l}")));
            }
        }
        out
    }
}
