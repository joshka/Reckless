//! Recursive full-width alpha-beta search.
//!
//! Full-width search is the normal alpha-beta node contract: it may generate and search the whole
//! legal move set, even though pruning, reductions, extensions, and PVS often avoid searching every
//! move at the same depth. This module is the algorithm spine for those nodes. It keeps the phase
//! order visible and delegates each major chess-search concept to the module that owns it: TT
//! proof, tablebase proof, eval setup, pruning, singular verification, ordered child search, and
//! node finalization. It does not own the detailed formulas for those concepts; it owns their order
//! and the contracts passed between them.

use crate::{
    thread::ThreadData,
    transposition::Bound,
    types::{ArrayVec, MAX_PLY, Move, Score, draw, is_decisive, is_valid, mate_in, mated_in},
};

use super::{
    NodeType,
    eval::{EvalInput, EvalState, StackPreparationInput, prepare_full_width_node},
    finalize::{FinalizationNode, FinalizationProof, NodeFinalizationInput, SearchedMoveLists, finish_full_width_node},
    moves::{
        self, MoveLoopContext, MoveLoopDepthPolicy, MoveLoopEval, MoveLoopInput, MoveLoopNode, MoveLoopOutcome,
        MoveLoopProof, MoveLoopResult,
    },
    pruning,
    qsearch::qsearch,
    singular, tt,
};

#[cfg(feature = "syzygy")]
use super::tablebase;

/// Search a full-width alpha-beta node.
///
/// The function should read as the node-phase pseudo-code: enter the node, take TT/tablebase
/// proofs, prepare eval and stack state, try pre-move pruning, verify singularity, search ordered
/// children, then finalize the node result.
#[inline(always)]
pub fn search<NODE: NodeType>(
    td: &mut ThreadData, alpha: i32, beta: i32, depth: i32, cut_node: bool, ply: isize,
) -> i32 {
    let mut window = SearchWindow::new(alpha, beta);

    let node = match enter_node::<NODE>(td, &mut window, depth, cut_node, ply) {
        NodeEntry::Return(score) => return score,
        NodeEntry::QSearch => return qsearch::<NODE>(td, window.alpha, window.beta, ply),
        NodeEntry::Continue(node) => node,
    };

    let mut node = NodeContext::new(node, window, depth.min(MAX_PLY as i32 - 1));

    let proof = match probe_proofs(td, node.facts, &mut node.window, node.depth) {
        Ok(proof) => proof,
        Err(score) => return score,
    };

    let eval = EvalState::compute(td, node.eval_input(proof));

    node.depth = prepare_full_width_node(td, node.stack_preparation_input(proof, eval));

    let mut state = FullSearchState::new(node, proof, eval);

    let pruning = state.pre_move_pruning();
    if let Some(score) = pruning.razor::<NODE>(td) {
        return score;
    }
    if let Some(score) = pruning.reverse_futility(td) {
        return score;
    }
    if let Some(score) = pruning.null_move(td) {
        return score;
    }
    if let Some(score) = pruning.probcut::<NODE>(td) {
        return score;
    }

    let singular = singular::search_if_needed(td, state.singular_input());
    if let Some(score) = singular.cutoff_score() {
        return score;
    }
    state.apply_singular(singular);

    let move_loop = match moves::search::<NODE>(td, state.move_loop_input()) {
        MoveLoopOutcome::Stopped => return Score::ZERO,
        MoveLoopOutcome::Finished(result) => result,
    };

    finish_full_width_node::<NODE>(td, state.finalization_input(move_loop))
}

/// Alpha-beta window for a full-width node.
///
/// Search phases mutate this as repetition and mate-distance rules tighten the bounds and as
/// tablebase/PV child results raise alpha. Keeping the bounds together makes the current window
/// explicit instead of passing independent scalars through every phase.
#[derive(Copy, Clone)]
struct SearchWindow {
    /// Lower bound that the node is trying to improve.
    alpha: i32,

    /// Upper bound that causes a fail-high cutoff.
    beta: i32,
}

impl SearchWindow {
    #[inline]
    const fn new(alpha: i32, beta: i32) -> Self {
        Self { alpha, beta }
    }
}

