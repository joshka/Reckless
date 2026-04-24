//! Quiescence search.
//!
//! Qsearch stabilizes depth-zero leaves by searching tactical continuations instead of all legal
//! moves. It has a stand-pat contract when not in check, a shallow TT policy, and a much smaller
//! history footprint than full-width search.
//!
//! Do not pull full-width pruning or reduction assumptions into this module without making the
//! qsearch-specific contract explicit. This module does not own full-width depth policy, singular
//! search, or root reporting; it owns leaf stabilization once full-width depth is exhausted.

use crate::{
    evaluation::correct_eval,
    movepick::MovePicker,
    thread::ThreadData,
    transposition::{Bound, TtDepth},
    types::{Color, MAX_PLY, Move, Score, draw, is_decisive, is_loss, is_valid, mated_in},
};

use super::{NodeType, eval::eval_correction, make_move, tt, undo_move};

/// Search tactical continuations after full-width depth is exhausted.
///
/// Qsearch keeps the alpha-beta contract but replaces normal move generation with stand-pat eval,
/// shallow TT interpretation, captures, checks, and evasions. It deliberately does not use
/// full-width pruning, reductions, singular search, or root reporting.
pub fn qsearch<NODE: NodeType>(td: &mut ThreadData, mut alpha: i32, beta: i32, ply: isize) -> i32 {
    let node = match enter_qsearch::<NODE>(td, &mut alpha, beta, ply) {
        QsearchEntry::Return(score) => return score,
        QsearchEntry::Continue(node) => node,
    };

    // Stand-pat eval: only non-check nodes can use static eval as a lower bound.
    let eval = QsearchEval::compute(td, node);
    let raw_eval = eval.raw;
    let correction_value = eval.correction;
    if eval.best_score >= beta {
        let best_score = stand_pat_cutoff_score(eval.best_score, beta);

        if !node.tt_probe.has_entry() {
            node.write_stand_pat_lower_bound(td, raw_eval, best_score);
        }

        return best_score;
    }

    let mut state = QsearchState::new(alpha, beta, eval.best_score);
    let mut move_picker = MovePicker::new_qsearch();

    // Tactical move loop: search captures, checks, and necessary evasions only.
    while let Some(mv) = move_picker.next::<NODE>(td, state.skips_quiets(node), node.ply) {
        state.begin_candidate();

        if state.late_move_prunes(td, mv) {
            break;
        }

        if state.see_prunes(td, mv, eval.corrected, correction_value) {
            continue;
        }

        make_move(td, node.ply, mv);
        let score = -qsearch::<NODE>(td, -state.beta, -state.alpha, node.ply + 1);
        undo_move(td, mv);

        if td.is_stopped() {
            return Score::ZERO;
        }

        if state.accept_child::<NODE>(td, node, mv, score).is_beta_cutoff() {
            break;
        }
    }

    if node.in_check && state.has_no_legal_moves() {
        return mated_in(node.ply);
    }

    state.update_cutoff_history(td, node);
    state.shape_beta_cutoff_score();

    node.write_final_bound(td, raw_eval, &state);

    debug_assert!(state.alpha < state.beta);
    debug_assert!(-Score::INFINITE < state.best_score && state.best_score < Score::INFINITE);

    state.best_score
}

/// Qsearch entry result after terminal guards and shallow TT proof.
enum QsearchEntry {
    /// A guard or TT proof determined the qsearch score.
    Return(i32),

    /// Qsearch should continue with stand-pat eval and tactical moves.
    Continue(QsearchNode),
}

/// Stable qsearch node facts after entry guards.
#[derive(Copy, Clone)]
struct QsearchNode {
    /// Current qsearch ply.
    ply: isize,

    /// Side to move, used by quiet-history feedback on qsearch cutoffs.
    stm: Color,

    /// Whether the side to move is in check and must search evasions.
    in_check: bool,

    /// Whether this qsearch node is on the principal variation.
    node_pv: bool,

    /// Position hash used for shallow TT writeback.
    hash: u64,

    /// Shallow TT probe for qsearch bound use and hash move ordering.
    tt_probe: tt::TtProbe,

    /// TT-PV marker copied into shallow qsearch writes.
    tt_pv: bool,
}

