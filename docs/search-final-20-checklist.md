# Search Final 20% Checklist

This tracks the remaining work after the main search split. The goal is not
more files. The goal is to reduce the live concepts a maintainer must hold
while preserving the tuned search shape and measured behavior.

## Completion Standard

- The root, full-width, move-loop, and qsearch algorithms read in the same
  order they execute.
- Concept types reduce live state rather than just renaming argument lists.
- Physical structs that mix concepts expose methods for the conceptual
  operations callers need.
- Hot-path ordering, stack writes, NNUE push/pop, TT writes, and node counts
  remain visible.
- Deterministic bench node counts stay equal after behavior-preserving work.
- Short speed samples remain in the expected noisy range unless a direct
  comparison proves a sustained regression.

## Items

- [x] Give `RootMove` a conceptual API.
  - Move root display/result/accounting mutation out of `moves.rs`.
  - Prefer methods such as `record_search_result`, `mark_unsearched_for_sorting`,
    `same_tablebase_group`, and `start_depth`.
  - Keep the physical struct in `thread.rs`; it is shared by root setup,
    tablebase setup, UCI, and sorting.
  - Done when root/move-loop callers no longer set `RootMove`
    score/display/bound/PV/accounting fields directly except in setup/default
    code.
  - Implemented with `RootMove::start_depth`, `same_tablebase_group`,
    `record_search_result`, `mark_unsearched_for_sorting`, and
    `RootSearchResult`.

- [x] Untangle move-loop child-result handling.
  - Look for the post-child block in `moves::search`.
  - Prefer named state transitions over inline field mutation when the name
    reduces live state:
    `record_tt_move_score`, `accept_child_result`, `record_beta_cutoff`, `raise_alpha`.
  - Keep make/search/undo and stop handling visibly ordered.
  - Done when the loop body reads as candidate pruning, make/search/undo,
    stop/root update, alpha-beta update, history-buffer update.
  - Implemented as `MoveLoopState::accept_child_result`, `raise_alpha`, and
    `record_beta_cutoff`.

- [x] Name TT write roles.
  - Keep writes at their behavioral timing points; do not centralize into an
    opaque manager.
  - Name or document the roles:
    raw eval cache, full-width cutoff, tablebase proof, ProbCut lower bound,
    early alpha raise, final node result, qsearch shallow result.
  - Done when a reader can identify why each TT write exists without searching
    the whole codebase.
  - Implemented with local role helpers for raw eval cache, tablebase proof,
    ProbCut lower bound, alpha-raise lower bound, final full-width result,
    qsearch stand-pat lower bound, and final qsearch bound. Full-width cutoff
    remains named by `tt::try_full_width_cutoff`.

- [x] Experiment with a search transition boundary.
  - Candidate concept: `search/transition.rs`.
  - It would own stack move metadata, continuation-history pointers, node
    increment, NNUE push/pop, board make/undo, and TT prefetch.
  - This is hot. Keep it separate and benchmark directly. If speed regresses,
    keep `make_move` / `undo_move` in `full.rs` and document the failed
    boundary.
  - Implemented as `search/transition.rs`. The boundary owns only
    make/search/undo transition mechanics and keeps node-count, stack, NNUE,
    board, and TT-prefetch ordering documented in one place.

- [x] Investigate small `ThreadData` phase views.
  - Do not create one replacement context bucket.
  - Candidate views: root state, shared search state, history access,
    board/NNUE/stack transition.
  - Start with narrow read-only or method-only views where repeated field
    traffic is highest.
  - Done when a view reduces reader burden without hiding cross-heuristic experimentation.
  - Implemented two narrow views: `search/transition.rs` for board/NNUE/stack
    transition mechanics and `ThreadData::{is_stopped, stop_search}` for the
    shared stop-state contract. Broader views were not added because they would
    mostly become new all-access context buckets.

- [x] Consolidate durable docs.
  - Keep broad algorithm docs and final architecture docs.
  - Move planning/audit material that is no longer useful out of the permanent
    reading path.
  - Done when a maintainer has one short reading-order document plus focused
    reference notes.
  - Implemented as `docs/search-reading-order.md`, with
    `docs/search-algorithm.md` updated to match the final module map. Working
    notes remain available but are explicitly outside the normal reading path.

