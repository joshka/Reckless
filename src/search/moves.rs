//! Full-width child move loop.
//!
//! This is the hottest and most coupled phase of full-width search. It owns move ordering, root
//! filtering, move pruning, reduced scout search, full-depth scout search, PVS, root move display
//! updates, alpha/beta updates, and searched-move buffers for later history feedback.
//!
//! The loop is intentionally extracted as one concept rather than many small helpers. Splitting
//! individual LMR/PVS/root-update branches further tends to hide tuned data flow and can make
//! codegen worse. It does not own the pruning or reduction formulas themselves; it owns when those
//! formulas are consulted while walking ordered moves.
//!
//! Read the local types as one candidate's path through the loop: `MoveLoopContext` is stable for
//! the node, `MoveLoopState` changes as candidates are searched, `MoveCandidate` names facts about
//! one ordered move, `MovePruningDecision` says whether that move reaches child search, and
//! `ChildSearchResult` reports the score/search-count pair needed by alpha-beta and history.

use crate::{
    movepick::{MovePicker, Stage},
    thread::{RootSearchResult, ThreadData},
    transposition::Bound,
    types::{ArrayVec, Color, Move, Score, is_decisive, is_loss},
};

use super::{
    NodeType, NonPV, PV, helper_reduction_bias, make_move, pruning, reductions::ReductionContext,
    search as full_search, tt::TtProbe, undo_move,
};

/// Search ordered full-width child moves for one node.
///
/// The loop owns candidate filtering, make/search/undo, reduced and PV child search, root display
/// updates, alpha-beta state, and searched-move buffers. The formulas it consults live in pruning
/// and reduction modules, but their order stays visible here because ordering is search behavior.
#[inline(always)]
pub fn search<NODE: NodeType>(td: &mut ThreadData, input: MoveLoopInput<'_>) -> MoveLoopOutcome {
    let context = input.context;
    let mut state = MoveLoopState::new(context, input.quiet_moves, input.noisy_moves);
    let mut move_picker = MovePicker::new(context.proof.tt_probe.mv);

    while let Some(mv) = move_picker.next::<NODE>(td, state.skip_quiets, context.node.ply) {
        if mv == td.stack[context.node.ply].excluded {
            continue;
        }

        if NODE::ROOT && !td.root_moves[td.pv_index..td.pv_end].iter().any(|rm| rm.mv == mv) {
            continue;
        }

        state.begin_candidate(context, td);
        let candidate = MoveCandidate::new(td, context, mv);

        match context.prune_candidate::<NODE>(td, &state, candidate, move_picker.stage()) {
            MovePruningDecision::Search => {}
            MovePruningDecision::Skip => continue,
            MovePruningDecision::SkipRemainingQuiets { score } => {
                state.record_pruned_fail_soft_score(score);
                state.skip_quiets = true;
                continue;
            }
            MovePruningDecision::StopBadNoisy { score } => {
                state.record_pruned_fail_soft_score(score);
                break;
            }
        }

        let initial_nodes = td.nodes();

        make_move(td, context.node.ply, candidate.mv);
        let child = search_child::<NODE>(td, context, &state, candidate);
        undo_move(td, candidate.mv);
        let score = child.score;
        state.current_search_count = child.search_count;

        if td.is_stopped() {
            return MoveLoopOutcome::Stopped;
        }

        if NODE::ROOT {
            context.update_root_move(td, candidate.mv, score, &state, initial_nodes);
        }

        if state.accept_child_result::<NODE>(td, context, candidate, score).is_beta_cutoff() {
            break;
        }

        state.searched_non_best(candidate);
    }

    MoveLoopOutcome::Finished(state.finish())
}

/// Result of the ordered child move loop.
pub enum MoveLoopOutcome {
    /// Shared stop was observed after a child search.
    ///
    /// The caller should return the neutral stopped score without writing final history or TT state
    /// for this incomplete node.
    Stopped,
    /// The move loop completed normally and produced final node state for post-loop history and TT
    /// finalization.
    Finished(MoveLoopResult),
}

