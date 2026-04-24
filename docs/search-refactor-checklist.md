# Search Refactor Completion Checklist

This checklist defines the end state for the search refactor. A pass is not
complete just because code moved into modules. It is complete when the algorithm
is visible from the code, the chess concepts are named, and the source docs
explain the decisions a maintainer would otherwise need to rediscover.

## Global Completion Standard

- [x] The reading path is obvious and documented in
  `docs/search-reading-order.md`.
- [x] Each module owns a real chess-engine concept or a clearly named
  engine-specific coupling.
- [x] Each major function fits in a maintainer's head without requiring the
  reader to hold unrelated phases at once.
- [x] The code distinguishes algorithm phases from scalar plumbing.
- [x] Parameter lists expose concepts, not long bags of unrelated scalars.
- [x] Hot-path extraction boundaries are justified by concept clarity and
  checked for behavior and speed.
- [x] Deterministic `bench` node counts remain equal after behavior-preserving
  refactors.
- [x] Speed tests show no clear isolated regression after accounting for known
  machine noise.
- [x] Source docs explain intent, invariants, ordering constraints, and safe
  change surfaces.
- [x] Source docs do not restate ordinary Rust control flow.

## Algorithm-Shape Checklist

- [x] `full::search()` reads like the full-width node pseudo-code:

  ```text
  enter node
  try TT proof
  try tablebase proof
  compute eval and stack contracts
  try pre-move pruning
  verify singular extension
  search ordered moves
  finalize node result
  ```

- [x] `full::search()` delegates phase mechanics to concept types or helpers
  instead of manually wiring every scalar.
- [x] `root::start()` reads like root iterative deepening:

  ```text
  initialize root state
  for depth
      prepare depth
      for MultiPV slot
          select tablebase-rank group
          search aspiration window retries
          sort/report root moves
      report completed depth
      update time-management feedback
  ```

- [x] `qsearch::qsearch()` reads like quiescence search:

  ```text
  enter qsearch node
  try shallow TT proof
  compute stand-pat eval
  search tactical moves
  write shallow TT result
  ```

- [x] `moves::search()` reads like ordered child search:

  ```text
  initialize move-loop state
  for ordered candidate
      derive move facts
      apply move pruning
      make move
      choose child search plan
      search child
      undo move
      update root/PV/alpha-beta state
  return move-loop result
  ```

## Required Concept Types

- [x] `FullNode` or equivalent captures ply, node kind, cut-node shape,
  side-to-move, check state, exclusion state, and root/PV facts.
- [x] `SearchWindow` or equivalent names alpha, beta, mate-distance adjustment,
  and window-shape invariants.
- [x] `ProofState` or equivalent groups TT and tablebase proof state used before
  eval.
- [x] `EvalState` has documented fields for raw eval, corrected eval,
  TT-adjusted estimate, correction value, improvement, and improving state.
- [x] `PreMovePruningContext` or equivalent groups the inputs to razor, reverse
  futility, null move, and ProbCut.
- [x] `SingularInput` or equivalent names the eligibility and TT-move
  verification inputs.
- [x] `MoveLoopInput` or equivalent groups full-width move-loop inputs.
- [x] `MoveLoopState` or equivalent owns best move, best score, bound, move
  count, alpha raises, searched move buffers, and TT-move score.
- [x] `MoveCandidate` or equivalent names per-move facts such as move, quietness,
  history, direct-check status, and root eligibility.
- [x] `MovePruningDecision` or equivalent explains whether the move is searched,
  skipped, stops quiets, or stops bad-noisy search.
- [x] Child-search branch structure names reduced scout, full-depth scout, PV
  re-search, extension, and new-depth choices without adding wrapper types that
  are immediately destructured.
- [x] `ChildSearchResult` or equivalent separates score and child-search count
  from move-loop state.
- [x] `NodeFinalizationInput` or equivalent groups finalization inputs instead
  of passing a long scalar list.

## Documentation Checklist

- [x] Every module has Rustdoc explaining what concept it owns, what it does not
  own, and why it exists.
- [x] Every struct has Rustdoc explaining the concept and ownership boundary.
- [x] Every field in dense search structs has Rustdoc explaining its role,
  consumer, and why it is distinct from similar fields.
- [x] Every enum has Rustdoc explaining what decision it represents.
- [x] Every enum variant has Rustdoc explaining when it is produced and what the
  caller must do with it.
- [x] Every phase helper has Rustdoc explaining ordering constraints and side
  effects.
- [x] Tuned formulas document heuristic purpose and branch-order constraints,
  not every numeric constant.
