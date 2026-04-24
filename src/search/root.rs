//! Root search and iterative-deepening control.
//!
//! Root search owns the parts of search that only exist at ply zero: legal root move groups,
//! MultiPV slots, aspiration retries, UCI reporting state, and time-management feedback. It
//! deliberately delegates interior alpha-beta decisions to the full-width driver.
//!
//! Tablebase root rank, root-move sorting, display bounds, and soft-stop voting are behavioral.
//! Keep their ordering visible when extracting helpers. This module does not own interior node
//! pruning, reductions, or TT/eval interpretation; those belong to the full-width search concepts.

use std::sync::atomic::Ordering;

use crate::{
    stack::Stack,
    thread::{RootMove, ThreadData},
    time::Limits,
    types::{MAX_PLY, Score, is_loss},
};

use super::{Root, search};

/// Run root iterative deepening for one worker thread.
///
/// Root search owns the ply-zero lifecycle: initialize root-local state, iterate depths, search
/// each MultiPV slot with aspiration retries, report completed depths, and feed stability back into
/// time management. Interior alpha-beta behavior belongs to `full::search`.
pub fn start(td: &mut ThreadData, report: Report, thread_count: usize) {
    td.completed_depth = 0;

    td.pv_table.clear(0);
    td.nnue.full_refresh(&td.board);

    td.multi_pv = td.multi_pv.min(td.root_moves.len());

    let mut average = vec![td.previous_best_score; td.multi_pv];
    let mut progress = RootProgress::new();

    for depth in 1..MAX_PLY as i32 {
        if td.id == 0
            && let Limits::Depth(maximum) = td.time_manager.limits()
            && depth > maximum
        {
            td.stop_search();
            break;
        }
        progress.start_depth();

        td.sel_depth = 0;
        td.root_depth = depth;
        td.best_move_changes = 0;

        td.pv_start = 0;
        td.pv_end = 0;

        for rm in &mut td.root_moves {
            rm.start_depth();
        }

        let mut retry = RootRetryState::new();

        for index in 0..td.multi_pv {
            td.pv_index = index;

            search_multipv_slot(td, report, depth, &mut average, &mut retry);
        }

        if !td.is_stopped() {
            td.completed_depth = depth;
        }

        if should_report_depth(td, report) {
            td.print_uci_info(depth);
        }

        if progress.finish_depth(td, average[td.pv_index], thread_count) {
            break;
        }
    }

    if report == Report::Minimal {
        td.print_uci_info(td.root_depth);
    }

    td.previous_best_score = td.root_moves[0].score;
}

/// UCI reporting mode for this root worker.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Report {
    /// Do not print root UCI `info` from this worker.
    ///
    /// Helper threads use this while still contributing search effort and possible final best-move
    /// voting.
    None,

    /// Print only the final minimal root information.
    ///
    /// This is used when search stops before normal full reporting should be emitted but the GUI
    /// still needs a final coherent line.
    Minimal,

    /// Print normal iterative-deepening and aspiration retry information.
    Full,
}

/// Root alpha-beta window for one MultiPV slot.
///
/// Iterative deepening expects most root scores to stay near the previous iteration. Starting with
/// a narrow window makes the common case cheaper, but the retry state must carry both `delta` and
/// root depth reduction across fail-low/fail-high attempts. The caller keeps the retry loop visible
/// because root sorting and UCI reporting happen between attempts.
struct AspirationWindow {
    /// Lower root search bound for the current retry.
    alpha: i32,

    /// Upper root search bound for the current retry.
    beta: i32,

    /// Current aspiration half-width carried across retry failures.
    delta: i32,

    /// Root-depth reduction used while the window is still narrow.
    reduction: i32,
}

impl AspirationWindow {
    fn new(average: i32, mut delta: i32, reduction: i32) -> Self {
        delta += average * average / 25833;

        Self {
            alpha: (average - delta).max(-Score::INFINITE),
            beta: (average + delta).min(Score::INFINITE),
            delta,
            reduction,
        }
    }

    fn search_depth(&self, depth: i32) -> i32 {
        (depth - self.reduction).max(1)
    }

    fn fail_low(&mut self, score: i32) {
        self.beta = (3 * self.alpha + self.beta) / 4;
        self.alpha = (score - self.delta).max(-Score::INFINITE);
        self.reduction = 0;
        self.delta += 28 * self.delta / 128;
    }

