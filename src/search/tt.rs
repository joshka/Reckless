//! Search-facing transposition-table policy.
//!
//! TT storage, replacement, and mate-score normalization live in
//! `transposition`. This module owns how search interprets a probed entry: as a
//! cutoff proof, move-ordering hint, eval bound, PV marker, or singularity
//! signal.
//!
//! Bound checks are not interchangeable. Callers should choose helpers that
//! name the role the TT score is playing.

use crate::{
    thread::ThreadData,
    transposition::{Bound, Entry},
    types::{Move, Score, is_decisive, is_valid},
};

/// Search view of a transposition-table lookup.
///
/// A TT hit has several roles in search: cutoff proof, move-ordering hint, eval
/// bound, PV marker, and singular-extension evidence. Keeping those values
/// together makes later phase dependencies explicit without changing TT
/// storage.
#[derive(Copy, Clone)]
pub(super) struct TtProbe {
    pub entry: Option<Entry>,
    pub depth: i32,
    pub mv: Move,
    pub score: i32,
    pub bound: Bound,
    pub tt_pv: bool,
}

impl TtProbe {
    #[inline]
    pub fn read(td: &ThreadData, hash: u64, ply: isize, pv: bool) -> Self {
        let entry = td.shared.tt.read(hash, td.board.halfmove_clock(), ply);
        let mut probe = Self {
            entry,
            depth: 0,
            mv: Move::NULL,
            score: Score::NONE,
            bound: Bound::None,
            tt_pv: pv,
        };

        if let Some(entry) = probe.entry {
            probe.depth = entry.depth;
            probe.mv = entry.mv;
            probe.score = entry.score;
            probe.bound = entry.bound;
            probe.tt_pv |= entry.tt_pv;
        }

        probe
    }

    #[inline]
    pub const fn has_entry(self) -> bool {
        self.entry.is_some()
    }

    #[inline]
    pub const fn raw_eval(self) -> i32 {
        match self.entry {
            Some(entry) => entry.raw_eval,
            None => Score::NONE,
        }
    }

    #[inline]
    pub fn can_cutoff_full_width(
        self, pv: bool, excluded: bool, depth: i32, alpha: i32, beta: i32, cut_node: bool,
    ) -> bool {
        !pv && !excluded
            && self.depth > depth - (self.score < beta) as i32
            && is_valid(self.score)
            && match self.bound {
                Bound::Upper => self.score <= alpha && (!cut_node || depth > 5),
                Bound::Lower => self.score >= beta && (cut_node || depth > 5),
                _ => true,
            }
    }

    #[inline]
    pub fn can_use_score_as_estimate(self, in_check: bool, excluded: bool, eval: i32) -> bool {
        !in_check
            && !excluded
            && is_valid(self.score)
            && match self.bound {
                Bound::Upper => self.score < eval,
                Bound::Lower => self.score > eval,
                _ => true,
            }
    }

    #[inline]
    pub fn can_use_score_as_in_check_eval(self, alpha: i32, beta: i32) -> bool {
        !is_decisive(self.score)
            && is_valid(self.score)
            && match self.bound {
                Bound::Upper => self.score <= alpha,
                Bound::Lower => self.score >= beta,
                _ => true,
            }
    }

    #[inline]
    pub fn can_cutoff_qsearch(self, pv: bool, alpha: i32, beta: i32) -> bool {
        is_valid(self.score)
            && (!pv || !is_decisive(self.score))
            && match self.bound {
                Bound::Upper => self.score <= alpha,
                Bound::Lower => self.score >= beta,
                _ => true,
            }
    }

    #[inline]
    pub fn can_use_qsearch_score(self, pv: bool, best_score: i32) -> bool {
        is_valid(self.score)
            && (!pv || !is_decisive(self.score))
            && match self.bound {
                Bound::Upper => self.score < best_score,
                Bound::Lower => self.score > best_score,
                _ => true,
            }
    }
}
