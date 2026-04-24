# Search Refactor Progress

## Current Status

- Stack is based on `joshka/stockfish-speedtest` over current `main`.
- Search is now split into concept modules for root, TT interpretation, eval,
  pre-move pruning, singular search, reductions, full-width move search, node
  finalization, history feedback, and qsearch.
- Current focus is making the full-width `search` driver match the algorithm
  pseudo-code shape, not just moving code between files.

## Concept Checklist

- Detailed end-state criteria are tracked in
  `docs/search-refactor-checklist.md`.
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
- Revisited full move-loop extraction after checking the older
  `joshka/wip-refactor-attempt1` audit. That audit recommends paired or
  interleaved samples and warns that one noisy speedtest should not reject a
  refactor. The move loop now lives in `search/moves.rs` as the ordered child
  search phase. A small optimization keeps quiet/noisy buffers owned by the
  parent finalization path instead of returning them by value. Validation:
  bench `3425249` nodes, `speedtest 1 16 30` at `33548030` nodes /
  `1283251` nps before the buffer tweak and `33445630` nodes / `1279726` nps
  after it.
- Introduced `NodeContext`/`NodeEntry` for full-width entry guards, moved
  stack initialization and parent-eval feedback into `eval::prepare_full_width_node`,
  and moved null-move and ProbCut searches behind the pre-move pruning concept.
  The driver now shows the pruning phase as razor, RFP, null move, then ProbCut.
  Validation: `cargo fmt --check`, `cargo check`, release build, bench
  `3425249` nodes, `speedtest 1 16 30` at `32450302` nodes / `1239791` nps.
- Moved post-loop node completion into `finalize::finish_full_width_node`.
  The full-width tail now has one explicit conceptual boundary for no-move
  results, history feedback, TT-PV propagation, TT writeback, and correction
  history learning. Validation: `cargo fmt --check`, `cargo check`, release
  build, bench `3425249` nodes, `speedtest 1 16 30` at `32282366` nodes /
  `1233375` nps.