/// Enter qsearch and handle guards that precede stand-pat eval.
///
/// Qsearch does not run full-width proof, pruning, singular search, or depth policy. Its entry
/// phase only maintains the alpha-beta contract, PV bookkeeping, draw/max-ply guards, time polling,
/// and the shallow TT proof used before static eval.
#[inline(always)]
fn enter_qsearch<NODE: NodeType>(td: &mut ThreadData, alpha: &mut i32, beta: i32, ply: isize) -> QsearchEntry {
    debug_assert!(!NODE::ROOT);
    debug_assert!(ply as usize <= MAX_PLY);
    debug_assert!(-Score::INFINITE <= *alpha && *alpha < beta && beta <= Score::INFINITE);
    debug_assert!(NODE::PV || *alpha == beta - 1);

    let draw_score = draw(td);
    if *alpha < draw_score && td.board.upcoming_repetition(ply as usize) {
        *alpha = draw_score;
        if *alpha >= beta {
            return QsearchEntry::Return(*alpha);
        }
    }

    let stm = td.board.side_to_move();
    let in_check = td.board.in_check();

    if NODE::PV {
        td.pv_table.clear(ply as usize);
        td.sel_depth = td.sel_depth.max(ply as i32);
    }

    if td.id == 0 && td.time_manager.check_time(td) {
        td.stop_search();
        return QsearchEntry::Return(Score::ZERO);
    }

    if td.board.is_draw(ply) {
        return QsearchEntry::Return(draw(td));
    }

    if ply as usize >= MAX_PLY - 1 {
        let score = if in_check { draw(td) } else { td.nnue.evaluate(&td.board) };
        return QsearchEntry::Return(score);
    }

    let hash = td.board.hash();
    let tt_probe = tt::TtProbe::read(td, hash, ply, NODE::PV);
    let tt_pv = tt_probe.tt_pv;

    if tt_probe.can_cutoff_qsearch(NODE::PV, *alpha, beta) {
        return QsearchEntry::Return(tt_probe.score);
    }

    QsearchEntry::Continue(QsearchNode { ply, stm, in_check, node_pv: NODE::PV, hash, tt_probe, tt_pv })
}

impl QsearchNode {
    /// Store the stand-pat beta cutoff for a qsearch leaf.
    ///
    /// This write is only used on a TT miss. A previous entry may carry a move or bound signal from
    /// a tactical search, so stand-pat eval should not overwrite it just because static eval
    /// crossed beta.
    #[inline(always)]
    fn write_stand_pat_lower_bound(self, td: &mut ThreadData, raw_eval: i32, score: i32) {
        td.shared.tt.write(
            self.hash,
            TtDepth::SOME,
            raw_eval,
            score,
            Bound::Lower,
            Move::NULL,
            self.ply,
            self.tt_pv,
            false,
        );
    }

    /// Store the final shallow TT bound produced by qsearch.
    ///
    /// Qsearch owns only the stabilized tactical leaf value. It stores a shallow lower or upper
    /// bound with the best tactical move it found, without claiming full-width depth or PV-search
    /// ownership.
    #[inline(always)]
    fn write_final_bound(self, td: &mut ThreadData, raw_eval: i32, state: &QsearchState) {
        td.shared.tt.write(
            self.hash,
            TtDepth::SOME,
            raw_eval,
            state.best_score,
            state.bound(),
            state.best_move,
            self.ply,
            self.tt_pv,
            false,
        );
    }
}

/// Eval state for qsearch's stand-pat contract.
///
/// Qsearch only has a static stand-pat value outside check. In check, it must search evasions and
/// starts from negative infinity. A TT score can adjust the best stand-pat score, but qsearch keeps
/// the corrected eval separately for SEE pruning.
struct QsearchEval {
    /// Raw NNUE value for shallow TT storage.
    ///
    /// In-check qsearch has no stand-pat eval and leaves this as `Score::NONE`.
    raw: i32,

    /// Correction-adjusted static eval used by SEE pruning.
    ///
    /// This remains separate from `best_score` because a compatible TT score may improve stand-pat
    /// without replacing the eval baseline for SEE.
    corrected: i32,

    /// Current best stand-pat score before tactical moves.
    ///
    /// Outside check this starts from corrected eval and may be tightened by TT; in check it starts
    /// at negative infinity because evasions must be searched.
    best_score: i32,

    /// Correction-history bias used to produce `corrected`.
    ///
    /// Qsearch uses the magnitude to soften SEE pruning when static eval is less trusted.
    correction: i32,
}

impl QsearchEval {
    fn compute(td: &mut ThreadData, node: QsearchNode) -> Self {
        let correction = eval_correction(td, node.ply);

        if node.in_check {
            return Self {
                raw: Score::NONE,
                corrected: Score::NONE,
                best_score: -Score::INFINITE,
                correction,
            };
        }

        let raw =
            if is_valid(node.tt_probe.raw_eval()) { node.tt_probe.raw_eval() } else { td.nnue.evaluate(&td.board) };
        let corrected = correct_eval(td, raw, correction);
        let mut best_score = corrected;

        if node.tt_probe.can_use_qsearch_score(node.node_pv, best_score) {
            best_score = node.tt_probe.score;
        }

        Self { raw, corrected, best_score, correction }
    }
}

/// Mutable alpha-beta state for qsearch's tactical move loop.
///
/// Qsearch keeps less state than full-width search, but the same values must move together: alpha,
/// beta, best score, best move, and searched tactical count. This type owns that contract so the
/// qsearch body can read as stand-pat, tactical loop, mate/cutoff handling, and TT writeback.
struct QsearchState {
    /// Current lower bound after stand-pat and searched tactical moves.
    alpha: i32,

    /// Upper bound that ends the tactical loop on fail-high.
    beta: i32,

    /// Best qsearch score from stand-pat or tactical search.
    best_score: i32,

