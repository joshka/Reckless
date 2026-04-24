//! Child-search depth policy.
//!
//! The move loop uses two related reduction formulas: the late-move-reduction
//! scout and the full-depth scout. They are intentionally kept as formulas
//! rather than abstract strategy objects because the tuned inputs are
//! cross-heuristic signals that engine experiments often reuse.

use crate::types::{Move, is_valid};

use super::tt::TtProbe;

#[derive(Copy, Clone)]
pub(super) struct ReductionContext {
    pub depth: i32,
    pub move_count: i32,
    pub alpha: i32,
    pub beta: i32,
    pub correction: i32,
    pub alpha_raises: i32,
    pub tt_probe: TtProbe,
    pub tt_pv: bool,
    pub cut_node: bool,
    pub improving: bool,
    pub improvement: i32,
    pub child_cutoff_count: i32,
    pub tt_move_score: i32,
    pub singular_score: i32,
    pub parent_reduction: i32,
    pub helper_bias: i32,
    pub root_delta: i32,
    pub node_pv: bool,
}

impl ReductionContext {
    #[inline]
    pub fn late_move_reduction(self, is_quiet: bool, history: i32, child_in_check: bool) -> i32 {
        let mut reduction = 225 * (self.move_count.ilog2() * self.depth.ilog2()) as i32;

        reduction -= 68 * self.move_count;
        reduction -= 3297 * self.correction.abs() / 1024;
        reduction += 1306 * self.alpha_raises;

        reduction += 546 * (is_valid(self.tt_probe.score) && self.tt_probe.score <= self.alpha) as i32;
        reduction += 322 * (is_valid(self.tt_probe.score) && self.tt_probe.depth < self.depth) as i32;

        if is_quiet {
            reduction += 1806;
            reduction -= 166 * history / 1024;
        } else {
            reduction += 1449;
            reduction -= 109 * history / 1024;
        }

        if self.node_pv {
            reduction -= 424 + 433 * (self.beta - self.alpha) / self.root_delta;
        }

        if self.tt_pv {
            reduction -= 361;
            reduction -= 636 * (is_valid(self.tt_probe.score) && self.tt_probe.score > self.alpha) as i32;
            reduction -= 830 * (is_valid(self.tt_probe.score) && self.tt_probe.depth >= self.depth) as i32;
        }

        if !self.tt_pv && self.cut_node {
            reduction += 1818;
            reduction += 2118 * self.tt_probe.mv.is_null() as i32;
        }

        if !self.improving {
            reduction += (430 - 263 * self.improvement / 128).min(1096);
        }

        if child_in_check {
            reduction -= 1021;
        }

        if self.child_cutoff_count > 2 {
            reduction += 1515;
        }

        if is_valid(self.tt_move_score) && is_valid(self.singular_score) {
            let margin = self.tt_move_score - self.singular_score;
            reduction += (512 * (margin - 160) / 128).clamp(0, 2048);
        }

        if !self.node_pv && self.parent_reduction > reduction + 485 {
            reduction += 129;
        }

        reduction + self.helper_bias
    }

    #[inline]
    pub fn late_move_reduced_depth(self, new_depth: i32, reduction: i32) -> i32 {
        (new_depth - reduction / 1024).clamp(1, new_depth + (self.move_count <= 3) as i32 + 1) + 2 * self.node_pv as i32
    }

    #[inline]
    pub fn full_depth_reduction(self, mv: Move, is_quiet: bool, history: i32) -> i32 {
        let mut reduction = 232 * (self.move_count.ilog2() * self.depth.ilog2()) as i32;

        reduction -= 48 * self.move_count;
        reduction -= 2408 * self.correction.abs() / 1024;

        if is_quiet {
            reduction += 1429;
            reduction -= 152 * history / 1024;
        } else {
            reduction += 1053;
            reduction -= 67 * history / 1024;
        }

        if self.tt_pv {
            reduction -= 936;
            reduction -= 1080 * (is_valid(self.tt_probe.score) && self.tt_probe.depth >= self.depth) as i32;
        }

        if !self.tt_pv && self.cut_node {
            reduction += 1543;
            reduction += 2058 * self.tt_probe.mv.is_null() as i32;
        }

        if !self.improving {
            reduction += (409 - 254 * self.improvement / 128).min(1488);
        }

        if self.child_cutoff_count > 2 {
            reduction += 1360;
        }

        if is_valid(self.tt_move_score) && is_valid(self.singular_score) {
            let margin = self.tt_move_score - self.singular_score;
            reduction += (400 * (margin - 160) / 128).clamp(0, 2048);
        }

        if mv == self.tt_probe.mv {
            reduction -= 3281;
        }

        if !self.node_pv && self.parent_reduction > reduction + 562 {
            reduction += 130;
        }

        reduction + self.helper_bias
    }

    #[inline]
    pub fn full_depth_reduced_depth(self, new_depth: i32, reduction: i32) -> i32 {
        new_depth - (reduction >= 2864) as i32 - (reduction >= 5585) as i32
    }
}
