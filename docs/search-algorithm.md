# Search Algorithm Map

This document describes the search as an algorithm. It is intentionally broader
than the source layout so refactors can preserve the ordering, invariants, and
tuned behavior that make the engine strong.

## Search Lifecycle

The UCI `go` command enters `uci::go`, parses time controls into a
`TimeManager`, and calls `ThreadPool::execute_searches`. The thread pool
increments the transposition-table age, resets node and tablebase counters,
marks shared search status as running, builds legal root moves for the main
thread, optionally ranks them with Syzygy, then starts one search per worker.
Helper threads receive the same board and root move list and run without UCI
reporting.

Each worker calls `search::start`. Startup clears the PV table, refreshes NNUE
for the current root, clips MultiPV to the legal root move count, and
initializes per-iteration stability trackers. The root search is iterative
deepening from depth 1 to `MAX_PLY`. For each depth and each MultiPV index, the
driver chooses the current tablebase-rank group, builds an aspiration window
around the rolling average score, sets root optimism, resets the stack, and
calls the recursive full-width search as a root PV node.

The recursive full-width search handles all interior alpha-beta nodes. It first
runs terminal and cheap guard logic, optionally dives to qsearch when depth
reaches zero, probes TT and tablebases, sets up static evaluation, applies
pre-move pruning, decides singular extensions, searches ordered moves with
pruning and reductions, updates PV and alpha-beta state, then writes history,
correction history, and TT results.

Qsearch is the leaf stabilizer. It searches checks and tactical moves from
depth-zero positions, uses stand-pat evaluation when not in check, applies
narrower TT and SEE logic, and writes shallow TT entries. It avoids the
full-width machinery for depth reductions, singular extensions, large-scale
quiet history updates, and most pre-move pruning.

When a worker stops, the main `go` path combines thread results. For normal
timed single-PV search it votes across worker best moves using score and
completed depth. It prints a final `info` line if a helper result wins the vote,
then reports `bestmove`.

## Root Search Responsibilities

Root setup owns the legal root move list. `ThreadPool::execute_searches`
generates legal moves from the UCI position, stores them as `RootMove` values,
and copies that list to helpers. With Syzygy enabled, tablebase root ranking
happens before workers start so the root search can group and report moves by
tablebase rank.

`search::start` owns MultiPV iteration. At each depth it searches PV indexes in
order, preserving groups of equal tablebase rank with `pv_start` and `pv_end`.
Root move sorting is restricted to the active rank group or already searched PV
prefix so MultiPV does not mix tablebase priorities.

Aspiration windows are root-only control flow. The window starts around the
rolling average for the PV index, expands after fail-low or fail-high, and may
reduce the re-search depth after fail-high.  This feedback loop is sensitive
because it changes how often the recursive search is called and with which
alpha-beta shape.

Root reporting is tied to `RootMove`. During a root node, every searched root
move accumulates node count, score, bound flags, selected depth, display score,
and a committed root PV. The iterative driver decides when to print UCI `info`:
after completed depths, after expensive aspiration re-searches, and for minimal
reporting at the end.

Time management also feeds back at the root. The driver tracks evaluation
stability, PV stability, best-move changes, and the node share spent on the
current best move. Those signals produce the soft limit multiplier. A
helper-thread vote stops the shared search once enough workers agree that the
soft limit has been reached.

## Full-Width Node Phases

Full-width search is ordered to keep cheap, high-probability exits ahead of
expensive work and to preserve node-count behavior. The current phase order is:

1. Validate alpha-beta invariants, derive side-to-move, check state, and
   exclusion state.
1. Clear PV storage for non-root PV nodes.
1. Stop immediately if shared status is stopped.
1. Enter qsearch when depth is zero or below.
1. Apply upcoming repetition draw adjustment before deeper work.
1. Update selective depth for PV nodes and poll the time manager on the main
   thread.
1. Handle non-root terminal guards: draw detection, maximum ply, and
   mate-distance pruning.
1. Probe TT, seed TT move/score/bound/PV state, and take non-PV cutoffs when
   the entry is deep enough, compatible with the window, and not blocked by
   exclusion or halfmove-clock safety.
1. Probe tablebases for eligible non-root nodes and either cut off, seed a PV
   lower bound, or cap the PV score with a maximum score.