- Moved the full-width TT cutoff mutation into `tt::try_full_width_cutoff` and
  interior Syzygy probing into `tablebase::probe_full_width`. The driver now
  reads as TT proof, tablebase proof, then eval setup instead of carrying the
  proof mechanics inline. Validation: `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `33117950` nodes / `1265300` nps.
- Moved the recursive full-width driver into `search/full.rs`. `search/mod.rs`
  is now a module index plus the small `NodeType` marker surface, while
  `full.rs` owns the pseudo-code sequence for full-width nodes. Validation:
  `cargo fmt --check`, `cargo check`, `cargo check --no-default-features`,
  release build, bench `3425249` nodes, `speedtest 1 16 30` at `33292030`
  nodes / `1272728` nps.
- Worked through the completion checklist for source docs and phase contracts.
  Added documented concepts for `FullNode`, `SearchWindow`,
  `PreMovePruningContext`, `SingularInput`, `MoveLoopInput`, and
  `NodeFinalizationInput`; documented dense fields and enum variants across
  eval, TT, singular, tablebase, move-loop, qsearch, and reductions. Validation:
  markdownlint, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `33337086` nodes / `1274110` nps.
- Finished the checklist pass. Added `ProofState`, `MoveLoopState`,
  `MoveCandidate`, `MovePruningDecision`, `ChildSearchPlan`,
  `ChildSearchResult`, `HistoryUpdateContext`, and root `RootRetryState` /
  `search_multipv_slot` concepts. The checklist now has no unchecked items.
  Validation: markdownlint, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `33259262` nodes / `1271816` nps, and default
  `speedtest` at `1615987315` nodes / `11034471` nps.
- Closed the follow-up source-shape items. Added `FullSearchState` as the
  post-proof/eval local frame for full-width search, with conversion methods
  for pruning, singular verification, move-loop search, and finalization. This
  removes the broad input-literal reconstruction from the algorithm spine while
  keeping each phase input explicit at the concept boundary. Also replaced
  search-internal restricted visibility with ordinary `pub` where helpers are
  intentionally shared across search modules. Validation: markdownlint,
  `cargo fmt --check`, `cargo check`, `cargo check --no-default-features`,
  release build, bench `3425249` nodes, `speedtest 1 16 30` at `33627902`
  nodes / `1285814` nps.
- Audited remaining long helper parameter shapes. Added `EvalInput`,
  `StackPreparationInput`, and `QsearchEvalInput` where field groups were
  really phase context rather than formula terms. Documented the rule in
  `search-state-audit.md`: pass concept values when fields travel together as a
  phase, but keep scalars visible for tuned predicates and recursive
  alpha-beta contracts. Validation: markdownlint, `cargo fmt --check`,
  `cargo check`, `cargo check --no-default-features`, release build, bench
  `3425249` nodes, `speedtest 1 16 30` at `32730878` nodes / `1250272` nps.
- Applied the source-organization checklist to the hottest files. Moved
  `full::search()` to the top of `full.rs` and `moves::search()` near the top
  of `moves.rs`, so each file now opens with its central algorithm before
  private implementation details. Removed `ChildSearchPlan` after the
  adversarial pass showed it was constructed and immediately destructured, and
  removed `QsearchEvalInput` because it wrapped four obvious local facts
  without reducing reader state. Exposed the pre-move pruning order in
  `full::search()` through `PreMovePruningContext::{razor, reverse_futility,
  null_move, probcut}` while keeping formulas in `pruning.rs`. Validation:
  markdownlint, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `33140478` nodes / `1268292` nps.
- Responded to the follow-up readability audit. Reopened checklist items that
  were too broad to count as complete across all search files, added a concrete
  todo to define core terms such as "full-width", and documented the
  full-width/qsearch boundary in `docs/search-algorithm.md` and `full.rs`.
  Reshaped the move-loop entry so `moves::search()` no longer destructures a
  wide `MoveLoopInput` and rebuilds `ChildSearchInput`; it now uses a stable
  `MoveLoopContext` plus the two borrowed history buffers, and `search_child`
  reuses that context directly. Validation: markdownlint, `cargo fmt --check`,
  `cargo check`, `cargo check --no-default-features`, release build, bench
  `3425249` nodes, `speedtest 1 16 30` at `33283838` nodes / `1272415` nps.
- Continued the target-alignment pass. Added `NodeContext` for the early
  full-width node/window/depth context so eval and stack-preparation inputs no
  longer come from loose `FullNode`, `SearchWindow`, and depth variables.
  Narrowed `FullSearchState` to the later mutable search frame. Kept
  `SingularOutcome` as a struct after review because move ordering consumes
  its score, TT move, extension, and cutoff independently; added docs and a
  `cutoff_score()` accessor to make the immediate-return path explicit. Audited
  `NodeType` and `ThreadData` use and left them in place because the current
  uses remain concentrated in real root/PV/codegen and phase-state boundaries.
  Validation: markdownlint, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `34707198` nodes / `1327032` nps.
- Continued the source-organization pass. Reflowed module Rustdoc toward the
  repo's 100-column preference, moved `qsearch()` above its stand-pat helper
  type, kept `FullNode` next to its constructor, and normalized doc/attribute
  ordering so explanations appear before inline hints. The search files now
  open with either the module's central algorithm entry or the concept type
  whose `impl` defines the module's abstraction. Validation: markdownlint,
  Rustdoc line-width check, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `35667710` nodes / `1363862` nps.
- Closed the root-concept gap without splitting the root move record. `RootMove`
  now documents the concepts that share one physical struct: legal move
  identity, search result, UCI display state, tablebase ranking, PV, and node
  accounting. This matches the target design note that root move identity,
  result, and display state can be conceptually separated even when performance
  and reporting code keep them in one vector. Validation: `cargo fmt --check`
  and Rustdoc line-width check passed.
- Closed the move-loop fragmentation review. The previous weak
  `ChildSearchPlan` wrapper had already been removed; the remaining local types
  now have an explicit reading order in `moves.rs`. Each type separates a
  different live question for one candidate: node-stable context, mutable loop
  state, candidate facts, pruning outcome, child-search score/count, and depth
  policy. No further split was taken because it would add indirection without
  lowering the reader's live fact count.
- Ran the stopping-point default speedtest after the checklist reached zero
  unchecked items. Result: `1652814972` nodes / `11278703` nps.
- Cleaned up remaining context-spread call sites found after review. Candidate
  pruning, reduction-context construction, root move display updates, and early
  TT lower-bound writes now hang off `MoveLoopContext`; full-width TT/tablebase
  proof handoffs hang off `FullNode`; history feedback helpers consume
  `HistoryUpdateContext`; null-move and ProbCut proof helpers consume
  `PreMovePruningContext`. Validation: markdownlint, `cargo fmt --check`,
  `cargo check`, `cargo check --no-default-features`, release build, bench
  `3425249` nodes, `speedtest 1 16 30` at `33179390` nodes / `1269053` nps.
- Applied `.notes/software-change-preferences.md` as a file-order review
  checklist over the new search code. Fixed stale `NodeType` spread in proof
  probing, tightened pruning helper visibility, added missing public-entry
  docs, documented root retry/progress fields, replaced long public TT/tablebase
  helper signatures with named input concepts, and kept scalar-heavy pruning
  formulas as scalars where the terms are the tuned concept. Validation:
  markdownlint, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `33367806` nodes / `1275479` nps.
- Started the final-20 checklist. `RootMove` now owns root-result/display
  mutation through `record_search_result`, `start_depth`, and
  `same_tablebase_group`, so the move loop and root driver no longer set its
  search-result fields directly. The move loop now folds child scores through
  `MoveLoopState::accept_child_result`, `raise_alpha`, and
  `record_beta_cutoff`, leaving the loop body in candidate pruning,
  make/search/undo, stop/root update, alpha-beta update, searched-buffer order.
  Validation: `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `32483070` nodes / `1241186` nps.
