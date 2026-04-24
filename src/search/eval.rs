//! Search-side static-eval state and correction policy.
//!
//! Static eval in search is NNUE plus correction-history bias plus selected TT bound information.
//! This module owns the eval state used by pruning, reductions, and final correction updates. It
//! does not own NNUE inference or correction-history storage; it owns how search combines those
//! signals into values consumed by later phases.

use crate::{
    evaluation::correct_eval,
    thread::ThreadData,
    transposition::{Bound, TtDepth},
    types::{Color, Move, Score, is_valid},
};

use super::tt::TtProbe;

/// Inputs for computing a full-width node's static-eval view.
///
/// Eval setup sits where node facts, TT proof state, and the current alpha-beta window meet.
/// Keeping those values together makes it clear that eval is not a standalone NNUE call; it is a
/// search phase that may reuse TT eval, write a raw-eval TT entry, and interpret TT scores through
/// the current window.
#[derive(Copy, Clone)]
pub struct EvalInput {
    /// Current position hash used for the raw-eval TT write.
    pub hash: u64,

    /// Current ply whose stack supplies previous evals and excluded-node state.
    pub ply: isize,

    /// Whether the node is in check, disabling normal stand-pat eval.
    pub in_check: bool,

    /// Whether this node searches a singular-verification move set.
    pub excluded: bool,

    /// TT probe that can provide raw eval or a window-compatible search score.
    pub tt_probe: TtProbe,

    /// TT-PV marker stored with any raw-eval-only TT entry.
    pub tt_pv: bool,

    /// Current lower bound used to interpret in-check TT scores.
    pub alpha: i32,

    /// Current upper bound used to interpret in-check TT scores.
    pub beta: i32,
}

/// Inputs for preparing the per-node stack contract after eval.
///
/// This phase writes the stack fields that later pruning, move ordering, reductions, and parent
/// feedback read. It is deliberately separate from `EvalInput`: eval computes values, while stack
/// preparation publishes those values and may adjust depth from parent feedback.
#[derive(Copy, Clone)]
pub struct StackPreparationInput {
    /// Current ply whose stack entry is being initialized.
    pub ply: isize,

    /// Whether this is the root node.
    pub node_root: bool,

    /// Side to move, used for parent quiet-history feedback.
    pub stm: Color,

    /// Whether the node is in check.
    pub in_check: bool,

    /// Whether this is a singular-verification node with an excluded move.
    pub excluded: bool,

    /// TT move published to the stack for move ordering and parent feedback.
    pub tt_probe: TtProbe,

    /// TT-PV marker published to the stack for reductions and finalization.
    pub tt_pv: bool,

    /// Corrected static eval published to the stack.
    pub eval: i32,

    /// Current remaining depth, adjusted by hindsight feedback in this phase.
    pub depth: i32,
}

/// Static eval view used by the full-width node after TT/tablebase probing.
///
/// Search eval is not just NNUE. The full-width node needs the raw NNUE value for TT storage, the
/// correction-adjusted value for history feedback, a TT-adjusted estimate for pruning, and
/// improvement signals for pruning and reductions. Keeping those together makes later consumers
/// name which eval role they mean.
#[derive(Copy, Clone)]
pub struct EvalState {
    /// Raw NNUE value before correction history.
    ///
    /// This is the value stored in TT entries so future probes can reuse the network result without
    /// also baking in a correction-history version.
    pub raw: i32,

    /// Static eval after applying correction-history bias.
    ///
    /// Full-width history feedback and correction-history training compare the searched result
    /// against this value.
    pub corrected: i32,

    /// Best static estimate available for pruning.
    ///
    /// This starts from `corrected` but may be tightened by a compatible TT bound. Pruning uses it
    /// because a proven TT bound can be a stronger static signal than corrected eval alone.
    pub estimated: i32,

    /// Correction-history bias used to produce `corrected`.
    ///
    /// Some heuristics use the magnitude as a confidence signal, so callers need the scalar even
    /// after `corrected` has been computed.
    pub correction: i32,

    /// Difference between the current eval and a recent previous eval.
    ///
    /// Null-move pruning and reduction policy use this as a trend signal rather than only asking
    /// whether the position is improving.
    pub improvement: i32,

