//! The token ledger (§4.3).
//!
//! ```text
//! TokenLedger = [ (item_id, tok_offset, tok_len, h_k) ]
//!    h_k  = H(h_{k-1} || tokens(item_k)),   h_{-1} = H(tokens(stable_prefix))
//! ```
//!
//! # The design constraint, stated as an API rule
//!
//! §4.3's first clause is that *"there is no rewrite operation to call"* -- a
//! prefix violation must be **blocked**, not merely detected. Three
//! mechanisms carry that here, at three different depths, and they are listed in
//! order of how hard they are to defeat:
//!
//! 1. **The kernel.** The memfd is sealed `F_SEAL_SHRINK`
//!    (`crate::region`), so the file cannot be shortened at all.
//! 2. **The store.** `transcript_item` carries `BEFORE UPDATE` and
//!    `BEFORE DELETE` triggers that `RAISE(ABORT)`, and an insert trigger that
//!    refuses any row whose `seq` or `tok_offset` does not continue the previous
//!    one (`crate::store`). SQL cannot rewrite history either.
//! 3. **This type.** [`TokenLedger`] exposes exactly one mutation, `append`.
//!    There is no `truncate`, `pop`, `remove`, `splice`, `drain`, `set`,
//!    `resize` or `as_mut_slice`, no `IndexMut`, and `tokens()` hands out
//!    `&[TokenId]`. Its public surface is pinned by the
//!    `no_rewrite_operation_exists` test below, which reads this file and
//!    `region.rs` and fails if a public method appears that is not on an
//!    explicit allowlist. Adding one is then a deliberate, reviewable act rather
//!    than a diff nobody notices -- which is the whole point of clause 1.
//!
//! # Fork, not truncate (§5.5)
//!
//! The operation people actually want when they reach for `truncate` is
//! [`TokenLedger::fork`], and it is a different thing: it builds a **new**
//! ledger over a **new** region with a **new** id, leaving this one untouched.
//! That is the correct shape because a truncation *is* a cache divergence, and
//! the design says a divergence must be visible as one rather than smuggled
//! through the same object. A fork of the same ledger twice produces two
//! ledgers; nothing anywhere gets shorter.
//!
//! # The span that goes on the wire
//!
//! [`PromptSpan`] has no settable offset. `offset()` returns `0`, always,
//! because request *N+1* is `(region, 0, len_{N+1})` over the same memory that
//! request *N* read as `(region, 0, len_N)`. A caller cannot ask for a window
//! that starts elsewhere, so it cannot ask for a window that is not a prefix.

use sha2::{Digest, Sha256};

use crate::region::TokenRegion;
use crate::vocab::TokenId;

/// One row: which item, where its tokens are, and the chain head after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerRow {
    pub item_id: String,
    pub tok_offset: u32,
    pub tok_len: u32,
    pub h_k: [u8; 32],
}

impl LedgerRow {
    pub fn end(&self) -> u32 {
        self.tok_offset + self.tok_len
    }
}

/// What a request addresses: the whole region, from the start, to here.
///
/// Deliberately has no public constructor and no offset field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptSpan {
    len: usize,
    head: [u8; 32],
}

impl PromptSpan {
    /// Always zero. A request is a prefix of the region or it is not a request.
    pub fn offset(&self) -> usize {
        0
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The chain head at `len`. This is what `prefix_handle` (§3.10-B) asserts:
    /// two requests whose heads differ at a shared index are two conversations,
    /// and the daemon says so before submitting.
    pub fn head(&self) -> &[u8; 32] {
        &self.head
    }

    /// True when `self` is a prefix extension of `earlier`: same start, no
    /// shrink. The type makes the start trivially equal; this is here so the
    /// property can be asserted rather than assumed.
    pub fn extends(&self, earlier: &PromptSpan) -> bool {
        self.offset() == earlier.offset() && self.len >= earlier.len
    }
}

#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    /// A replayed row's recomputed `h_k` is not the one that was persisted.
    /// Either the store was edited, or a renderer changed under us.
    ChainMismatch {
        index: usize,
        item_id: String,
        expected: [u8; 32],
        recomputed: [u8; 32],
    },
    /// A replayed row does not start where the previous one ended.
    NotContiguous {
        index: usize,
        item_id: String,
        expected_offset: u32,
        found_offset: u32,
    },
    /// Rows and their token blobs disagree about length.
    LengthMismatch {
        index: usize,
        item_id: String,
        row_len: u32,
        tokens_len: usize,
    },
    /// `fork` was asked to keep more rows than exist.
    ForkPastEnd {
        keep: usize,
        rows: usize,
    },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Io(e) => write!(f, "token region: {e}"),
            LedgerError::ChainMismatch {
                index,
                item_id,
                expected,
                recomputed,
            } => write!(
                f,
                "hash chain broken at row {index} ({item_id}): persisted {}, recomputed {}",
                hex(expected),
                hex(recomputed)
            ),
            LedgerError::NotContiguous {
                index,
                item_id,
                expected_offset,
                found_offset,
            } => write!(
                f,
                "row {index} ({item_id}) starts at {found_offset}, should start at {expected_offset}"
            ),
            LedgerError::LengthMismatch {
                index,
                item_id,
                row_len,
                tokens_len,
            } => write!(
                f,
                "row {index} ({item_id}) says {row_len} tokens, blob holds {tokens_len}"
            ),
            LedgerError::ForkPastEnd { keep, rows } => {
                write!(f, "cannot fork keeping {keep} of {rows} rows")
            }
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e)
    }
}