- Named TT write roles without moving them away from their behavioral timing
  points. Raw eval cache, tablebase proof, ProbCut lower bound,
  alpha-raise lower bound, final full-width result, qsearch stand-pat lower
  bound, and final qsearch bound now have local helper names and docs; the
  full-width cutoff role remains `tt::try_full_width_cutoff`. Validation:
  markdownlint, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `32421630` nodes / `1238933` nps.
- Extracted the search-side move transition boundary into
  `search/transition.rs`. `make_move` / `undo_move` now own stack move
  metadata, continuation-history pointers, deterministic node increment, NNUE
  push/pop, board mutation, and TT prefetch outside the full-width spine while
  preserving the visible make/search/undo order at call sites. Validation:
  `cargo fmt --check`, `cargo check`, `cargo check --no-default-features`,
  release build, bench `3425249` nodes, `speedtest 1 16 30` at `32122622`
  nodes / `1227225` nps.
- Investigated small `ThreadData` phase views and kept the scope narrow.
  Search modules now use `ThreadData::is_stopped` and `stop_search` for the
  shared stop-state contract instead of reaching through `shared.status`; the
  thread pool still owns the lower-level RUNNING/STOPPED lifecycle. Broader
  root/history/shared views were deferred because they risked becoming a second
  all-access context. Validation: markdownlint, `cargo fmt --check`,
  `cargo check`, `cargo check --no-default-features`, release build, bench
  `3425249` nodes, `speedtest 1 16 30` at `32843518` nodes / `1255198` nps.
- Consolidated the durable reading path. Added
  `docs/search-reading-order.md`, updated `docs/search-algorithm.md` to include
  `reductions`, `transition`, `finalize`, and interior tablebase handling in
  the module map, and marked progress/checklist/audit files as working notes
  rather than the normal maintainer entry point.
- Ran final validation after the final-20 checklist reached zero unchecked
  items. Validation: markdownlint, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  default `speedtest` at `1517349735` nodes / `10354438` nps.
- Ran a follow-up readability pass over recently modified methods. The move
  loop now names fail-soft pruning score updates and child-search scout/PV
  sequencing through `MoveLoopState`, `ChildSearchState`, and
  `initial_child_depth`. Qsearch now carries tactical-loop alpha/beta state in
  `QsearchState`. ProbCut now uses `ProbCutProof` for the qsearch pretest,
  full-width confirmation, lower-bound write, and return-score shaping. Root
  UCI reporting now uses `RootMove::uci_report` and `RootMoveReport`.
  Validation: `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  `speedtest 1 16 30` at `32745214` nodes / `1251250` nps. Markdownlint still
  reports 80-column line-length errors in `.notes/software-change-preferences.md`;
  that note intentionally records the 100-column prose preference.
- Ran the `.notes/software-change-preferences.md` checklist against the new
  search code and reduced the remaining field traffic. `MoveLoopContext` is no
  longer a flat scalar bag; it now groups node/window facts, eval signals, TT
  proof state, and child-depth policy. Child search now uses
  `ChildSearchContext` for one already-made candidate. The full-search
  move-loop handoff builds named sub-concepts, razoring/RFP predicates moved
  onto `PreMovePruningContext`, and qsearch entry guards/TT proof moved into
  `QsearchEntry` / `QsearchNode`. Root slot bookkeeping was rechecked and left
  unchanged because the current loop still reads in root-search order.
  Validation: `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, `markdownlint-cli2 "docs/**/*.md"`,
  release build, bench `3425249` nodes, `speedtest 1 16 30` at `32216830`
  nodes / `1231766` nps.