- [x] State mutation helpers document exact invariants, especially stack writes,
  NNUE push/pop, node counting, TT prefetch, TT writes, and stop handling.
- [x] Abstractions that intentionally stay flat or scalar-heavy document why a
  richer abstraction would be harmful.
- [x] Docs make the safe experimentation surface clear: what can be tuned
  locally, what requires paired validation, and what changes node counts.

## File-Specific Checklist

### `search/mod.rs`

- [x] `NodeType` docs explain why node kind is monomorphized.
- [x] `Root`, `PV`, and `NonPV` docs explain which branches each marker enables.
- [x] The file remains a small module index and shared node-kind surface.

### `search/full.rs`

- [x] `search()` is reduced to the algorithm spine rather than scalar plumbing.
- [x] Node entry, proof, eval, pruning, singular, move-loop, and finalization
  phase contracts are explicit.
- [x] `NodeEntry` and every variant are documented.
- [x] `NodeContext` or its replacement has documented fields.
- [x] `make_move` and `undo_move` live in `search/transition.rs`, the concept
  that owns search-side board/NNUE/stack transition invariants.

### `search/root.rs`

- [x] `Report` and its variants are documented.
- [x] `start()` is split or structured so iterative deepening, MultiPV,
  aspiration retries, reporting, and time feedback are visible concepts.
- [x] Tablebase rank-group handling documents why root move sorting is
  restricted.
- [x] Aspiration retry reporting documents why reporting happens inside retries.

### `search/moves.rs`

- [x] `MoveLoopOutcome` and every variant are documented.
- [x] `MoveLoopResult` and every field are documented.
- [x] The move loop has named concepts for candidate facts, pruning decisions,
  child search planning, child result handling, and root move updates.
- [x] The loop still preserves the make/search/undo/stop-check ordering.
- [x] Root-specific behavior is either isolated or clearly marked as root-only.

### `search/pruning.rs`

- [x] `try_null_move` documents null-move make/undo, verification search,
  `nmp_min_ply`, and stop handling.
- [x] `try_probcut` documents raised beta, qsearch pretest, reduced full-width
  confirmation, TT write, and fail-soft return.
- [x] Pre-move pruning and move-loop pruning are separated enough that their
  different ordering constraints are obvious.

### `search/eval.rs`

- [x] `EvalState` fields are documented individually.
- [x] Correction-history lookup docs explain why magnitude matters to pruning
  and reductions.
- [x] Stack-preparation docs explain every field written and which later phase
  reads it.

### `search/tt.rs`

- [x] `TtProbe` fields are documented individually.
- [x] Full-width and qsearch cutoff predicates document why their bound/depth
  requirements differ.
- [x] TT write timing remains documented where writes happen.

### `search/tablebase.rs`

- [x] `ProbeResult` variants are documented individually.
- [x] Interior-node tablebase probing remains separate from root tablebase rank
  handling.

### `search/singular.rs`

- [x] `SingularOutcome` fields are documented individually.
- [x] The docs explain when the TT move is suppressed and why `score` is kept
  even when there is no cutoff.
- [x] The stack exclusion restore invariant is documented at the mutation site.

### `search/reductions.rs`

- [x] `ReductionContext` fields are documented individually.
- [x] Late-move reduction and full-depth scout reduction docs explain what each
  formula is trying to protect or exploit.
- [x] Helper-thread bias docs explain why it exists and why it is part of depth
  policy.

### `search/history.rs`

- [x] History update context is either represented as a value type or parameter
  docs explain the ordering and ownership.
- [x] Best-move rewards, alternative maluses, parent fail-low feedback, and
  continuation-history updates remain separate concepts.

### `search/finalize.rs`

- [x] `finish_full_width_node` takes a concept input rather than a long scalar
  list, or documents why the scalar boundary is retained.
- [x] No-move handling, history feedback, TT-PV propagation, score shaping, TT
  writeback, and correction-history learning remain visibly ordered.

### `search/qsearch.rs`

- [x] `qsearch()` has named entry, TT/stand-pat, tactical move-loop, and final
  write phases.
- [x] `QsearchEval` fields are documented individually.
- [x] Qsearch-specific pruning helpers have docs explaining how they differ from
  full-width pruning.
- [x] The module docs continue to prevent full-width assumptions from leaking
  into qsearch.

## Safe Experimentation Surface

- Tunable formulas can be changed inside their owning concept module, but they
  still require deterministic bench checks because many formulas affect node
  counts.
- Phase-order changes in `full.rs`, `root.rs`, `moves.rs`, and `qsearch.rs`
  are behavioral changes, not cosmetic refactors.
