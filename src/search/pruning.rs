//! Pre-move pruning gates.
//!
//! These checks can return or search a reduced tactical subset before normal move generation. Their
//! order is part of search behavior, so the full-width driver keeps the sequence visible and calls
//! these helpers only for the gate predicates. This module does not own move ordering or history
//! updates; it only decides whether a node or candidate can be skipped or proven early.

use crate::{
    movepick::{MovePicker, Stage},
    thread::ThreadData,
    transposition::Bound,
    types::{Color, Move, Piece, PieceType, Score, is_decisive, is_loss, is_valid, is_win},
};

use super::{NodeType, NonPV, make_move, qsearch::qsearch, search as full_search, tt::TtProbe, undo_move};

/// Inputs shared by the full-width pre-move pruning phase.
///
/// These values are produced by node entry, TT/tablebase proof, eval setup, and stack preparation.
/// Grouping them makes the full-width driver call one ordered pruning phase while keeping the
/// individual pruning formulas in this module.
#[derive(Copy, Clone)]
pub struct PreMovePruningContext {
    /// Node and alpha-beta window facts for the pruning phase.
    pub node: PreMoveNode,

    /// Static eval and trend signals used by pruning margins.
    pub eval: PreMoveEval,

    /// TT proof and writeback state used by pruning gates.
    pub proof: PreMoveProof,

    /// Singular/null-move guard state shared by pruning proofs.
    pub guard: PreMoveGuard,
}

/// Node and alpha-beta window facts for pre-move pruning.
#[derive(Copy, Clone)]
pub struct PreMoveNode {
    /// Current ply searched by the pruning proof.
    pub ply: isize,

    /// Side to move, used to test whether own threats are empty for RFP.
    pub stm: Color,

    /// Cut-node shape required by null move and ProbCut.
    pub cut_node: bool,

    /// Whether the node is in check; pre-move pruning mostly opts out in check.
    pub in_check: bool,

    /// Whether this is a singular-verification node with one move excluded.
    pub excluded: bool,

    /// Current lower alpha bound.
    pub alpha: i32,

    /// Current upper beta bound.
    pub beta: i32,

    /// Remaining full-width depth.
    pub depth: i32,
}

/// Static eval and trend signals used by pre-move pruning.
#[derive(Copy, Clone)]
pub struct PreMoveEval {
    /// Static estimate after correction history and compatible TT bounds.
    pub estimated_score: i32,

    /// Numeric eval trend signal used by null move.
    pub improvement: i32,

    /// Boolean eval trend signal used by RFP and ProbCut.
    pub improving: bool,

    /// Correction-history value used by RFP margins.
    pub correction: i32,

    /// Corrected static eval used by ProbCut eligibility.
    pub corrected: i32,
}

/// TT proof and writeback state used by pre-move pruning.
#[derive(Copy, Clone)]
pub struct PreMoveProof {
    /// TT-PV marker that tightens pruning margins.
    pub tt_pv: bool,

    /// TT probe used by razor, null move, and ProbCut guards.
    pub tt_probe: TtProbe,

    /// Raw NNUE eval written if ProbCut stores a TT bound.
    pub raw_eval: i32,

    /// Position hash used by ProbCut TT writeback.
    pub hash: u64,
}

/// Singular/null-move guard state shared by pre-move pruning proofs.
#[derive(Copy, Clone)]
pub struct PreMoveGuard {
    /// Whether TT evidence says the node may be singular, blocking null move.
    pub potential_singularity: bool,
}

impl PreMovePruningContext {
    /// Try the shallow razoring fallback.
    ///
    /// Razoring is first because it is a cheap non-PV gate that uses qsearch as the tactical
    /// fallback before the node pays for stronger pruning proofs or normal move generation.
    #[inline(always)]
    pub fn razor<NODE: NodeType>(self, td: &mut ThreadData) -> Option<i32> {
        self.can_razor::<NODE>().then(|| qsearch::<NonPV>(td, self.node.alpha, self.node.beta, self.node.ply))
    }

    /// Try reverse futility pruning from static information.
    ///
    /// RFP follows razoring because it can return from the corrected/TT-adjusted estimate without
    /// changing board state. It still consults current threats because this engine tunes the margin
    /// around whether the side to move has own threats.
    #[inline(always)]
    pub fn reverse_futility(self, td: &ThreadData) -> Option<i32> {
        self.reverse_futility_score((td.board.all_threats() & td.board.colors(self.node.stm)).is_empty())
    }

