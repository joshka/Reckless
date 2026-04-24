# Search Refactor Target

This document describes the intended end state for the search refactor. It is
not a checklist of mechanical file moves. The goal is a search implementation
whose structure explains the engine algorithm while preserving the exact
ordering, node-count behavior, and hot-path shape that make the engine strong.

## Goal

The final search code should let a maintainer answer these questions without
loading the entire recursive search function into memory:

- What phase of the algorithm am I reading?
- Which earlier phase initialized the state this phase depends on?
- Which side effects are part of search behavior rather than incidental code?
- Which formulas are tuned heuristics whose order should not be casually moved?
- Which abstractions are performance contracts, and which are only convenience
  buckets?

The desired outcome is not “more modules.” A module or type is useful only when
it names a real chess-search concept and reduces the number of live facts a
reader has to track.

## Design Rules

Keep the phase order visible. The full-width driver should read top-to-bottom
like the algorithm: guards, TT/tablebase, eval, pre-move pruning, singular
extension, move loop, finalization. Avoid generic pipeline abstractions that
hide this order.

Prefer value objects for phase results. A good extraction packages values that
already belong together, such as a TT probe result or static eval state. Avoid
large manager objects that span the whole node and increase register pressure.

Document contracts near boundaries. Broad explanation belongs in
`docs/search-algorithm.md`; local module docs should say what the module owns,
what it does not own, which invariants it relies on, and which side effects are
behavioral.

Question existing abstractions before building on them. Some current types are
performance-shaped or convenience-shaped rather than concept-shaped. Treat them
as constraints to understand, not necessarily as boundaries to preserve.

Use established chess-programming concepts as anchors when they fit. A reader
who knows alpha-beta, transposition-table cutoffs, aspiration windows, null-move
pruning, ProbCut, singular extensions, LMR, PVS, qsearch, and history heuristics
should recognize those shapes in the code. When a concept has a well-known
external explanation, prefer names and docs that make the connection obvious.

Do not force novel couplings into textbook boxes. This engine intentionally
mixes signals across heuristics: TT information affects eval, eval correction
affects pruning and reductions, singular search feeds reduction margins, and
history feedback appears in several phases. If a cross-signal is an experiment
or a tuned interaction, keep it visible instead of hiding it behind a standard
concept wrapper that makes the code less honest.

## Abstractions To Question

`NodeType` is useful because const generics let the compiler remove root, PV,
and non-PV branches. It becomes harmful if every helper is generic over
`NODE` just to avoid thinking about which node-kind behavior the helper really
needs. Treat `NodeType` as an early simplification target, not a permanent
interface. The refactor should look for ways to keep the hot root/PV/non-PV
branch shape cheap while avoiding a generic parameter that infects every helper.
Possible directions include explicit `NodeKind` values, separate root entry
points, or narrower booleans on helpers that only need one distinction. Any
replacement must be validated with direct speed tests because this abstraction
may currently be carrying real codegen value.

`ThreadData` is a convenience bucket. It contains board state, NNUE state,
stack, root search state, local histories, time management, and shared context.
New abstractions should not treat `ThreadData` itself as the concept. They
should name the smaller search concept they read or update. This is also an
early simplification target: avoid creating new APIs that take `&mut ThreadData`
by default. Prefer to discover smaller views such as board/NNUE state, stack
state, local histories, root state, and shared search state. Even if those views
start as documentation or local bindings, they should make ownership boundaries
clearer than the current all-access bucket.

`StackEntry` is a performance contract. It connects parent, current, and child
plies without allocation. Its fields are phase contracts: eval, move count,
reduction, TT move, TT-PV state, exclusion, and continuation-history pointers.
Refactors should document those contracts before moving code that depends on
them.

`RootMove` currently mixes legal root move input, tablebase rank, search score,
display score, bound flags, PV, selected depth, and node accounting. Root search
can probably become clearer by separating root move identity, search result, and
UCI display state conceptually even if they remain in one struct physically.

`MovePicker` is a useful ordering abstraction, but search still depends on its
stages, especially `BadNoisy`. Treat those stage checks as part of the pruning
contract, not as private implementation details of move picking.

`Bound` is TT vocabulary, but search uses bounds in several roles: pruning
proof, qsearch stand-pat proof, eval adjustment, final node result, and root
display state. Helpers should name which role they mean.

`ThreadPool::execute_searches` is a lifecycle boundary. It owns root move
generation, root tablebase ranking, per-worker setup, shared counter reset, and
worker launch. Root-search refactors should decide which responsibilities stay
there and which belong in `search/root.rs`.

## Concept Sources

Before inventing a new abstraction name, check whether the code is implementing
or modifying a standard chess-programming idea. Candidate anchors include:

- alpha-beta search and fail-soft/fail-hard window behavior;
- principal variation search;
- iterative deepening;
- aspiration windows;
- transposition table probes, bounds, replacement, and mate-score adjustment;
- quiescence search and stand-pat evaluation;
- static exchange evaluation pruning;
- null-move pruning and verification search;
- reverse futility pruning and razoring;
- ProbCut;
- singular extensions and multi-cut behavior;
- late-move pruning and late-move reductions;
- move ordering by TT move, captures, killers or equivalents, and history;
- quiet, noisy, continuation, and correction histories;
- tablebase probing and root tablebase ranking;
- repetition, draw, and mate-distance pruning.

Use those concepts when they describe the code closely enough. For example,
`SingularOutcome` is useful because the code really does perform a singular
extension verification search. `TtProbe` is useful because the code really does
interpret TT entries as pruning proofs, eval bounds, PV markers, and ordering
hints.

When the code combines concepts in a way that is not standard, name the
combination directly or keep the expression local. For example, a reduction
formula that uses correction-history magnitude, TT depth, alpha-raise count,
helper-thread bias, and singular-score margin is not just “LMR” anymore. It is
the engine's child-search depth policy, and the code should make the inputs
visible enough for experimentation.

The flat search function has one real advantage: it makes it easy to try
cross-heuristic experiments. The refactor should preserve that ability by
keeping phase data flow explicit. A maintainer should still be able to ask,
“can the value computed for one heuristic shortcut another heuristic?” without
fighting opaque trait objects or overly narrow textbook abstractions.

Useful local docs can cite the external concept by name without turning source
comments into a wiki. Broad explanations and references belong in
`docs/search-algorithm.md` or focused design notes; source docs should state
which variant this engine uses and what ordering or state invariant matters.

## Target Full-Width Shape

The full-width search driver should coordinate named concepts without hiding
the order:

```rust
fn full_search<Node>(td, window, depth, ply) -> Score {
    let node = NodeContext::new::<Node>(td, window, depth, ply);

    if let Some(score) = terminal_guards(td, node) {
        return score;
    }

    if node.depth <= 0 {
        return qsearch::<Node>(td, window, ply);
    }

    let tt = TtProbe::read(td, node);
    if let Some(score) = tt.full_width_cutoff(td, node, window) {
        HistoryUpdate::tt_cutoff(td, node, tt);
        return score;
    }

    let tb = TablebaseProbe::try_probe(td, node, tt);
    if let Some(score) = tb.cutoff_score() {
        tt.write_tablebase_cutoff(td, node, score);
        return score;
    }

    let eval = EvalState::compute(td, node, tt);
    StackState::initialize(td, node, tt, eval);

    HistoryUpdate::parent_eval_feedback(td, node, eval);
    DepthAdjustment::apply_hindsight(td, node, eval, tt);

    let pruning = PreMovePruning::new(node, tt, eval);
    if let Some(score) = pruning.razor(td) {
        return score;
    }
    if let Some(score) = pruning.reverse_futility(td) {
        return score;
    }
    if let Some(score) = pruning.null_move(td) {
        return score;
    }
    if let Some(score) = pruning.probcut(td) {
        return score;
    }

    let singular = SingularSearch::run_if_needed(td, node, tt, eval);
    if let Some(score) = singular.cutoff_score() {
        return score;
    }

    let mut moves = MoveLoop::new(td, node, tt, eval, singular);
    while let Some(candidate) = moves.next(td) {
        if !candidate.is_searchable_root_move(td, node) {
            continue;
        }

        if let Some(score) = candidate.pruned_score(td, node, eval) {
            moves.record_pruned(score);
            continue;
        }

        let child = candidate.make(td, node);
        let score = SearchPlan::for_move(td, node, candidate, moves.state())
            .search_child(td, child);
        child.undo(td);

        if td.stopped() {
            return Score::ZERO;
        }

        moves.record_result(td, candidate, score, window);
        if moves.beta_cutoff() {
            break;
        }
    }

    NodeFinalization::new(node, tt, eval, moves)
        .finish(td, window)
}
```

This pseudocode is intentionally not the first implementation target. It is the
shape that future extractions should converge toward.

## Useful Concept Objects

`NodeContext` should name stable per-node facts: node kind, ply, depth, window,
side to move, check state, exclusion state, and cut-node state. This may remain
mostly local if a struct hurts codegen. It is also the natural place to test
whether `NodeType` can become a plain node-kind value rather than a monomorphized
generic that leaks into every helper.

`SearchState` or narrower state views should reduce dependence on `ThreadData`.
This does not need to be one large struct. In fact, one large replacement bucket
would repeat the same problem. More useful views are:

- board and NNUE state that must move together through make/undo;
- stack state for parent/current/child ply contracts;
- local history tables used for ordering and feedback;
- root-only iteration and reporting state;
- shared stop, node, TT, tablebase, and correction-history state.

The goal is not to hide access behind getters. The goal is to make each phase
state which part of the search state it actually owns or mutates.

`TtProbe` should replace loose TT locals:

```rust
struct TtProbe {
    entry: Option<Entry>,
    depth: i32,
    mv: Move,
    score: Score,
    bound: Bound,
    tt_pv: bool,
}
```

It should own search-facing TT interpretation: full-width cutoff eligibility,
qsearch cutoff eligibility, TT-adjusted eval eligibility, singularity
eligibility, and write-policy helpers. TT storage remains in
`src/transposition.rs`.