    /// Whether recent eval history says the side to move is improving.
    ///
    /// This boolean feeds tuned pruning margins where only the direction of the trend matters.
    pub improving: bool,
}

impl EvalState {
    /// Build the eval state in the order required by search.
    ///
    /// Excluded singular-verification nodes reuse the stack eval instead of refreshing NNUE, and
    /// fresh NNUE evals write a raw-eval-only TT entry before pruning can return. TT bounds may
    /// adjust the estimate after the corrected eval exists, but only when the bound direction is
    /// compatible.
    #[inline]
    pub fn compute(td: &mut ThreadData, input: EvalInput) -> Self {
        let EvalInput { hash, ply, in_check, excluded, tt_probe, tt_pv, alpha, beta } = input;
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

            write_raw_eval_cache(td, hash, raw, ply, tt_pv);
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

/// Store a fresh NNUE value before any pruning phase can return from the node.
///
/// This is an eval cache entry, not a search result. It deliberately stores no move and no bound
/// score so later probes can reuse `raw_eval` without treating the entry as an alpha-beta proof.
#[inline(always)]
fn write_raw_eval_cache(td: &mut ThreadData, hash: u64, raw: i32, ply: isize, tt_pv: bool) {
    td.shared.tt.write(hash, TtDepth::SOME, raw, Score::NONE, Bound::None, Move::NULL, ply, tt_pv, false);
}

/// Correction-history bias for the current side to move and recent context.
///
/// This combines pawn, non-pawn, and continuation-correction histories. The result is intentionally
/// a scalar because pruning and reductions use the magnitude as a confidence signal, not just as an
/// eval offset.
#[inline]
pub fn eval_correction(td: &ThreadData, ply: isize) -> i32 {
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
/// The caller decides whether the result is trustworthy enough to learn from. This function only
/// applies the same bonus to the correction tables that explain the current static-eval context.
#[inline]
pub fn update_correction_histories(td: &mut ThreadData, depth: i32, diff: i32, ply: isize) {
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

/// Initialize per-node stack state and parent-eval feedback.
///
/// This phase sits immediately after eval because later pruning and move-loop formulas depend on
/// stack eval, TT move, TT-PV, move count, and reduction fields already having the current-node
/// values. It writes:
///
/// - `eval`, consumed by later child reductions and future parent feedback.
/// - `tt_move`, consumed by move ordering and parent fail-low history feedback.
/// - `tt_pv`, consumed by reductions and final TT-PV propagation.
/// - `reduction`, reset before child search stores the move's reduction.
/// - `move_count`, reset before the move loop records searched candidates.
/// - grandchild `cutoff_count`, reset before children report cutoffs upward.
///
/// It also applies parent eval-difference history feedback and hindsight depth tweaks before
/// pruning, because those later phases depend on the adjusted depth.
#[inline(always)]
pub fn prepare_full_width_node(td: &mut ThreadData, input: StackPreparationInput) -> i32 {
    let StackPreparationInput {
        ply,
        node_root,
        stm,
        in_check,
        excluded,
        tt_probe,
        tt_pv,
        eval,
        mut depth,
    } = input;

    td.stack[ply].eval = eval;
    td.stack[ply].tt_move = tt_probe.mv;
    td.stack[ply].tt_pv = tt_pv;
    td.stack[ply].reduction = 0;
    td.stack[ply].move_count = 0;
    td.stack[ply + 2].cutoff_count = 0;

    if !node_root && !in_check && !excluded && td.stack[ply - 1].mv.is_quiet() && is_valid(td.stack[ply - 1].eval) {
        let value = 824 * (-(eval + td.stack[ply - 1].eval)) / 128;
        let bonus = value.clamp(-133, 348);

        td.quiet_history.update(td.board.prior_threats(), !stm, td.stack[ply - 1].mv, bonus);
    }

    if !node_root && !in_check && !excluded && td.stack[ply - 1].reduction >= 2367 && eval + td.stack[ply - 1].eval < 0
    {
        depth += 1;
    }

    if !node_root
        && !tt_pv
        && !in_check
        && !excluded
        && depth >= 2
        && td.stack[ply - 1].reduction > 0
        && is_valid(td.stack[ply - 1].eval)
        && eval + td.stack[ply - 1].eval > 59
    {
        depth -= 1;
    }

    depth
}