    fn fail_high(&mut self, score: i32) {
        self.alpha = (self.beta - self.delta).max(self.alpha);
        self.beta = (score + self.delta).min(Score::INFINITE);
        self.reduction += 1;
        self.delta += 62 * self.delta / 128;
    }
}

/// Time-management feedback accumulated across completed root depths.
///
/// This is root-only state. It tracks whether the PV, score, and best move are stable enough to
/// spend less time, or volatile enough to keep searching. It also owns this thread's soft-stop vote
/// so repeated limit checks do not double-count votes in shared state.
struct RootProgress {
    /// Last stable best move, restored when a stopped losing line would otherwise replace it.
    last_best_rootmove: RootMove,

    /// Number of consecutive depths with a score close to the rolling average.
    eval_stability: i32,

    /// Number of consecutive depths with the same best root move.
    pv_stability: i32,

    /// Smoothed count of best-move changes, used to spend more time on volatile positions.
    best_move_changes: usize,

    /// Whether this thread currently owns one shared soft-stop vote.
    soft_stop_voted: bool,
}

impl RootProgress {
    fn new() -> Self {
        Self {
            last_best_rootmove: RootMove::default(),
            eval_stability: 0,
            pv_stability: 0,
            best_move_changes: 0,
            soft_stop_voted: false,
        }
    }

    fn start_depth(&mut self) {
        self.best_move_changes /= 2;
    }

    fn finish_depth(&mut self, td: &mut ThreadData, average_score: i32, thread_count: usize) -> bool {
        self.update_stability(td, average_score);
        self.update_last_best_rootmove(td);

        if td.is_stopped() {
            return true;
        }

        self.vote_soft_stop(td, thread_count);

        td.is_stopped()
    }

    fn update_stability(&mut self, td: &ThreadData, average_score: i32) {
        if (td.root_moves[0].score - average_score).abs() < 12 {
            self.eval_stability += 1;
        } else {
            self.eval_stability = 0;
        }

        if self.last_best_rootmove.mv == td.root_moves[0].mv {
            self.pv_stability += 1;
        } else {
            self.pv_stability = 0;
        }

        self.best_move_changes += td.best_move_changes;
    }

    fn update_last_best_rootmove(&mut self, td: &mut ThreadData) {
        if td.root_moves[0].score != -Score::INFINITE && is_loss(td.root_moves[0].score) && td.is_stopped() {
            if let Some(pos) = td.root_moves.iter().position(|rm| rm.mv == self.last_best_rootmove.mv) {
                td.root_moves.remove(pos);
                td.root_moves.insert(0, self.last_best_rootmove.clone());
            }
        } else {
            self.last_best_rootmove = td.root_moves[0].clone();
        }
    }