/// Cheap full-width node facts captured once at entry.
///
/// Full-width search reads side-to-move, check state, and singular-exclusion state across many
/// phases. Grouping them makes the driver less dependent on repeated `ThreadData` lookups without
/// hiding that the board and stack remain the source of truth.
#[derive(Copy, Clone)]
struct FullNode {
    /// Whether this node is a root node.
    ///
    /// Root nodes own root-move filtering, MultiPV behavior, and reporting.
    node_root: bool,

    /// Whether this node is on the principal variation.
    ///
    /// PV nodes maintain PV table state and may use a wider window.
    node_pv: bool,

    /// Current search ply.
    ///
    /// Stack, PV table, mate-distance scores, and repetition checks all use this coordinate.
    ply: isize,

    /// Whether this node is expected to fail high.
    ///
    /// Several pruning gates and reduction formulas are tuned around cut-node shape, so the value
    /// is captured once at entry.
    cut_node: bool,

    /// Side to move at node entry.
    ///
    /// Eval, history, threats, and root optimism all interpret side-relative data through this
    /// color.
    stm: crate::types::Color,

    /// Whether the side to move is currently in check.
    ///
    /// Check nodes skip stand-pat-like eval assumptions and disable many pruning shortcuts.
    in_check: bool,

    /// Whether this is a singular-verification node with one move excluded.
    ///
    /// Excluded nodes must avoid ordinary TT cutoffs, tablebase cutoffs, and final TT writes
    /// because they do not search the normal legal move set.
    excluded: bool,
}

impl FullNode {
    #[inline]
    fn new<NODE: NodeType>(td: &ThreadData, cut_node: bool, ply: isize) -> Self {
        Self {
            node_root: NODE::ROOT,
            node_pv: NODE::PV,
            ply,
            cut_node,
            stm: td.board.side_to_move(),
            in_check: td.board.in_check(),
            excluded: td.stack[ply].excluded.is_present(),
        }
    }

    /// Try the TT proof for this node against the current full-width window.
    ///
    /// `FullNode` owns the node-kind, ply, side-to-move, exclusion, and cut-node facts that make
    /// the TT bound safe or unsafe. Keeping the call here avoids spelling those fields out at the
    /// proof-phase site.
    #[inline(always)]
    fn tt_cutoff(self, td: &mut ThreadData, probe: tt::TtProbe, window: SearchWindow, depth: i32) -> Option<i32> {
        tt::try_full_width_cutoff(
            td,
            tt::FullWidthCutoffInput {
                ply: self.ply,
                stm: self.stm,
                probe,
                node_pv: self.node_pv,
                excluded: self.excluded,
                depth,
                alpha: window.alpha,
                beta: window.beta,
                cut_node: self.cut_node,
            },
        )
    }

    /// Probe interior tablebases for this node using the current full-width window.
    ///
    /// Root tablebase ranking is a different concept. This method only names the interior proof
    /// handoff so `probe_proofs` does not rebuild `FullNode` field traffic at the call site.
    #[cfg(feature = "syzygy")]
    #[inline(always)]
    fn tablebase_probe(
        self, td: &mut ThreadData, hash: u64, window: SearchWindow, depth: i32, tt_pv: bool,
    ) -> tablebase::ProbeResult {
        tablebase::probe_full_width(
            td,
            tablebase::ProbeInput {
                ply: self.ply,
                hash,
                node_root: self.node_root,
                node_pv: self.node_pv,
                excluded: self.excluded,
                depth,
                alpha: window.alpha,
                beta: window.beta,
                tt_pv,
            },
        )
    }
}

/// Stable full-width node context after entry guards.
///
/// This is the point where node facts, the current alpha-beta window, and adjusted depth travel
/// together. Keeping them as one value avoids rebuilding early phase inputs from loose
/// `FullNode`/`SearchWindow`/`depth` variables while still leaving the mutable window and depth
/// changes visible in `search()`.
#[derive(Copy, Clone)]
struct NodeContext {
    /// Cheap node facts captured at entry.
    facts: FullNode,

    /// Current alpha-beta window after guard and tablebase tightening.
    window: SearchWindow,

    /// Remaining depth after root clamp and stack-preparation feedback.
    depth: i32,
}

impl NodeContext {
    #[inline]
    const fn new(facts: FullNode, window: SearchWindow, depth: i32) -> Self {
        Self { facts, window, depth }
    }