    /// Try the null-move pruning proof.
    ///
    /// Null move is the first pruning phase here that mutates board state. It deliberately follows
    /// the pure static gates and precedes ProbCut because it tries to prove the whole node by
    /// passing rather than searching tactical captures.
    #[inline(always)]
    pub fn null_move(self, td: &mut ThreadData) -> Option<i32> {
        try_null_move(td, self)
    }

    /// Try the ProbCut tactical pre-test.
    ///
    /// ProbCut stays last because it searches promising captures against a raised beta before
    /// normal move generation. A successful proof may also write a TT lower bound.
    #[inline(always)]
    pub fn probcut<NODE: NodeType>(self, td: &mut ThreadData) -> Option<i32> {
        try_probcut::<NODE>(td, self)
    }

    /// Razoring gate before normal move generation.
    ///
    /// This is a shallow, non-PV shortcut for positions whose estimated score is far below alpha.
    /// The context owns all guard inputs; the public phase method owns the qsearch fallback
    /// searched before returning.
    #[inline]
    fn can_razor<NODE: NodeType>(self) -> bool {
        !NODE::PV
            && !self.node.in_check
            && self.eval.estimated_score < self.node.alpha - 295 - 261 * self.node.depth * self.node.depth
            && self.node.alpha < 2048
            && !self.proof.tt_probe.mv.is_quiet()
            && self.proof.tt_probe.bound != crate::transposition::Bound::Lower
    }

    /// Reverse futility pruning result for quiet non-PV nodes.
    ///
    /// The score is fail-soft: when static information is far above beta, the node returns a damped
    /// score instead of proving moves. TT-PV, checks, exclusions, and decisive windows opt out
    /// because those cases need exact search shape.
    #[inline]
    fn reverse_futility_score(self, own_threats_empty: bool) -> Option<i32> {
        if self.proof.tt_pv
            || self.node.in_check
            || self.node.excluded
            || is_loss(self.node.beta)
            || is_win(self.eval.estimated_score)
        {
            return None;
        }

        let margin = (1165 * self.node.depth * self.node.depth / 128 - (80 * self.eval.improving as i32)
            + 25 * self.node.depth
            + 560 * self.eval.correction.abs() / 1024
            - 59 * own_threats_empty as i32
            + 30)
            .max(0);

        (self.eval.estimated_score >= self.node.beta + margin)
            .then_some(self.node.beta + (self.eval.estimated_score - self.node.beta) / 3)
    }

    /// Null-move pruning eligibility.
    ///
    /// Null move is a cut-node proof attempt, so it is blocked in checks, excluded
    /// singular-verification nodes, potential singular nodes, low material, and tactical TT-capture
    /// situations where zugzwang or a capture race can make the shortcut misleading.
    #[inline]
    fn can_try_null_move(self, td: &ThreadData) -> bool {
        let child_cutoff_count = td.stack[self.node.ply + 1].cutoff_count;

        self.node.cut_node
            && !self.node.in_check
            && !self.node.excluded
            && !self.guard.potential_singularity
            && self.eval.estimated_score
                >= self.node.beta
                    + (-8 * self.node.depth + 116 * self.proof.tt_pv as i32
                        - 106 * self.eval.improvement / 1024
                        - 20 * (child_cutoff_count < 2) as i32
                        + 304)
                        .max(0)
            && self.node.ply as i32 >= td.nmp_min_ply
            && td.board.material() > 600
            && !is_loss(self.node.beta)
            && !(self.proof.tt_probe.bound == crate::transposition::Bound::Lower
                && self.proof.tt_probe.mv.is_capture()
                && td.board.piece_on(self.proof.tt_probe.mv.to()).value() >= PieceType::Knight.value())
    }

    /// ProbCut eligibility before the main move loop.
    ///
    /// ProbCut searches promising captures against a raised beta. The TT and eval guards avoid
    /// paying that tactical search unless there is already evidence that a capture can plausibly
    /// exceed the raised threshold.
    #[inline]
    fn can_try_probcut(self, probcut_beta: i32) -> bool {
        self.node.cut_node
            && !is_win(self.node.beta)
            && if is_valid(self.proof.tt_probe.score) {
                self.proof.tt_probe.score >= probcut_beta && !is_decisive(self.proof.tt_probe.score)
            } else {
                self.eval.corrected >= self.node.beta
            }
            && !self.proof.tt_probe.mv.is_quiet()
    }
}