pub fn hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `H(tokens)` -- the seed of the chain, over the stable prefix.
///
/// Tokens are hashed as little-endian `u32`, explicitly and not via a
/// `bytemuck`-style cast, so the digest is the same on any machine that ever
/// reads this store back.
pub fn hash_tokens(tokens: &[TokenId]) -> [u8; 32] {
    let mut h = Sha256::new();
    for t in tokens {
        h.update(t.to_le_bytes());
    }
    h.finalize().into()
}

/// `h_k = H(h_{k-1} || tokens(item_k))`.
///
/// The previous head is the *only* input from the past (§4.3 clause 3: the
/// hasher is fed forward only), so the chain cannot be recomputed differently by
/// feeding it history in another order.
pub fn chain(previous: &[u8; 32], tokens: &[TokenId]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(previous);
    for t in tokens {
        h.update(t.to_le_bytes());
    }
    h.finalize().into()
}

/// One transcript's tokens and the chain over them.
pub struct TokenLedger {
    region: TokenRegion,
    rows: Vec<LedgerRow>,
    h_init: [u8; 32],
    prefix_len: u32,
}

impl TokenLedger {
    /// Open a ledger over a stable prefix.
    ///
    /// `name` becomes part of the memfd's name; pass the transcript id.
    pub fn new(name: &str, prefix_tokens: &[TokenId]) -> Result<Self, LedgerError> {
        let h_init = hash_tokens(prefix_tokens);
        let mut region = TokenRegion::create(name)?;
        region.append(prefix_tokens, &h_init)?;
        Ok(TokenLedger {
            region,
            rows: Vec::new(),
            h_init,
            prefix_len: prefix_tokens.len() as u32,
        })
    }

    /// The one mutation.
    ///
    /// A zero-length append is legal: a `SegmentMark` renders to nothing, takes
    /// a row, and still advances the chain -- `H(h_{k-1} || <>)` is well defined
    /// and is not `h_{k-1}`, so the mark is part of the identity of everything
    /// after it.
    pub fn append(&mut self, item_id: &str, tokens: &[TokenId]) -> Result<&LedgerRow, LedgerError> {
        let h_prev = self.head();
        let h_k = chain(&h_prev, tokens);
        let tok_offset = self.region.len() as u32;
        self.region.append(tokens, &h_k)?;
        self.rows.push(LedgerRow {
            item_id: item_id.to_string(),
            tok_offset,
            tok_len: tokens.len() as u32,
            h_k,
        });
        Ok(self.rows.last().expect("just pushed"))
    }

    /// `h_{-1}` -- the hash of the stable prefix alone.
    pub fn h_init(&self) -> [u8; 32] {
        self.h_init
    }

    /// The current chain head: `h_{-1}` when no item has been appended.
    pub fn head(&self) -> [u8; 32] {
        self.rows.last().map_or(self.h_init, |r| r.h_k)
    }

    /// The request to submit: the whole region, from zero, to here.
    pub fn span(&self) -> PromptSpan {
        PromptSpan {
            len: self.region.len(),
            head: self.head(),
        }
    }

    pub fn rows(&self) -> &[LedgerRow] {
        &self.rows
    }

    /// Every token, prefix first. Read-only, by value of the return type.
    pub fn tokens(&self) -> &[TokenId] {
        self.region.as_slice()
    }

