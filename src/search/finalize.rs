//! Full-width node finalization rules.
//!
//! Once the move loop has produced a bound and best move, search still has a few result-shaping
//! steps: propagate PV information, damp non-decisive beta cutoffs, decide whether the TT owns this
//! result, and decide whether static eval correction should learn from it. This module does not own
//! child search; it owns the ordered feedback and writeback that happen once child search has
//! produced a final move-loop result.

use crate::{
    thread::ThreadData,
    transposition::Bound,
    types::{ArrayVec, Color, Move, Score, draw, is_decisive, mated_in},
};

use super::{
    NodeType,
    eval::update_correction_histories,
    history::{HistoryUpdateContext, update_node_histories},
    moves::MoveLoopResult,
};

/// Inputs needed to finish a full-width node after child search.
///
/// This is the contract between the ordered move loop and post-loop feedback. It keeps finalization
/// from taking a long scalar list while still making each dependency explicit at the call site.
pub struct NodeFinalizationInput<'a> {
    /// Node and window facts for final feedback.
    pub node: FinalizationNode,

    /// Eval and TT writeback facts for this node result.
    pub proof: FinalizationProof,

    /// Searched alternatives consumed by history feedback.
    pub searched: SearchedMoveLists<'a>,

    /// Final result of the ordered child move loop.
    pub move_loop: MoveLoopResult,

    /// Syzygy upper cap for PV nodes after a non-cutoff tablebase probe.
    #[cfg(feature = "syzygy")]
    pub max_score: i32,
}

/// Node and window facts used by post-loop feedback.
#[derive(Copy, Clone)]
pub struct FinalizationNode {
    /// Current ply whose stack entry and PV state are being finalized.
    pub ply: isize,

    /// Remaining depth used for history bonuses, score shaping, and TT write.
    pub depth: i32,

    /// Whether the node had cut-node shape on entry.
    pub cut_node: bool,

    /// Side to move at this node, used by history feedback.
    pub stm: Color,

    /// Whether this node searched an excluded move set for singular verification and must avoid
    /// ordinary final TT ownership.
    pub excluded: bool,

    /// Whether the node was in check, controlling no-move and correction history behavior.
    pub in_check: bool,

    /// Upper search window bound used for beta-cutoff shaping and history.
    pub beta: i32,
}

impl FinalizationNode {
    /// Score for a node where the move loop searched no legal moves.
    #[inline]
    fn no_move_score(self, td: &ThreadData) -> i32 {
        if self.excluded {
            return -Score::TB_WIN_IN_MAX + 1;
        }

        if self.in_check { mated_in(self.ply) } else { draw(td) }
    }

    /// Parent TT-PV marker used when propagating upper-bound PV status.
    #[inline]
    fn parent_tt_pv<NODE: NodeType>(self, td: &ThreadData) -> bool {
        !NODE::ROOT && td.stack[self.ply - 1].tt_pv
    }

    /// Damp non-decisive fail-soft beta cutoffs before storing the result.
    ///
    /// The raw cutoff can be overly optimistic. Blending it toward beta gives TT and history
    /// consumers a more conservative score while leaving decisive scores and root scores untouched.
    #[inline]
    fn scale_beta_cutoff_score<NODE: NodeType>(self, best_score: i32, alpha: i32) -> i32 {
        if NODE::ROOT || best_score < self.beta || is_decisive(best_score) || is_decisive(alpha) {
            return best_score;
        }

        let weight = self.depth.min(8);
        (best_score * weight + self.beta) / (weight + 1)
    }

    /// Whether this node owns a final TT write.
    ///
    /// Excluded singular-verification nodes and secondary root MultiPV slots avoid writing final
    /// entries because their search window or move set is not the ordinary position contract.
    #[inline]
    fn should_write_final_result<NODE: NodeType>(self, pv_index: usize) -> bool {
        !(self.excluded || NODE::ROOT && pv_index > 0)
    }
}

/// Eval and TT writeback facts for finalizing a full-width node.
#[derive(Copy, Clone)]
pub struct FinalizationProof {
    /// TT-PV marker entering finalization; this may be widened before TT writeback.
    pub tt_pv: bool,

    /// Position hash for final TT writeback.
    pub hash: u64,

    /// Raw NNUE eval stored with the TT entry.
    pub raw_eval: i32,

    /// Corrected static eval used by history and correction-history feedback.
    pub eval: i32,
}