/// Summary of the ordered child move search.
///
/// Finalization consumes this as the move-loop contract instead of reaching back into the loop's
/// internal counters and best-move bookkeeping.
pub struct MoveLoopResult {
    /// Final alpha after all searched moves.
    ///
    /// This is used only for result-shaping assertions and beta-cutoff scaling.
    pub alpha: i32,

    /// Best score found by the ordered move loop.
    pub best_score: i32,

    /// Move that produced `best_score`, or `Move::NULL` when no legal move was searched.
    pub best_move: Move,

    /// Bound represented by `best_score` after the loop.
    ///
    /// This controls history feedback, TT-PV propagation, and final TT storage.
    pub bound: Bound,

    /// Number of legal searched candidates accepted by the loop.
    ///
    /// A zero count means mate, stalemate/draw, or excluded-move TB sentinel is handled by
    /// finalization.
    pub move_count: i32,

    /// Number of child searches used for the current best move.
    ///
    /// History updates use this to scale the best-move bonus.
    pub current_search_count: i32,
}

/// Scalar context for the full-width ordered child move loop.
///
/// These values are stable while the loop walks ordered candidates. The groups mirror the concepts
/// the move loop consults: node/window shape, eval signals, TT proof state, and child-depth policy.
/// Keeping them together under one input preserves the single move-loop contract without turning
/// the context into a flat scalar bag.
#[derive(Copy, Clone)]
pub struct MoveLoopContext {
    /// Node and alpha-beta window facts for this move loop.
    pub node: MoveLoopNode,

    /// Eval and trend signals used by pruning, reductions, and TT writeback.
    pub eval: MoveLoopEval,

    /// TT proof and writeback state visible to move ordering and reductions.
    pub proof: MoveLoopProof,

    /// Singular-extension and verification score used by child-depth policy.
    pub depth_policy: MoveLoopDepthPolicy,

    /// Best score entering the loop, possibly seeded by tablebase proof.
    pub best_score: i32,
}

/// Node and alpha-beta window facts for an ordered move loop.
#[derive(Copy, Clone)]
pub struct MoveLoopNode {
    /// Current ply whose legal moves are being searched.
    pub ply: isize,

    /// Current alpha bound entering the loop; raised by successful children.
    pub alpha: i32,

    /// Current beta bound; fail-high returns stop the loop.
    pub beta: i32,

    /// Remaining full-width depth before extensions and reductions.
    pub depth: i32,

    /// Cut-node shape used by reductions and child search planning.
    pub cut_node: bool,

    /// Side to move, used by quiet history lookups.
    pub stm: Color,

    /// Whether this node is in check, disabling some move pruning.
    pub in_check: bool,
}

/// Eval and trend signals used while ordering and pruning moves.
#[derive(Copy, Clone)]
pub struct MoveLoopEval {
    /// Raw NNUE eval for TT writes during alpha-raise lower-bound updates.
    pub raw_eval: i32,

    /// Corrected static eval for pruning and reductions.
    pub corrected: i32,

    /// Correction-history value used by reductions.
    pub correction: i32,

    /// Numeric eval trend signal used by pruning and reductions.
    pub improvement: i32,

    /// Boolean eval trend signal used by reduction policy.
    pub improving: bool,
}

/// Singular-extension result used when choosing child search depth.
#[derive(Copy, Clone)]
pub struct MoveLoopDepthPolicy {
    /// Singular-extension adjustment for the TT move.
    pub extension: i32,

    /// Score from singular verification, used to tune reductions.
    pub singular_score: i32,
}

/// TT proof state used by move ordering, reductions, and early lower-bound writes.
#[derive(Copy, Clone)]
pub struct MoveLoopProof {
    /// TT probe supplying the hash move, score, depth, and bound signals.
    pub tt_probe: TtProbe,