## Current Priority

The first final-20 pass exposed several methods that still read as extracted
code rather than named search concepts. Work through these before treating the
source split as complete:

1. [x] Reduce move-loop pruning-result noise.
   The pruning decision block in `src/search/moves.rs` repeats fail-soft
   `best_score` updates. Move that policy onto `MoveLoopState` so the loop body
   reads as pruning action, not score bookkeeping.
   Implemented as `MoveLoopState::record_pruned_fail_soft_score`.

2. [x] Split child-search sequencing into named steps.
   `search_child` still mixes initial depth, reduced scout search, retry depth
   adjustment, full-depth scout search, and PV re-search. Keep the make/undo
   boundary in the caller, but make the already-made child policy readable as
   a short sequence.
   Implemented as `ChildSearchState` plus `initial_child_depth`.

3. [x] Re-audit wide adapter methods.
   `FullSearchState::move_loop_input` and `finalization_input` may still be
   field-traffic adapters. Keep them only if the call site reads better than
   passing a coherent concept directly.
   Left in place. They are wide, but they are the only adapters that translate
   the full node frame into phase contracts with borrowed history buffers. A
   wrapper around them would mostly hide the same field traffic one level down.

4. [x] Re-audit pre-move pruning proofs.
   `try_null_move` and `try_probcut` contain real chess-programming concepts,
   but their local proof setup may still hide the algorithm behind plumbing.
   `try_probcut` now uses `ProbCutProof` for the qsearch pretest, full-width
   confirmation, TT lower-bound write, and return-score shaping. `try_null_move`
   was left in place because its make/undo/verify sequence already reads in
   algorithm order and a new wrapper would mostly hide the board transition.

5. [x] Re-audit qsearch and root reporting tails.
   `qsearch`, `search_multipv_slot`, `print_uci_info`, and
   `RootMove::record_search_result` are smaller than the original search body,
   but they still need the same "one readable concept per method" check.
   `qsearch` now uses `QsearchState` for tactical-loop alpha/beta state,
   searched count, PV update, cutoff history, score shaping, and bound choice.
   Root UCI reporting now uses `RootMove::uci_report` and `RootMoveReport` for
   report eligibility, tablebase score replacement, score formatting, and bound
   flags.

6. [x] Validate the hot-path cleanup.
   Run formatting, check builds, deterministic bench, and a short speedtest
   after the move-loop changes. Defer the default speedtest until the full
   refactor stopping point.
   Validation: `cargo fmt --check`, `cargo check`,
   `cargo check --no-default-features`, release build, bench `3425249` nodes,
   and `speedtest 1 16 30` at `32745214` nodes / `1251250` nps. Markdownlint
   still reports 80-column line-length errors in
   `.notes/software-change-preferences.md`; that file intentionally records the
   100-column prose preference for this refactor work.

## Preference Recheck

The `.notes/software-change-preferences.md` review exposed a second pass of
remaining cognitive-load issues:

1. [x] Split the wide move-loop context into coherent phase concepts.
   `MoveLoopContext` should not be one 18-field scalar bag. Group node/window,
   eval, TT proof, and child-depth policy so call sites show which search
   concept they are using.
   Implemented with `MoveLoopNode`, `MoveLoopEval`, `MoveLoopProof`, and
   `MoveLoopDepthPolicy`.

2. [x] Rework child-search helper traffic after the context split.
   `search_child` and `ChildSearchState` should not repeatedly pass unrelated
   fragments if one already-made-child concept can own them.
   Implemented with `ChildSearchContext`, a per-candidate snapshot of the
   already-made child facts used by reduced scout, full-depth scout, and PV
   re-search.

3. [x] Re-evaluate full-search adapter literals after the move-loop split.
   `FullSearchState::move_loop_input` and `finalization_input` should either
   build smaller real concepts or remain with an explicit note explaining why
   the wide handoff is the least-bad shape.
   `move_loop_input` now assembles named move-loop sub-concepts through helper
   methods. `finalization_input` remains a wide final writeback contract because
   its fields are consumed together by history, TT-PV propagation, TT storage,
   and correction-history feedback.