    /// The stable prefix's tokens.
    pub fn prefix_tokens(&self) -> &[TokenId] {
        &self.region.as_slice()[..self.prefix_len as usize]
    }

    pub fn prefix_len(&self) -> u32 {
        self.prefix_len
    }

    /// The tokens of one item.
    pub fn item_tokens(&self, index: usize) -> Option<&[TokenId]> {
        let row = self.rows.get(index)?;
        Some(&self.region.as_slice()[row.tok_offset as usize..row.end() as usize])
    }

    pub fn len(&self) -> usize {
        self.region.len()
    }

    pub fn is_empty(&self) -> bool {
        self.region.is_empty()
    }

    /// The read-only fd to hand the server.
    pub fn readonly_fd(&self) -> Result<std::os::fd::OwnedFd, LedgerError> {
        Ok(self.region.readonly_fd()?)
    }

    pub fn region(&self) -> &TokenRegion {
        &self.region
    }

    /// The full re-hash of §4.3 clause 5.
    ///
    /// Run it on every request in debug and every 25th in production. It is what
    /// turns "the chain agrees with itself" into "the chain agrees with the
    /// tokens", and a failure here points at the renderer, which is where §7.2's
    /// CI diff already points.
    pub fn verify_chain(&self) -> Result<(), LedgerError> {
        let tokens = self.region.as_slice();
        let recomputed_init = hash_tokens(&tokens[..self.prefix_len as usize]);
        if recomputed_init != self.h_init {
            return Err(LedgerError::ChainMismatch {
                index: 0,
                item_id: "<stable_prefix>".into(),
                expected: self.h_init,
                recomputed: recomputed_init,
            });
        }
        let mut h = self.h_init;
        let mut cursor = self.prefix_len;
        for (i, row) in self.rows.iter().enumerate() {
            if row.tok_offset != cursor {
                return Err(LedgerError::NotContiguous {
                    index: i,
                    item_id: row.item_id.clone(),
                    expected_offset: cursor,
                    found_offset: row.tok_offset,
                });
            }
            h = chain(&h, &tokens[row.tok_offset as usize..row.end() as usize]);
            if h != row.h_k {
                return Err(LedgerError::ChainMismatch {
                    index: i,
                    item_id: row.item_id.clone(),
                    expected: row.h_k,
                    recomputed: h,
                });
            }
            cursor = row.end();
        }
        Ok(())
    }

    /// Rebuild a ledger from what was persisted (§4.3 clause 6).
    ///
    /// Replays the rows' spans into a fresh region and re-verifies every link as
    /// it goes. It never re-renders: re-rendering during recovery would
    /// reintroduce the renderer non-determinism the chain exists to catch, at
    /// the one moment nobody is watching.
    pub fn restore(
        name: &str,
        prefix_tokens: &[TokenId],
        h_init: [u8; 32],
        rows: &[(LedgerRow, Vec<TokenId>)],
    ) -> Result<Self, LedgerError> {
        let recomputed = hash_tokens(prefix_tokens);
        if recomputed != h_init {
            return Err(LedgerError::ChainMismatch {
                index: 0,
                item_id: "<stable_prefix>".into(),
                expected: h_init,
                recomputed,
            });
        }
        let mut region = TokenRegion::create(name)?;
        region.append(prefix_tokens, &h_init)?;

        let mut out_rows = Vec::with_capacity(rows.len());
        let mut h = h_init;
        let mut cursor = prefix_tokens.len() as u32;
        for (i, (row, tokens)) in rows.iter().enumerate() {
            if row.tok_len as usize != tokens.len() {
                return Err(LedgerError::LengthMismatch {
                    index: i,
                    item_id: row.item_id.clone(),
                    row_len: row.tok_len,
                    tokens_len: tokens.len(),
                });
            }
            if row.tok_offset != cursor {
                return Err(LedgerError::NotContiguous {
                    index: i,
                    item_id: row.item_id.clone(),
                    expected_offset: cursor,
                    found_offset: row.tok_offset,
                });
            }
            h = chain(&h, tokens);
            if h != row.h_k {
                return Err(LedgerError::ChainMismatch {
                    index: i,
                    item_id: row.item_id.clone(),
                    expected: row.h_k,
                    recomputed: h,
                });
            }
            region.append(tokens, &h)?;
            cursor = row.end();
            out_rows.push(row.clone());
        }

        Ok(TokenLedger {
            region,
            rows: out_rows,
            h_init,
            prefix_len: prefix_tokens.len() as u32,
        })
    }

