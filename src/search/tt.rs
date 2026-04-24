//! Search-facing transposition-table policy.
//!
//! TT storage, replacement, and mate-score normalization live in `transposition`. This module owns
//! how search interprets a probed entry: as a cutoff proof, move-ordering hint, eval bound, PV
//! marker, or singularity signal.
//!
//! Bound checks are not interchangeable. Callers should choose helpers that name the role the TT
//! score is playing. This module does not own TT storage or replacement; it owns the search
//! interpretation of an already-probed entry.

use crate::{
    thread::ThreadData,
    transposition::{Bound, Entry},
    types::{Color, Move, Score, is_decisive, is_valid},
};

use super::history::update_continuation_histories;

/// Search view of a transposition-table lookup.
///
/// A TT hit has several roles in search: cutoff proof, move-ordering hint, eval bound, PV marker,
/// and singular-extension evidence. Keeping those values together makes later phase dependencies
/// explicit without changing TT storage.
#[derive(Copy, Clone)]
pub struct TtProbe {
    /// Raw entry from TT storage, if present.
    ///
    /// Keeping the entry lets eval recover `raw_eval` while the other fields expose search-friendly
    /// defaults for miss cases.
    pub entry: Option<Entry>,

    /// Stored search depth for full-width cutoff and singular checks.
    ///
    /// Qsearch intentionally ignores this; full-width search requires enough depth before trusting
    /// the bound as a proof.
    pub depth: i32,

    /// Stored best move, used for move ordering and singular verification.
    ///
    /// `Move::NULL` means there is no useful move from this probe.
    pub mv: Move,

    /// Stored score after mate-distance normalization for this ply.
    ///
    /// The score is only meaningful when `is_valid(score)` and must be interpreted together with
    /// `bound`.
    pub score: i32,

    /// Bound type proving how `score` relates to the true node value.
    ///
    /// Search may only use the score in directions allowed by this bound.
    pub bound: Bound,

    /// Whether this node should be treated as part of the TT principal variation.
    ///
    /// The marker can come from either the caller's node kind or the TT entry, and later affects
    /// pruning, reductions, and final TT writeback.
    pub tt_pv: bool,
}

impl TtProbe {
    /// Read a TT entry and convert absence into neutral search defaults.
    ///
    /// The probe starts with the caller's PV status because PV-ness can be inherited from the
    /// current node even when the table has no entry. A hit may then widen that status through the
    /// entry's `tt_pv` marker.
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

    /// Whether the TT read found a stored entry.
    #[inline]
    pub const fn has_entry(self) -> bool {
        self.entry.is_some()
    }

    /// Raw NNUE eval stored with the TT entry, or `Score::NONE` on a miss.
    #[inline]
    pub const fn raw_eval(self) -> i32 {
        match self.entry {
            Some(entry) => entry.raw_eval,
            None => Score::NONE,
        }
    }

    /// Whether this TT bound is strong enough to skip full-width search.
    ///
    /// This is deliberately stricter than qsearch cutoff logic. It depends on node kind,
    /// excluded-move verification, stored depth, and cut-node shape; changing those guards changes
    /// both strength and node counts.
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

    /// Whether the TT score can replace corrected static eval for pruning.
    ///
    /// The bound may only move the estimate in the direction it actually proves. This keeps TT
    /// information useful for pruning without pretending an upper bound is an exact eval.
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

    /// Whether a TT score can stand in for eval at an in-check node.
    ///
    /// In-check nodes have no static stand-pat value, so only non-decisive scores whose bound is
    /// compatible with the current window are usable.
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

    /// Whether a shallow TT qsearch hit proves the current qsearch window.
    ///
    /// Qsearch has no depth requirement here, but PV qsearch still rejects decisive TT scores so
    /// mate-distance-sensitive PV reporting stays sane.
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

    /// Whether qsearch should use the TT score as its best stand-pat score.
    ///
    /// Like full-width eval adjustment, the TT bound may only improve the current best score in the
    /// direction its bound proves.
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

/// Full-width node context needed to treat a TT hit as a cutoff proof.
///
/// TT storage does not know about node kind, singular-exclusion searches, or the current alpha-beta
/// window. This value names the search-side facts that make a stored bound safe to use as an
/// immediate full-width return.
#[derive(Copy, Clone)]
pub struct FullWidthCutoffInput {
    /// Current ply, used for stack-history feedback.
    pub ply: isize,

    /// Side to move, used when rewarding a quiet TT cutoff move.
    pub stm: Color,

    /// TT probe being interpreted as a possible proof.
    pub probe: TtProbe,

    /// Whether the current node is a PV node.
    pub node_pv: bool,

    /// Whether this node excludes one move for singular verification.
    pub excluded: bool,

    /// Remaining full-width depth.
    pub depth: i32,

    /// Current alpha lower bound.
    pub alpha: i32,

    /// Current beta upper bound.
    pub beta: i32,

    /// Whether the node has cut-node shape.
    pub cut_node: bool,
}

/// Try the full-width TT cutoff and apply the small cutoff-history bonus.
///
/// This is the TT phase's only mutation before eval. The returned score is blocked near the
/// fifty-move limit because stored mate/tablebase-like values can be unsafe when the halfmove clock
/// has moved since the entry was stored.
#[inline(always)]
pub fn try_full_width_cutoff(td: &mut ThreadData, input: FullWidthCutoffInput) -> Option<i32> {
    let FullWidthCutoffInput {
        ply,
        stm,
        probe,
        node_pv,
        excluded,
        depth,
        alpha,
        beta,
        cut_node,
    } = input;

    if !probe.has_entry() || !probe.can_cutoff_full_width(node_pv, excluded, depth, alpha, beta, cut_node) {
        return None;
    }

    if probe.mv.is_quiet() && probe.score >= beta && td.stack[ply - 1].move_count < 4 {
        let quiet_bonus = (175 * depth - 79).min(1637);
        let cont_bonus = (114 * depth - 57).min(1284);

        td.quiet_history.update(td.board.all_threats(), stm, probe.mv, quiet_bonus);
        update_continuation_histories(td, ply, td.board.moved_piece(probe.mv), probe.mv.to(), cont_bonus);
    }

    (td.board.halfmove_clock() < 90).then_some(probe.score)
}