/// Search the null-move pruning proof.
///
/// The side to move deliberately passes, then a reduced non-PV child tries to prove the position
/// still fails high. Deep null-move cutoffs are verified by a second search guarded by
/// `nmp_min_ply` to reduce zugzwang risk. The helper owns the null move make/undo pair, restores
/// board state before returning, and converts shared stop into the neutral stopped score.
#[inline(always)]
fn try_null_move(td: &mut ThreadData, ctx: PreMovePruningContext) -> Option<i32> {
    if !ctx.can_try_null_move(td) {
        return None;
    }

    debug_assert_ne!(td.stack[ctx.node.ply - 1].mv, Move::NULL);

    let r =
        (5335 + 260 * ctx.node.depth + 493 * (ctx.eval.estimated_score - ctx.node.beta).clamp(0, 1003) / 128) / 1024;

    td.stack[ctx.node.ply].conthist = td.stack.sentinel().conthist;
    td.stack[ctx.node.ply].contcorrhist = td.stack.sentinel().contcorrhist;
    td.stack[ctx.node.ply].piece = Piece::None;
    td.stack[ctx.node.ply].mv = Move::NULL;

    td.board.make_null_move();
    td.shared.tt.prefetch(td.board.hash());

    let score =
        -full_search::<NonPV>(td, -ctx.node.beta, -ctx.node.beta + 1, ctx.node.depth - r, false, ctx.node.ply + 1);

    td.board.undo_null_move();

    if td.is_stopped() {
        return Some(Score::ZERO);
    }

    if score < ctx.node.beta || is_win(score) {
        return None;
    }

    if td.nmp_min_ply > 0 || ctx.node.depth < 16 {
        return Some(score);
    }

    td.nmp_min_ply = ctx.node.ply as i32 + 3 * (ctx.node.depth - r) / 4;
    let verified_score =
        full_search::<NonPV>(td, ctx.node.beta - 1, ctx.node.beta, ctx.node.depth - r, false, ctx.node.ply);
    td.nmp_min_ply = 0;

    if td.is_stopped() {
        return Some(Score::ZERO);
    }

    (verified_score >= ctx.node.beta).then_some(score)
}

/// Search the ProbCut tactical pre-test.
///
/// ProbCut raises beta and asks whether a promising capture can cheaply prove a cutoff before the
/// full move loop. A qsearch pretest filters candidates; a reduced full-width confirmation supplies
/// the real proof. Successful non-decisive scores are blended back toward beta before returning,
/// and the confirmed lower bound is written to TT for later move ordering and pruning.
#[inline(always)]
fn try_probcut<NODE: NodeType>(td: &mut ThreadData, ctx: PreMovePruningContext) -> Option<i32> {
    let mut probcut_beta = ctx.node.beta + 270 - 75 * ctx.eval.improving as i32;

    if !ctx.can_try_probcut(probcut_beta) {
        return None;
    }

    let mut move_picker = MovePicker::new_probcut(probcut_beta - ctx.eval.corrected);

    while let Some(mv) = move_picker.next::<NODE>(td, true, ctx.node.ply) {
        if move_picker.stage() == Stage::BadNoisy {
            break;
        }

        if mv == td.stack[ctx.node.ply].excluded {
            continue;
        }

        make_move(td, ctx.node.ply, mv);

        let mut proof = ProbCutProof::qsearch_pretest(td, ctx, probcut_beta);
        proof.confirm_if_promising(td, ctx);

        undo_move(td, mv);

        if td.is_stopped() {
            return Some(Score::ZERO);
        }

        probcut_beta = proof.beta;

        if proof.proves_cutoff() {
            proof.write_lower_bound(td, ctx, mv);
            return Some(proof.return_score(ctx));
        }
    }

    None
}

/// Per-candidate ProbCut proof state.
///
/// ProbCut first asks qsearch whether the capture can reach a raised beta. A promising candidate
/// then receives a reduced full-width confirmation. The confirmation may raise beta with depth so
/// the stored lower bound reflects the actual proof searched for this candidate.
struct ProbCutProof {
    /// Raised beta used by this candidate.
    beta: i32,

    /// Maximum reduced depth available to the confirmation search.
    base_depth: i32,

    /// Depth actually used by the confirmation search and TT lower-bound write.
    depth: i32,

    /// Score returned by the qsearch pretest or full-width confirmation.
    score: i32,
}

impl ProbCutProof {
    #[inline(always)]
    fn qsearch_pretest(td: &mut ThreadData, ctx: PreMovePruningContext, beta: i32) -> Self {
        let score = -qsearch::<NonPV>(td, -beta, -beta + 1, ctx.node.ply + 1);
        let base_depth = (ctx.node.depth - 4).max(0);
        let depth = (base_depth - (score - beta) / 319).clamp(0, base_depth);

        Self { beta, base_depth, depth, score }
    }

