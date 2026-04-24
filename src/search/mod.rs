//! Search module index and shared node-kind surface.
//!
//! The recursive full-width algorithm lives in `full`; root iterative deepening, qsearch, TT
//! policy, eval, pruning, singular verification, move search, and finalization live in sibling
//! concept modules. This file keeps the public search entry point and the small compile-time
//! node-kind markers that the hot path still relies on for branch elimination. It does not own
//! search behavior beyond wiring the module tree and the node kind markers shared by those modules.

#[allow(unused_imports)]
use crate::misc::{dbg_hit, dbg_stats};

mod eval;
mod finalize;
mod full;
mod history;
mod moves;
mod pruning;
mod qsearch;
mod reductions;
mod root;
mod singular;
#[cfg(feature = "syzygy")]
mod tablebase;
mod transition;
mod tt;

pub use full::{helper_reduction_bias, search};
pub use root::{Report, start};
pub use transition::{make_move, undo_move};

/// Compile-time search node kind.
///
/// Root, PV, and non-PV nodes have different alpha-beta contracts and many hot branches depend on
/// those contracts. Keeping the kind as associated constants lets LLVM remove impossible branches
/// in recursive search and move picking. Replacing this with runtime flags would need direct speed
/// validation.
pub trait NodeType {
    /// Whether this node keeps principal-variation state and may search with a wider window than a
    /// null-window non-PV node.
    const PV: bool;
    /// Whether this node is the root search, with root-move filtering, MultiPV behavior, root
    /// reporting, and no parent node.
    const ROOT: bool;
}

/// Root PV node at ply zero.
///
/// Enables both PV behavior and root-only behavior: root move filtering, root-result updates,
/// MultiPV restrictions, and root-specific TT write rules.
struct Root;
impl NodeType for Root {
    const PV: bool = true;
    const ROOT: bool = true;
}

/// Interior principal-variation node.
///
/// Enables PV-table maintenance and wider-window PVS behavior while keeping root-only reporting,
/// root move filtering, and MultiPV rules disabled.
struct PV;
impl NodeType for PV {
    const PV: bool = true;
    const ROOT: bool = false;
}

/// Interior non-PV node.
///
/// Uses the null-window alpha-beta contract. Most pruning and TT cutoffs are tuned around this
/// shape, and the implementation assumes `alpha == beta - 1` at entry.
struct NonPV;
impl NodeType for NonPV {
    const PV: bool = false;
    const ROOT: bool = false;
}