    #[inline]
    fn eval_input(self, proof: ProofState) -> EvalInput {
        EvalInput {
            hash: proof.hash,
            ply: self.facts.ply,
            in_check: self.facts.in_check,
            excluded: self.facts.excluded,
            tt_probe: proof.tt_probe,
            tt_pv: proof.tt_pv,
            alpha: self.window.alpha,
            beta: self.window.beta,
        }
    }

    #[inline]
    fn stack_preparation_input(self, proof: ProofState, eval: EvalState) -> StackPreparationInput {
        StackPreparationInput {
            ply: self.facts.ply,
            node_root: self.facts.node_root,
            stm: self.facts.stm,
            in_check: self.facts.in_check,
            excluded: self.facts.excluded,
            tt_probe: proof.tt_probe,
            tt_pv: proof.tt_pv,
            eval: eval.corrected,
            depth: self.depth,
        }
    }
}

/// Result of entering a full-width node.
///
/// Node entry either proves an immediate return, hands control to qsearch, or provides the cheap
/// node facts needed by the remaining full-width phases.
enum NodeEntry {
    /// A terminal guard, stop check, repetition adjustment, draw, max-ply, or mate-distance rule
    /// has already determined the node score.
    Return(i32),

    /// Depth reached zero and the caller must switch to qsearch with the adjusted alpha-beta
    /// window.
    QSearch,

    /// Full-width search should continue with these node facts.
    Continue(FullNode),
}

/// TT and tablebase proof state produced before eval.
///
/// Full-width search probes proof sources before static eval because a cutoff can avoid eval and
/// move generation entirely. Non-cutoff proof information still matters: TT supplies move ordering
/// and bound signals, while tablebase can seed a PV lower bound or cap the final PV score.
#[derive(Copy, Clone)]
struct ProofState {
    /// Current position hash used by TT reads and later TT writes.
    hash: u64,

    /// Search-facing TT probe for this node.
    tt_probe: tt::TtProbe,

    /// TT-PV marker after combining node kind and TT entry state.
    tt_pv: bool,

    /// Best score entering the move loop, possibly seeded by tablebase proof.
    best_score: i32,

    /// Maximum PV score allowed by a non-cutoff tablebase upper bound.
    #[cfg(feature = "syzygy")]
    max_score: i32,
}

/// Search-phase state carried after proof and eval setup.
///
/// This is the local frame of a full-width node after the cheap proof phases have completed. It
/// owns the facts that several later concepts need, and provides explicit conversions into the
/// narrower phase inputs. That keeps `search()` reading as the algorithm spine instead of
/// repeatedly reconstructing broad argument packets at each call site.
struct FullSearchState {
    /// Cheap node facts captured at entry.
    node: NodeContext,

    /// Position hash used by TT writeback phases.
    hash: u64,

    /// Search-facing TT probe, with the move updated after singular verification.
    tt_probe: tt::TtProbe,

    /// TT-PV marker shared by pruning, reductions, and final writeback.
    tt_pv: bool,

    /// Best score entering the ordered move loop.
    best_score: i32,

    /// Eval roles produced by the eval phase.
    eval: EvalState,

    /// Whether TT evidence is strong enough to try singular verification.
    potential_singularity: bool,

    /// Extension returned by singular verification for the TT move.
    extension: i32,

    /// Excluded-move search score used by reduction policy.
    singular_score: i32,

    /// Quiet alternatives searched before the best move.
    quiet_moves: ArrayVec<Move, 32>,

    /// Noisy alternatives searched before the best move.
    noisy_moves: ArrayVec<Move, 32>,

    /// Maximum PV score allowed by a non-cutoff tablebase upper bound.
    #[cfg(feature = "syzygy")]
    max_score: i32,
}

impl FullSearchState {
    #[inline]
    fn new(node: NodeContext, proof: ProofState, eval: EvalState) -> Self {
        let potential_singularity = node.depth >= 5 + proof.tt_pv as i32
            && proof.tt_probe.depth >= node.depth - 3
            && proof.tt_probe.bound != Bound::Upper
            && is_valid(proof.tt_probe.score)
            && !is_decisive(proof.tt_probe.score);

        Self {
            node,
            hash: proof.hash,
            tt_probe: proof.tt_probe,
            tt_pv: proof.tt_pv,
            best_score: proof.best_score,
            eval,
            potential_singularity,
            extension: 0,
            singular_score: Score::NONE,
            quiet_moves: ArrayVec::new(),
            noisy_moves: ArrayVec::new(),
            #[cfg(feature = "syzygy")]
            max_score: proof.max_score,
        }
    }