    /// Fork at a row boundary: a **new** ledger, over a **new** region.
    ///
    /// This is what §5.5 means by "fork, not truncate". `self` is untouched --
    /// note the `&self` -- and the caller gets a second object it must give a
    /// new transcript id, so the divergence has to be recorded to be used.
    pub fn fork(&self, name: &str, keep_rows: usize) -> Result<TokenLedger, LedgerError> {
        if keep_rows > self.rows.len() {
            return Err(LedgerError::ForkPastEnd {
                keep: keep_rows,
                rows: self.rows.len(),
            });
        }
        let tokens = self.region.as_slice();
        let mut forked = TokenLedger::new(name, &tokens[..self.prefix_len as usize])?;
        for row in &self.rows[..keep_rows] {
            forked.append(
                &row.item_id,
                &tokens[row.tok_offset as usize..row.end() as usize],
            )?;
        }
        Ok(forked)
    }
}

impl std::fmt::Debug for TokenLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenLedger")
            .field("rows", &self.rows.len())
            .field("tokens", &self.region.len())
            .field("head", &hex(&self.head()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> TokenLedger {
        TokenLedger::new("t-test", &[1, 2, 3, 4, 5]).unwrap()
    }

    #[test]
    fn h_init_is_the_hash_of_the_stable_prefix() {
        let l = ledger();
        assert_eq!(l.h_init(), hash_tokens(&[1, 2, 3, 4, 5]));
        assert_eq!(l.head(), l.h_init(), "an empty ledger's head is h_-1");
        assert_eq!(l.tokens(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn the_chain_is_the_formula_in_the_plan() {
        let mut l = ledger();
        let h0 = l.h_init();
        l.append("i0", &[10, 11]).unwrap();
        assert_eq!(l.head(), chain(&h0, &[10, 11]));
        let h1 = l.head();
        l.append("i1", &[12]).unwrap();
        assert_eq!(l.head(), chain(&h1, &[12]));
        l.verify_chain().unwrap();
    }

    #[test]
    fn a_zero_length_item_still_advances_the_chain() {
        // A SegmentMark renders to nothing. It must still be part of the identity
        // of everything after it, or two transcripts that differ only in their
        // segment structure would share a cache entry.
        let mut l = ledger();
        let before = l.head();
        let row = l.append("segment-open", &[]).unwrap().clone();
        assert_eq!(row.tok_len, 0);
        assert_ne!(l.head(), before, "H(h || <>) is not h");
        assert_eq!(l.len(), 5, "and it contributed no tokens");
        l.verify_chain().unwrap();
    }

    #[test]
    fn every_append_is_a_strict_prefix_extension() {
        // The round trip the strand exists to guarantee: after any sequence of
        // appends, every snapshot ever taken is still a prefix of the live
        // region, byte for byte, and the span never moves or shrinks.
        let mut l = ledger();
        let mut snapshots: Vec<(PromptSpan, Vec<TokenId>)> = vec![(l.span(), l.tokens().to_vec())];

        for k in 0..64u32 {
            let payload: Vec<TokenId> = (0..(k % 7)).map(|j| 1000 + k * 10 + j).collect();
            l.append(&format!("item-{k}"), &payload).unwrap();

            let now = l.span();
            let live = l.tokens();
            for (earlier, bytes) in &snapshots {
                assert!(now.extends(earlier), "a span must never shrink or move");
                assert_eq!(earlier.offset(), 0);
                assert_eq!(
                    &live[..bytes.len()],
                    bytes.as_slice(),
                    "tokens a previous request read must still be there, unchanged"
                );
            }
            snapshots.push((now, live.to_vec()));
        }
        assert_eq!(l.rows().len(), 64);
        l.verify_chain().unwrap();
    }

    #[test]
    fn growth_does_not_disturb_what_was_already_written() {
        // Crosses the 16 Ki-token initial capacity several times.
        let mut l = TokenLedger::new("t-grow", &[7; 3]).unwrap();
        let mut expected: Vec<TokenId> = vec![7, 7, 7];
        for k in 0..300u32 {
            let payload: Vec<TokenId> = (0..200).map(|j| k * 1000 + j).collect();
            l.append(&format!("i{k}"), &payload).unwrap();
            expected.extend_from_slice(&payload);
            assert_eq!(l.tokens(), expected.as_slice());
        }
        assert!(
            l.len() > 16 * 1024,
            "must have grown past the initial capacity"
        );
        assert!(l.region().capacity() >= l.len());
        l.verify_chain().unwrap();
    }

    #[test]
    fn rows_are_contiguous_and_start_after_the_prefix() {
        let mut l = ledger();
        for k in 0..10u32 {
            l.append(&format!("i{k}"), &[k, k, k]).unwrap();
        }
        let mut cursor = l.prefix_len();
        for row in l.rows() {
            assert_eq!(row.tok_offset, cursor);
            cursor = row.end();
        }
        assert_eq!(cursor as usize, l.len());
        for (i, row) in l.rows().iter().enumerate() {
            let k = i as u32;
            assert_eq!(l.item_tokens(i).unwrap(), &[k, k, k]);
            assert_eq!(row.item_id, format!("i{i}"));
        }
    }

    #[test]
    fn restore_replays_the_spans_and_reverifies_the_chain() {
        let mut l = ledger();
        for k in 0..20u32 {
            l.append(&format!("i{k}"), &[k * 3, k * 3 + 1]).unwrap();
        }
        let persisted: Vec<(LedgerRow, Vec<TokenId>)> = l
            .rows()
            .iter()
            .enumerate()
            .map(|(i, r)| (r.clone(), l.item_tokens(i).unwrap().to_vec()))
            .collect();

        let restored =
            TokenLedger::restore("t-restored", l.prefix_tokens(), l.h_init(), &persisted).unwrap();
        assert_eq!(restored.tokens(), l.tokens());
        assert_eq!(restored.head(), l.head());
        assert_eq!(restored.rows(), l.rows());
        restored.verify_chain().unwrap();
    }

    #[test]
    fn restore_refuses_a_store_that_was_edited() {
        let mut l = ledger();
        l.append("i0", &[1, 1]).unwrap();
        l.append("i1", &[2, 2]).unwrap();
        let mut persisted: Vec<(LedgerRow, Vec<TokenId>)> = l
            .rows()
            .iter()
            .enumerate()
            .map(|(i, r)| (r.clone(), l.item_tokens(i).unwrap().to_vec()))
            .collect();

        // Somebody rewrote history in the database.
        persisted[0].1 = vec![9, 9];
        let err = TokenLedger::restore("t-bad", l.prefix_tokens(), l.h_init(), &persisted)
            .expect_err("a changed token must break the chain");
        assert!(
            matches!(err, LedgerError::ChainMismatch { index: 0, .. }),
            "{err}"
        );

        // And a hole where a row used to be.
        let mut gapped: Vec<(LedgerRow, Vec<TokenId>)> = l
            .rows()
            .iter()
            .enumerate()
            .map(|(i, r)| (r.clone(), l.item_tokens(i).unwrap().to_vec()))
            .collect();
        gapped.remove(0);
        let err = TokenLedger::restore("t-gap", l.prefix_tokens(), l.h_init(), &gapped)
            .expect_err("a removed row must not replay");
        assert!(
            matches!(err, LedgerError::NotContiguous { index: 0, .. }),
            "{err}"
        );
    }

    #[test]
    fn a_fork_is_a_new_ledger_and_leaves_the_original_alone() {
        let mut l = ledger();
        for k in 0..8u32 {
            l.append(&format!("i{k}"), &[k, k]).unwrap();
        }
        let before_len = l.len();
        let before_head = l.head();

        let forked = l.fork("t-fork", 3).unwrap();

        assert_eq!(l.len(), before_len, "fork must not touch the original");
        assert_eq!(l.head(), before_head);
        assert_eq!(l.rows().len(), 8);

        assert_eq!(forked.rows().len(), 3);
        assert_eq!(forked.h_init(), l.h_init());
        assert_eq!(
            forked.head(),
            l.rows()[2].h_k,
            "the chain up to the fork point is identical"
        );
        assert_eq!(forked.tokens(), &l.tokens()[..l.rows()[2].end() as usize]);
        assert_ne!(forked.region().as_fd(), l.region().as_fd(), "a new region");
        forked.verify_chain().unwrap();

        assert!(matches!(
            l.fork("t-x", 9),
            Err(LedgerError::ForkPastEnd { .. })
        ));
    }

    #[test]
    fn a_forked_ledger_diverges_visibly_rather_than_silently() {
        // The point of "fork, not truncate": once the two ledgers append
        // different things, their heads differ, which is what the daemon checks
        // before submitting. A truncate-in-place would have produced one object
        // whose head silently became the head of a different conversation.
        let mut l = ledger();
        for k in 0..4u32 {
            l.append(&format!("i{k}"), &[k]).unwrap();
        }
        let mut forked = l.fork("t-fork2", 2).unwrap();
        forked.append("other", &[99]).unwrap();
        assert_ne!(forked.head(), l.head());
        assert_eq!(forked.span().len(), 5 + 2 + 1);
        assert_eq!(l.span().len(), 5 + 4);
        // Neither is a prefix extension of the other past the fork point.
        assert!(!forked.span().extends(&l.span()));
    }

    #[test]
    fn a_prompt_span_cannot_be_told_to_start_elsewhere() {
        let mut l = ledger();
        l.append("i0", &[1]).unwrap();
        let a = l.span();
        l.append("i1", &[2]).unwrap();
        let b = l.span();
        assert_eq!(a.offset(), 0);
        assert_eq!(b.offset(), 0);
        assert!(b.extends(&a));
        assert!(!a.extends(&b));
        assert_ne!(a.head(), b.head());
    }

    #[test]
    fn the_region_publishes_length_and_head_as_a_consistent_pair() {
        let mut l = ledger();
        assert_eq!(l.region().read_published(), (5, l.h_init()));
        l.append("i0", &[1, 2, 3]).unwrap();
        let (len, head) = l.region().read_published();
        assert_eq!(len, 8);
        assert_eq!(head, l.head());
        assert_eq!(head, l.region().head_hash());
    }

    /// The test §4.3 clause 1 asks for: one that fails if somebody later adds a
    /// rewrite operation.
    ///
    /// It reads this crate's two append-only modules and pins their public
    /// method surfaces to an allowlist. A `truncate`, `pop`, `splice`, `drain`,
    /// `set_len`, `as_mut_slice` or `IndexMut` added to either one fails this
    /// test by name; anything else added fails it by not being on the list. The
    /// second half is the part that matters -- a rewrite dressed up as
    /// `rollback_to` or `rebase` is caught too, because the list is a list of
    /// what is allowed and not a list of what is forbidden.
    #[test]
    fn no_rewrite_operation_exists() {
        const LEDGER_ALLOWED: &[&str] = &[
            "end",
            "offset",
            "len",
            "is_empty",
            "head",
            "extends",
            "fmt",
            "from",
            "hex",
            "hash_tokens",
            "chain",
            "new",
            "append",
            "h_init",
            "span",
            "rows",
            "tokens",
            "prefix_tokens",
            "prefix_len",
            "item_tokens",
            "readonly_fd",
            "region",
            "verify_chain",
            "restore",
            "fork",
            "drop",
        ];
        const REGION_ALLOWED: &[&str] = &[
            "create",
            "len",
            "is_empty",
            "capacity",
            "as_slice",
            "append",
            "head_hash",
            "read_published",
            "readonly_fd",
            "as_fd",
            "drop",
            "fmt",
        ];

        for (file, source, allowed) in [
            ("ledger.rs", include_str!("ledger.rs"), LEDGER_ALLOWED),
            ("region.rs", include_str!("region.rs"), REGION_ALLOWED),
        ] {
            // Only the non-test half of the file: test helpers are not API.
            let api = source.split("\n#[cfg(test)]").next().unwrap();
            for line in api.lines() {
                let line = line.trim();
                let Some(rest) = line.strip_prefix("pub fn ") else {
                    continue;
                };
                let name = rest.split(['(', '<']).next().unwrap_or("").trim();
                assert!(
                    allowed.contains(&name),
                    "{file} grew a public method `{name}` that is not on the append-only \
                     allowlist. If it can shorten, reorder or overwrite what is already in \
                     the region, it must not exist: §4.3 clause 1 says a prefix violation is \
                     blocked, not merely detected. If it genuinely cannot, add it to \
                     the allowlist in this test on purpose."
                );
            }
            // The method-call forms, so that `libc::ftruncate` -- which only ever
            // grows this file, and which the F_SEAL_SHRINK seal makes unable to do
            // anything else -- is not mistaken for `Vec::truncate`.
            let code: String = api
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for forbidden in [
                ".truncate(",
                ".set_len(",
                ".as_mut_slice(",
                "IndexMut",
                ".splice(",
                ".drain(",
                ".retain(",
                ".swap_remove(",
                ".pop(",
                ".remove(",
                ".clear(",
                ".insert(",
            ] {
                assert!(
                    !code.contains(forbidden),
                    "{file} calls `{forbidden}` -- an operation that can shorten or \
                     reorder what is already in the region"
                );
            }
        }
    }
}
