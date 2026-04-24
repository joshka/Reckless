//! Full-width node finalization rules.
//!
//! Once the move loop has produced a bound and best move, search still has a
//! few result-shaping steps: propagate PV information, damp non-decisive beta
//! cutoffs, decide whether the TT owns this result, and decide whether static
//! eval correction should learn from it.

use crate::{
    transposition::Bound,
    types::{Move, is_decisive},
};

#[inline]
pub(super) fn propagate_tt_pv(tt_pv: bool, node_root: bool, bound: Bound, move_count: i32, parent_tt_pv: bool) -> bool {
    tt_pv || !node_root && bound == Bound::Upper && move_count > 2 && parent_tt_pv
}

#[inline]
pub(super) fn scale_beta_cutoff_score(best_score: i32, node_root: bool, beta: i32, alpha: i32, depth: i32) -> i32 {
    if node_root || best_score < beta || is_decisive(best_score) || is_decisive(alpha) {
        return best_score;
    }

    let weight = depth.min(8);
    (best_score * weight + beta) / (weight + 1)
}

#[inline]
pub(super) fn should_write_tt(excluded: bool, node_root: bool, pv_index: usize) -> bool {
    !(excluded || node_root && pv_index > 0)
}

#[inline]
pub(super) fn should_update_correction_history(
    in_check: bool, best_move: Move, bound: Bound, best_score: i32, eval: i32,
) -> bool {
    !(in_check
        || best_move.is_noisy()
        || (bound == Bound::Upper && best_score >= eval)
        || (bound == Bound::Lower && best_score <= eval))
}