1. Compute correction-history value, raw static eval, corrected eval, and
   optionally store a shallow eval-only TT entry.
1. Tighten estimated score from TT bounds when the bound direction agrees with
   static eval. In-check nodes may use a compatible non-decisive TT score as
   eval.
1. Initialize stack state for this ply, including eval, TT move, TT PV flag,
   reductions, move count, and the grandchild cutoff counter.
1. Apply eval-difference quiet-history feedback and hindsight depth changes.
1. Detect potential singularity from depth, TT depth, TT bound, TT score, and
   decisive-score guards.
1. Compute improvement and improving state from prior stack evals.
1. Run pre-move pruning in tuned order: razoring, reverse futility pruning,
   null-move pruning with verification, and ProbCut.
1. Run singular-extension search for the TT move when eligible. The result may
   extend, multi-cut, suppress the TT move, or apply a negative extension.
1. Initialize move-loop state: best move, bound, quiet/noisy searched move
   buffers, move picker, quiet skipping, search-count tracking, alpha-raise
   count, and TT-move score.
1. Iterate ordered moves, skipping excluded moves and non-selected root moves.
1. Score each move with quiet or noisy history and run move pruning: late-move
   pruning, futility pruning, bad-noisy futility pruning, and SEE pruning.
1. Make the move, count the node, update stack continuation pointers, push NNUE,
   update the board, and prefetch the child TT entry.
1. Choose new depth and run late-move reductions or full-depth reduced scout
   search according to depth, move count, node type, TT state, history,
   improvement, prior
   reductions, helper bias, and singular-margin feedback.
1. Run PV re-search for PV nodes when the first move or a scout result raises
   alpha.
1. Undo the move and stop if shared status changed.
1. At root, update the matching `RootMove` with node count, bound display state,
   score, selected
   depth, PV, and best-move-change accounting.
1. Update best score, alpha, bound, PV table, early lower-bound TT write,
   alpha-raise count, and quiet/noisy searched-move buffers.
1. If there were no legal moves, return mate, draw, or excluded-move tablebase
   sentinel.
1. Apply post-loop history updates for the best move, failed quiet/noisy
   alternatives, continuation histories, prior-move fail-low bonuses, and TT-PV
   propagation.
1. Scale non-decisive beta cutoffs, cap Syzygy PV scores, write the final TT
   entry, update correction history when the static eval was meaningfully wrong,
   and return `best_score`.

The stack is a contract between phases. Earlier phases must fill `eval`,
`tt_move`, `tt_pv`, `move_count`, and reduction state before later pruning,
reduction, and history phases read them.  Move make/undo must maintain `mv`,
`piece`, continuation-history pointers, NNUE stack, board state, node counters,
and TT prefetch ordering.

## Qsearch

Qsearch keeps the alpha-beta contract but narrows the search to tactical
stabilization. It handles upcoming repetition, PV clearing and selected-depth
updates, time polling, draw and maximum-ply guards, and a TT probe. Its TT
cutoff is simpler than full-width search and does not use depth.

When not in check, qsearch evaluates the current position, applies correction
history, optionally tightens stand-pat with a compatible TT score, and uses
stand-pat as the initial best score. A stand-pat beta cutoff can write a shallow
lower-bound TT entry. In-check nodes do not stand pat; they must search evasions
and return mate if no legal move exists.

The qsearch move loop uses `MovePicker::new_qsearch`, usually skips quiets,
stops after late non-checking moves, and applies SEE pruning against the
stand-pat margin. It recursively calls qsearch rather than full-width search. On
beta cutoffs it updates a small quiet or noisy history bonus, scales
non-decisive cutoffs toward beta, and writes a shallow TT bound.

Qsearch deliberately does not own root reporting, iterative deepening, singular
extensions, null-move pruning, ProbCut, full LMR formulas, continuation-history
maluses, or correction-history training from full-width best-score differences.

## Cross-Cutting State

`ThreadData` is the per-worker search context. It owns the current board, NNUE
state, stack, root moves, PV table, local quiet/noisy/continuation histories,
time manager copy, root/MultiPV counters, optimism, selected depth, completed
depth, null-move verification guard, and previous best score.

`SharedContext` is the cross-worker state. It owns the TT, search status,
sharded node and tablebase counters, tablebase probing flags, soft-stop votes,
root best-score statistics, NUMA-replicated correction histories, and replicated
NNUE parameters.

