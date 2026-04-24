# Search Reading Order

This is the maintainer path for understanding search after the refactor. It
separates durable explanation from working notes so readers do not have to
reconstruct the algorithm from audit history.

## Primary Path

1. Read `docs/search-algorithm.md` for the algorithm, phase order, state
   contracts, and module map.
2. Read `src/search/mod.rs` as the module index and node-kind contract.
3. Read `src/search/root.rs` for UCI-facing iterative deepening, MultiPV,
   aspiration windows, reporting, and time feedback.
4. Read `src/search/full.rs` for the recursive full-width algorithm spine.
5. Follow the phase modules only as needed:
   - `src/search/tt.rs` for search-facing TT interpretation.
   - `src/search/eval.rs` for static eval, correction history, and stack setup.
   - `src/search/pruning.rs` for pre-move pruning and move-pruning formulas.
   - `src/search/singular.rs` for TT-move singular verification.
   - `src/search/moves.rs` for ordered child search.
   - `src/search/reductions.rs` for child-search depth policy.
   - `src/search/transition.rs` for make/search/undo transition invariants.
   - `src/search/history.rs` for post-loop history feedback.
   - `src/search/finalize.rs` for final node-result writeback and learning.
   - `src/search/qsearch.rs` for tactical leaf stabilization.
   - `src/search/tablebase.rs` for interior Syzygy proof handling.

## Reference Notes

- `docs/search-refactor-target.md` records the design rules and tradeoffs that
  shaped the refactor.
- `.notes/software-change-preferences.md` is the durable review checklist for
  future changes.
- `docs/search-state-audit.md` is a focused reference for `ThreadData`,
  `NodeType`, and parameter-shape decisions.

## Working Notes

These are useful for reviewing this change, but they are not the normal reading
path for future search work:

- `docs/search-refactor-progress.md`
- `docs/search-refactor-checklist.md`
- `docs/search-final-20-checklist.md`
- `docs/source-reorg-audit.md`