4. [x] Move wide pruning predicates onto their owning context.
   Razoring and reverse futility pruning no longer pass broad scalar lists to
   standalone helpers. Their predicates now live on `PreMovePruningContext`,
   where the phase already owns the necessary inputs.

5. [x] Revisit qsearch entry/finalization shape.
   `qsearch` is phase-aligned but still large. Split only if a local concept
   reduces live state without hiding the qsearch ordering.
   Implemented `QsearchEntry` and `QsearchNode` for terminal guards, PV
   bookkeeping, draw/max-ply handling, time polling, and shallow TT proof. The
   remaining `qsearch` body starts at stand-pat eval and then reads as tactical
   move loop plus writeback.

6. [x] Revisit root slot/result bookkeeping.
   `search_multipv_slot` and `RootMove` are acceptable but still flat. Improve
   only where a method owns a real root-search concept.
   Rechecked and left unchanged in this pass. The loop reads in root-search
   order, and further extraction would mainly introduce retry/update helpers
   with broad parameter lists rather than a stronger concept.

7. [x] Validate behavior and hot-path performance.
   Run formatting, checks, deterministic bench, and a short speedtest after
   the hot move-loop changes.
   Validation: `cargo fmt --check`, `cargo check`,
   `cargo check --no-default-features`, `markdownlint-cli2 "docs/**/*.md"`,
   release build, bench `3425249` nodes, and `speedtest 1 16 30` at
   `32216830` nodes / `1231766` nps.

## Preference Recheck 2

The latest `.notes/software-change-preferences.md` pass shows the algorithm
shape is much closer, but a few context and formula boundaries still carry too
many live concepts:

1. [x] Split `PreMovePruningContext`.
   It still mixes node/window facts, eval signals, TT proof/writeback data, and
   singular/null-move guards. Mirror the move-loop split so pruning methods
   name the concept they are consulting.
   Implemented with `PreMoveNode`, `PreMoveEval`, `PreMoveProof`, and
   `PreMoveGuard`.

2. [x] Consider a move-loop candidate pruning context.
   The calls to `late_move_prunes`, `futility_prune_score`, and
   `bad_noisy_futility_score` still pass many scalars. Extract only if a
   candidate-pruning concept reduces the call-site burden without hiding the
   formula order.
   Implemented `CandidatePruningContext` inside `moves.rs`. The move-loop
   branch order remains visible while formula argument lists are local to one
   short-lived candidate-pruning value.

3. [x] Recheck `ChildSearchContext`.
   It is currently acceptable as an already-made child snapshot. Change it only
   if the next scan shows it acts like a second broad context rather than one
   candidate search attempt.
   Left unchanged. It still represents one already-made child search attempt,
   not a general replacement for parent search state.

4. [x] Leave root search alone unless a real concept appears.
   The root slot loop reads in execution order. Further extraction currently
   looks likely to create weak retry/update helpers.
   Left unchanged for that reason.

5. [x] Recheck qsearch and finalization writeback contracts.
   `qsearch` reads well now, but final TT writeback and full-width
   finalization still use long contracts. Change only if a local result concept
   owns the data better than the current explicit handoff.
   Implemented `QsearchBound` for shallow qsearch TT writeback. Full-width
   finalization remains explicit because history, TT-PV propagation, TT
   storage, and correction-history feedback consume the result in one ordered
   post-loop phase.

6. [x] Validate the hot-path cleanup.
   Run formatting, checks, deterministic bench, and a short speedtest after
   pruning or move-loop changes.
   Validation: `cargo fmt --check`, `cargo check`,
   `cargo check --no-default-features`, `markdownlint-cli2 "docs/**/*.md"`,
   release build, and bench `3425249` nodes. The first `speedtest 1 16 30`
   sample was low at `24145662` nodes / `915232` nps; the required repeat for
   a drastic sample recovered to `30734078` nodes / `1174805` nps. Treat the
   first sample as noise unless later runs sustain the same drop.

## Preference Recheck 3

The next scan against `.notes/software-change-preferences.md` found a smaller
set of remaining issues. These should be fixed only where they lower field
traffic or make ownership clearer:

1. [x] Split full-width finalization input.
   `NodeFinalizationInput` is still a wide final packet. Group node/window
   facts, eval/TT writeback facts, and searched-move buffers so
   `finish_full_width_node` reads as post-loop feedback rather than a large
   destructuring block.
   Implemented with `FinalizationNode`, `FinalizationProof`, and
   `SearchedMoveLists`.

2. [x] Move pre-move eligibility predicates onto their phase context.
   `can_try_null_move` and `can_try_probcut` still read like scalar helper
   leftovers. Keep tuned formula terms visible, but make the owning context
   answer eligibility.
   Implemented as private `PreMovePruningContext` methods.

3. [x] Tighten qsearch node/eval/writeback handoffs.
   `QsearchEval::compute` and the stand-pat lower-bound write still pass
   scalars that already belong to `QsearchNode`. Add the missing node fact and
   move the TT write handoff onto that concept.
   Implemented by carrying node PV status in `QsearchNode`, passing the node to
   `QsearchEval::compute`, and moving stand-pat lower-bound storage onto
   `QsearchNode`.

4. [x] Revalidate after the cleanup.
   Run markdownlint, formatting, checks, deterministic bench, and a short
   speedtest. Only repeat the speedtest if the first sample is drastically
   outside the noisy range.
   Validation: `markdownlint-cli2 "docs/**/*.md"`, `cargo fmt --check`,
   `cargo check`, `cargo check --no-default-features`, release build, bench
   `3425249` nodes, and `speedtest 1 16 30` at `33793790` nodes /
   `1292503` nps.

## Preference Recheck 4

The next pass found two places where the previous cleanup still carried the
same preference risks in smaller form:

1. [x] Remove immediate wrapper/write patterns.
   `QsearchBound::new(...).write(...)` constructs a value only to consume it at
   once. Move final qsearch TT writeback onto the node or state that owns the
   facts instead of keeping a short-lived wrapper.
   Implemented as `QsearchNode::write_final_bound`; removed `QsearchBound`.

2. [x] Move finalization scalar predicates onto finalization concepts.
   `propagate_tt_pv`, `scale_beta_cutoff_score`,
   `should_write_final_result`, and `write_final_result` now take facts that
   belong to `FinalizationNode` or `FinalizationProof`. Keep the finalization
   order visible, but make the concept own the field traffic.
   Implemented as methods on `FinalizationNode` and `FinalizationProof`.

3. [x] Revalidate after the fourth pass.
   Run markdownlint, formatting, checks, deterministic bench, and one short
   speedtest if code changed.
   Validation: `markdownlint-cli2 "docs/**/*.md"`, `cargo fmt --check`,
   `cargo check`, `cargo check --no-default-features`, release build, bench
   `3425249` nodes, and `speedtest 1 16 30` at `32642814` nodes /
   `1247528` nps.

## Preference Recheck 5

The next scan found only small remaining field-traffic issues. These are worth
fixing because they make hot call sites read in terms of existing concepts
instead of concept fields:

1. [x] Let qsearch state methods accept `QsearchNode`.
   `qsearch()` still passes `node.in_check`, `node.tt_probe`, `node.ply`, and
   `node.stm` into state methods. Use the existing node concept at those
   boundaries.
   Implemented for quiet skipping, child acceptance, and cutoff-history update.

2. [x] Let move-loop setup accept `MoveLoopContext`.
   `moves::search()` still initializes loop state and candidates from
   `context.node.alpha`, `context.best_score`, `context.node.ply`, and
   `context.node.stm`. Keep the algorithm order visible, but pass the concept
   where the concept owns the fields.
   Implemented for `MoveLoopState::new`, `MoveLoopState::begin_candidate`, and
   `MoveCandidate::new`.

3. [x] Revalidate after the fifth pass.
   Run markdownlint, formatting, checks, deterministic bench, and one short
   speedtest if code changed.
   Validation: `markdownlint-cli2 "docs/**/*.md"`, `cargo fmt --check`,
   `cargo check`, `cargo check --no-default-features`, release build, bench
   `3425249` nodes, and `speedtest 1 16 30` at `31397630` nodes /
   `1199711` nps.
