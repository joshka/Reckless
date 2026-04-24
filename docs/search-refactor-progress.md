# Search Refactor Progress

## Current Status

- Stack is based on `joshka/stockfish-speedtest` over current `main`.
- Search is now split into concept modules for root, TT interpretation, eval,
  pruning, singular search, reductions, history feedback, finalization, and
  qsearch.
- Final validation is complete for this pass.

## Concept Checklist

- Recognizable chess-programming concept or clearly named engine-specific
  coupling.
- Fits in a maintainer's head: small functions, structs, and modules.
- Explicit ownership and side effects.
- Phase order remains visible where order is behavioral.
- Data flow supports cross-heuristic experimentation.
- Idiomatic Rust when it has no measured performance cost.
- No sustained speed regression; less than 2% speedtest movement is treated as
  normal noise unless it repeats across future work.

## Completed Blocks

- Wrote the target design and source reorganization audit.
- Rebased the stack on the `joshka/stockfish-speedtest` branch.
- Correctly validated quoted `speedtest 1 16 30` command form.
- Added source-level module contracts.
- Audited `ThreadData` field groups and `NodeType` branch roles in
  `docs/search-state-audit.md`.
- Introduced `TtProbe` for full-width search and qsearch TT interpretation.
  Validation: `cargo fmt --check`, `cargo check`, release build, bench
  `3425249` nodes, `speedtest 1 16 30` at `37508862` nodes / `1436186` nps.
- Introduced `EvalState` for full-width raw eval, corrected eval, TT-adjusted
  estimate, correction value, and improvement signals. Validation: bench
  `3425249` nodes, `speedtest 1 16 30` at `36939518` nodes / `1414007` nps.
- Isolated singular-extension verification into `SingularOutcome`. Validation:
  bench `3425249` nodes, `speedtest 1 16 30` at `37519102` nodes /
  `1436468` nps.
- Named pre-move pruning gates for razoring, reverse futility, null move, and
  ProbCut. Validation: bench `3425249` nodes, `speedtest 1 16 30` at
  `36759294` nodes / `1407378` nps.
- Extracted post-loop history feedback and final node-result guards. The
  full-width tail now reads as history learning, TT-PV propagation, beta-cutoff
  score shaping, TT writeback, and correction-history learning. Validation:
  `cargo fmt --check`, `cargo check`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `36062974` nodes / `1375924` nps.
- Named root-search concepts for aspiration windows, tablebase/MultiPV root
  groups, score/PV stability, and soft-stop voting. Validation: bench
  `3425249` nodes, `speedtest 1 16 30` at `35092222` nodes / `1341805` nps.
- Named qsearch eval state and tactical gates for stand-pat cutoff shaping,
  quiet skipping, LMP, SEE pruning, and final beta-cutoff shaping. Validation:
  bench `3425249` nodes, `speedtest 1 16 30` at `36419326` nodes /
  `1393988` nps.
- Reduced helper-level `NodeType` spread in qsearch eval and singular
  verification. The recursive search entry points remain monomorphized, but
  helper concepts now receive explicit root/PV facts where possible.
  Validation: bench `3425249` nodes, `speedtest 1 16 30` at `36460286` nodes /
  `1395876` nps.
- Named move-loop pruning gates and child-search reduction policy. The driver
  still shows reduced scout, full-depth scout, PVS, root result update, and
  alpha/beta update order, but the tuned LMP/futility/SEE and LMR/FDS formulas
  now live behind named concepts. Validation: bench `3425249` nodes,
  `speedtest 1 16 30` at `36151038` nodes / `1383772` nps.

## Problems / Risks

- `ThreadData` remains the large state bucket. The refactor avoids making it a
  conceptual boundary, but a deeper split would need a separate state-view
  design and more performance validation.
- `search::<NODE>` remains monomorphized. Helper spread is reduced, but the
  recursive entry and move picker still use the const-generic node kind for
  codegen.
- `make_move`/`undo_move` remain in `search/mod.rs`. A previous extraction of
  this area showed speed risk, and this block is still the most sensitive
  boundary.

## Final Validation

- `markdownlint-cli2 docs/search-algorithm.md docs/source-reorg-audit.md
  docs/search-refactor-target.md docs/search-refactor-progress.md
  docs/search-state-audit.md`: passed.
- `cargo fmt --check`: passed.
- `cargo check`: passed.
- `cargo rustc --release -- -C target-cpu=native`: passed.
- `target/release/reckless bench`: `3425249` nodes.
- `target/release/reckless "speedtest"`: `1772853147` nodes /
  `12110563` nps.

## Remaining Work

- None for this pass.