`EvalState` should replace loose eval locals:

```rust
struct EvalState {
    raw: Score,
    corrected: Score,
    estimated: Score,
    correction: i32,
    improvement: i32,
    improving: bool,
}
```

It should own NNUE reuse, correction-history adjustment, TT-bound adjustment,
excluded-node eval reuse, in-check behavior, and correction-history training.

`SingularOutcome` should make the singular-extension phase explicit:

```rust
enum SingularOutcome {
    None,
    Extend(i32),
    MultiCut(Score),
    SuppressTtMove,
    NegativeExtension(i32),
}
```

This is a good abstraction because singular search temporarily excludes the TT
move and has several distinct outcomes.

`PreMovePruning` should own pruning gates that can return before normal move
generation: razoring, reverse futility, null move, and ProbCut. The driver must
still show their order.

`MoveLoopState` may be useful, but it is risky. It would name best score, best
move, bound, searched quiet/noisy buffers, move count, alpha raises,
current-search count, TT-move score, and quiet-skipping state. Because this is
the hottest part of the search, implement it only if it does not create a
measurable sustained slowdown.

`SearchPlan` should name the child-search choice:

```rust
enum SearchPlan {
    ReducedScout { depth: i32, cut_node: bool },
    FullScout { depth: i32, cut_node: bool },
    PvSearch { depth: i32 },
}
```

This can clarify the relationship between LMR, full-depth scout search, and PV
re-search without changing the branch order.

`NodeFinalization` should collect post-loop behavior: no-legal-move handling,
history updates, prior-move bonuses, TT-PV propagation, beta cutoff scaling,
Syzygy score cap, TT writeback, and correction-history update. This is a
high-value target because the current tail of full-width search is dense and
easy to misread.

## Target Root Shape

Root search should also become concept-oriented, not just moved into
`root.rs`:

```rust
fn start(td, report, thread_count) {
    RootSearch::prepare(td);

    for depth in IterativeDepths::new(td) {
        let mut iteration = RootIteration::new(td, depth);

        while let Some(slot) = iteration.next_multipv_slot(td) {
            let mut aspiration = AspirationWindow::new(td, slot);

            loop {
                aspiration.prepare_root_call(td);

                let score = full_search::<Root>(
                    td,
                    aspiration.window(),
                    aspiration.search_depth(depth),
                    RootPly,
                );

                td.sort_current_root_group();

                match aspiration.accept_or_expand(td, score) {
                    Accepted => break,
                    Retry => continue,
                    Stopped => break,
                }
            }
        }

        iteration.finish_depth(td, report);

        if RootTimeFeedback::new(td, iteration).should_stop(thread_count) {
            td.shared.status.set(Status::STOPPED);
            break;
        }
    }

    RootSearch::finish(td, report);
}
```

The root concepts are:

- `RootIteration`: one depth of iterative deepening.
- `MultiPvSlot`: current PV index and tablebase-rank group.
- `AspirationWindow`: current alpha, beta, delta, and fail-low/fail-high retry.
- `RootTimeFeedback`: PV stability, eval stability, node share, best-move
  changes, and soft-stop vote.
- `RootResult`: score, display score, bound flags, selected depth, PV, and
  root-move node count.

## Documentation Target

Each search module should start with concise module docs:

- what concept the module owns;
- what concept it deliberately does not own;
- which phase-order constraints matter;
- which stack fields or TT/history side effects are behavioral;
- which formulas are tuned and should not be moved casually.

Do not add comments that paraphrase Rust control flow. Add comments where a
future maintainer would otherwise have to reconstruct intent from surrounding
code.

## Refactor Strategy

Start with documentation and named local concepts, not file moves. The first
production changes should make `search::<NODE>` easier to read while keeping
the function in place.

1. Add module docs and phase-boundary comments to the current files.
1. Audit every use of `ThreadData` fields inside full-width search and group
   them by concept: board/NNUE, stack, histories, root state, shared state, and
   time state.
1. Audit every `NODE::ROOT` and `NODE::PV` branch and classify whether it is a
   root-only entry concern, a PV-window concern, or a codegen concern.
1. Prototype a non-generic `NodeContext` or `NodeKind` shape in pseudocode, then
   benchmark only if the code change remains simple and readable.
1. Introduce `TtProbe` locally and replace loose TT locals.
1. Introduce `EvalState` locally and replace loose eval locals.
1. Introduce `SingularOutcome` and isolate singular-extension decisions.
1. Name pre-move pruning gates while keeping the order visible in the driver.
1. Name post-loop finalization before moving it to another module.
1. Move mature concepts to modules only after they have proven useful in place.
1. Treat move-loop and make/undo abstractions as optional until speed evidence
   says they are safe.

Every step should preserve deterministic bench nodes. Hot-path extractions
should be compared against their direct parent with `speedtest 1 16 30`, and
differences under roughly 2 percent should generally be treated as noise unless
they repeat in a targeted comparison.
