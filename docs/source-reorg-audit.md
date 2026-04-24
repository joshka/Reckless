# Search Source Reorganization Audit

This audit uses `docs/search-algorithm.md` as the algorithm map and
`docs/search-refactor-target.md` as the target design. The goal is not a smaller
file count by itself. The goal is for each source module or helper type to own a
maintainer-visible chess-engine concept while preserving search behavior and
hot-path performance.

## Current Shape

`src/search.rs` currently owns root iterative deepening, root result reporting,
the recursive full-width search, qsearch, search-facing TT policy, static eval
setup, correction-history policy, pruning gates, singular extensions, move-loop
mechanics, history updates, and make/undo wrappers. That makes the file useful
as a behavior reference, but it forces a reader to keep root search, interior
node search, qsearch, and many update policies in one mental frame.

Several adjacent concepts already have separate storage modules:

- `src/transposition.rs` owns TT storage layout and score normalization.
- `src/history.rs` owns history table storage and update mechanics.
- `src/thread.rs` owns `ThreadData`, `SharedContext`, root moves, and PV tables.
- `src/threadpool.rs` owns worker setup, legal root move generation, and
  thread-level search execution.

The search refactor should keep storage modules stable unless an extraction
needs a small documentation or visibility adjustment. The first production
target should be named concepts inside the full-width driver, not a module tree
by itself. File moves are useful only after the concept boundary has proven that
it reduces cognitive load.

## Cognitive-Load Problems

The largest problem is that phase boundaries are implicit. A reader must infer
where TT policy ends, static eval policy begins, and which later pruning or
history phase depends on fields initialized earlier in the node.

Root and interior responsibilities are interleaved. The same `search` function
handles root filtering and reporting side effects while also carrying all
non-root pruning and history policy.

Qsearch is algorithmically distinct but sits after full-width search in the
same module and shares helpers with no explicit boundary. That makes it easy to
accidentally move full-width assumptions into qsearch or vice versa.

TT, eval, pruning, singular extension, move loop, and post-loop update code all
share many scalar locals. Some of that is unavoidable in a tuned alpha-beta
search, but the current shape makes every extraction look risky because the
dependencies are not named.

Another problem is that some existing abstractions are not necessarily good
boundaries. `ThreadData`, `NodeType`, `RootMove`, `StackEntry`, `Bound`,
`MovePicker`, and `ThreadPool::execute_searches` each combine multiple concerns
or encode performance constraints. A successful refactor should question these
boundaries rather than automatically building more layers on top of them.
`ThreadData` and the monomorphized `NodeType` parameter are especially important
to evaluate early because they shape nearly every proposed helper API.

## Proposed Module Boundaries

`search/root.rs` should move first. It owns iterative deepening, MultiPV rank
groups, aspiration windows, root optimism, UCI reporting decisions, soft-stop
feedback, and the root call into full-width search. This is a real abstraction
because root search has a different lifecycle and reporting contract from
interior nodes.

`search/full.rs` should eventually contain the recursive alpha-beta driver and
the `NodeType` markers. Before that move, the current full-width function should
grow named local concepts such as `TtProbe`, `EvalState`, `SingularOutcome`,
and `NodeFinalization`. Once those concepts are clear, `full.rs` should read
like the phase map: guards, TT/tablebase, eval, pre-move pruning, singular
extension, move loop, post-loop finalization. This module coordinates concepts;
it should not hide the phase order behind a generic pipeline.

`search/tt.rs` should contain search-facing TT policy: probe snapshots, cutoff
predicates, TT-adjusted eval predicates, early lower-bound write policy, final
write policy, and small bound helpers. TT storage remains in
`src/transposition.rs`. This is a real abstraction because search uses TT
entries as pruning proofs, move-ordering hints, eval bounds, and PV markers.
Predicate helpers alone are not enough; the useful abstraction is a search view
of a TT probe that replaces the loose `tt_depth`, `tt_move`, `tt_score`,
`tt_bound`, and `tt_pv` locals.

`search/eval.rs` should contain correction-history lookup, raw/corrected eval
setup, TT-adjusted estimated score, in-check eval fallback, and
correction-history training. This boundary is meaningful because eval in search
is a compound concept, not just an NNUE call. A helper that only moves
correction-history arithmetic is incomplete.

`search/pruning.rs` should contain named pruning gates and predicates:
razoring, reverse futility pruning, null-move pruning eligibility, ProbCut
coordination, late-move pruning, futility pruning, bad-noisy futility, and SEE
thresholds. The extraction should prefer small inline predicates and result
types over heap state or dynamic dispatch.