    fn vote_soft_stop(&mut self, td: &mut ThreadData, thread_count: usize) {
        if td.time_manager.soft_limit(td, || self.time_multiplier(td)) {
            if !self.soft_stop_voted {
                self.soft_stop_voted = true;

                let votes = td.shared.soft_stop_votes.fetch_add(1, Ordering::AcqRel) + 1;
                let majority = (thread_count * 65).div_ceil(100);
                if votes >= majority {
                    td.stop_search();
                }
            }
        } else if self.soft_stop_voted {
            self.soft_stop_voted = false;
            td.shared.soft_stop_votes.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn time_multiplier(&self, td: &ThreadData) -> f32 {
        let nodes_factor = (2.7168 - 2.2669 * (td.root_moves[0].nodes as f32 / td.nodes() as f32)).max(0.5630_f32);
        let pv_stability = (1.25 - 0.05 * self.pv_stability as f32).max(0.85);
        let eval_stability = (1.2 - 0.04 * self.eval_stability as f32).max(0.88);
        let score_trend = (0.8 + 0.05 * (td.previous_best_score - td.root_moves[0].score) as f32).clamp(0.80, 1.45);
        let best_move_stability = 1.0 + self.best_move_changes as f32 / 4.0;

        nodes_factor * pv_stability * eval_stability * score_trend * best_move_stability
    }
}

/// Aspiration retry state shared across MultiPV slots at one depth.
///
/// `delta` and `reduction` carry successful retry behavior forward so the next slot starts with a
/// window shape informed by the previous slot.
struct RootRetryState {
    /// Aspiration half-width reused by the next MultiPV slot.
    delta: i32,

    /// Root-depth reduction reused by the next MultiPV slot.
    reduction: i32,
}

impl RootRetryState {
    const fn new() -> Self {
        Self { delta: 15, reduction: 0 }
    }
}

/// Search one MultiPV slot at the current root depth.
///
/// This owns the root-only sequence for a slot: select the current tablebase rank group, initialize
/// the aspiration window, run retry searches, sort root moves in the legal reporting ranges, and
/// update the rolling average on a successful in-window result.
fn search_multipv_slot(
    td: &mut ThreadData, report: Report, depth: i32, average: &mut [i32], retry: &mut RootRetryState,
) {
    advance_root_pv_group(td);

    let mut window = AspirationWindow::new(average[td.pv_index], retry.delta, retry.reduction);

    set_root_optimism(td, average[td.pv_index]);

    loop {
        td.stack = Stack::new();
        td.root_delta = window.beta - window.alpha;

        let score = search::<Root>(td, window.alpha, window.beta, window.search_depth(depth), false, 0);

        td.root_moves[td.pv_index..td.pv_end].sort_by_key(|rm| std::cmp::Reverse(rm.score));

        if td.is_stopped() {
            break;
        }

        match score {
            s if s <= window.alpha => window.fail_low(score),
            s if s >= window.beta => window.fail_high(score),
            _ => {
                average[td.pv_index] =
                    if average[td.pv_index] == Score::NONE { score } else { (average[td.pv_index] + score) / 2 };

                td.shared.best_stats[td.pv_index]
                    .fetch_max(((depth as u32) << 16) | (average[td.pv_index] + 32768) as u32, Ordering::AcqRel);

                retry.delta = window.delta;
                retry.reduction = window.reduction;
                break;
            }
        }

        retry.delta = window.delta;
        retry.reduction = window.reduction;

        td.root_moves[td.pv_start..=td.pv_index].sort_by_key(|rm| std::cmp::Reverse(rm.score));

        if should_report_aspiration_retry(td, report) {
            td.print_uci_info(depth);
        }
    }
}

/// Advance `pv_start..pv_end` to the next root tablebase-rank group.
///
/// MultiPV searches must not let a lower-ranked tablebase move displace an unsearched higher-ranked
/// move. The active root slice therefore advances by equal TB rank, and root sorting is restricted
/// to the searched prefix plus the current rank group.
fn advance_root_pv_group(td: &mut ThreadData) {
    if td.pv_index != td.pv_end {
        return;
    }

    td.pv_start = td.pv_end;
    while td.pv_end < td.root_moves.len() {
        if !td.root_moves[td.pv_end].same_tablebase_group(&td.root_moves[td.pv_start]) {
            break;
        }
        td.pv_end += 1;
    }
}

/// Set root optimism from this MultiPV slot's rolling score estimate.
///
/// Root optimism biases eval for the current side during the next root search. It is intentionally
/// updated immediately before the slot search so aspiration retries see the same optimism value.
fn set_root_optimism(td: &mut ThreadData, average_score: i32) {
    let best_avg =
        ((td.shared.best_stats[td.pv_index].load(Ordering::Acquire) & 0xffff) as i32 - 32768 + average_score) / 2;

    td.optimism[td.board.side_to_move()] = 159 * best_avg / (best_avg.abs() + 186);
    td.optimism[!td.board.side_to_move()] = -td.optimism[td.board.side_to_move()];
}

/// Whether an aspiration retry is expensive enough to report.
///
/// Reporting inside the retry loop is intentionally delayed until enough nodes have been spent.
/// This keeps GUIs informed during long fail-low/fail-high re-searches without turning cheap
/// aspiration misses into noisy UCI output.
fn should_report_aspiration_retry(td: &ThreadData, report: Report) -> bool {
    report == Report::Full && td.shared.nodes.aggregate() > 10_000_000
}

/// Whether the completed root depth is reportable to UCI.
///
/// Reporting waits for either a stop, the last MultiPV slot, or enough searched nodes. This avoids
/// noisy partial-depth output while still keeping GUIs updated during long searches.
fn should_report_depth(td: &ThreadData, report: Report) -> bool {
    report == Report::Full
        && !(is_loss(td.root_moves[0].display_score) && td.is_stopped())
        && (td.is_stopped() || td.pv_index + 1 == td.multi_pv || td.shared.nodes.aggregate() > 10_000_000)
}