    #[inline]
    fn pre_move_pruning(&self) -> pruning::PreMovePruningContext {
        pruning::PreMovePruningContext {
            node: pruning::PreMoveNode {
                ply: self.node.facts.ply,
                stm: self.node.facts.stm,
                cut_node: self.node.facts.cut_node,
                in_check: self.node.facts.in_check,
                excluded: self.node.facts.excluded,
                alpha: self.node.window.alpha,
                beta: self.node.window.beta,
                depth: self.node.depth,
            },
            eval: pruning::PreMoveEval {
                estimated_score: self.eval.estimated,
                improvement: self.eval.improvement,
                improving: self.eval.improving,
                correction: self.eval.correction,
                corrected: self.eval.corrected,
            },
            proof: pruning::PreMoveProof {
                tt_pv: self.tt_pv,
                tt_probe: self.tt_probe,
                raw_eval: self.eval.raw,
                hash: self.hash,
            },
            guard: pruning::PreMoveGuard { potential_singularity: self.potential_singularity },
        }
    }

    #[inline]
    fn singular_input(&self) -> singular::SingularInput {
        singular::SingularInput {
            ply: self.node.facts.ply,
            depth: self.node.depth,
            beta: self.node.window.beta,
            cut_node: self.node.facts.cut_node,
            node_root: self.node.facts.node_root,
            node_pv: self.node.facts.node_pv,
            excluded: self.node.facts.excluded,
            potential: self.potential_singularity,
            tt_probe: self.tt_probe,
            correction: self.eval.correction,
        }
    }

    #[inline]
    fn apply_singular(&mut self, outcome: singular::SingularOutcome) {
        self.extension = outcome.extension;
        self.singular_score = outcome.score;
        self.tt_probe.mv = outcome.tt_move;
    }

    #[inline]
    fn move_loop_input(&mut self) -> MoveLoopInput<'_> {
        MoveLoopInput {
            context: MoveLoopContext {
                node: self.move_loop_node(),
                eval: self.move_loop_eval(),
                proof: self.move_loop_proof(),
                depth_policy: self.move_loop_depth_policy(),
                best_score: self.best_score,
            },
            quiet_moves: &mut self.quiet_moves,
            noisy_moves: &mut self.noisy_moves,
        }
    }

    #[inline]
    fn move_loop_node(&self) -> MoveLoopNode {
        MoveLoopNode {
            ply: self.node.facts.ply,
            alpha: self.node.window.alpha,
            beta: self.node.window.beta,
            depth: self.node.depth,
            cut_node: self.node.facts.cut_node,
            stm: self.node.facts.stm,
            in_check: self.node.facts.in_check,
        }
    }

    #[inline]
    fn move_loop_eval(&self) -> MoveLoopEval {
        MoveLoopEval {
            raw_eval: self.eval.raw,
            corrected: self.eval.corrected,
            correction: self.eval.correction,
            improvement: self.eval.improvement,
            improving: self.eval.improving,
        }
    }

    #[inline]
    fn move_loop_proof(&self) -> MoveLoopProof {
        MoveLoopProof { tt_probe: self.tt_probe, tt_pv: self.tt_pv, hash: self.hash }
    }

    #[inline]
    fn move_loop_depth_policy(&self) -> MoveLoopDepthPolicy {
        MoveLoopDepthPolicy {
            extension: self.extension,
            singular_score: self.singular_score,
        }
    }

    #[inline]
    fn finalization_input(&self, move_loop: MoveLoopResult) -> NodeFinalizationInput<'_> {
        NodeFinalizationInput {
            node: FinalizationNode {
                ply: self.node.facts.ply,
                depth: self.node.depth,
                cut_node: self.node.facts.cut_node,
                stm: self.node.facts.stm,
                excluded: self.node.facts.excluded,
                in_check: self.node.facts.in_check,
                beta: self.node.window.beta,
            },
            proof: FinalizationProof {
                tt_pv: self.tt_pv,
                hash: self.hash,
                raw_eval: self.eval.raw,
                eval: self.eval.corrected,
            },
            searched: SearchedMoveLists {
                quiet_moves: &self.quiet_moves,
                noisy_moves: &self.noisy_moves,
            },
            move_loop,
            #[cfg(feature = "syzygy")]
            max_score: self.max_score,
        }
    }
}

