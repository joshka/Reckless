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

/// Propagate TT-PV status from a failed interior node.
///
/// Upper-bound nodes with enough searched moves can inherit the parent PV mark.
/// This happens after the move loop because the final bound and move count are
/// needed, and before TT writeback because the TT entry stores the marker.
#[inline]
pub(super) fn propagate_tt_pv(tt_pv: bool, node_root: bool, bound: Bound, move_count: i32, parent_tt_pv: bool) -> bool {
    tt_pv || !node_root && bound == Bound::Upper && move_count > 2 && parent_tt_pv
}

/// Damp non-decisive fail-soft beta cutoffs before storing the result.
///
/// The raw cutoff can be overly optimistic. Blending it toward beta gives TT
/// and history consumers a more conservative score while leaving decisive
/// scores and root scores untouched.
#[inline]
pub(super) fn scale_beta_cutoff_score(best_score: i32, node_root: bool, beta: i32, alpha: i32, depth: i32) -> i32 {
    if node_root || best_score < beta || is_decisive(best_score) || is_decisive(alpha) {
        return best_score;
    }

    let weight = depth.min(8);
    (best_score * weight + beta) / (weight + 1)
}

/// Whether this node owns a final TT write.
///
/// Excluded singular-verification nodes and secondary root MultiPV slots avoid
/// writing final entries because their search window or move set is not the
/// ordinary position contract.
#[inline]
pub(super) fn should_write_tt(excluded: bool, node_root: bool, pv_index: usize) -> bool {
    !(excluded || node_root && pv_index > 0)
}

/// Whether the final score is a trustworthy correction-history target.
///
/// Correction history learns from quiet positions where the final score
/// contradicts static eval in the useful direction. Checks, captures, and bound
/// results that do not improve on eval are excluded.
#[inline]
pub(super) fn should_update_correction_history(
    in_check: bool, best_move: Move, bound: Bound, best_score: i32, eval: i32,
) -> bool {
    !(in_check
        || best_move.is_noisy()
        || (bound == Bound::Upper && best_score >= eval)
        || (bound == Bound::Lower && best_score <= eval))
}
