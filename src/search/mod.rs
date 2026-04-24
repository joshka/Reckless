//! Full-width search driver and shared search glue.
//!
//! This module owns the recursive alpha-beta search order. The order is part of
//! the engine's behavior: cheap terminal guards, TT and tablebase probes,
//! static-eval setup, pre-move pruning, singular extension, move ordering,
//! child search, history feedback, and TT writeback all feed later phases.
//!
//! Keep the phase sequence visible here. Move details into concept modules only
//! when the extracted concept has a stable chess-search meaning and does not
//! hide tuned cross-heuristic data flow.

use std::sync::atomic::Ordering;

use crate::{
    movepick::{MovePicker, Stage},
    thread::{Status, ThreadData},
    transposition::Bound,
    types::{ArrayVec, MAX_PLY, Move, Piece, Score, draw, is_decisive, is_loss, is_valid, is_win, mate_in, mated_in},
};

#[cfg(feature = "syzygy")]
use crate::{
    tb,
    types::{tb_loss_in, tb_win_in},
};

#[allow(unused_imports)]
use crate::misc::{dbg_hit, dbg_stats};

mod eval;
mod finalize;
mod history;
mod pruning;
mod qsearch;
mod root;
mod singular;
mod tt;

use eval::{EvalState, update_correction_histories};
use finalize::{propagate_tt_pv, scale_beta_cutoff_score, should_update_correction_history, should_write_tt};
use history::{update_continuation_histories, update_node_histories};
use qsearch::qsearch;
pub use root::{Report, start};

pub trait NodeType {
    const PV: bool;
    const ROOT: bool;
}

struct Root;
impl NodeType for Root {
    const PV: bool = true;
    const ROOT: bool = true;
}

struct PV;
impl NodeType for PV {
    const PV: bool = true;
    const ROOT: bool = false;
}

struct NonPV;
impl NodeType for NonPV {
    const PV: bool = false;
    const ROOT: bool = false;
}

