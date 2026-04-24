//! Interior-node Syzygy tablebase probing.
//!
//! Root tablebase ranking belongs to root setup, but interior tablebase probes are a full-width
//! proof source. The probe can return an immediate bound, seed a PV lower bound, or cap a PV score
//! with an upper bound. Its eligibility is deliberately before static eval and move generation
//! because successful probes avoid both. This module does not own root tablebase ranking or Syzygy
//! probing internals; it owns how an interior probe feeds full-width search.

use std::sync::atomic::Ordering;

use crate::{
    tb,
    thread::ThreadData,
    transposition::Bound,
    types::{MAX_PLY, Move, Score, tb_loss_in, tb_win_in},
};

/// Result of a full-width tablebase probe.
pub enum ProbeResult {
    /// The tablebase result proves the current alpha-beta window.
    ///
    /// The caller should return this score immediately.
    Cutoff(i32),

    /// A PV tablebase win is below beta but above the current lower bound.
    ///
    /// The caller should seed `best_score` and raise alpha before eval and move generation
    /// continue.
    PvLower(i32),

    /// A PV tablebase loss is above alpha but caps the final PV score.
    ///
    /// The caller should carry this as a maximum score into node finalization.
    PvUpper(i32),

    /// No usable tablebase information for this node.
    None,
}

/// Interior full-width tablebase probe request.
///
/// Interior probes are only safe for the ordinary full-width move set and the current alpha-beta
/// window. Root tablebase ranking uses a different contract and should not build this value.
#[derive(Copy, Clone)]
pub struct ProbeInput {
    /// Current ply for mate-distance tablebase score conversion.
    pub ply: isize,

    /// Position hash used for TT writeback on cutoffs.
    pub hash: u64,

    /// Whether this is the root node, where root tablebase ranking owns the result.
    pub node_root: bool,

    /// Whether this is a PV node that can carry non-cutoff tablebase bounds.
    pub node_pv: bool,

    /// Whether this node excludes one move for singular verification.
    pub excluded: bool,

    /// Remaining full-width depth used for tablebase TT write depth.
    pub depth: i32,

    /// Current alpha lower bound.
    pub alpha: i32,

    /// Current beta upper bound.
    pub beta: i32,

    /// TT-PV marker stored with tablebase cutoff entries.
    pub tt_pv: bool,
}

/// Probe Syzygy for an eligible interior node.
///
/// Non-root, non-excluded zeroing positions with no castling rights and few enough pieces may be
/// proved without eval or move generation. Exact results and window-compatible bounds cut off
/// immediately; otherwise PV nodes can use a lower bound as the current best score or an upper
/// bound as a final cap.
#[inline(always)]
pub fn probe_full_width(td: &mut ThreadData, input: ProbeInput) -> ProbeResult {
    let ProbeInput {
        ply,
        hash,
        node_root,
        node_pv,
        excluded,
        depth,
        alpha,
        beta,
        tt_pv,
    } = input;

    if node_root
        || excluded
        || td.shared.stop_probing_tb.load(Ordering::Relaxed)
        || td.board.halfmove_clock() != 0
        || td.board.castling().raw() != 0
        || td.board.occupancies().popcount() > tb::size()
    {
        return ProbeResult::None;
    }

    let Some(outcome) = tb::probe(&td.board) else {
        return ProbeResult::None;
    };

    td.shared.tb_hits.increment(td.id);

    let (score, bound) = match outcome {
        tb::GameOutcome::Win => (tb_win_in(ply), Bound::Lower),
        tb::GameOutcome::Loss => (tb_loss_in(ply), Bound::Upper),
        tb::GameOutcome::Draw => (Score::ZERO, Bound::Exact),
    };

    if bound == Bound::Exact || (bound == Bound::Lower && score >= beta) || (bound == Bound::Upper && score <= alpha) {
        write_tablebase_cutoff(td, hash, depth, score, bound, ply, tt_pv);
        return ProbeResult::Cutoff(score);
    }

    if !node_pv {
        return ProbeResult::None;
    }

    match bound {
        Bound::Lower => ProbeResult::PvLower(score),
        Bound::Upper => ProbeResult::PvUpper(score),
        _ => ProbeResult::None,
    }
}

/// Store an interior tablebase proof for later full-width probes.
///
/// Tablebase cutoffs are search proofs, but they have no searched best move and no raw eval. The
/// stored depth is inflated so nearby full-width probes can trust the result as a durable bound.
#[inline(always)]
fn write_tablebase_cutoff(
    td: &mut ThreadData, hash: u64, depth: i32, score: i32, bound: Bound, ply: isize, tt_pv: bool,
) {
    let depth = (depth + 6).min(MAX_PLY as i32 - 1);
    td.shared.tt.write(hash, depth, Score::NONE, score, bound, Move::NULL, ply, tt_pv, false);
}
