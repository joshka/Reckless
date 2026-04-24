//! Search history feedback.
//!
//! Full-width search records which moves caused cutoffs and which alternatives failed to do so. The
//! tables are shared by later move ordering, so these updates belong after the node result is known
//! and before TT finalization. This module does not own the history table storage layout; it owns
//! the feedback events produced by a completed full-width node.

use crate::{
    thread::ThreadData,
    transposition::Bound,
    types::{ArrayVec, Color, Move, Piece, Square, is_valid},
};

/// Inputs for post-node history feedback.
///
/// History updates happen after the move loop because they need the final bound, best move,
/// searched alternatives, and child-search count. Keeping the context together prevents
/// finalization from passing an opaque scalar list.
pub struct HistoryUpdateContext<'a> {
    /// Current ply whose stack context supplies parent moves and history tables.
    pub ply: isize,

    /// Search depth used to scale bonuses and maluses.
    pub depth: i32,

    /// Whether the node had cut-node shape.
    pub cut_node: bool,

    /// Whether this is a root node with no parent history to reward.
    pub node_root: bool,

    /// Side to move for quiet-history updates.
    pub stm: Color,

    /// Final node bound from the move loop.
    pub bound: Bound,

    /// Best move found by the node.
    pub best_move: Move,

    /// Best score found by the node.
    pub best_score: i32,

    /// Beta bound used to identify fail-high feedback.
    pub beta: i32,

    /// Number of child searches spent on the current best move.
    pub current_search_count: i32,

    /// Quiet alternatives searched before the best move.
    pub quiet_moves: &'a ArrayVec<Move, 32>,

    /// Noisy alternatives searched before the best move.
    pub noisy_moves: &'a ArrayVec<Move, 32>,

    /// Whether the node was in check, used by parent fail-low feedback.
    pub in_check: bool,

    /// Corrected static eval used by parent fail-low feedback.
    pub eval: i32,
}

/// Update histories from the move that won the node and the moves it beat.
///
/// This is search feedback, not storage logic. Quiet, noisy, and continuation histories all learn
/// from the same cutoff event, and the searched move lists provide maluses for alternatives that
/// were tried before the best move.
#[inline]
fn update_best_move_histories(td: &mut ThreadData, ctx: &HistoryUpdateContext<'_>) {
    let HistoryUpdateContext {
        ply,
        depth,
        cut_node,
        node_root,
        stm,
        best_move,
        best_score,
        beta,
        current_search_count,
        quiet_moves,
        noisy_moves,
        ..
    } = *ctx;

    if !best_move.is_present() {
        return;
    }

    let noisy_bonus = (115 * depth).min(778) - 50 - 77 * cut_node as i32;
    let noisy_malus = (176 * depth).min(1343) - 51 - 21 * noisy_moves.len() as i32;

    let quiet_bonus = (172 * depth).min(1508) - 76 - 55 * cut_node as i32;
    let quiet_malus = (156 * depth).min(1065) - 45 - 36 * quiet_moves.len() as i32;

    let cont_bonus = (99 * depth).min(995) - 65 - 49 * cut_node as i32;
    let cont_malus = (371 * depth).min(914) - 44 - 18 * quiet_moves.len() as i32;

    if best_move.is_noisy() {
        td.noisy_history.update(
            td.board.all_threats(),
            td.board.moved_piece(best_move),
            best_move.to(),
            td.board.type_on(best_move.to()),
            noisy_bonus,
        );
    } else {
        td.quiet_history.update(td.board.all_threats(), stm, best_move, quiet_bonus);
        update_continuation_histories(td, ply, td.board.moved_piece(best_move), best_move.to(), cont_bonus);

        for &mv in quiet_moves.iter() {
            td.quiet_history.update(td.board.all_threats(), stm, mv, -quiet_malus);
            update_continuation_histories(td, ply, td.board.moved_piece(mv), mv.to(), -cont_malus);
        }
    }

    for &mv in noisy_moves.iter() {
        let captured = td.board.type_on(mv.to());
        td.noisy_history.update(td.board.all_threats(), td.board.moved_piece(mv), mv.to(), captured, -noisy_malus);
    }

    if !node_root && td.stack[ply - 1].mv.is_quiet() && td.stack[ply - 1].move_count < 2 {
        let malus = (90 * depth - 58).min(789);
        update_continuation_histories(td, ply - 1, td.stack[ply - 1].piece, td.stack[ply - 1].mv.to(), -malus);
    }

    if current_search_count > 1 && best_move.is_quiet() && best_score >= beta {
        let bonus = (194 * depth - 89).min(1595);
        update_continuation_histories(td, ply, td.stack[ply].piece, best_move.to(), bonus);
    }
}

/// Reward the parent move after an interior fail-low.
///
/// A quiet parent that led to an upper-bound child becomes more attractive in future ordering,
/// especially when it was late, matched the parent TT move, or made static eval look too
/// optimistic. Noisy parents get only a small direct history reward.
#[inline]
fn update_fail_low_parent_history(td: &mut ThreadData, ctx: &HistoryUpdateContext<'_>) {
    let ply = ctx.ply;
    let prior_move = td.stack[ply - 1].mv;
    if prior_move.is_quiet() {
        let factor = 116
            + 202 * (td.stack[ply - 1].move_count > 7) as i32
            + 116 * (prior_move == td.stack[ply - 1].tt_move) as i32
            + 138 * (!ctx.in_check && ctx.best_score <= ctx.eval - 93) as i32
            + 321 * (is_valid(td.stack[ply - 1].eval) && ctx.best_score <= -td.stack[ply - 1].eval - 128) as i32;

        let scaled_bonus = factor * (165 * ctx.depth - 35).min(2467) / 128;

        td.quiet_history.update(td.board.prior_threats(), !td.board.side_to_move(), prior_move, scaled_bonus);

        let entry = &td.stack[ply - 2];
        if entry.mv.is_present() {
            let bonus = (159 * ctx.depth - 39).min(1160);
            td.continuation_history.update(entry.conthist, td.stack[ply - 1].piece, prior_move.to(), bonus);
        }
    } else if prior_move.is_noisy() {
        let captured = td.board.captured_piece().unwrap_or_default().piece_type();
        let bonus = 60;

        td.noisy_history.update(
            td.board.prior_threats(),
            td.board.piece_on(prior_move.to()),
            prior_move.to(),
            captured,
            bonus,
        );
    }
}

/// Apply all post-node history feedback in its required order.
///
/// Best-move feedback is always considered first. Parent fail-low feedback only applies to non-root
/// upper bounds because root nodes do not have a meaningful parent move to reward.
#[inline]
pub fn update_node_histories(td: &mut ThreadData, ctx: HistoryUpdateContext<'_>) {
    update_best_move_histories(td, &ctx);

    if !ctx.node_root && ctx.bound == Bound::Upper {
        update_fail_low_parent_history(td, &ctx);
    }
}

/// Update continuation-history tables for recent parent distances.
///
/// The offsets are the continuation-history context depths this engine trains. The stack entries
/// must already contain `conthist` pointers from make-move.
#[inline]
pub fn update_continuation_histories(td: &mut ThreadData, ply: isize, piece: Piece, sq: Square, bonus: i32) {
    for offset in [1, 2, 4, 6] {
        let entry = &td.stack[ply - offset];
        if entry.mv.is_present() {
            td.continuation_history.update(entry.conthist, piece, sq, bonus);
        }
    }
}