Stack entries connect parent, current, and child phases without allocation. They
carry the move and piece just made, static eval, singular-exclusion move, TT
move and TT-PV flag, cutoff count, move count, reduction amount, and raw
pointers to continuation-history subtables. Negative indexing is intentional:
sentinel entries provide stable history pointers for early plies.

The TT is both a pruning source and a move-ordering/eval source. Search code
depends on bound type, stored depth, TT-PV marker, mate-distance normalization,
halfmove-clock safety for false TB/mate scores, and replacement behavior. TT
writes are part of the algorithm, not just caching; early lower-bound writes and
eval-only writes feed later move ordering and pruning.

History tables serve different concepts. Quiet and noisy histories order moves
and drive pruning thresholds. Continuation history connects the current move to
prior moves. Correction histories train static eval by pawn key, non-pawn keys,
and continuation context. NNUE provides raw eval and must be pushed and popped
in exact board-move order.

The time manager is mostly root and main-thread state. Interior nodes poll only
on the main thread, while root stability and helper votes decide the soft stop.
Shared status is checked at several phase boundaries so workers can return
quickly without corrupting board, NNUE, or stack state.

## Why Ordering Matters

Search strength and speed depend on phase order. Cheap guards, repetition
handling, qsearch entry, draw detection, mate-distance pruning, and TT cutoffs
run before expensive tablebase probes, evaluation, and move generation. Changing
that order can alter node counts even if returned moves are usually the same.

Node counting is an invariant of `make_move`: the engine increments nodes
exactly when it commits to searching a child and before making the board move.
Deterministic bench equality is therefore a good behavior check for refactors.
Moving counters, prefetches, or early exits across make/undo boundaries can
change both reported nodes and time-management behavior.

Hot-path branch shape matters. Node type is encoded with const generics so root,
PV, and non-PV branches can be optimized away. Extraction boundaries in
full-width search should preserve this shape with inlining where needed and
avoid packaging hot scalar state into opaque heap-allocated or dynamically
dispatched objects.

Many formulas are tuned together. Their constants should be treated as SPSA
output unless an experiment intentionally retunes them. Comments should explain
each formula's heuristic purpose and branch-order constraint rather than
restating arithmetic constants.

## Candidate Module Map

`search/root.rs` should own iterative deepening, MultiPV rank groups, aspiration
windows, root UCI reporting decisions, root time-management feedback, and
root-specific result updates. This is a real concept because root search has
responsibilities that do not exist at interior nodes.

`search/full.rs` should own the recursive full-width alpha-beta driver and read
like the node-phase map above. It should coordinate concept helpers but avoid
hiding the order of major phases.

`search/tt.rs` should own search-facing TT interpretation: probe result shaping,
cutoff predicates, TT-adjusted eval predicates, early/final write policies, and
bound helpers. Storage layout remains in `transposition.rs`; search TT policy is
a separate concept.

`search/eval.rs` should own raw eval setup, correction-history lookup, corrected
eval, TT-adjusted estimated score, in-check eval fallback, and
correction-history update policy. It exists because static eval is not just
`nnue.evaluate`; it is a bundle of TT and correction-history semantics.

`search/pruning.rs` should own pre-move pruning gates and move-pruning
predicates: razoring, reverse futility, null move, ProbCut coordination,
late-move pruning, futility pruning, bad-noisy futility, and SEE thresholds.
These are conceptually pruning decisions, but the hottest call sites may need
inline predicates rather than a large state object.

`search/singular.rs` should own potential-singularity detection and
singular-extension outcomes: extension count, multi-cut score, TT-move
suppression, and negative extension. This phase is a distinct TT-move
verification search with its own temporary stack exclusion invariant.

`search/moves.rs` should own move-loop mechanics that are not history policy:
make/undo wrappers, node counting, NNUE push/pop, continuation pointer setup, TT
prefetch, root-move filtering, and root move result updates.

`search/history.rs` should own search-side history updates and history-derived
move scores. The history table storage can stay in `src/history.rs`; this module
should collect the full-width and qsearch update policies that currently sit at
several distant points in the driver.

`search/qsearch.rs` should own quiescence search. It is an algorithmically
different search with a stand-pat contract, tactical move set, shallow TT
policy, and much smaller history footprint.