    /// Move that raised alpha, used for PV, history, and shallow TT storage.
    best_move: Move,

    /// Number of tactical candidates considered by qsearch.
    move_count: i32,
}

impl QsearchState {
    #[inline]
    fn new(alpha: i32, beta: i32, best_score: i32) -> Self {
        Self {
            alpha: alpha.max(best_score),
            beta,
            best_score,
            best_move: Move::NULL,
            move_count: 0,
        }
    }

    /// Whether qsearch should skip quiet moves in the current tactical state.
    #[inline]
    fn skips_quiets(&self, node: QsearchNode) -> bool {
        !((node.in_check && is_loss(self.best_score))
            || (node.tt_probe.mv.is_quiet() && node.tt_probe.bound != Bound::Upper))
    }

    #[inline]
    fn begin_candidate(&mut self) {
        self.move_count += 1;
    }

    /// Whether qsearch should stop the remaining tactical tail.
    ///
    /// This is intentionally simpler than full-width LMP: after a few non-checking tactical
    /// candidates fail to improve a non-losing stand-pat score, qsearch stops the tail rather than
    /// spending full move-loop machinery.
    #[inline]
    fn late_move_prunes(&self, td: &ThreadData, mv: Move) -> bool {
        !is_loss(self.best_score) && self.move_count >= 3 && !td.board.is_direct_check(mv)
    }

    /// Whether SEE proves this qsearch candidate cannot plausibly raise alpha.
    #[inline]
    fn see_prunes(&self, td: &ThreadData, mv: Move, eval: i32, correction: i32) -> bool {
        !is_loss(self.best_score)
            && is_valid(eval)
            && !td.board.see(mv, (self.alpha - eval) / 8 - correction.abs().min(64) - 79)
    }

    /// Fold one tactical child into qsearch alpha-beta state.
    #[inline(always)]
    fn accept_child<NODE: NodeType>(
        &mut self, td: &mut ThreadData, node: QsearchNode, mv: Move, score: i32,
    ) -> QsearchChildOutcome {
        if score <= self.best_score {
            return QsearchChildOutcome::Continue;
        }

        self.best_score = score;

        if score <= self.alpha {
            return QsearchChildOutcome::Continue;
        }

        self.best_move = mv;

        if NODE::PV {
            td.pv_table.update(node.ply as usize, mv);
        }

        if score >= self.beta {
            return QsearchChildOutcome::BetaCutoff;
        }

        self.alpha = score;
        QsearchChildOutcome::Continue
    }

    #[inline]
    fn has_no_legal_moves(&self) -> bool {
        self.move_count == 0
    }

    /// Update quiet or noisy history for a qsearch beta cutoff.
    #[inline]
    fn update_cutoff_history(&self, td: &mut ThreadData, node: QsearchNode) {
        if self.best_score < self.beta {
            return;
        }

        let is_noisy = self.best_move.is_noisy();
        let bonus = if is_noisy { 106 } else { 172 };

        if is_noisy {
            td.noisy_history.update(
                td.board.all_threats(),
                td.board.moved_piece(self.best_move),
                self.best_move.to(),
                td.board.type_on(self.best_move.to()),
                bonus,
            );
        } else {
            td.quiet_history.update(td.board.all_threats(), node.stm, self.best_move, bonus);
        }
    }

    /// Shape qsearch move cutoffs before TT storage.
    #[inline]
    fn shape_beta_cutoff_score(&mut self) {
        if self.best_score >= self.beta {
            self.best_score = beta_cutoff_score(self.best_score, self.beta);
        }
    }

    #[inline]
    fn bound(&self) -> Bound {
        if self.best_score >= self.beta { Bound::Lower } else { Bound::Upper }
    }
}

/// Whether accepting a qsearch child ended the node with a beta cutoff.
enum QsearchChildOutcome {
    /// Continue searching tactical candidates.
    Continue,

    /// Stop the loop because the child crossed beta.
    BetaCutoff,
}

impl QsearchChildOutcome {
    /// True when the caller should stop searching tactical candidates.
    #[inline]
    const fn is_beta_cutoff(&self) -> bool {
        matches!(self, Self::BetaCutoff)
    }
}

/// Shape a qsearch stand-pat beta cutoff.
///
/// Qsearch stand-pat is a static lower-bound proof, so non-decisive scores are damped toward beta
/// before TT storage. Full-width beta cutoffs use a different depth-sensitive rule in finalization.
#[inline]
fn stand_pat_cutoff_score(best_score: i32, beta: i32) -> i32 {
    if is_decisive(best_score) || is_decisive(beta) {
        return best_score;
    }

    beta + (best_score - beta) / 3
}

/// Shape a qsearch move beta cutoff before TT storage.
///
/// This is shallower than full-width beta-cutoff shaping because qsearch does not have a
/// depth-reduced child tree behind the score.
#[inline]
fn beta_cutoff_score(best_score: i32, beta: i32) -> i32 {
    if is_decisive(best_score) || is_decisive(beta) {
        return best_score;
    }

    (best_score + beta) / 2
}