impl FinalizationProof {
    /// Propagate TT-PV status from a failed interior node.
    ///
    /// Upper-bound nodes with enough searched moves can inherit the parent PV mark. This happens
    /// after the move loop because the final bound and move count are needed, and before TT
    /// writeback because the TT entry stores the marker.
    #[inline]
    fn propagate_tt_pv<NODE: NodeType>(self, bound: Bound, move_count: i32, parent_tt_pv: bool) -> bool {
        self.tt_pv || !NODE::ROOT && bound == Bound::Upper && move_count > 2 && parent_tt_pv
    }

    /// Store the final full-width result after history, TT-PV propagation, and score shaping.
    ///
    /// This is the ordinary full-width TT ownership point. Earlier writes publish partial proofs
    /// such as ProbCut or alpha raises; this write stores the final bound and best move for the
    /// completed move set.
    #[inline(always)]
    fn write_final_result<NODE: NodeType>(
        self, td: &mut ThreadData, node: FinalizationNode, best_score: i32, bound: Bound, best_move: Move, tt_pv: bool,
    ) {
        td.shared.tt.write(
            self.hash,
            node.depth,
            self.raw_eval,
            best_score,
            bound,
            best_move,
            node.ply,
            tt_pv,
            NODE::PV,
        );
    }

    /// Whether the final score is a trustworthy correction-history target.
    ///
    /// Correction history learns from quiet positions where the final score contradicts static eval
    /// in the useful direction. Checks, captures, and bound results that do not improve on eval are
    /// excluded.
    #[inline]
    fn should_update_correction_history(
        self, node: FinalizationNode, best_move: Move, bound: Bound, best_score: i32,
    ) -> bool {
        !(node.in_check
            || best_move.is_noisy()
            || (bound == Bound::Upper && best_score >= self.eval)
            || (bound == Bound::Lower && best_score <= self.eval))
    }
}

/// Searched move lists used by post-loop history feedback.
#[derive(Copy, Clone)]
pub struct SearchedMoveLists<'a> {
    /// Quiet moves searched before the best move, used for history maluses.
    pub quiet_moves: &'a ArrayVec<Move, 32>,

    /// Noisy moves searched before the best move, used for history maluses.
    pub noisy_moves: &'a ArrayVec<Move, 32>,
}

/// Finish a full-width node after the ordered move loop.
///
/// This is deliberately one post-loop concept because the steps are ordered by data dependency: the
/// move loop establishes the final bound, history consumes the winning and failed move lists, TT-PV
/// shaping must happen before TT writeback, and correction history learns only from the final
/// stored score.
#[inline(always)]
pub fn finish_full_width_node<NODE: NodeType>(td: &mut ThreadData, input: NodeFinalizationInput<'_>) -> i32 {
    let NodeFinalizationInput {
        node,
        proof,
        searched,
        move_loop,
        #[cfg(feature = "syzygy")]
        max_score,
    } = input;

    let alpha = move_loop.alpha;
    let mut best_score = move_loop.best_score;
    let best_move = move_loop.best_move;
    let bound = move_loop.bound;
    let move_count = move_loop.move_count;

    if move_count == 0 {
        return node.no_move_score(td);
    }

    update_node_histories(
        td,
        HistoryUpdateContext {
            ply: node.ply,
            depth: node.depth,
            cut_node: node.cut_node,
            node_root: NODE::ROOT,
            stm: node.stm,
            bound,
            best_move,
            best_score,
            beta: node.beta,
            current_search_count: move_loop.current_search_count,
            quiet_moves: searched.quiet_moves,
            noisy_moves: searched.noisy_moves,
            in_check: node.in_check,
            eval: proof.eval,
        },
    );

    let tt_pv = proof.propagate_tt_pv::<NODE>(bound, move_count, node.parent_tt_pv::<NODE>(td));
    best_score = node.scale_beta_cutoff_score::<NODE>(best_score, alpha);

    #[cfg(feature = "syzygy")]
    if NODE::PV {
        best_score = best_score.min(max_score);
    }

    if node.should_write_final_result::<NODE>(td.pv_index) {
        proof.write_final_result::<NODE>(td, node, best_score, bound, best_move, tt_pv);
    }

    if proof.should_update_correction_history(node, best_move, bound, best_score) {
        update_correction_histories(td, node.depth, best_score - proof.eval, node.ply);
    }

    debug_assert!(alpha < node.beta);
    debug_assert!(-Score::INFINITE < best_score && best_score < Score::INFINITE);

    best_score
}
