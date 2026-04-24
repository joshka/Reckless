//! Pre-move pruning gates.
//!
//! These checks can return or search a reduced tactical subset before normal
//! move generation. Their order is part of search behavior, so the full-width
//! driver keeps the sequence visible and calls these helpers only for the gate
//! predicates.

use crate::{
    board::Board,
    movepick::Stage,
    types::{PieceType, is_decisive, is_loss, is_valid, is_win},
};

use super::tt::TtProbe;

/// Razoring gate before normal move generation.
///
/// This is a shallow, non-PV shortcut for positions whose estimated score is
/// far below alpha. It is kept as a predicate so the driver still shows that
/// qsearch is the tactical fallback searched before returning.
#[inline]
pub(super) fn can_razor(pv: bool, in_check: bool, estimated_score: i32, alpha: i32, depth: i32, tt: TtProbe) -> bool {
    !pv && !in_check
        && estimated_score < alpha - 295 - 261 * depth * depth
        && alpha < 2048
        && !tt.mv.is_quiet()
        && tt.bound != crate::transposition::Bound::Lower
}

/// Reverse futility pruning result for quiet non-PV nodes.
///
/// The score is fail-soft: when static information is far above beta, the node
/// returns a damped score instead of proving moves. TT-PV, checks, exclusions,
/// and decisive windows opt out because those cases need exact search shape.
#[inline]
pub(super) fn reverse_futility_score(
    tt_pv: bool, in_check: bool, excluded: bool, estimated_score: i32, beta: i32, depth: i32, improving: bool,
    correction: i32, own_threats_empty: bool,
) -> Option<i32> {
    if tt_pv || in_check || excluded || is_loss(beta) || is_win(estimated_score) {
        return None;
    }

    let margin = (1165 * depth * depth / 128 - (80 * improving as i32) + 25 * depth + 560 * correction.abs() / 1024
        - 59 * own_threats_empty as i32
        + 30)
        .max(0);

    (estimated_score >= beta + margin).then_some(beta + (estimated_score - beta) / 3)
}

/// Null-move pruning eligibility.
///
/// Null move is a cut-node proof attempt, so it is blocked in checks, excluded
/// singular-verification nodes, potential singular nodes, low material, and
/// tactical TT-capture situations where zugzwang or a capture race can make the
/// shortcut misleading.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn can_try_null_move(
    board: &Board, cut_node: bool, in_check: bool, excluded: bool, potential_singularity: bool, estimated_score: i32,
    beta: i32, depth: i32, tt_pv: bool, improvement: i32, child_cutoff_count: i32, ply: isize, nmp_min_ply: i32,
    tt: TtProbe,
) -> bool {
    cut_node
        && !in_check
        && !excluded
        && !potential_singularity
        && estimated_score
            >= beta
                + (-8 * depth + 116 * tt_pv as i32 - 106 * improvement / 1024 - 20 * (child_cutoff_count < 2) as i32
                    + 304)
                    .max(0)
        && ply as i32 >= nmp_min_ply
        && board.material() > 600
        && !is_loss(beta)
        && !(tt.bound == crate::transposition::Bound::Lower
            && tt.mv.is_capture()
            && board.piece_on(tt.mv.to()).value() >= PieceType::Knight.value())
}

/// ProbCut eligibility before the main move loop.
///
/// ProbCut searches promising captures against a raised beta. The TT and eval
/// guards avoid paying that tactical search unless there is already evidence
/// that a capture can plausibly exceed the raised threshold.
#[inline]
pub(super) fn can_try_probcut(cut_node: bool, beta: i32, tt: TtProbe, probcut_beta: i32, eval: i32) -> bool {
    cut_node
        && !is_win(beta)
        && if is_valid(tt.score) { tt.score >= probcut_beta && !is_decisive(tt.score) } else { eval >= beta }
        && !tt.mv.is_quiet()
}

/// Late move pruning gate inside the move loop.
///
/// Once enough quiet moves have failed, a later non-checking quiet can be
/// skipped. The history and improvement terms keep this as an ordering-aware
/// pruning rule rather than a raw move-count cutoff.
#[inline]
pub(super) fn late_move_prunes(
    in_check: bool, gives_direct_check: bool, is_quiet: bool, move_count: i32, improvement: i32, depth: i32,
    history: i32,
) -> bool {
    !in_check
        && !gives_direct_check
        && is_quiet
        && move_count >= (3006 + 70 * improvement / 16 + 1455 * depth * depth + 68 * history / 1024) / 1024
}

/// Quiet futility pruning score inside the move loop.
///
/// Returning a value instead of `bool` preserves the fail-soft update to
/// `best_score`; the caller still owns the decision to skip remaining quiets.
#[inline]
pub(super) fn futility_prune_score(
    in_check: bool, gives_direct_check: bool, is_quiet: bool, eval: i32, beta: i32, depth: i32, history: i32,
    alpha: i32,
) -> Option<i32> {
    let futility_value = eval + 79 * depth + 64 * history / 1024 + 84 * (eval >= beta) as i32 - 115;

    (!in_check && is_quiet && depth < 15 && futility_value <= alpha && !gives_direct_check).then_some(futility_value)
}

/// Bad-noisy futility pruning score.
///
/// This only applies after move ordering has reached the bad-noisy stage. A
/// failure here stops the noisy tail instead of just skipping one move because
/// the remaining captures are ordered as even less promising.
#[inline]
pub(super) fn bad_noisy_futility_score(
    in_check: bool, gives_direct_check: bool, stage: Stage, eval: i32, depth: i32, history: i32, alpha: i32,
) -> Option<i32> {
    let noisy_futility_value = eval + 71 * depth + 68 * history / 1024 + 23;

    (!in_check && depth < 11 && stage == Stage::BadNoisy && noisy_futility_value <= alpha && !gives_direct_check)
        .then_some(noisy_futility_value)
}

/// SEE pruning threshold for the current move class.
///
/// The threshold is intentionally separate from the SEE call so the driver
/// keeps the tactical legality check visible while the tuned quiet/noisy
/// formulas stay named.
#[inline]
pub(super) fn see_threshold(is_quiet: bool, depth: i32, history: i32) -> i32 {
    if is_quiet {
        (-17 * depth * depth + 52 * depth - 21 * history / 1024 + 20).min(0)
    } else {
        (-8 * depth * depth - 36 * depth - 32 * history / 1024 + 11).min(0)
    }
}