    /// TT-PV marker used by reductions and TT writes.
    pub tt_pv: bool,

    /// Position hash for alpha-raise lower-bound TT writes.
    pub hash: u64,
}

impl MoveLoopContext {
    /// Apply ordering-aware pruning to a candidate before make-move.
    ///
    /// The loop context owns the node-wide pruning facts, while `MoveLoopState` owns the current
    /// alpha and move count. Keeping this as a method avoids rebuilding those two local frames into
    /// a long positional helper call at the hot move-loop site.
    #[inline(always)]
    fn prune_candidate<NODE: NodeType>(
        self, td: &ThreadData, state: &MoveLoopState<'_>, candidate: MoveCandidate, stage: Stage,
    ) -> MovePruningDecision {
        if NODE::ROOT || is_loss(state.best_score) {
            return MovePruningDecision::Search;
        }

        let pruning = CandidatePruningContext::new(self, state, candidate, stage);

        if pruning.late_move_prunes() {
            return MovePruningDecision::SkipRemainingQuiets { score: None };
        }

        if let Some(score) = pruning.futility_prune_score() {
            return MovePruningDecision::SkipRemainingQuiets { score: Some(score) };
        }

        if let Some(score) = pruning.bad_noisy_futility_score() {
            return MovePruningDecision::StopBadNoisy { score: Some(score) };
        }

        let threshold = pruning.see_threshold();
        if !td.board.see(candidate.mv, threshold) {
            return MovePruningDecision::Skip;
        }

        MovePruningDecision::Search
    }

    /// Build the depth-policy context for one already-made child.
    ///
    /// Reduction formulas deliberately depend on both node-wide facts and loop progress. This
    /// method names that handoff without hiding the tuned formulas in `reductions.rs`.
    #[inline(always)]
    fn reduction_context<NODE: NodeType>(self, td: &ThreadData, state: &MoveLoopState<'_>) -> ReductionContext {
        ReductionContext {
            depth: self.node.depth,
            move_count: state.move_count,
            alpha: state.alpha,
            beta: self.node.beta,
            correction: self.eval.correction,
            alpha_raises: state.alpha_raises,
            tt_probe: self.proof.tt_probe,
            tt_pv: self.proof.tt_pv,
            cut_node: self.node.cut_node,
            improving: self.eval.improving,
            improvement: self.eval.improvement,
            child_cutoff_count: td.stack[self.node.ply + 1].cutoff_count,
            tt_move_score: state.tt_move_score,
            singular_score: self.depth_policy.singular_score,
            parent_reduction: if NODE::PV { 0 } else { td.stack[self.node.ply - 1].reduction },
            helper_bias: helper_reduction_bias(td),
            root_delta: td.root_delta,
            node_pv: NODE::PV,
        }
    }

    /// Whether an alpha raise should seed an early TT lower bound before finalization.
    ///
    /// Root secondary MultiPV slots and TT-move repeats are intentionally skipped because their
    /// root move set or ordering role is not the ordinary interior lower-bound contract.
    #[inline]
    fn writes_early_lower_bound<NODE: NodeType>(self, td: &ThreadData, mv: Move) -> bool {
        !(NODE::ROOT && td.pv_index > 0) && mv != self.proof.tt_probe.mv
    }

    /// Publish an early lower bound after an interior alpha raise.
    ///
    /// The final node write still happens in finalization. This write is a move-loop side effect
    /// used by later probes before the node is complete, so the loop calls it at the alpha-raise
    /// point instead of hiding it in post-loop code.
    #[inline(always)]
    fn write_alpha_raise_lower_bound(self, td: &mut ThreadData, score: i32, mv: Move) {
        td.shared.tt.write(
            self.proof.hash,
            self.node.depth,
            self.eval.raw_eval,
            score,
            Bound::Lower,
            mv,
            self.node.ply,
            true,
            false,
        );
    }

