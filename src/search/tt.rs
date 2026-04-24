//! Search-facing transposition-table policy.
//!
//! TT storage, replacement, and mate-score normalization live in
//! `transposition`. This module owns how search interprets a probed entry: as a
//! cutoff proof, move-ordering hint, eval bound, PV marker, or singularity
//! signal.
//!
//! Bound checks are not interchangeable. Callers should choose helpers that
//! name the role the TT score is playing.

use crate::{transposition::Bound, types::is_valid};

#[inline]
pub(super) fn can_cutoff_full_width(
    pv: bool, excluded: bool, tt_depth: i32, depth: i32, tt_score: i32, tt_bound: Bound, alpha: i32, beta: i32,
    cut_node: bool,
) -> bool {
    !pv && !excluded
        && tt_depth > depth - (tt_score < beta) as i32
        && is_valid(tt_score)
        && match tt_bound {
            Bound::Upper => tt_score <= alpha && (!cut_node || depth > 5),
            Bound::Lower => tt_score >= beta && (cut_node || depth > 5),
            _ => true,
        }
}

#[inline]
pub(super) fn can_use_score_as_estimate(
    in_check: bool, excluded: bool, tt_score: i32, tt_bound: Bound, eval: i32,
) -> bool {
    !in_check
        && !excluded
        && is_valid(tt_score)
        && match tt_bound {
            Bound::Upper => tt_score < eval,
            Bound::Lower => tt_score > eval,
            _ => true,
        }
}

#[inline]
pub(super) fn can_use_score_as_in_check_eval(tt_score: i32, tt_bound: Bound, alpha: i32, beta: i32) -> bool {
    !crate::types::is_decisive(tt_score)
        && is_valid(tt_score)
        && match tt_bound {
            Bound::Upper => tt_score <= alpha,
            Bound::Lower => tt_score >= beta,
            _ => true,
        }
}

#[inline]
pub(super) fn can_cutoff_qsearch(pv: bool, tt_score: i32, tt_bound: Bound, alpha: i32, beta: i32) -> bool {
    is_valid(tt_score)
        && (!pv || !crate::types::is_decisive(tt_score))
        && match tt_bound {
            Bound::Upper => tt_score <= alpha,
            Bound::Lower => tt_score >= beta,
            _ => true,
        }
}

#[inline]
pub(super) fn can_use_qsearch_score(pv: bool, tt_score: i32, tt_bound: Bound, best_score: i32) -> bool {
    is_valid(tt_score)
        && (!pv || !crate::types::is_decisive(tt_score))
        && match tt_bound {
            Bound::Upper => tt_score < best_score,
            Bound::Lower => tt_score > best_score,
            _ => true,
        }
}
