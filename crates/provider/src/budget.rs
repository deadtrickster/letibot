//! How many pictures one request may carry, and how many bytes of them: a cap for a door that
//! refuses a request too rich in pictures, and the sentence that keeps what was dropped legible.
//! Off by default everywhere, because no provider in this tree documents such a limit.

/// The most pictures, and the most decoded picture bytes, that one request may carry. `None` is no
/// cap at all, and that is every preset's: a cap here is the operator's word (`[provider]
/// max_images`), never a number this build guessed from one refusal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Budget {
    pub images: Option<usize>,
    pub bytes: Option<u64>,
}

impl Budget {
    /// No cap.
    pub const NONE: Budget = Budget {
        images: None,
        bytes: None,
    };

    /// Whether this budget can drop anything at all.
    pub fn is_none(&self) -> bool {
        self.images.is_none() && self.bytes.is_none()
    }

    /// The same budget for ONE more try after a provider's generic parameter error, which names no
    /// field and so says only "something here". Half of what the refused request actually carried,
    /// because that is the one number the refusal is about: a count that halves to none is the
    /// smallest ask that can still be answered, and a request carrying no picture at all.
    pub fn half_of(&self, carried: usize) -> Budget {
        Budget {
            images: Some(carried / 2),
            bytes: self.bytes,
        }
    }

    /// Which of a request's pictures it KEEPS, in the order they arrive: the newest that fit both
    /// caps. A picture whose size cannot be read counts as nothing against the byte cap - the fate
    /// for those bytes is [`crate::messages::NOT_A_PICTURE`]'s, not the budget's.
    pub fn keeps(&self, sizes: &[Option<u64>]) -> Vec<bool> {
        let mut keep = vec![false; sizes.len()];
        if self.is_none() {
            keep.fill(true);
            return keep;
        }
        let (mut pictures, mut bytes) = (0usize, 0u64);
        for i in (0..sizes.len()).rev() {
            // A count cap is reached once and for all: everything older is out.
            if self.images.is_some_and(|max| pictures >= max) {
                break;
            }
            let size = sizes[i].unwrap_or(0);
            // A byte cap does not stop at the first picture too big for it: a smaller, older one
            // may fit, and what a model can still see is worth more than a strict newest-first run.
            if self.bytes.is_some_and(|max| bytes + size > max) {
                continue;
            }
            bytes += size;
            pictures += 1;
            keep[i] = true;
        }
        keep
    }

    /// The reason a dropped picture's placeholder carries: what this request's budget is, and how
    /// much of the conversation it kept. Deterministic, like every other reason a picture is not
    /// sent, because these words are prompt prefix.
    pub fn said(&self, kept: usize, total: usize) -> String {
        format!(
            "only the newest {kept} of {total} pictures fit this request ({})",
            self.why()
        )
    }

    /// The budget in the words the operator wrote it in.
    fn why(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = self.images {
            parts.push(format!("at most {n} pictures"));
        }
        if let Some(b) = self.bytes {
            parts.push(format!("at most {} of picture data", size_said(b)));
        }
        if parts.is_empty() {
            return "no cap".to_string();
        }
        parts.join(" and ")
    }
}

/// A byte count a person reads: `8 MiB`, `512 KiB`. The unit is binary, as the parse is.
fn size_said(bytes: u64) -> String {
    const K: u64 = 1024;
    if bytes >= K * K {
        format!("{:.1} MiB", bytes as f64 / (K * K) as f64)
    } else if bytes >= K {
        format!("{} KiB", bytes / K)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(images: usize, bytes: u64) -> Budget {
        Budget {
            images: Some(images),
            bytes: Some(bytes),
        }
    }

    /// No cap keeps everything, which is every door's default.
    #[test]
    fn no_budget_keeps_every_picture() {
        assert!(Budget::NONE.is_none());
        assert_eq!(
            Budget::NONE.keeps(&[Some(1), Some(2), None]),
            vec![true, true, true]
        );
    }

    /// The cap keeps the NEWEST pictures: the conversation's tail is what the model is looking at.
    #[test]
    fn the_newest_pictures_are_the_ones_kept() {
        let keep = budget(2, 1_000).keeps(&[Some(10), Some(10), Some(10)]);
        assert_eq!(keep, vec![false, true, true]);
    }

    /// A byte cap is checked newest-first and skips one too big for it rather than stopping.
    #[test]
    fn a_byte_cap_skips_a_picture_too_big_for_it_and_keeps_a_smaller_one() {
        // 100 bytes of room: the newest is 90, the middle 50, the oldest 10.
        let keep = Budget {
            images: None,
            bytes: Some(100),
        }
        .keeps(&[Some(10), Some(50), Some(90)]);
        assert_eq!(keep, vec![true, false, true]);
    }

    /// A budget that fits still keeps everything, so a cap never drops a picture for being old.
    #[test]
    fn a_budget_that_fits_drops_nothing() {
        assert_eq!(
            budget(3, 1_000).keeps(&[Some(10), Some(10), Some(10)]),
            vec![true, true, true]
        );
    }

    /// The retry halves the count it actually carried, and a count of one halves to none.
    #[test]
    fn half_is_half_of_what_the_request_carried() {
        assert_eq!(budget(40, 500).half_of(51).images, Some(25));
        assert_eq!(budget(40, 500).half_of(51).bytes, Some(500));
        assert_eq!(Budget::NONE.half_of(1).images, Some(0));
        assert_eq!(Budget::NONE.half_of(60).images, Some(30));
    }

    /// The sentence names the cap and what was kept, because a model told only "not sent" cannot
    /// tell a picture that was dropped from one that never arrived.
    #[test]
    fn the_sentence_names_the_cap_and_what_was_kept() {
        let said = budget(40, 8 * 1024 * 1024).said(40, 61);
        assert_eq!(
            said,
            "only the newest 40 of 61 pictures fit this request (at most 40 pictures and at most \
             8.0 MiB of picture data)"
        );
        assert_eq!(
            budget(2, 0).said(2, 9),
            "only the newest 2 of 9 pictures fit this request (at most 2 pictures and at most 0 \
             bytes of picture data)"
        );
    }
}