    /// Update UCI-facing root move state after one root candidate search.
    ///
    /// Root moves carry score, bound flags, selected depth, PV, and node counts. Keeping the call
    /// on the root context leaves the move loop with a named root-only side effect instead of a
    /// positional list of display fields.
    #[inline(always)]
    fn update_root_move(
        self, td: &mut ThreadData, mv: Move, score: i32, state: &MoveLoopState<'_>, initial_nodes: u64,
    ) {
        let current_nodes = td.nodes();
        let root_move = td.root_moves.iter_mut().find(|v| v.mv == mv).unwrap();
        let changed_best = root_move.record_search_result(RootSearchResult {
            score,
            alpha: state.alpha,
            beta: self.node.beta,
            move_count: state.move_count,
            sel_depth: td.sel_depth,
            pv: &td.pv_table,
            start_ply: 1,
            nodes: current_nodes - initial_nodes,
        });

        if changed_best && td.pv_index == 0 {
            td.best_move_changes += 1;
        }
    }
}

/// Ordering-aware pruning facts for one move-loop candidate.
///
/// Full-width move pruning combines node state, current loop progress, the candidate's move class,
/// and the move-picker stage. Capturing that as one short-lived value keeps the move-loop branch
/// order visible while avoiding repeated scalar argument lists at each tuned formula call.
#[derive(Copy, Clone)]
struct CandidatePruningContext {
    /// Node and alpha-beta window facts for this move loop.
    node: MoveLoopNode,

    /// Eval and trend signals used by pruning formulas.
    eval: MoveLoopEval,

    /// Candidate being considered before make-move.
    candidate: MoveCandidate,

    /// Current loop alpha.
    alpha: i32,

    /// One-based move count for ordered candidate pruning.
    move_count: i32,

    /// Current move-picker stage, used by bad-noisy pruning.
    stage: Stage,
}

impl CandidatePruningContext {
    #[inline]
    fn new(context: MoveLoopContext, state: &MoveLoopState<'_>, candidate: MoveCandidate, stage: Stage) -> Self {
        Self {
            node: context.node,
            eval: context.eval,
            candidate,
            alpha: state.alpha,
            move_count: state.move_count,
            stage,
        }
    }

    #[inline]
    fn late_move_prunes(self) -> bool {
        pruning::late_move_prunes(
            self.node.in_check,
            self.candidate.gives_direct_check,
            self.candidate.is_quiet,
            self.move_count,
            self.eval.improvement,
            self.node.depth,
            self.candidate.history,
        )
    }

    #[inline]
    fn futility_prune_score(self) -> Option<i32> {
        pruning::futility_prune_score(
            self.node.in_check,
            self.candidate.gives_direct_check,
            self.candidate.is_quiet,
            self.eval.corrected,
            self.node.beta,
            self.node.depth,
            self.candidate.history,
            self.alpha,
        )
    }

    #[inline]
    fn bad_noisy_futility_score(self) -> Option<i32> {
        pruning::bad_noisy_futility_score(
            self.node.in_check,
            self.candidate.gives_direct_check,
            self.stage,
            self.eval.corrected,
            self.node.depth,
            self.candidate.history,
            self.alpha,
        )
    }

    #[inline]
    fn see_threshold(self) -> i32 {
        pruning::see_threshold(self.candidate.is_quiet, self.node.depth, self.candidate.history)
    }
}

/// Inputs for the full-width ordered child move loop.
///
/// This is the phase contract from `full::search` to move ordering, pruning, reductions, PVS, root
/// updates, and searched-move history buffers.
pub struct MoveLoopInput<'a> {
    /// Stable scalar facts for this move loop.
    pub context: MoveLoopContext,

    /// Quiet searched-move buffer later consumed by history feedback.
    pub quiet_moves: &'a mut ArrayVec<Move, 32>,

    /// Noisy searched-move buffer later consumed by history feedback.
    pub noisy_moves: &'a mut ArrayVec<Move, 32>,
}