`search/singular.rs` should contain potential-singularity detection and the
singular-extension result. It is a separate concept because it temporarily
excludes the TT move, performs a verification search, and can produce an
extension, multi-cut, TT-move suppression, or negative extension.

`search/moves.rs` should contain make/undo wrappers, node counting, NNUE
push/pop, continuation pointer setup, TT prefetch, root move filtering, and
root result updates. This is a real abstraction around move-loop mechanics, not
move ordering itself.

`search/history.rs` should contain search-side history scoring and update
policy: quiet/noisy move scores, continuation-history updates, best-move
bonuses, maluses for searched alternatives, prior-move fail-low bonuses, and
qsearch cutoff bonuses. Storage remains in `src/history.rs`.

`search/qsearch.rs` should contain quiescence search. It has a separate
stand-pat contract, tactical move set, shallower TT policy, and narrower
history footprint, so it should be readable without loading the full-width
driver.

## Extraction Order

1. Add module docs and phase-boundary comments that explain ownership,
   invariants, and tuned ordering constraints.
1. Audit full-width `ThreadData` field usage and group it into smaller state
   concepts before introducing more helpers that take all of `ThreadData`.
1. Audit `NODE::ROOT` and `NODE::PV` usage to decide which branches are real
   algorithm distinctions and which only exist for current codegen shape.
1. Introduce a local `TtProbe` search view before moving TT policy to
   `search/tt.rs`.
1. Introduce a local `EvalState` before moving eval policy to `search/eval.rs`.
1. Introduce `SingularOutcome` before moving singular logic to
   `search/singular.rs`.
1. Name pre-move pruning gates while keeping their order visible.
1. Name post-loop finalization before moving history and TT writeback helpers.
1. Move root search and qsearch into modules only when their local docs explain
   their algorithmic contracts.
1. Treat move-loop and make/undo extraction as optional and performance-gated.

Each step should be one jj change with one conceptual purpose. If an extraction
needs visibility changes, keep them in the same change only when the extracted
concept cannot compile without them.

## Hot-Path Risks

The hottest boundary is `search::<NODE>`. Its `NODE` const-generic branches let
the compiler remove root/PV/non-PV paths. Extracted helpers that depend on node
type should either stay generic and inlineable or take explicit booleans only
when codegen stays equivalent.

Scalar local packing is risky. Large context structs can increase register
pressure in the move loop and pruning formulas. Prefer result structs for
natural phase outputs, such as TT probe state or singular-extension result, and
avoid long-lived mutable bundles that span the whole node.

Make/undo and node counting are behavior boundaries. Moving node increments,
NNUE push/pop, board mutation, TT prefetch, or stop checks can break
deterministic node counts even if the best move does not change.

TT write timing is part of search behavior. Eval-only writes, early lower-bound
writes, ProbCut writes, final writes, and qsearch writes feed later cutoffs and
move ordering. Preserve their order unless a dedicated experiment says
otherwise.

History updates are search feedback, not cosmetic cleanup. The update order and
the exact move buffers used for maluses should stay attached to the same search
events.

## Validation

For documentation-only and plan-only changes:

- Run `markdownlint-cli2` on touched markdown files.

For mechanical module moves:

- Run `cargo fmt --check`.
- Run `cargo check`.
- Run deterministic `bench` before and after the change and require node-count
  equality.

For hot-path helper extractions:

- Run `cargo fmt --check`.
- Run `cargo check`.
- Build with `cargo rustc --release -- -C target-cpu=native`.
- Run deterministic `bench` and require node-count equality.
- Compare direct parent/current with `speedtest 1 16 30`.
- Treat small NPS differences as noise unless repeated and isolated.

For boundary changes that touch pruning, reductions, TT write timing,
make/undo, or history updates:

- Require direct parent/current deterministic node-count equality before
  considering speed.
- Repeat `speedtest 1 16 30` if the first result suggests a regression.
- If a regression repeats, first try `#[inline]` or adjust the boundary. If it
  still repeats, keep the change separate and call out the risk.

## Success Criteria

The refactor is successful when `search/full.rs` reads like the algorithm map,
root search is understandable without scanning interior pruning, qsearch is
isolated behind its own contract, and each extracted module owns a named
chess-engine concept. It should also be clear which existing abstractions were
kept as performance contracts, which were treated as convenience buckets, and
which were replaced by better concepts. Behavior should be preserved by
deterministic node-count checks, and hot-path performance should show no clear
isolated regression.
