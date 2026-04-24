use std::sync::atomic::Ordering;

use crate::{
    stack::Stack,
    thread::{RootMove, Status, ThreadData},
    time::Limits,
    types::{MAX_PLY, Score, is_loss},
};

use super::{Root, search};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Report {
    None,
    Minimal,
    Full,
}

pub fn start(td: &mut ThreadData, report: Report, thread_count: usize) {
    td.completed_depth = 0;

    td.pv_table.clear(0);
    td.nnue.full_refresh(&td.board);

    td.multi_pv = td.multi_pv.min(td.root_moves.len());

    let mut average = vec![td.previous_best_score; td.multi_pv];
    let mut last_best_rootmove = RootMove::default();

    let mut eval_stability = 0;
    let mut pv_stability = 0;
    let mut best_move_changes = 0;
    let mut soft_stop_voted = false;

    // Iterative Deepening
    for depth in 1..MAX_PLY as i32 {
        if td.id == 0
            && let Limits::Depth(maximum) = td.time_manager.limits()
            && depth > maximum
        {
            td.shared.status.set(Status::STOPPED);
            break;
        }
        best_move_changes /= 2;

        td.sel_depth = 0;
        td.root_depth = depth;
        td.best_move_changes = 0;

        td.pv_start = 0;
        td.pv_end = 0;

        for rm in &mut td.root_moves {
            rm.previous_score = rm.score;
        }

        let mut delta = 15;
        let mut reduction = 0;

        for index in 0..td.multi_pv {
            td.pv_index = index;

            if td.pv_index == td.pv_end {
                td.pv_start = td.pv_end;
                while td.pv_end < td.root_moves.len() {
                    if td.root_moves[td.pv_end].tb_rank != td.root_moves[td.pv_start].tb_rank {
                        break;
                    }
                    td.pv_end += 1;
                }
            }

            // Aspiration Windows
            delta += average[td.pv_index] * average[td.pv_index] / 25833;

            let mut alpha = (average[td.pv_index] - delta).max(-Score::INFINITE);
            let mut beta = (average[td.pv_index] + delta).min(Score::INFINITE);

            let best_avg = ((td.shared.best_stats[td.pv_index].load(Ordering::Acquire) & 0xffff) as i32 - 32768
                + average[td.pv_index])
                / 2;
            td.optimism[td.board.side_to_move()] = 159 * best_avg / (best_avg.abs() + 186);
            td.optimism[!td.board.side_to_move()] = -td.optimism[td.board.side_to_move()];

            loop {
                td.stack = Stack::new();
                td.root_delta = beta - alpha;

                // Root Search
                let score = search::<Root>(td, alpha, beta, (depth - reduction).max(1), false, 0);

                td.root_moves[td.pv_index..td.pv_end].sort_by_key(|rm| std::cmp::Reverse(rm.score));

                if td.shared.status.get() == Status::STOPPED {
                    break;
                }

                match score {
                    s if s <= alpha => {
                        beta = (3 * alpha + beta) / 4;
                        alpha = (score - delta).max(-Score::INFINITE);
                        reduction = 0;
                        delta += 28 * delta / 128;
                    }
                    s if s >= beta => {
                        alpha = (beta - delta).max(alpha);
                        beta = (score + delta).min(Score::INFINITE);
                        reduction += 1;
                        delta += 62 * delta / 128;
                    }
                    _ => {
                        average[td.pv_index] = if average[td.pv_index] == Score::NONE {
                            score
                        } else {
                            (average[td.pv_index] + score) / 2
                        };

                        td.shared.best_stats[td.pv_index].fetch_max(
                            ((depth as u32) << 16) | (average[td.pv_index] + 32768) as u32,
                            Ordering::AcqRel,
                        );

                        break;
                    }
                }

                td.root_moves[td.pv_start..=td.pv_index].sort_by_key(|rm| std::cmp::Reverse(rm.score));

                if report == Report::Full && td.shared.nodes.aggregate() > 10_000_000 {
                    td.print_uci_info(depth);
                }
            }
        }

        if td.shared.status.get() != Status::STOPPED {
            td.completed_depth = depth;
        }

        if report == Report::Full
            && !(is_loss(td.root_moves[0].display_score) && td.shared.status.get() == Status::STOPPED)
            && (td.shared.status.get() == Status::STOPPED
                || td.pv_index + 1 == td.multi_pv
                || td.shared.nodes.aggregate() > 10_000_000)
        {
            td.print_uci_info(depth);
        }

        if (td.root_moves[0].score - average[td.pv_index]).abs() < 12 {
            eval_stability += 1;
        } else {
            eval_stability = 0;
        }

        if last_best_rootmove.mv == td.root_moves[0].mv {
            pv_stability += 1;
        } else {
            pv_stability = 0;
        }

        best_move_changes += td.best_move_changes;

        if td.root_moves[0].score != -Score::INFINITE
            && is_loss(td.root_moves[0].score)
            && td.shared.status.get() == Status::STOPPED
        {
            if let Some(pos) = td.root_moves.iter().position(|rm| rm.mv == last_best_rootmove.mv) {
                td.root_moves.remove(pos);
                td.root_moves.insert(0, last_best_rootmove.clone());
            }
        } else {
            last_best_rootmove = td.root_moves[0].clone();
        }

        if td.shared.status.get() == Status::STOPPED {
            break;
        }

        let multiplier = || {
            let nodes_factor = (2.7168 - 2.2669 * (td.root_moves[0].nodes as f32 / td.nodes() as f32)).max(0.5630_f32);

            let pv_stability = (1.25 - 0.05 * pv_stability as f32).max(0.85);

            let eval_stability = (1.2 - 0.04 * eval_stability as f32).max(0.88);

            let score_trend = (0.8 + 0.05 * (td.previous_best_score - td.root_moves[0].score) as f32).clamp(0.80, 1.45);

            let best_move_stability = 1.0 + best_move_changes as f32 / 4.0;

            nodes_factor * pv_stability * eval_stability * score_trend * best_move_stability
        };

        if td.time_manager.soft_limit(td, multiplier) {
            if !soft_stop_voted {
                soft_stop_voted = true;

                let votes = td.shared.soft_stop_votes.fetch_add(1, Ordering::AcqRel) + 1;
                let majority = (thread_count * 65).div_ceil(100);
                if votes >= majority {
                    td.shared.status.set(Status::STOPPED);
                }
            }
        } else if soft_stop_voted {
            soft_stop_voted = false;
            td.shared.soft_stop_votes.fetch_sub(1, Ordering::AcqRel);
        }

        if td.shared.status.get() == Status::STOPPED {
            break;
        }
    }

    if report == Report::Minimal {
        td.print_uci_info(td.root_depth);
    }

    td.previous_best_score = td.root_moves[0].score;
}