fn search<NODE: NodeType>(
    td: &mut ThreadData, mut alpha: i32, mut beta: i32, depth: i32, cut_node: bool, ply: isize,
) -> i32 {
    debug_assert!(ply as usize <= MAX_PLY);
    debug_assert!(-Score::INFINITE <= alpha && alpha < beta && beta <= Score::INFINITE);
    debug_assert!(NODE::PV || alpha == beta - 1);

    let stm = td.board.side_to_move();
    let in_check = td.board.in_check();
    let excluded = td.stack[ply].excluded.is_present();

    if !NODE::ROOT && NODE::PV {
        td.pv_table.clear(ply as usize);
    }

    if td.shared.status.get() == Status::STOPPED {
        return Score::ZERO;
    }

    // Qsearch Dive
    if depth <= 0 {
        return qsearch::<NODE>(td, alpha, beta, ply);
    }

    let draw_score = draw(td);
    if !NODE::ROOT && alpha < draw_score && td.board.upcoming_repetition(ply as usize) {
        alpha = draw_score;
        if alpha >= beta {
            return alpha;
        }
    }

    if NODE::PV {
        td.sel_depth = td.sel_depth.max(ply as i32);
    }

    if td.id == 0 && td.time_manager.check_time(td) {
        td.shared.status.set(Status::STOPPED);
        return Score::ZERO;
    }

    if !NODE::ROOT {
        if td.board.is_draw(ply) {
            return draw(td);
        }

        if ply as usize >= MAX_PLY - 1 {
            return if in_check { draw(td) } else { td.nnue.evaluate(&td.board) };
        }

        // Mate Distance Pruning (MDP)
        alpha = alpha.max(mated_in(ply));
        beta = beta.min(mate_in(ply + 1));

        if alpha >= beta {
            return alpha;
        }
    }

    #[cfg(feature = "syzygy")]
    let mut max_score = Score::INFINITE;

    let mut best_score = -Score::INFINITE;

    let mut depth = depth.min(MAX_PLY as i32 - 1);

    let hash = td.board.hash();
    let mut tt_probe = tt::TtProbe::read(td, hash, ply, NODE::PV);
    let mut tt_pv = tt_probe.tt_pv;

    // Search early TT cutoff
    if tt_probe.has_entry() {
        if tt_probe.can_cutoff_full_width(NODE::PV, excluded, depth, alpha, beta, cut_node) {
            if tt_probe.mv.is_quiet() && tt_probe.score >= beta && td.stack[ply - 1].move_count < 4 {
                let quiet_bonus = (175 * depth - 79).min(1637);
                let cont_bonus = (114 * depth - 57).min(1284);

                td.quiet_history.update(td.board.all_threats(), stm, tt_probe.mv, quiet_bonus);
                update_continuation_histories(td, ply, td.board.moved_piece(tt_probe.mv), tt_probe.mv.to(), cont_bonus);
            }

            if td.board.halfmove_clock() < 90 {
                return tt_probe.score;
            }
        }
    }

    // Tablebases Probe
    #[cfg(feature = "syzygy")]
    if !NODE::ROOT
        && !excluded
        && !td.shared.stop_probing_tb.load(Ordering::Relaxed)
        && td.board.halfmove_clock() == 0
        && td.board.castling().raw() == 0
        && td.board.occupancies().popcount() <= tb::size()
        && let Some(outcome) = tb::probe(&td.board)
    {
        td.shared.tb_hits.increment(td.id);

        let (score, bound) = match outcome {
            tb::GameOutcome::Win => (tb_win_in(ply), Bound::Lower),
            tb::GameOutcome::Loss => (tb_loss_in(ply), Bound::Upper),
            tb::GameOutcome::Draw => (Score::ZERO, Bound::Exact),
        };

        if bound == Bound::Exact
            || (bound == Bound::Lower && score >= beta)
            || (bound == Bound::Upper && score <= alpha)
        {
            let depth = (depth + 6).min(MAX_PLY as i32 - 1);
            td.shared.tt.write(hash, depth, Score::NONE, score, bound, Move::NULL, ply, tt_pv, false);
            return score;
        }

        if NODE::PV {
            if bound == Bound::Lower {
                best_score = score;
                alpha = alpha.max(best_score);
            } else {
                max_score = score;
            }
        }
    }

    let eval_state = EvalState::compute(td, hash, ply, in_check, excluded, tt_probe, tt_pv, alpha, beta);
    let raw_eval = eval_state.raw;
    let eval = eval_state.corrected;
    let estimated_score = eval_state.estimated;
    let correction_value = eval_state.correction;
    let improvement = eval_state.improvement;
    let improving = eval_state.improving;

    td.stack[ply].eval = eval;
    td.stack[ply].tt_move = tt_probe.mv;
    td.stack[ply].tt_pv = tt_pv;
    td.stack[ply].reduction = 0;
    td.stack[ply].move_count = 0;
    td.stack[ply + 2].cutoff_count = 0;

    // Quiet move ordering using eval difference
    if !NODE::ROOT && !in_check && !excluded && td.stack[ply - 1].mv.is_quiet() && is_valid(td.stack[ply - 1].eval) {
        let value = 824 * (-(eval + td.stack[ply - 1].eval)) / 128;
        let bonus = value.clamp(-133, 348);

        td.quiet_history.update(td.board.prior_threats(), !stm, td.stack[ply - 1].mv, bonus);
    }

    // Hindsight reductions
    if !NODE::ROOT && !in_check && !excluded && td.stack[ply - 1].reduction >= 2367 && eval + td.stack[ply - 1].eval < 0
    {
        depth += 1;
    }

    if !NODE::ROOT
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

    let potential_singularity = depth >= 5 + tt_pv as i32
        && tt_probe.depth >= depth - 3
        && tt_probe.bound != Bound::Upper
        && is_valid(tt_probe.score)
        && !is_decisive(tt_probe.score);

    // Razoring
    if pruning::can_razor(NODE::PV, in_check, estimated_score, alpha, depth, tt_probe) {
        return qsearch::<NonPV>(td, alpha, beta, ply);
    }

    // Reverse Futility Pruning (RFP)
    if let Some(score) = pruning::reverse_futility_score(
        tt_pv,
        in_check,
        excluded,
        estimated_score,
        beta,
        depth,
        improving,
        correction_value,
        (td.board.all_threats() & td.board.colors(stm)).is_empty(),
    ) {
        return score;
    }

    // Null Move Pruning (NMP)
    if pruning::can_try_null_move(
        &td.board,
        cut_node,
        in_check,
        excluded,
        potential_singularity,
        estimated_score,
        beta,
        depth,
        tt_pv,
        improvement,
        td.stack[ply + 1].cutoff_count,
        ply,
        td.nmp_min_ply,
        tt_probe,
    ) {
        debug_assert_ne!(td.stack[ply - 1].mv, Move::NULL);

        let r = (5335 + 260 * depth + 493 * (estimated_score - beta).clamp(0, 1003) / 128) / 1024;

        td.stack[ply].conthist = td.stack.sentinel().conthist;
        td.stack[ply].contcorrhist = td.stack.sentinel().contcorrhist;
        td.stack[ply].piece = Piece::None;
        td.stack[ply].mv = Move::NULL;

        td.board.make_null_move();
        td.shared.tt.prefetch(td.board.hash());

        let score = -search::<NonPV>(td, -beta, -beta + 1, depth - r, false, ply + 1);

        td.board.undo_null_move();

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if score >= beta && !is_win(score) {
            if td.nmp_min_ply > 0 || depth < 16 {
                return score;
            }

            td.nmp_min_ply = ply as i32 + 3 * (depth - r) / 4;
            let verified_score = search::<NonPV>(td, beta - 1, beta, depth - r, false, ply);
            td.nmp_min_ply = 0;

            if td.shared.status.get() == Status::STOPPED {
                return Score::ZERO;
            }

            if verified_score >= beta {
                return score;
            }
        }
    }

    // ProbCut
    let mut probcut_beta = beta + 270 - 75 * improving as i32;

    if pruning::can_try_probcut(cut_node, beta, tt_probe, probcut_beta, eval) {
        let mut move_picker = MovePicker::new_probcut(probcut_beta - eval);

        while let Some(mv) = move_picker.next::<NODE>(td, true, ply) {
            if move_picker.stage() == Stage::BadNoisy {
                break;
            }

            if mv == td.stack[ply].excluded {
                continue;
            }

            make_move(td, ply, mv);

            let mut score = -qsearch::<NonPV>(td, -probcut_beta, -probcut_beta + 1, ply + 1);

            let base_depth = (depth - 4).max(0);
            let mut probcut_depth = (base_depth - (score - probcut_beta) / 319).clamp(0, base_depth);

            if score >= probcut_beta && probcut_depth > 0 {
                let adjusted_beta = (probcut_beta + 260 * (base_depth - probcut_depth)).min(Score::INFINITE);

                score = -search::<NonPV>(td, -adjusted_beta, -adjusted_beta + 1, probcut_depth, false, ply + 1);

                if score < adjusted_beta && probcut_beta < adjusted_beta {
                    probcut_depth = base_depth;
                    score = -search::<NonPV>(td, -probcut_beta, -probcut_beta + 1, probcut_depth, false, ply + 1);
                } else {
                    probcut_beta = adjusted_beta;
                }
            }

            undo_move(td, mv);

            if td.shared.status.get() == Status::STOPPED {
                return Score::ZERO;
            }

            if score >= probcut_beta {
                td.shared.tt.write(hash, probcut_depth + 1, raw_eval, score, Bound::Lower, mv, ply, tt_pv, false);

                if is_decisive(score) {
                    return score;
                }
                return (3 * score + beta) / 4;
            }
        }
    }

    let singular = singular::search_if_needed::<NODE>(
        td,
        ply,
        depth,
        beta,
        cut_node,
        excluded,
        potential_singularity,
        tt_probe,
        correction_value,
    );
    if let Some(score) = singular.cutoff {
        return score;
    }
    let extension = singular.extension;
    let singular_score = singular.score;
    tt_probe.mv = singular.tt_move;

    let mut best_move = Move::NULL;
    let mut bound = Bound::Upper;

    let mut quiet_moves = ArrayVec::<Move, 32>::new();
    let mut noisy_moves = ArrayVec::<Move, 32>::new();

    let mut move_count = 0;
    let mut move_picker = MovePicker::new(tt_probe.mv);
    let mut skip_quiets = false;
    let mut current_search_count = 0;
    let mut alpha_raises = 0;
    let mut tt_move_score = Score::NONE;

    while let Some(mv) = move_picker.next::<NODE>(td, skip_quiets, ply) {
        if mv == td.stack[ply].excluded {
            continue;
        }

        if NODE::ROOT && !td.root_moves[td.pv_index..td.pv_end].iter().any(|rm| rm.mv == mv) {
            continue;
        }

        move_count += 1;
        current_search_count = 0;
        td.stack[ply].move_count = move_count;

        let is_quiet = mv.is_quiet();

        let history = if is_quiet {
            td.quiet_history.get(td.board.all_threats(), stm, mv) + td.conthist(ply, 1, mv) + td.conthist(ply, 2, mv)
        } else {
            let captured = td.board.type_on(mv.to());
            td.noisy_history.get(td.board.all_threats(), td.board.moved_piece(mv), mv.to(), captured)
        };

        if !NODE::ROOT && !is_loss(best_score) {
            // Late Move Pruning (LMP)
            if !in_check
                && !td.board.is_direct_check(mv)
                && is_quiet
                && move_count >= (3006 + 70 * improvement / 16 + 1455 * depth * depth + 68 * history / 1024) / 1024
            {
                skip_quiets = true;
                continue;
            }

            // Futility Pruning (FP)
            let futility_value = eval + 79 * depth + 64 * history / 1024 + 84 * (eval >= beta) as i32 - 115;

            if !in_check && is_quiet && depth < 15 && futility_value <= alpha && !td.board.is_direct_check(mv) {
                if !is_decisive(best_score) && best_score < futility_value {
                    best_score = futility_value;
                }
                skip_quiets = true;
                continue;
            }

            // Bad Noisy Futility Pruning (BNFP)
            let noisy_futility_value = eval + 71 * depth + 68 * history / 1024 + 23;

            if !in_check
                && depth < 11
                && move_picker.stage() == Stage::BadNoisy
                && noisy_futility_value <= alpha
                && !td.board.is_direct_check(mv)
            {
                if !is_decisive(best_score) && best_score < noisy_futility_value {
                    best_score = noisy_futility_value;
                }
                break;
            }

            // Static Exchange Evaluation Pruning (SEE Pruning)
            let threshold = if is_quiet {
                (-17 * depth * depth + 52 * depth - 21 * history / 1024 + 20).min(0)
            } else {
                (-8 * depth * depth - 36 * depth - 32 * history / 1024 + 11).min(0)
            };

            if !td.board.see(mv, threshold) {
                continue;
            }
        }

        let initial_nodes = td.nodes();

        make_move(td, ply, mv);

        let mut new_depth = depth - 1 + if move_count == 1 { extension } else { (extension > 0) as i32 };
        let mut score = Score::ZERO;

        // Late Move Reductions (LMR)
        if depth >= 2 && move_count >= 2 {
            let mut reduction = 225 * (move_count.ilog2() * depth.ilog2()) as i32;

            reduction -= 68 * move_count;
            reduction -= 3297 * correction_value.abs() / 1024;
            reduction += 1306 * alpha_raises;

            reduction += 546 * (is_valid(tt_probe.score) && tt_probe.score <= alpha) as i32;
            reduction += 322 * (is_valid(tt_probe.score) && tt_probe.depth < depth) as i32;

            if is_quiet {
                reduction += 1806;
                reduction -= 166 * history / 1024;
            } else {
                reduction += 1449;
                reduction -= 109 * history / 1024;
            }

            if NODE::PV {
                reduction -= 424 + 433 * (beta - alpha) / td.root_delta;
            }

            if tt_pv {
                reduction -= 361;
                reduction -= 636 * (is_valid(tt_probe.score) && tt_probe.score > alpha) as i32;
                reduction -= 830 * (is_valid(tt_probe.score) && tt_probe.depth >= depth) as i32;
            }

            if !tt_pv && cut_node {
                reduction += 1818;
                reduction += 2118 * tt_probe.mv.is_null() as i32;
            }

            if !improving {
                reduction += (430 - 263 * improvement / 128).min(1096);
            }

            if td.board.in_check() {
                reduction -= 1021;
            }

            if td.stack[ply + 1].cutoff_count > 2 {
                reduction += 1515;
            }

            if is_valid(tt_move_score) && is_valid(singular_score) {
                let margin = tt_move_score - singular_score;
                reduction += (512 * (margin - 160) / 128).clamp(0, 2048);
            }

            if !NODE::PV && td.stack[ply - 1].reduction > reduction + 485 {
                reduction += 129;
            }

            reduction += helper_reduction_bias(td);

            let reduced_depth =
                (new_depth - reduction / 1024).clamp(1, new_depth + (move_count <= 3) as i32 + 1) + 2 * NODE::PV as i32;

            td.stack[ply].reduction = reduction;
            score = -search::<NonPV>(td, -alpha - 1, -alpha, reduced_depth, true, ply + 1);
            td.stack[ply].reduction = 0;
            current_search_count += 1;

            if score > alpha {
                if !NODE::ROOT {
                    new_depth += (score > best_score + 61) as i32;
                    new_depth += (score > best_score + 801) as i32;
                    new_depth -= (score < best_score + 5 + reduced_depth) as i32;
                }

                if new_depth > reduced_depth {
                    score = -search::<NonPV>(td, -alpha - 1, -alpha, new_depth, !cut_node, ply + 1);
                    current_search_count += 1;
                }
            }
        }
        // Full Depth Search (FDS)
        else if !NODE::PV || move_count >= 2 {
            let mut reduction = 232 * (move_count.ilog2() * depth.ilog2()) as i32;

            reduction -= 48 * move_count;
            reduction -= 2408 * correction_value.abs() / 1024;

            if is_quiet {
                reduction += 1429;
                reduction -= 152 * history / 1024;
            } else {
                reduction += 1053;
                reduction -= 67 * history / 1024;
            }

            if tt_pv {
                reduction -= 936;
                reduction -= 1080 * (is_valid(tt_probe.score) && tt_probe.depth >= depth) as i32;
            }

            if !tt_pv && cut_node {
                reduction += 1543;
                reduction += 2058 * tt_probe.mv.is_null() as i32;
            }

            if !improving {
                reduction += (409 - 254 * improvement / 128).min(1488);
            }

            if td.stack[ply + 1].cutoff_count > 2 {
                reduction += 1360;
            }

            if is_valid(tt_move_score) && is_valid(singular_score) {
                let margin = tt_move_score - singular_score;
                reduction += (400 * (margin - 160) / 128).clamp(0, 2048);
            }

            if mv == tt_probe.mv {
                reduction -= 3281;
            }

            if !NODE::PV && td.stack[ply - 1].reduction > reduction + 562 {
                reduction += 130;
            }

            reduction += helper_reduction_bias(td);

            let reduced_depth = new_depth - (reduction >= 2864) as i32 - (reduction >= 5585) as i32;

            score = -search::<NonPV>(td, -alpha - 1, -alpha, reduced_depth, !cut_node, ply + 1);
            current_search_count += 1;
        }

        // Principal Variation Search (PVS)
        if NODE::PV && (move_count == 1 || score > alpha) {
            if mv == tt_probe.mv && tt_probe.depth > 1 && td.root_depth > 8 {
                new_depth = new_depth.max(1);
            }

            score = -search::<PV>(td, -beta, -alpha, new_depth, false, ply + 1);
            current_search_count += 1;
        }

        undo_move(td, mv);

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if NODE::ROOT {
            let current_nodes = td.nodes();
            let root_move = td.root_moves.iter_mut().find(|v| v.mv == mv).unwrap();

            root_move.nodes += current_nodes - initial_nodes;

            if move_count == 1 || score > alpha {
                root_move.upperbound = false;
                root_move.lowerbound = false;
                match score {
                    v if v <= alpha => {
                        root_move.display_score = alpha;
                        root_move.upperbound = true;
                    }
                    v if v >= beta => {
                        root_move.display_score = beta;
                        root_move.lowerbound = true;
                    }
                    _ => {
                        root_move.display_score = score;
                    }
                }

                root_move.score = score;
                root_move.sel_depth = td.sel_depth;
                root_move.pv.commit_full_root_pv(&td.pv_table, 1);

                if move_count > 1 && td.pv_index == 0 {
                    td.best_move_changes += 1;
                }
            } else {
                root_move.score = -Score::INFINITE;
            }
        }

        if mv == tt_probe.mv {
            tt_move_score = score;
        }

        if score > best_score {
            best_score = score;

            if score > alpha {
                bound = Bound::Exact;
                best_move = mv;

                if !NODE::ROOT && NODE::PV {
                    td.pv_table.update(ply as usize, mv);
                }

                if score >= beta {
                    bound = Bound::Lower;
                    td.stack[ply].cutoff_count += 1;
                    break;
                }

                alpha = score;

                if !(NODE::ROOT && td.pv_index > 0) && mv != tt_probe.mv {
                    td.shared.tt.write(hash, depth, raw_eval, score, Bound::Lower, mv, ply, true, false);
                }

                if !is_decisive(score) {
                    alpha_raises += 1;
                }
            }
        }

        if mv != best_move && move_count < 32 {
            if is_quiet {
                quiet_moves.push(mv);
            } else {
                noisy_moves.push(mv);
            }
        }
    }

    if move_count == 0 {
        if excluded {
            return -Score::TB_WIN_IN_MAX + 1;
        }

        return if in_check { mated_in(ply) } else { draw(td) };
    }

    update_node_histories(
        td,
        ply,
        depth,
        cut_node,
        NODE::ROOT,
        stm,
        bound,
        best_move,
        best_score,
        beta,
        current_search_count,
        &quiet_moves,
        &noisy_moves,
        in_check,
        eval,
    );

    let parent_tt_pv = !NODE::ROOT && td.stack[ply - 1].tt_pv;
    tt_pv = propagate_tt_pv(tt_pv, NODE::ROOT, bound, move_count, parent_tt_pv);
    best_score = scale_beta_cutoff_score(best_score, NODE::ROOT, beta, alpha, depth);

    #[cfg(feature = "syzygy")]
    if NODE::PV {
        best_score = best_score.min(max_score);
    }

    if should_write_tt(excluded, NODE::ROOT, td.pv_index) {
        td.shared.tt.write(hash, depth, raw_eval, best_score, bound, best_move, ply, tt_pv, NODE::PV);
    }

    if should_update_correction_history(in_check, best_move, bound, best_score, eval) {
        update_correction_histories(td, depth, best_score - eval, ply);
    }

    debug_assert!(alpha < beta);
    debug_assert!(-Score::INFINITE < best_score && best_score < Score::INFINITE);

    best_score
}

fn helper_reduction_bias(td: &ThreadData) -> i32 {
    if td.id == 0 {
        return 0;
    }

    match td.id % 4 {
        1 => -96,
        2 => 96,
        3 => -48,
        _ => 48,
    }
}

fn make_move(td: &mut ThreadData, ply: isize, mv: Move) {
    td.stack[ply].mv = mv;
    td.stack[ply].piece = td.board.moved_piece(mv);
    td.stack[ply].conthist =
        td.continuation_history.subtable_ptr(td.board.in_check(), mv.is_noisy(), td.board.moved_piece(mv), mv.to());
    td.stack[ply].contcorrhist =
        td.continuation_corrhist.subtable_ptr(td.board.in_check(), mv.is_noisy(), td.board.moved_piece(mv), mv.to());

    td.shared.nodes.increment(td.id);

    td.nnue.push(mv, &td.board);
    td.board.make_move(mv, &mut td.nnue);

    td.shared.tt.prefetch(td.board.hash());
}

fn undo_move(td: &mut ThreadData, mv: Move) {
    td.nnue.pop();
    td.board.undo_move(mv);
}
