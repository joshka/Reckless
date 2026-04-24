//! Singular-extension verification.
//!
//! Singular search asks whether the TT move is much better than the alternatives
//! by temporarily excluding it and searching a reduced node. The result can
//! extend the TT move, cut the node by multi-cut, suppress a misleading TT move,
//! or apply a negative extension.

use crate::{
    thread::{Status, ThreadData},
    transposition::Bound,
    types::{Move, Score, is_decisive, is_valid},
};

use super::{NodeType, NonPV, search, tt::TtProbe};

#[derive(Copy, Clone)]
pub(super) struct SingularOutcome {
    pub extension: i32,
    pub score: i32,
    pub tt_move: Move,
    pub cutoff: Option<i32>,
}

impl SingularOutcome {
    const fn none(tt_move: Move) -> Self {
        Self { extension: 0, score: Score::NONE, tt_move, cutoff: None }
    }
}

pub(super) fn search_if_needed<NODE: NodeType>(
    td: &mut ThreadData, ply: isize, depth: i32, beta: i32, cut_node: bool, excluded: bool, potential: bool,
    tt_probe: TtProbe, correction: i32,
) -> SingularOutcome {
    if NODE::ROOT || excluded || !potential {
        return SingularOutcome::none(tt_probe.mv);
    }

    debug_assert!(is_valid(tt_probe.score));

    let singular_margin = if tt_probe.bound == Bound::Exact { (depth as u32).div_ceil(4) as i32 } else { depth }
        + depth * (tt_probe.tt_pv && !NODE::PV) as i32;
    let singular_beta = tt_probe.score - singular_margin;
    let singular_depth = (depth - 1) / 2;

    td.stack[ply].excluded = tt_probe.mv;
    td.stack[ply].mv = Move::NULL;
    let score = search::<NonPV>(td, singular_beta - 1, singular_beta, singular_depth, cut_node, ply);
    td.stack[ply].excluded = Move::NULL;

    if td.shared.status.get() == Status::STOPPED {
        return SingularOutcome {
            cutoff: Some(Score::ZERO),
            ..SingularOutcome::none(tt_probe.mv)
        };
    }

    if score < singular_beta {
        let double_margin = 204 * NODE::PV as i32 - 16 * tt_probe.mv.is_quiet() as i32 - 16 * correction.abs() / 128;
        let triple_margin =
            257 * NODE::PV as i32 - 16 * tt_probe.mv.is_quiet() as i32 - 15 * correction.abs() / 128 + 32;

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