    #[inline(always)]
    fn confirm_if_promising(&mut self, td: &mut ThreadData, ctx: PreMovePruningContext) {
        if self.score < self.beta || self.depth == 0 {
            return;
        }

        let adjusted_beta = (self.beta + 260 * (self.base_depth - self.depth)).min(Score::INFINITE);

        self.score = -full_search::<NonPV>(td, -adjusted_beta, -adjusted_beta + 1, self.depth, false, ctx.node.ply + 1);

        if self.score < adjusted_beta && self.beta < adjusted_beta {
            self.depth = self.base_depth;
            self.score = -full_search::<NonPV>(td, -self.beta, -self.beta + 1, self.depth, false, ctx.node.ply + 1);
        } else {
            self.beta = adjusted_beta;
        }
    }

    #[inline]
    fn proves_cutoff(&self) -> bool {
        self.score >= self.beta
    }

    /// Store the confirmed lower bound before score blending.
    #[inline(always)]
    fn write_lower_bound(&self, td: &mut ThreadData, ctx: PreMovePruningContext, mv: Move) {
        write_probcut_lower_bound(td, ctx, self.depth + 1, self.score, mv);
    }

    /// Return the cutoff score seen by the caller.
    ///
    /// Non-decisive ProbCut results are blended toward the original beta after the TT write. The
    /// stored lower bound keeps the searched proof score; the caller gets the damped fail-soft
    /// value used by the parent search.
    #[inline]
    fn return_score(&self, ctx: PreMovePruningContext) -> i32 {
        if is_decisive(self.score) { self.score } else { (3 * self.score + ctx.node.beta) / 4 }
    }
}

/// Store a confirmed ProbCut lower bound.
///
/// ProbCut proves the ordinary position with a reduced full-width search after a qsearch pretest.
/// The write belongs at the successful candidate, before the score is blended toward beta for the
/// caller, so later probes see the actual confirmed lower-bound score and move.
#[inline(always)]
fn write_probcut_lower_bound(td: &mut ThreadData, ctx: PreMovePruningContext, depth: i32, score: i32, mv: Move) {
    td.shared.tt.write(
        ctx.proof.hash,
        depth,
        ctx.proof.raw_eval,
        score,
        Bound::Lower,
        mv,
        ctx.node.ply,
        ctx.proof.tt_pv,
        false,
    );
}

/// Late move pruning gate inside the move loop.
///
/// Once enough quiet moves have failed, a later non-checking quiet can be skipped. The history and
/// improvement terms keep this as an ordering-aware pruning rule rather than a raw move-count
/// cutoff.
#[inline]
pub fn late_move_prunes(
    in_check: bool, gives_direct_check: bool, is_quiet: bool, move_count: i32, improvement: i32, depth: i32,
    history: i32,
) -> bool {
    !in_check
        && !gives_direct_check
        && is_quiet
        && move_count >= (3006 + 70 * improvement / 16 + 1455 * depth * depth + 68 * history / 1024) / 1024
}

/// Quiet futility pruning score inside the move loop.
///
/// Returning a value instead of `bool` preserves the fail-soft update to `best_score`; the caller
/// still owns the decision to skip remaining quiets.
#[inline]
pub fn futility_prune_score(
    in_check: bool, gives_direct_check: bool, is_quiet: bool, eval: i32, beta: i32, depth: i32, history: i32,
    alpha: i32,
) -> Option<i32> {
    let futility_value = eval + 79 * depth + 64 * history / 1024 + 84 * (eval >= beta) as i32 - 115;

    (!in_check && is_quiet && depth < 15 && futility_value <= alpha && !gives_direct_check).then_some(futility_value)
}

/// Bad-noisy futility pruning score.
///
/// This only applies after move ordering has reached the bad-noisy stage. A failure here stops the
/// noisy tail instead of just skipping one move because the remaining captures are ordered as even
/// less promising.
#[inline]
pub fn bad_noisy_futility_score(
    in_check: bool, gives_direct_check: bool, stage: Stage, eval: i32, depth: i32, history: i32, alpha: i32,
) -> Option<i32> {
    let noisy_futility_value = eval + 71 * depth + 68 * history / 1024 + 23;

    (!in_check && depth < 11 && stage == Stage::BadNoisy && noisy_futility_value <= alpha && !gives_direct_check)
        .then_some(noisy_futility_value)
}

/// SEE pruning threshold for the current move class.
///
/// The threshold is intentionally separate from the SEE call so the driver keeps the tactical
/// legality check visible while the tuned quiet/noisy formulas stay named.
#[inline]
pub fn see_threshold(is_quiet: bool, depth: i32, history: i32) -> i32 {
    if is_quiet {
        (-17 * depth * depth + 52 * depth - 21 * history / 1024 + 20).min(0)
    } else {
        (-8 * depth * depth - 36 * depth - 32 * history / 1024 + 11).min(0)
    }
}
