//! Singular-extension verification.
//!
//! Singular search asks whether the TT move is much better than the alternatives by temporarily
//! excluding it and searching a reduced node. The result can extend the TT move, cut the node by
//! multi-cut, suppress a misleading TT move, or apply a negative extension. This module does not
//! own normal move ordering; it only verifies whether the TT move deserves special treatment before
//! the move loop begins.

use crate::{
    thread::ThreadData,
    transposition::Bound,
    types::{Move, Score, is_decisive, is_valid},
};

use super::{NonPV, search, tt::TtProbe};

/// Result of the singular-extension verification search.
///
/// This remains a struct rather than an enum because the move loop consumes the effects
/// independently: the TT move may be replaced, the singular score feeds reductions even without a
/// cutoff, and the extension can be positive, negative, or zero. An enum named every branch but
/// made the caller translate back into these same three values before move ordering.
#[derive(Copy, Clone)]
pub struct SingularOutcome {
    /// Extension applied to the TT move in the normal move loop.
    ///
    /// Positive values mean the excluded-move search showed the TT move is singular enough to
    /// search deeper. Negative values deliberately reduce a TT move that failed to prove
    /// singularity strongly enough.
    pub extension: i32,

    /// Score returned by the excluded-move verification search.
    ///
    /// The move loop uses this even without a cutoff to adjust reduction margins around the
    /// singular-search evidence.
    pub score: i32,

    /// TT move that remains valid for ordering after verification.
    ///
    /// This may become `Move::NULL` when the verification search shows a different parent move has
    /// made the original TT move misleading.
    pub tt_move: Move,

    /// Immediate score if singular verification proves a multi-cut or stop.
    ///
    /// The caller must return this before normal move generation.
    pub cutoff: Option<i32>,
}

impl SingularOutcome {
    const fn none(tt_move: Move) -> Self {
        Self { extension: 0, score: Score::NONE, tt_move, cutoff: None }
    }

    /// Immediate score if singular verification proves a multi-cut or observes stop.
    #[inline]
    pub const fn cutoff_score(self) -> Option<i32> {
        self.cutoff
    }
}

/// Inputs for singular-extension verification.
///
/// The full-width node builds this after eval and pre-move pruning because the phase depends on
/// final depth, cut-node shape, TT evidence, and correction-history confidence.
#[derive(Copy, Clone)]
pub struct SingularInput {
    /// Current ply whose stack entry will temporarily exclude the TT move.
    pub ply: isize,

    /// Remaining full-width depth used to derive verification depth and margin.
    pub depth: i32,

    /// Current beta bound for multi-cut and negative-extension decisions.
    pub beta: i32,

    /// Whether this node is expected to fail high.
    pub cut_node: bool,

    /// Root nodes do not run singular verification.
    pub node_root: bool,

    /// PV nodes use different singular margins from non-PV nodes.
    pub node_pv: bool,

    /// Excluded singular-verification nodes must not recursively singular-search.
    pub excluded: bool,

    /// Eligibility computed from TT depth, bound, score, and current depth.
    pub potential: bool,

    /// TT move, score, bound, depth, and TT-PV marker driving verification.
    pub tt_probe: TtProbe,

    /// Correction-history magnitude used to soften double/triple-extension margins when static eval
    /// is less trusted.
    pub correction: i32,
}

/// Run singular-extension verification when the TT evidence is strong enough.
///
/// The search temporarily excludes the TT move at the current ply and searches alternatives with a
/// reduced non-PV window. That stack mutation is the core invariant: the excluded move must be
/// restored before returning, and the caller must apply the returned TT-move replacement before
/// move ordering.
pub fn search_if_needed(td: &mut ThreadData, input: SingularInput) -> SingularOutcome {
    let SingularInput {
        ply,
        depth,
        beta,
        cut_node,
        node_root,
        node_pv,
        excluded,
        potential,
        tt_probe,
        correction,
    } = input;

    if node_root || excluded || !potential {
        return SingularOutcome::none(tt_probe.mv);
    }

    debug_assert!(is_valid(tt_probe.score));

    let singular_margin = if tt_probe.bound == Bound::Exact { (depth as u32).div_ceil(4) as i32 } else { depth }
        + depth * (tt_probe.tt_pv && !node_pv) as i32;
    let singular_beta = tt_probe.score - singular_margin;
    let singular_depth = (depth - 1) / 2;

    td.stack[ply].excluded = tt_probe.mv;
    td.stack[ply].mv = Move::NULL;
    let score = search::<NonPV>(td, singular_beta - 1, singular_beta, singular_depth, cut_node, ply);
    td.stack[ply].excluded = Move::NULL;

    if td.is_stopped() {
        return SingularOutcome {
            cutoff: Some(Score::ZERO),
            ..SingularOutcome::none(tt_probe.mv)
        };
    }

    if score < singular_beta {
        let double_margin = 204 * node_pv as i32 - 16 * tt_probe.mv.is_quiet() as i32 - 16 * correction.abs() / 128;
        let triple_margin =
            257 * node_pv as i32 - 16 * tt_probe.mv.is_quiet() as i32 - 15 * correction.abs() / 128 + 32;

        let mut extension = 1;
        extension += (score < singular_beta - double_margin) as i32;
        extension += (score < singular_beta - triple_margin) as i32;

        return SingularOutcome { extension, score, tt_move: tt_probe.mv, cutoff: None };
    }

    if score >= beta && !is_decisive(score) {
        return SingularOutcome {
            score,
            tt_move: tt_probe.mv,
            cutoff: Some((2 * score + beta) / 3),
            extension: 0,
        };
    }

    if score > tt_probe.score && td.stack[ply].mv != Move::NULL {
        return SingularOutcome { score, tt_move: Move::NULL, extension: 0, cutoff: None };
    }

    if tt_probe.score >= beta || cut_node {
        return SingularOutcome { score, tt_move: tt_probe.mv, extension: -2, cutoff: None };
    }

    SingularOutcome {
        score,
        tt_move: tt_probe.mv,
        ..SingularOutcome::none(tt_probe.mv)
    }
}
