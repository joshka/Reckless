//! Quiescence search.
//!
//! Qsearch stabilizes depth-zero leaves by searching tactical continuations
//! instead of all legal moves. It has a stand-pat contract when not in check, a
//! shallow TT policy, and a much smaller history footprint than full-width
//! search.
//!
//! Do not pull full-width pruning or reduction assumptions into this module
//! without making the qsearch-specific contract explicit.

use crate::{
    evaluation::correct_eval,
    movepick::MovePicker,
    thread::{Status, ThreadData},
    transposition::{Bound, TtDepth},
    types::{MAX_PLY, Move, Score, draw, is_decisive, is_loss, is_valid, mated_in},
};

use super::{NodeType, eval::eval_correction, make_move, tt, undo_move};

/// Eval state for qsearch's stand-pat contract.
///
/// Qsearch only has a static stand-pat value outside check. In check, it must
/// search evasions and starts from negative infinity. A TT score can adjust the
/// best stand-pat score, but qsearch keeps the corrected eval separately for
/// SEE pruning.
struct QsearchEval {
    raw: i32,
    corrected: i32,
    best_score: i32,
    correction: i32,
}

impl QsearchEval {
    fn compute(td: &mut ThreadData, ply: isize, in_check: bool, node_pv: bool, tt_probe: tt::TtProbe) -> Self {
        let correction = eval_correction(td, ply);

        if in_check {
            return Self {
                raw: Score::NONE,
                corrected: Score::NONE,
                best_score: -Score::INFINITE,
                correction,
            };
        }

        let raw = if is_valid(tt_probe.raw_eval()) { tt_probe.raw_eval() } else { td.nnue.evaluate(&td.board) };
        let corrected = correct_eval(td, raw, correction);
        let mut best_score = corrected;

        if tt_probe.can_use_qsearch_score(node_pv, best_score) {
            best_score = tt_probe.score;
        }

        Self { raw, corrected, best_score, correction }
    }
}

pub(super) fn qsearch<NODE: NodeType>(td: &mut ThreadData, mut alpha: i32, beta: i32, ply: isize) -> i32 {
    debug_assert!(!NODE::ROOT);
    debug_assert!(ply as usize <= MAX_PLY);
    debug_assert!(-Score::INFINITE <= alpha && alpha < beta && beta <= Score::INFINITE);
    debug_assert!(NODE::PV || alpha == beta - 1);

    let draw_score = draw(td);
    if alpha < draw_score && td.board.upcoming_repetition(ply as usize) {
        alpha = draw_score;
        if alpha >= beta {
            return alpha;
        }
    }

    let stm = td.board.side_to_move();
    let in_check = td.board.in_check();

    if NODE::PV {
        td.pv_table.clear(ply as usize);
        td.sel_depth = td.sel_depth.max(ply as i32);
    }

    if td.id == 0 && td.time_manager.check_time(td) {
        td.shared.status.set(Status::STOPPED);
        return Score::ZERO;
    }

    if td.board.is_draw(ply) {
        return draw(td);
    }

    if ply as usize >= MAX_PLY - 1 {
        return if in_check { draw(td) } else { td.nnue.evaluate(&td.board) };
    }

    let hash = td.board.hash();
    let tt_probe = tt::TtProbe::read(td, hash, ply, NODE::PV);
    let tt_pv = tt_probe.tt_pv;

    // QS early TT cutoff
    if tt_probe.can_cutoff_qsearch(NODE::PV, alpha, beta) {
        return tt_probe.score;
    }

    // Evaluation
    let eval = QsearchEval::compute(td, ply, in_check, NODE::PV, tt_probe);
    let raw_eval = eval.raw;
    let correction_value = eval.correction;
    let mut best_score = eval.best_score;

    // Stand Pat
    if best_score >= beta {
        best_score = stand_pat_cutoff_score(best_score, beta);

        if !tt_probe.has_entry() {
            td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, Bound::Lower, Move::NULL, ply, tt_pv, false);
        }

        return best_score;
    }

    if best_score > alpha {
        alpha = best_score;
    }

    let mut best_move = Move::NULL;

    let mut move_count = 0;
    let mut move_picker = MovePicker::new_qsearch();

    let skip_quiets = |best_score| qsearch_skips_quiets(in_check, best_score, tt_probe);

    while let Some(mv) = move_picker.next::<NODE>(td, skip_quiets(best_score), ply) {
        move_count += 1;

        if late_move_prunes(td, mv, move_count, best_score) {
            break;
        }

        if see_prunes(td, mv, alpha, eval.corrected, correction_value, best_score) {
            continue;
        }

        make_move(td, ply, mv);
        let score = -qsearch::<NODE>(td, -beta, -alpha, ply + 1);
        undo_move(td, mv);

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if score > best_score {
            best_score = score;

            if score > alpha {
                best_move = mv;

                if NODE::PV {
                    td.pv_table.update(ply as usize, mv);
                }

                if score >= beta {
                    break;
                }

                alpha = score;
            }
        }
    }

    if in_check && move_count == 0 {
        return mated_in(ply);
    }

    if best_score >= beta {
        let is_noisy = best_move.is_noisy();
        let bonus = if is_noisy { 106 } else { 172 };

        if is_noisy {
            td.noisy_history.update(
                td.board.all_threats(),
                td.board.moved_piece(best_move),
                best_move.to(),
                td.board.type_on(best_move.to()),
                bonus,
            );
        } else {
            td.quiet_history.update(td.board.all_threats(), stm, best_move, bonus);
        }
    }

    if best_score >= beta {
        best_score = beta_cutoff_score(best_score, beta);
    }

    let bound = if best_score >= beta { Bound::Lower } else { Bound::Upper };

    td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, bound, best_move, ply, tt_pv, false);

    debug_assert!(alpha < beta);
    debug_assert!(-Score::INFINITE < best_score && best_score < Score::INFINITE);

    best_score
}

#[inline]
fn stand_pat_cutoff_score(best_score: i32, beta: i32) -> i32 {
    if is_decisive(best_score) || is_decisive(beta) {
        return best_score;
    }

    beta + (best_score - beta) / 3
}

#[inline]
fn qsearch_skips_quiets(in_check: bool, best_score: i32, tt_probe: tt::TtProbe) -> bool {
    !((in_check && is_loss(best_score)) || (tt_probe.mv.is_quiet() && tt_probe.bound != Bound::Upper))
}

#[inline]
fn late_move_prunes(td: &ThreadData, mv: Move, move_count: i32, best_score: i32) -> bool {
    !is_loss(best_score) && move_count >= 3 && !td.board.is_direct_check(mv)
}

#[inline]
fn see_prunes(td: &ThreadData, mv: Move, alpha: i32, eval: i32, correction: i32, best_score: i32) -> bool {
    !is_loss(best_score) && is_valid(eval) && !td.board.see(mv, (alpha - eval) / 8 - correction.abs().min(64) - 79)
}

#[inline]
fn beta_cutoff_score(best_score: i32, beta: i32) -> i32 {
    if is_decisive(best_score) || is_decisive(beta) {
        return best_score;
    }

    (best_score + beta) / 2
}
