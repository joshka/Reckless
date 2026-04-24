//! Search-side move transitions.
//!
//! Making a child move in search is not just a board operation. The transition also records stack
//! metadata for history feedback, increments the deterministic node counter, pushes NNUE state,
//! mutates the board, and prefetches the child TT bucket. Keeping those coupled side effects in one
//! small module makes the make/search/undo contract visible without making full-width search own a
//! shared primitive used by qsearch, singular verification, null move, and ProbCut.

use crate::{thread::ThreadData, types::Move};

/// Make a child move and update the coupled search-side state.
///
/// Node counting, stack move metadata, continuation-history pointers, NNUE push, board mutation,
/// and TT prefetch are kept in this exact order because downstream pruning, time management, and
/// deterministic node counts depend on it.
#[inline(always)]
pub fn make_move(td: &mut ThreadData, ply: isize, mv: Move) {
    td.stack[ply].mv = mv;
    td.stack[ply].piece = td.board.moved_piece(mv);
    td.stack[ply].conthist =
        td.continuation_history.subtable_ptr(td.board.in_check(), mv.is_noisy(), td.board.moved_piece(mv), mv.to());
    td.stack[ply].contcorrhist =
        td.continuation_corrhist.subtable_ptr(td.board.in_check(), mv.is_noisy(), td.board.moved_piece(mv), mv.to());

    td.shared.nodes.increment(td.id);

    td.nnue.push(mv, &td.board);
    td.board.make_move(mv, &mut td.nnue);

    td.shared.tt.prefetch(td.board.hash());
}

/// Undo a child move after all search-side consumers have finished.
///
/// This is intentionally paired with `make_move`: NNUE is popped before the board move is undone,
/// matching the push/make order and keeping later stack state owned by the caller.
#[inline(always)]
pub fn undo_move(td: &mut ThreadData, mv: Move) {
    td.nnue.pop();
    td.board.undo_move(mv);
}