- `make_move`/`undo_move`, TT write timing, stack writes, NNUE push/pop, and
  stop checks are node-count and correctness-sensitive.
- Hot-path concept boundaries should be validated with direct parent/current
  speed comparisons before reverting for apparent speed loss.
- Small short-run NPS movement is treated as noise unless it repeats in direct
  comparisons under similar machine conditions.

## Validation Checklist

- [x] `markdownlint-cli2 docs/search-algorithm.md docs/source-reorg-audit.md
  docs/search-refactor-target.md docs/search-refactor-progress.md
  docs/search-state-audit.md docs/search-refactor-checklist.md`
- [x] `cargo fmt --check`
- [x] `cargo check`
- [x] `cargo check --no-default-features`
- [x] `cargo rustc --release -- -C target-cpu=native`
- [x] `target/release/reckless bench`
- [x] `target/release/reckless "speedtest 1 16 30"` after hot-path boundary
  changes.
- [x] `target/release/reckless "speedtest"` at the final stopping point.
- [x] If speed appears worse, compare direct parent/current before reverting.
- [x] If repeated direct parent/current tests show an isolated regression, try
  inline hints or adjust the abstraction boundary.

## Follow-Up Shape Checks

- [x] Audit wide concept-input call sites for noisy reconstruction of the
  caller's local frame. A helper such as `finish_full_width_node` should not
  require the full-width algorithm spine to spell out a broad
  `NodeFinalizationInput` literal if most fields are already present in nearby
  phase state. Prefer a smaller owned concept, a conversion from existing phase
  state, or a method on the owning state when that makes the call site read like
  the algorithm again. Keep explicit literals only when the field list is the
  clearest documentation of a real boundary.
- [x] Reconsider restricted visibility such as `pub(in crate::search)` and
  `pub(super)` where it adds precision without practical value. This engine is
  a single monolithic crate, so most search helpers can use simple `pub` or
  private visibility unless the tighter boundary communicates an actual
  invariant. Avoid visibility ceremony that makes signatures harder to scan
  without protecting a meaningful API surface.
- [x] Format dense Rustdoc for reading rather than vertical compression. When
  documented struct fields are adjacent, leave a blank line between the field
  and the next field's doc comment so each comment visually belongs to the item
  below it. Wrap Rustdoc prose around 100 columns unless a local file or repo
  standard requires a narrower width.
- [x] Audit long helper parameter lists for missing concepts. Prefer passing an
  existing phase state or a named input value when fields travel together as a
  search phase, but keep scalar parameters where they are the visible terms of a
  tuned predicate or the recursive alpha-beta contract.
- [x] Define core search vocabulary in docs/source docs where the term is not
  obvious. In particular, explain what "full-width" means in this engine, why
  it differs from qsearch, and which phases or modules are inside that
  boundary.

## Target-Alignment Gaps

These items compare the current source against `docs/search-refactor-target.md`
from an adversarial readability stance. Do them only when the change makes the
method shapes easier to read; do not add indirection merely to match the target
pseudocode.

- [x] Re-evaluate `FullSearchState` as a readability tradeoff. It removes broad
  input literals from `full::search()`, but it can also become a second local
  frame that hides data flow behind conversion methods. A good end state should
  let a reader see the phase order and the important phase data without jumping
  between `search()` and several `*_input()` builders. `FullSearchState` now
  starts after proof/eval setup and owns the later mutable search frame, while
  `NodeContext` owns the earlier node/window/depth context.
- [x] Decide whether `FullNode`, `SearchWindow`, and adjusted depth should form
  a clearer pre-eval node context. The target describes a `NodeContext` that
  carries stable node facts, depth, and window. The current split may be better
  for codegen and mutation visibility, but it leaves eval and stack-preparation
  inputs assembled from several nearby concepts. `NodeContext` now carries
  those early facts and builds eval/stack-preparation inputs.
- [x] Audit concept wrappers that may be performative rather than useful.
  `ChildSearchPlan` was removed because it was constructed and immediately
  destructured inside the same branch. Keep future wrapper types only if they
  make the child-search policy easier to understand or enable a clearer
  `SearchPlan` flow; otherwise prefer direct, well-named branches.
- [x] Revisit pre-move pruning visibility. The target says the full-width driver
  should still show razoring, reverse futility, null move, and ProbCut order.
  `full::search()` now calls ordered methods on `PreMovePruningContext`, while
  `pruning.rs` still owns the formula details and side-effectful proof
  searches.
