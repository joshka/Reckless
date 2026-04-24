//! Search-side static-eval state and correction policy.
//!
//! Static eval in search is NNUE plus correction-history bias plus selected TT
//! bound information. This module owns the eval state used by pruning,
//! reductions, and final correction updates.

use crate::{
    evaluation::correct_eval,
    thread::ThreadData,
    transposition::{Bound, TtDepth},
    types::{Color, Move, Score, is_valid},
};

use super::tt::TtProbe;

/// Static eval view used by the full-width node after TT/tablebase probing.
///
/// Search eval is not just NNUE. The full-width node needs the raw NNUE value
/// for TT storage, the correction-adjusted value for history feedback, a
/// TT-adjusted estimate for pruning, and improvement signals for pruning and
/// reductions. Keeping those together makes later consumers name which eval
/// role they mean.
#[derive(Copy, Clone)]
pub(super) struct EvalState {
    pub raw: i32,
    pub corrected: i32,
    pub estimated: i32,
    pub correction: i32,
    pub improvement: i32,
    pub improving: bool,
}

impl EvalState {
    /// Build the eval state in the order required by search.
    ///
    /// Excluded singular-verification nodes reuse the stack eval instead of
    /// refreshing NNUE, and fresh NNUE evals write a raw-eval-only TT entry
    /// before pruning can return. TT bounds may adjust the estimate after the
    /// corrected eval exists, but only when the bound direction is compatible.
    #[inline]
    pub fn compute(
        td: &mut ThreadData, hash: u64, ply: isize, in_check: bool, excluded: bool, tt_probe: TtProbe, tt_pv: bool,
        alpha: i32, beta: i32,
    ) -> Self {
        let correction = eval_correction(td, ply);

        let raw;
        let mut corrected;

        if in_check {
            raw = Score::NONE;
            corrected = Score::NONE;
        } else if excluded {
            raw = Score::NONE;
            corrected = td.stack[ply].eval;
        } else if is_valid(tt_probe.raw_eval()) {
            raw = tt_probe.raw_eval();
            corrected = correct_eval(td, raw, correction);
        } else {
            raw = td.nnue.evaluate(&td.board);
            corrected = correct_eval(td, raw, correction);

            td.shared.tt.write(hash, TtDepth::SOME, raw, Score::NONE, Bound::None, Move::NULL, ply, tt_pv, false);
        }

        let mut estimated = corrected;
        if tt_probe.can_use_score_as_estimate(in_check, excluded, corrected) {
            estimated = tt_probe.score;
        }

        if in_check && tt_probe.can_use_score_as_in_check_eval(alpha, beta) {
            corrected = tt_probe.score;
        }

        let improvement = if in_check {
            0
        } else if is_valid(td.stack[ply - 2].eval) {
            corrected - td.stack[ply - 2].eval
        } else if is_valid(td.stack[ply - 4].eval) {
            corrected - td.stack[ply - 4].eval
        } else {
            0
        };

        Self {
            raw,
            corrected,
            estimated,
            correction,
            improvement,
            improving: improvement > 0,
        }
    }
}

/// Correction-history bias for the current side to move and recent context.
///
/// This combines pawn, non-pawn, and continuation-correction histories. The
/// result is intentionally a scalar because pruning and reductions use the
/// magnitude as a confidence signal, not just as an eval offset.
#[inline]
pub(super) fn eval_correction(td: &ThreadData, ply: isize) -> i32 {
    let stm = td.board.side_to_move();
    let corrhist = td.corrhist();

    (corrhist.pawn.get(stm, td.board.pawn_key())
        + corrhist.non_pawn[Color::White].get(stm, td.board.non_pawn_key(Color::White))
        + corrhist.non_pawn[Color::Black].get(stm, td.board.non_pawn_key(Color::Black))
        + td.continuation_corrhist.get(
            td.stack[ply - 2].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
        )
        + td.continuation_corrhist.get(
            td.stack[ply - 4].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
        ))
        / 73
}

/// Train correction histories from a quiet, non-check full-width node result.
///
/// The caller decides whether the result is trustworthy enough to learn from.
/// This function only applies the same bonus to the correction tables that
/// explain the current static-eval context.
#[inline]
pub(super) fn update_correction_histories(td: &mut ThreadData, depth: i32, diff: i32, ply: isize) {
    let stm = td.board.side_to_move();
    let corrhist = td.corrhist();
    let bonus = (142 * depth * diff / 128).clamp(-4771, 3001);

    corrhist.pawn.update(stm, td.board.pawn_key(), bonus);

    corrhist.non_pawn[Color::White].update(stm, td.board.non_pawn_key(Color::White), bonus);
    corrhist.non_pawn[Color::Black].update(stm, td.board.non_pawn_key(Color::Black), bonus);

    if td.stack[ply - 1].mv.is_present() && td.stack[ply - 2].mv.is_present() {
        td.continuation_corrhist.update(
            td.stack[ply - 2].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
            bonus,
        );
    }

    if td.stack[ply - 1].mv.is_present() && td.stack[ply - 4].mv.is_present() {
        td.continuation_corrhist.update(
            td.stack[ply - 4].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
            bonus,
        );
    }
}