/// Mutable state owned by the ordered move loop.
///
/// This gathers the values that change as candidates are searched. The buffers stay borrowed from
/// the parent because finalization consumes them after the loop, but the loop owns when moves are
/// pushed into them.
struct MoveLoopState<'a> {
    /// Current alpha bound after searched moves.
    alpha: i32,

    /// Best score found so far.
    best_score: i32,

    /// Move that produced `best_score`.
    best_move: Move,

    /// Bound represented by the current best score.
    bound: Bound,

    /// Number of accepted legal candidates searched or pruned by the loop.
    move_count: i32,

    /// Number of child searches used for the current move.
    current_search_count: i32,

    /// Number of non-decisive alpha raises, used by reduction policy.
    alpha_raises: i32,

    /// Score observed for the TT move, used with singular score in reductions.
    tt_move_score: i32,

    /// Whether move ordering should skip the remaining quiet tail.
    skip_quiets: bool,

    /// Quiet searched-move buffer later consumed by history feedback.
    quiet_moves: &'a mut ArrayVec<Move, 32>,

    /// Noisy searched-move buffer later consumed by history feedback.
    noisy_moves: &'a mut ArrayVec<Move, 32>,
}

impl<'a> MoveLoopState<'a> {
    #[inline]
    fn new(
        context: MoveLoopContext, quiet_moves: &'a mut ArrayVec<Move, 32>, noisy_moves: &'a mut ArrayVec<Move, 32>,
    ) -> Self {
        Self {
            alpha: context.node.alpha,
            best_score: context.best_score,
            best_move: Move::NULL,
            bound: Bound::Upper,
            move_count: 0,
            current_search_count: 0,
            alpha_raises: 0,
            tt_move_score: Score::NONE,
            skip_quiets: false,
            quiet_moves,
            noisy_moves,
        }
    }

    #[inline]
    fn begin_candidate(&mut self, context: MoveLoopContext, td: &mut ThreadData) {
        self.move_count += 1;
        self.current_search_count = 0;
        td.stack[context.node.ply].move_count = self.move_count;
    }

    /// Apply a fail-soft pruning score without letting pruning invent decisive results.
    #[inline]
    fn record_pruned_fail_soft_score(&mut self, score: Option<i32>) {
        if let Some(score) = score
            && !is_decisive(self.best_score)
            && self.best_score < score
        {
            self.best_score = score;
        }
    }

    /// Fold one completed child search into alpha-beta state.
    ///
    /// This is the alpha-beta acceptance transition for the loop: remember TT-move score, update
    /// the best score, raise alpha and PV state when the child beats the window floor, and report
    /// whether beta was crossed. Root display is deliberately done before this call so it sees the
    /// same window and move-count facts as the original inline code.
    #[inline(always)]
    fn accept_child_result<NODE: NodeType>(
        &mut self, td: &mut ThreadData, context: MoveLoopContext, candidate: MoveCandidate, score: i32,
    ) -> ChildResultOutcome {
        if candidate.mv == context.proof.tt_probe.mv {
            self.tt_move_score = score;
        }

        if score <= self.best_score {
            return ChildResultOutcome::Continue;
        }

        self.best_score = score;

        if score <= self.alpha {
            return ChildResultOutcome::Continue;
        }

        self.raise_alpha::<NODE>(td, context, candidate.mv, score)
    }