- [x] Revisit `SingularOutcome` shape. The target proposes explicit variants
  such as extend, multi-cut, suppress TT move, and negative extension. The
  current struct is compact, but readers must know how `extension`, `score`,
  `tt_move`, and `cutoff` combine. Use an enum only if it makes the singular
  phase easier to reason about without making the hot path branchier or more
  verbose. The struct was kept because the caller consumes those effects
  independently; source docs now state that tradeoff and the cutoff accessor
  names the immediate-return path.
- [x] Do an adversarial pass over the recent input structs. `EvalInput` and
  `StackPreparationInput` still reduce a real full-width phase handoff.
  `QsearchEvalInput` was removed because it only wrapped four obvious local
  facts and made `qsearch.rs` harder to enter.
- [x] Re-check whether `NodeType` still infects helpers beyond where it buys
  codegen. The target treats monomorphized node kind as an abstraction to
  question. Any replacement or narrowing should be prototyped carefully and
  benchmarked directly, but helper signatures should not stay generic just from
  inertia. Current uses remain concentrated in root/PV branching, recursive
  calls, move picking, qsearch, and finalization, so no obvious safe narrowing
  was taken in this pass.
- [x] Re-check whether `ThreadData` access has become more honest or merely
  more fragmented. The target asks for smaller state views where they make
  ownership clearer. Avoid creating a second all-access bucket, but look for
  phases where a local stack, root, history, or shared-state view would reduce
  how much of `ThreadData` a reader must mentally load. Current `ThreadData`
  use remains broad but phase-local; a smaller-view split would be a separate
  design change rather than a clear cleanup in this pass.
- [x] Revisit root concepts against the target root shape. `RootProgress`,
  `RootRetryState`, and `search_multipv_slot()` are improvements, but root move
  identity, search result, display state, tablebase rank group, and time
  feedback still mostly live as `ThreadData`/`RootMove` field traffic. Extract
  only the parts that make root iteration easier to read without hiding UCI and
  tablebase ordering. The remaining mixed physical record is documented on
  `RootMove` as separate legal-input, search-result, display, tablebase, and
  accounting concepts.
- [x] Reassess the move-loop extraction from a fragmentation perspective. The
  full-width driver is much shorter, but understanding one searched move now
  requires reading `MoveLoopInput`, `MoveLoopState`, `MoveCandidate`,
  `MovePruningDecision`, `ReductionContext`, and `ChildSearchResult`.
  Consolidate or inline concepts that do not reduce the reader's live fact
  count. `ChildSearchPlan` was removed as performative; the remaining local
  types each answer a distinct move-loop question and are documented in
  `moves.rs` reading order.

## Source Organization Checklist

These items adapt epage's Rust style guidance to the search refactor. The
organizing principle is reader locality: strong concepts can stand alone, while
weak helper abstractions should stay close to the code that gives them meaning.

- [x] Make each search file's central item appear first after imports and
  module docs. For `full.rs` that should be `search()` or a small titular type
  that immediately points to `search()`. For `moves.rs` that should be the move
  loop entry or its primary state. Supporting input structs should not force a
  reader through a long prelude before reaching the algorithm.
- [x] Arrange helpers in caller-before-callee order unless the callee is a
  strong standalone concept. Weak helpers such as conversion builders,
  one-branch predicates, and immediate destructuring wrappers should sit close
  to the caller or disappear.
- [x] Keep type definitions next to their inherent methods when the methods are
  the abstraction. Avoid separating a struct from the `impl` that explains how
  it is meant to be used.
- [x] Group public items before private implementation details where that helps
  the file act as a table of contents. In this single-crate engine, `pub` does
  not mean external API, so prioritize the reader's entry point over mechanical
  visibility grouping when the two conflict.
- [x] Use blank lines inside functions as algorithm paragraphs. Each paragraph
  should correspond to a search phase, state publication, candidate step, or
  finalization action. Avoid both dense walls of statements and gratuitous
  blank lines that split one logical action.
- [x] Open state-building blocks with the state being built when possible. For
  example, a move-loop or finalization phase should make its output state
  obvious before listing supporting facts.
- [x] Avoid mixing pure expression construction with visible mutation in the
  same visual block. Search has many behavioral side effects, so side-effectful
  code should usually be explicit statements or `for` loops, not hidden inside
  combinators or helper literals.
- [x] Re-check import style for search modules. Prefer imports that keep call
  sites obvious and avoid merge-conflict-prone compound lists when that does
  not make the top of the file too noisy.
- [x] Ensure `search/mod.rs` remains a true table of contents. It should not
  grow production logic beyond module wiring, re-exports, and the shared
  node-kind surface unless the target module shape changes deliberately.