/// Probe TT and tablebases before eval.
///
/// `Err(score)` means a proof source cut the node immediately. `Ok` carries the proof state that
/// later eval, pruning, singular verification, move ordering, and finalization need.
#[inline(always)]
fn probe_proofs(td: &mut ThreadData, node: FullNode, window: &mut SearchWindow, depth: i32) -> Result<ProofState, i32> {
    let hash = td.board.hash();
    let tt_probe = tt::TtProbe::read(td, hash, node.ply, node.node_pv);
    let tt_pv = tt_probe.tt_pv;

    if let Some(score) = node.tt_cutoff(td, tt_probe, *window, depth) {
        return Err(score);
    }

    #[cfg(feature = "syzygy")]
    let mut max_score = Score::INFINITE;

    #[cfg(feature = "syzygy")]
    let mut best_score = -Score::INFINITE;
    #[cfg(not(feature = "syzygy"))]
    let best_score = -Score::INFINITE;

    #[cfg(feature = "syzygy")]
    match node.tablebase_probe(td, hash, *window, depth, tt_pv) {
        tablebase::ProbeResult::Cutoff(score) => return Err(score),
        tablebase::ProbeResult::PvLower(score) => {
            best_score = score;
            window.alpha = window.alpha.max(best_score);
        }
        tablebase::ProbeResult::PvUpper(score) => max_score = score,
        tablebase::ProbeResult::None => {}
    }

    Ok(ProofState {
        hash,
        tt_probe,
        tt_pv,
        best_score,
        #[cfg(feature = "syzygy")]
        max_score,
    })
}

/// Enter a full-width node and handle guards that must precede TT/eval work.
///
/// These guards are intentionally front-loaded: stop checks, qsearch entry, repetition adjustment,
/// draw/max-ply handling, and mate-distance pruning are cheap and define the alpha-beta window seen
/// by every later phase.
#[inline(always)]
fn enter_node<NODE: NodeType>(
    td: &mut ThreadData, window: &mut SearchWindow, depth: i32, cut_node: bool, ply: isize,
) -> NodeEntry {
    debug_assert!(ply as usize <= MAX_PLY);
    debug_assert!(-Score::INFINITE <= window.alpha && window.alpha < window.beta && window.beta <= Score::INFINITE);
    debug_assert!(NODE::PV || window.alpha == window.beta - 1);

    let node = FullNode::new::<NODE>(td, cut_node, ply);

    if !node.node_root && node.node_pv {
        td.pv_table.clear(ply as usize);
    }

    if td.is_stopped() {
        return NodeEntry::Return(Score::ZERO);
    }

    if depth <= 0 {
        return NodeEntry::QSearch;
    }

    let draw_score = draw(td);
    if !node.node_root && window.alpha < draw_score && td.board.upcoming_repetition(ply as usize) {
        window.alpha = draw_score;
        if window.alpha >= window.beta {
            return NodeEntry::Return(window.alpha);
        }
    }

    if node.node_pv {
        td.sel_depth = td.sel_depth.max(ply as i32);
    }

    if td.id == 0 && td.time_manager.check_time(td) {
        td.stop_search();
        return NodeEntry::Return(Score::ZERO);
    }

    if !node.node_root {
        if td.board.is_draw(ply) {
            return NodeEntry::Return(draw(td));
        }

        if ply as usize >= MAX_PLY - 1 {
            let score = if node.in_check { draw(td) } else { td.nnue.evaluate(&td.board) };
            return NodeEntry::Return(score);
        }

        window.alpha = window.alpha.max(mated_in(ply));
        window.beta = window.beta.min(mate_in(ply + 1));

        if window.alpha >= window.beta {
            return NodeEntry::Return(window.alpha);
        }
    }

    NodeEntry::Continue(node)
}

/// Depth-policy bias for helper threads.
///
/// Helper threads deliberately perturb reduction depth to diversify their search without changing
/// the root thread's deterministic shape. This is part of child-search depth policy, but it lives
/// here because it depends only on thread identity and is shared by the move-loop reduction code.
#[inline]
pub fn helper_reduction_bias(td: &ThreadData) -> i32 {
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