    /// Apply the side effects of a child result that raises alpha.
    ///
    /// Successful children set the exact-bound best move, repair the PV table for interior PV
    /// nodes, may produce a beta cutoff, and may publish an early TT lower bound. Keeping these
    /// together preserves the tuned ordering while leaving the loop body at the algorithm level.
    #[inline(always)]
    fn raise_alpha<NODE: NodeType>(
        &mut self, td: &mut ThreadData, context: MoveLoopContext, mv: Move, score: i32,
    ) -> ChildResultOutcome {
        self.bound = Bound::Exact;
        self.best_move = mv;

        if !NODE::ROOT && NODE::PV {
            td.pv_table.update(context.node.ply as usize, mv);
        }

        if score >= context.node.beta {
            self.record_beta_cutoff(td, context.node.ply);
            return ChildResultOutcome::BetaCutoff;
        }

        self.alpha = score;

        if context.writes_early_lower_bound::<NODE>(td, mv) {
            context.write_alpha_raise_lower_bound(td, score, mv);
        }

        if !is_decisive(score) {
            self.alpha_raises += 1;
        }

        ChildResultOutcome::Continue
    }

    /// Record the beta-cutoff shape used by finalization and later sibling reductions.
    #[inline(always)]
    fn record_beta_cutoff(&mut self, td: &mut ThreadData, ply: isize) {
        self.bound = Bound::Lower;
        td.stack[ply].cutoff_count += 1;
    }

    #[inline]
    fn searched_non_best(&mut self, candidate: MoveCandidate) {
        if candidate.mv == self.best_move || self.move_count >= 32 {
            return;
        }

        if candidate.is_quiet {
            self.quiet_moves.push(candidate.mv);
        } else {
            self.noisy_moves.push(candidate.mv);
        }
    }

    #[inline]
    fn finish(self) -> MoveLoopResult {
        MoveLoopResult {
            alpha: self.alpha,
            best_score: self.best_score,
            best_move: self.best_move,
            bound: self.bound,
            move_count: self.move_count,
            current_search_count: self.current_search_count,
        }
    }
}

/// Per-move facts derived before pruning and child search.
///
/// These values are intentionally computed once so pruning, reductions, and history buffers agree
/// on the same move class and history score.
#[derive(Copy, Clone)]
struct MoveCandidate {
    /// Move selected by move ordering.
    mv: Move,

    /// Whether the move is quiet from the search heuristics' perspective.
    is_quiet: bool,

    /// Quiet or noisy history score used by pruning and reductions.
    history: i32,

    /// Whether the move gives a direct check, which protects it from pruning.
    gives_direct_check: bool,
}

impl MoveCandidate {
    #[inline]
    fn new(td: &ThreadData, context: MoveLoopContext, mv: Move) -> Self {
        let is_quiet = mv.is_quiet();
        let history = if is_quiet {
            td.quiet_history.get(td.board.all_threats(), context.node.stm, mv)
                + td.conthist(context.node.ply, 1, mv)
                + td.conthist(context.node.ply, 2, mv)
        } else {
            let captured = td.board.type_on(mv.to());
            td.noisy_history.get(td.board.all_threats(), td.board.moved_piece(mv), mv.to(), captured)
        };

        Self {
            mv,
            is_quiet,
            history,
            gives_direct_check: td.board.is_direct_check(mv),
        }
    }
}

/// Result of move-loop pruning for a candidate.
///
/// The decision is more than searched-or-not: some pruning updates fail-soft `best_score`, late
/// quiet pruning skips the quiet tail, and bad-noisy futility stops the noisy tail entirely because
/// remaining captures are even less promising.
enum MovePruningDecision {
    /// Search this candidate normally.
    Search,

    /// Skip only this candidate.
    Skip,

    /// Skip this candidate and tell move ordering to skip later quiet moves.
    SkipRemainingQuiets {
        /// Fail-soft score that may improve the loop's current best score.
        score: Option<i32>,
    },

    /// Stop the bad-noisy tail.
    StopBadNoisy {
        /// Fail-soft score that may improve the loop's current best score.
        score: Option<i32>,
    },
}

/// Whether accepting a child result ended the node with a beta cutoff.
enum ChildResultOutcome {
    /// Continue with later ordered moves.
    Continue,

    /// Stop the loop because the child crossed beta.
    BetaCutoff,
}

