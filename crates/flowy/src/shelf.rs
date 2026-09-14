//! The fabric's shelf: rows of `kind=skill`, as a [`SkillShelf`] for the `skill`
//! tool.
//!
//! What this replaces, measured on lab2x1 on 2026-09-14: 76 hand-built curls to
//! `/api/artifacts` and `/api/artifact/{id}` in one seat's transcripts, half of
//! them for `kind=skill`, and the guessed filters that answer nothing —
//! `type=skill`, `/api/search?type=skill`. A skill somebody wrote on another box
//! is loaded by id or title through the same `skill` tool a disk skill is, and
//! `skill list` shows both, labelled.
//!
//! Reads go through the seat's node with the seat's token, so the shelf is what
//! this seat may read — the node's permission filter, not a claim. A node that
//! is away is reported as unreadable, never as an empty shelf.

use letibot_tools::builtins::skill::{ShelfEntry, Skill, SkillShelf};

use crate::seat::Seat;

pub struct FabricShelf {
    seat: Seat,
}

impl FabricShelf {
    pub fn new(seat: Seat) -> FabricShelf {
        FabricShelf { seat }
    }
}

impl SkillShelf for FabricShelf {
    fn describe(&self) -> String {
        format!(
            "kind=skill rows on {}, as `{}`",
            self.seat.credentials().addr,
            self.seat.name()
        )
    }

    fn list(&self) -> Result<Vec<ShelfEntry>, String> {
        let rows = self
            .seat
            .node_artifacts_of_kind("skill", 200)
            .map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .map(|a| ShelfEntry {
                id: a.id,
                title: a.title,
                visibility: if a.visibility.is_empty() {
                    "?".into()
                } else {
                    a.visibility
                },
            })
            .collect())
    }

    fn load(&self, key: &str) -> Result<Skill, String> {
        let key = key.trim();
        // A ULID is 26 characters of Crockford base32; anything else is a title.
        let looks_like_id = key.len() == 26 && key.chars().all(|c| c.is_ascii_alphanumeric());
        let art = if looks_like_id {
            self.seat.node_artifact(key).map_err(|e| e.to_string())?
        } else {
            let rows = self
                .seat
                .node_artifacts_of_kind("skill", 200)
                .map_err(|e| e.to_string())?;
            let Some(hit) = rows.into_iter().find(|a| a.title.eq_ignore_ascii_case(key)) else {
                return Err(format!("nothing on the shelf is titled `{key}`"));
            };
            hit
        };
        if art.kind != "skill" {
            return Err(format!(
                "{} is kind=`{}`, not a skill; read it with the flowy tool if it is what you meant",
                art.id, art.kind
            ));
        }
        Ok(Skill::new(
            art.title,
            format!("{} · {} · updated {}", art.id, art.visibility, art.updated),
            art.body,
        ))
    }
}
