use crate::{
    evaluation::correct_eval,
    movepick::MovePicker,
    thread::{Status, ThreadData},
    transposition::{Bound, TtDepth},
    types::{MAX_PLY, Move, Score, draw, is_decisive, is_loss, is_valid, mated_in},
};

use super::{NodeType, eval_correction, make_move, tt, undo_move};

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
    let entry = td.shared.tt.read(hash, td.board.halfmove_clock(), ply);

    let mut tt_move = Move::NULL;
    let mut tt_score = Score::NONE;
    let mut tt_bound = Bound::None;
    let mut tt_pv = NODE::PV;

    // QS early TT cutoff
    if let Some(entry) = &entry {
        tt_move = entry.mv;
        tt_score = entry.score;
        tt_bound = entry.bound;
        tt_pv |= entry.tt_pv;

        if tt::can_cutoff_qsearch(NODE::PV, tt_score, tt_bound, alpha, beta) {
            return tt_score;
        }
    }

    let raw_eval;
    let eval;
    let mut best_score;
    let correction_value = eval_correction(td, ply);

    // Evaluation
    if in_check {
        raw_eval = Score::NONE;
        eval = Score::NONE;
        best_score = -Score::INFINITE;
    } else {
        raw_eval = match &entry {
            Some(entry) if is_valid(entry.raw_eval) => entry.raw_eval,
            _ => td.nnue.evaluate(&td.board),
        };
        eval = correct_eval(td, raw_eval, correction_value);
        best_score = eval;

        if tt::can_use_qsearch_score(NODE::PV, tt_score, tt_bound, best_score) {
            best_score = tt_score;
        }
    }

    // Stand Pat
    if best_score >= beta {
        if !is_decisive(best_score) && !is_decisive(beta) {
            best_score = beta + (best_score - beta) / 3;
        }

        if entry.is_none() {
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

    let skip_quiets =
        |best_score| !((in_check && is_loss(best_score)) || (tt_move.is_quiet() && tt_bound != Bound::Upper));

    while let Some(mv) = move_picker.next::<NODE>(td, skip_quiets(best_score), ply) {
        move_count += 1;

        if !is_loss(best_score) {
            // Late Move Pruning (LMP)
            if move_count >= 3 && !td.board.is_direct_check(mv) {
                break;
            }

            // Static Exchange Evaluation Pruning (SEE Pruning)
            if is_valid(eval) && !td.board.see(mv, (alpha - eval) / 8 - correction_value.abs().min(64) - 79) {
                continue;
            }
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

    if best_score >= beta && !is_decisive(best_score) && !is_decisive(beta) {
        best_score = (best_score + beta) / 2;
    }

    let bound = if best_score >= beta { Bound::Lower } else { Bound::Upper };

    td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, bound, best_move, ply, tt_pv, false);

    debug_assert!(alpha < beta);
    debug_assert!(-Score::INFINITE < best_score && best_score < Score::INFINITE);

    best_score
}