impl ChildResultOutcome {
    /// True when the caller should stop searching siblings.
    #[inline]
    const fn is_beta_cutoff(&self) -> bool {
        matches!(self, Self::BetaCutoff)
    }
}

/// Result of all child searches for one candidate.
///
/// Separating this from move-loop state keeps the make/search/undo block explicit while giving
/// history feedback the exact number of searches spent on the move that ultimately wins the node.
struct ChildSearchResult {
    /// Final score returned by the child search sequence.
    score: i32,

    /// Number of child searches performed for this candidate.
    search_count: i32,
}

/// Search one candidate after `make_move`.
///
/// The caller owns make/undo and stop handling. This helper only chooses the reduced scout,
/// full-depth scout, and PV search sequence for the already-made child position.
#[inline(always)]
fn search_child<NODE: NodeType>(
    td: &mut ThreadData, context: MoveLoopContext, loop_state: &MoveLoopState<'_>, candidate: MoveCandidate,
) -> ChildSearchResult {
    let child_context = ChildSearchContext::new::<NODE>(td, context, loop_state, candidate);
    let mut child = ChildSearchState::new(child_context);

    if child_context.uses_late_move_reduction() {
        child.search_reduced_scout::<NODE>(td, child_context);
    } else if child_context.uses_full_depth_scout::<NODE>() {
        child.search_full_depth_scout(td, child_context);
    }

    if child.needs_pv_search::<NODE>(child_context) {
        child.search_pv(td, child_context);
    }

    child.finish()
}

/// Facts for one candidate after `make_move` and before child search.
///
/// This value is a snapshot rather than a borrowed parameter bag. The reduced scout, full-depth
/// scout, and PV search all need the same parent window, candidate facts, loop progress, and
/// reduction policy. Capturing those facts once keeps the child-search methods focused on the
/// search sequence instead of repeatedly threading unrelated fragments.
#[derive(Copy, Clone)]
struct ChildSearchContext {
    /// Stable parent move-loop context.
    parent: MoveLoopContext,

    /// Candidate being searched in the already-made child position.
    candidate: MoveCandidate,

    /// Reduction formula inputs for this candidate.
    reductions: ReductionContext,

    /// One-based ordered move index.
    move_count: i32,

    /// Parent alpha at the time this candidate was selected.
    alpha: i32,

    /// Best parent score before this candidate.
    best_score: i32,
}

impl ChildSearchContext {
    #[inline]
    fn new<NODE: NodeType>(
        td: &ThreadData, parent: MoveLoopContext, loop_state: &MoveLoopState<'_>, candidate: MoveCandidate,
    ) -> Self {
        Self {
            parent,
            candidate,
            reductions: parent.reduction_context::<NODE>(td, loop_state),
            move_count: loop_state.move_count,
            alpha: loop_state.alpha,
            best_score: loop_state.best_score,
        }
    }

    #[inline]
    fn uses_late_move_reduction(self) -> bool {
        self.parent.node.depth >= 2 && self.move_count >= 2
    }

    #[inline]
    fn uses_full_depth_scout<NODE: NodeType>(self) -> bool {
        !NODE::PV || self.move_count >= 2
    }
}

/// Mutable policy state for the child search sequence after the move is made.
///
/// Reduced scout, full-depth scout, and PV search all update the same depth and score. Keeping
/// those values together avoids threading several mutable locals through helper calls while still
/// leaving the caller's make/search/undo ordering explicit.
struct ChildSearchState {
    /// Depth used by the next child search.
    new_depth: i32,

    /// Last child score observed by the scout or PV search sequence.
    score: i32,

    /// Number of child searches used for this candidate.
    search_count: i32,
}

impl ChildSearchState {
    #[inline]
    fn new(context: ChildSearchContext) -> Self {
        Self {
            new_depth: initial_child_depth(context),
            score: Score::ZERO,
            search_count: 0,
        }
    }