- Ran a second preference-checklist pass over the remaining wide contexts.
  `PreMovePruningContext` now groups node/window facts, eval signals, TT
  proof/writeback state, and singular/null-move guard state. Move-loop
  candidate pruning now uses `CandidatePruningContext`, so late-move pruning,
  futility pruning, bad-noisy pruning, and SEE threshold checks no longer
  repeat broad scalar argument lists at the loop site. Qsearch final TT
  writeback now uses `QsearchBound`. `ChildSearchContext`, root slot search,
  and full-width finalization were rechecked and left in place because further
  extraction looked like a weaker wrapper around the same facts.
  Validation: `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, `markdownlint-cli2 "docs/**/*.md"`,
  release build, bench `3425249` nodes, and repeated `speedtest 1 16 30`.
  The first speed sample was anomalously low at `24145662` nodes /
  `915232` nps; the repeat recovered to `30734078` nodes / `1174805` nps.
- Ran a third preference-checklist pass on the remaining wide handoffs.
  Full-width finalization now receives `FinalizationNode`,
  `FinalizationProof`, and `SearchedMoveLists` instead of one flat packet.
  Null-move and ProbCut eligibility now live on `PreMovePruningContext`.
  Qsearch stand-pat eval and shallow TT writeback now consume `QsearchNode`
  rather than passing hash, ply, PV, and TT facts separately. Validation:
  `markdownlint-cli2 "docs/**/*.md"`, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  and `speedtest 1 16 30` at `33793790` nodes / `1292503` nps.
- Ran a fourth preference-checklist pass after the finalization split.
  Removed the short-lived `QsearchBound` wrapper because it was constructed
  only to write immediately; qsearch final TT storage now lives on
  `QsearchNode`. Moved finalization predicates and final TT writeback onto
  `FinalizationNode` and `FinalizationProof`, so the post-loop phase keeps its
  order without scalar helper calls. Validation: `markdownlint-cli2
  "docs/**/*.md"`, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  and `speedtest 1 16 30` at `32642814` nodes / `1247528` nps.
- Ran a fifth preference-checklist pass over remaining field traffic at hot
  call sites. Qsearch state methods now accept `QsearchNode` for quiet
  skipping, child acceptance, and cutoff-history update. Move-loop setup now
  passes `MoveLoopContext` to loop-state initialization and candidate
  construction instead of spelling out `context.node` fields. Validation:
  `markdownlint-cli2 "docs/**/*.md"`, `cargo fmt --check`, `cargo check`,
  `cargo check --no-default-features`, release build, bench `3425249` nodes,
  and `speedtest 1 16 30` at `31397630` nodes / `1199711` nps.

## Problems / Risks

- `ThreadData` remains the large state bucket. The refactor avoids making it a
  conceptual boundary, but a deeper split would need a separate state-view
  design and more performance validation.
- `search::<NODE>` remains monomorphized. Helper spread is reduced, but the
  recursive entry and move picker still use the const-generic node kind for
  codegen.
- `make_move`/`undo_move` now live in `search/transition.rs`. This is still one
  of the most sensitive boundaries because node counting, stack metadata, NNUE
  push/pop, board mutation, and TT prefetch all happen there.
- The full move loop and post-loop finalization are hot boundaries. Current
  short speed samples around `1.17M` to `1.29M` nps are lower than the best
  earlier samples but still within the broader noisy range noted during this
  work. Treat this as a review checkpoint for paired retesting or targeted
  codegen inspection, not as a proven sustained regression.
- The final default speedtest landed below the older recorded default baseline.
  Short direct samples during the final-20 work stayed in the expected noisy
  range, but the long result should be treated as a review checkpoint for
  paired retesting or targeted codegen inspection before publication.

## Latest Validation

- `cargo fmt --check`: passed.
- `cargo check`: passed.
- `cargo check --no-default-features`: passed.
- `cargo rustc --release -- -C target-cpu=native`: passed.
- `target/release/reckless bench`: `3425249` nodes / `1179568` nps.
- Latest `target/release/reckless "speedtest 1 16 30"`: `31397630` nodes /
  `1199711` nps.
- Anomalous prior sample from the same pass:
  `24145662` nodes / `915232` nps.
- Latest default `target/release/reckless speedtest` after the second
  preference-checklist pass: `1654432878` nodes / `11299768` nps.
- Latest `target/release/reckless speedtest`: `1517349735` nodes /
  `10354438` nps.
- Earlier `target/release/reckless "speedtest"`: `1372074520` nodes /
  `9325657` nps.
- Latest default `target/release/reckless "speedtest"` after completing the
  checklist pass: `1615987315` nodes / `11034471` nps.
- Follow-up direct comparison after the default run: parent
  `target/release/reckless "speedtest 1 16 30"` at `29759230` nodes /
  `1131616` nps; current at `33033982` nodes / `1263153` nps.

## Remaining Work

- `docs/search-refactor-checklist.md` and
  `docs/search-final-20-checklist.md` currently have no unchecked items.
- Final review should focus on paired performance confirmation, not more
  source reshaping. The source now has a coherent reading path and all core
  start/search concepts have named boundaries.