    #[inline(always)]
    fn search_reduced_scout<NODE: NodeType>(&mut self, td: &mut ThreadData, context: ChildSearchContext) {
        let reduction = context.reductions.late_move_reduction(
            context.candidate.is_quiet,
            context.candidate.history,
            td.board.in_check(),
        );
        let reduced_depth = context.reductions.late_move_reduced_depth(self.new_depth, reduction);

        td.stack[context.parent.node.ply].reduction = reduction;
        self.score = -full_search::<NonPV>(
            td,
            -context.alpha - 1,
            -context.alpha,
            reduced_depth,
            true,
            context.parent.node.ply + 1,
        );
        td.stack[context.parent.node.ply].reduction = 0;
        self.search_count += 1;

        if self.score > context.alpha {
            self.adjust_retry_depth::<NODE>(context, reduced_depth);
            self.retry_scout_if_deeper(td, context, reduced_depth);
        }
    }

    #[inline(always)]
    fn search_full_depth_scout(&mut self, td: &mut ThreadData, context: ChildSearchContext) {
        let reduction = context.reductions.full_depth_reduction(
            context.candidate.mv,
            context.candidate.is_quiet,
            context.candidate.history,
        );
        let reduced_depth = context.reductions.full_depth_reduced_depth(self.new_depth, reduction);

        self.score = -full_search::<NonPV>(
            td,
            -context.alpha - 1,
            -context.alpha,
            reduced_depth,
            !context.parent.node.cut_node,
            context.parent.node.ply + 1,
        );
        self.search_count += 1;
    }

    #[inline]
    fn adjust_retry_depth<NODE: NodeType>(&mut self, context: ChildSearchContext, reduced_depth: i32) {
        if NODE::ROOT {
            return;
        }

        self.new_depth += (self.score > context.best_score + 61) as i32;
        self.new_depth += (self.score > context.best_score + 801) as i32;
        self.new_depth -= (self.score < context.best_score + 5 + reduced_depth) as i32;
    }

    #[inline(always)]
    fn retry_scout_if_deeper(&mut self, td: &mut ThreadData, context: ChildSearchContext, reduced_depth: i32) {
        if self.new_depth <= reduced_depth {
            return;
        }

        self.score = -full_search::<NonPV>(
            td,
            -context.alpha - 1,
            -context.alpha,
            self.new_depth,
            !context.parent.node.cut_node,
            context.parent.node.ply + 1,
        );
        self.search_count += 1;
    }

    #[inline]
    fn needs_pv_search<NODE: NodeType>(&self, context: ChildSearchContext) -> bool {
        NODE::PV && (context.move_count == 1 || self.score > context.alpha)
    }

    #[inline(always)]
    fn search_pv(&mut self, td: &mut ThreadData, context: ChildSearchContext) {
        if context.candidate.mv == context.parent.proof.tt_probe.mv
            && context.parent.proof.tt_probe.depth > 1
            && td.root_depth > 8
        {
            self.new_depth = self.new_depth.max(1);
        }

        self.score = -full_search::<PV>(
            td,
            -context.parent.node.beta,
            -context.alpha,
            self.new_depth,
            false,
            context.parent.node.ply + 1,
        );
        self.search_count += 1;
    }

    #[inline]
    fn finish(self) -> ChildSearchResult {
        ChildSearchResult { score: self.score, search_count: self.search_count }
    }
}

/// Initial child depth before reductions and PV retry adjustment.
///
/// The first move receives the full singular-extension value. Later moves only keep a one-ply
/// signal that an extension was present, because reductions and retries decide how much extra depth
/// the alternative move earns.
#[inline]
fn initial_child_depth(context: ChildSearchContext) -> i32 {
    context.parent.node.depth - 1
        + if context.move_count == 1 {
            context.parent.depth_policy.extension
        } else {
            (context.parent.depth_policy.extension > 0) as i32
        }
}
