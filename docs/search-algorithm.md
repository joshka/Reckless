# Search Algorithm Map

<!-- markdownlint-configure-file {"MD013": {"line_length": 100}} -->

Search runs as nested control loops. `uci::go` prepares limits, `ThreadPool::execute_searches`
creates worker state, `search::start` drives root [iterative deepening], root search handles
[MultiPV] and [aspiration window] retries, full-width search evaluates recursive [alpha-beta]
nodes, and [quiescence search] stabilizes tactical leaves.

Full-width search is the normal recursive node contract. It may consider the whole legal move set,
although pruning, reductions, extensions, and [PVS] often avoid searching every move to the same
depth. The full-width contract includes terminal guards, TT and tablebase proofs, static eval,
pre-move pruning, singular verification, ordered move search, history feedback, TT writeback, and
correction-history learning.

Qsearch starts when full-width depth reaches zero. It uses a smaller contract: shallow TT policy,
stand-pat eval outside check, tactical move generation, [SEE] pruning, small history bonuses, and
shallow TT writeback. Qsearch omits full-width reduction policy, singular search, root reporting,
ordinary quiet move loops, and full-width correction-history training.

## Search Lifecycle

The UCI layer parses the `go` command into a `TimeManager`. `ThreadPool::execute_searches` then
increments TT age, resets shared counters, marks shared status as running, builds legal root moves,
applies Syzygy root ranking when available, and starts one worker search per thread. Helper workers
receive the same board and root move list but do not print UCI `info`.

Each worker enters `search::start`. Root startup clears the PV table, refreshes NNUE for the root
board, clips MultiPV to the legal root move count, and initializes root progress state. The root
loop searches depths from 1 to `MAX_PLY`. Each depth searches every active MultiPV slot, possibly
with several aspiration retries, then reports completed-depth information and feeds stability
signals back into time management.

When workers stop, the main `go` path combines thread results. Timed single-PV search votes across
worker best moves using score and completed depth. If a helper result wins, the main path prints a
final `info` line for that result before reporting `bestmove`.

## Root Search

Root search handles ply-zero responsibilities: legal root move groups, MultiPV indexes,
aspiration windows, root reporting, root tablebase grouping, root optimism, and time feedback.
Interior nodes do not sort root moves, print UCI output, or vote on soft stops.

A root depth runs in this order:

1. Stop if the configured depth limit has been exceeded.
2. Start root progress accounting for this depth.
3. Reset selected depth, root depth, best-move-change count, and PV rank-group bounds.
4. Mark every `RootMove` as starting a new depth.
5. Search each MultiPV slot.
6. Record completed depth if the shared search was not stopped.
7. Print depth information when reporting rules allow it.
8. Feed root stability into time management and possibly stop.

A MultiPV slot search runs in this order:

1. Advance to the current tablebase-rank group.
2. Build an [aspiration window] around the rolling average score.
3. Set root optimism from shared best-score statistics.
4. Reset the stack.
5. Store root delta for reduction policy.
6. Search the root as a PV full-width node.
7. Sort the active root group after each retry.
8. Expand the aspiration window after fail-low or fail-high.
9. Report expensive aspiration retries when UCI reporting is enabled.

`RootMove` is the root result record. A searched root move accumulates node count, score, display
score, bound flags, selected depth, PV, tablebase rank, tablebase score, and previous score for
reporting unsearched MultiPV moves. The move loop updates the matching `RootMove` after each root
candidate; the root driver decides when to sort, group, and report those results.

Root progress turns search stability into time feedback. It tracks evaluation stability against the
rolling average, PV stability from repeated best moves, best-move-change count, node share spent on
the current best move, and this worker's soft-stop vote. Helper votes stop the shared search once
enough workers agree that the soft limit has been reached.

## Full-Width Search

Full-width search preserves a strict phase order because many exits change node counts, TT
contents, history feedback, or time behavior. The driver order is:

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

Node entry validates the window, captures root/PV and cut-node facts, derives side-to-move and
check state, clears PV storage for interior PV nodes, returns on shared stop, enters qsearch at
depth zero, applies repetition adjustment, updates selected depth, polls time on the main thread,
handles draw and maximum-ply guards, and applies mate-distance pruning.

The proof phase reads the [transposition table] into `TtProbe`. A TT entry can provide a cutoff
proof, a move-ordering hint, a TT-adjusted eval bound, a PV marker, or singular-verification
evidence. Full-width TT cutoffs require compatible depth, bound direction, exclusion state, search
window, node kind, and halfmove-clock safety. Interior [tablebases] can also cut off, seed a PV
lower bound, or cap a PV score with an upper bound.

Eval setup computes the correction-history value, obtains raw eval from TT or NNUE, stores a
raw-eval-only TT entry for fresh NNUE results, builds corrected eval, and tightens the estimated
score from compatible TT bounds. In-check nodes can use compatible non-decisive TT scores as eval
because they have no static stand-pat value.

Stack preparation publishes the current node contract to later phases. The stack entry receives
eval, TT move, TT-PV flag, reduction state, move count, and child cutoff counter state before
pruning, reductions, and history feedback read those fields. This phase also applies
eval-difference quiet-history feedback, hindsight depth changes, and improvement signals from
prior stack evals.

Pre-move pruning runs before ordinary move generation:

1. [Razoring].
2. [Reverse futility pruning].
3. [Null-move pruning] with verification.
4. [ProbCut].

Null move and ProbCut run child searches before the move loop. Their make/undo order, TT prefetch,
stop handling, and TT writes are part of search behavior.

[Singular search] verifies whether the TT move is much better than alternatives by temporarily
excluding that move and searching a reduced node. The result can extend the TT move, multi-cut the
node, suppress a misleading TT move, apply a negative extension, or keep a singular score for
later reduction policy. The stack exclusion move must be restored before normal move ordering
continues.

The ordered move loop walks move-picker candidates in search order:

1. Skip the singular-excluded move.
2. At root, skip moves outside the current MultiPV rank group.
3. Increment the move count and publish it to the stack.
4. Derive `MoveCandidate`: move, quiet/noisy class, history score, direct-check status.
5. Apply move pruning:
   - [late-move pruning];
   - quiet [futility pruning];
   - bad-noisy [futility pruning];
   - [SEE] pruning.
6. Record the node count before child search for root reporting.
7. Make the move through `search/transition.rs`.
8. Search the child with reduced scout, full-depth scout, and PV re-search as needed.
9. Undo the move.
10. Stop if shared search status changed.
11. At root, update the matching `RootMove`.
12. Accept the child result into alpha-beta state.
13. Add non-best searched moves to quiet/noisy buffers for later history feedback.

Child search combines [late-move reductions] and [PVS] with engine-specific signals: depth, move
count, node kind, cut-node shape, TT move, TT depth, TT-PV, TT-move score, quiet/noisy history,
correction-history value, improvement state, alpha-raise count, parent reduction, helper-thread
bias, root delta, and singular-score margin. A reduced scout can trigger a full-depth retry, and a
PV node can re-search when the first move or a scout result raises alpha.

Alpha-beta acceptance records TT-move score, updates best score, raises alpha, updates the PV table
for interior PV nodes, records beta cutoffs, publishes early TT lower bounds after allowed
alpha raises, and counts non-decisive alpha raises for reduction policy.

Finalization handles the completed move set. No-legal-move nodes return mate, draw, or the
excluded-move tablebase sentinel. Searched nodes update best-move [history] bonuses, quiet/noisy
maluses, continuation histories, prior-move fail-low feedback, TT-PV propagation, beta-cutoff
score shaping, Syzygy PV caps, final TT writeback, and correction-history learning.

## Qsearch

Qsearch keeps alpha-beta semantics but restricts the move set to tactical stabilization. Entry
handles repetition adjustment, side-to-move and check state, PV state, selected depth, time polling,
draw and maximum-ply guards, and a shallow TT probe.

Outside check, qsearch computes correction history, gets raw eval from TT or NNUE, builds corrected
eval, tightens stand-pat with compatible TT scores, and uses stand-pat as the current best score.
A stand-pat beta cutoff writes a shallow lower-bound TT entry only when no TT entry existed. In
check, qsearch has no stand-pat value; it searches evasions and returns mate if no legal move
exists.

The tactical loop uses qsearch move ordering, skips quiets when allowed, stops after late
non-checking moves, applies qsearch [SEE] pruning against the stand-pat margin, makes the move,
recurses into qsearch, undoes the move, and updates best score, PV, and alpha. A beta cutoff
applies a small quiet or noisy history bonus, scales non-decisive cutoffs toward beta, and writes
the final shallow qsearch TT bound.

## Shared State And Ordering

`ThreadData` is the per-worker search context. Search reads and mutates it as board and NNUE state,
stack state, root move and PV state, local histories, correction histories, time state, root and
MultiPV counters, selected and completed depths, null-move verification state, and a handle to
shared search state. Narrow methods such as `is_stopped`, `stop_search`, and `nodes` name common
contracts without replacing `ThreadData` with another all-access context.

`SharedContext` contains cross-worker state: TT, shared status, sharded node and tablebase
counters, tablebase probing flags, soft-stop votes, root best-score statistics, NUMA-replicated
correction histories, and replicated NNUE parameters.

Stack entries connect parent, current, and child phases without allocation. They carry the move and
piece just made, static eval, singular-exclusion move, TT move, TT-PV flag, cutoff count, move
count, reduction amount, and continuation-history subtable pointers. Negative indexing uses
sentinel entries to provide stable history pointers for early plies.

TT writes are algorithmic events. Eval-only writes, tablebase proofs, ProbCut lower bounds,
alpha-raise lower bounds, final full-width results, stand-pat qsearch bounds, and final qsearch
bounds all feed later pruning or ordering.

History tables have separate roles. Quiet history orders quiet moves and feeds quiet pruning
thresholds. Noisy history orders captures and feeds noisy pruning thresholds. Continuation history
relates a move to prior moves. Correction history trains static eval by pawn, non-pawn, and
continuation context.

Phase order protects strength, speed, and deterministic node counts. Cheap guards, repetition
handling, qsearch entry, draw detection, mate-distance pruning, and TT cutoffs run before tablebase
probes, NNUE eval, and move generation. `make_move` increments nodes exactly when search commits to
a child and before making the board move. Moving counters, prefetches, or early exits across
make/undo boundaries changes reported nodes and time-management behavior.

Node kind uses const generics so LLVM can remove root, PV, and non-PV branches from specialized
call paths. Hot helper boundaries preserve that branch shape with inlining and avoid opaque runtime
dispatch or heap state. Tuned formula docs explain heuristic purpose and branch-order constraints
rather than each numeric constant.

## Source Map

`search/mod.rs` defines the module index and node-kind marker surface. `search/root.rs` handles
root iterative deepening, MultiPV rank groups, aspiration retries, reporting, and time feedback.
`search/full.rs` coordinates the recursive full-width phase order. `search/qsearch.rs` handles
tactical leaf stabilization.

The remaining modules own phase mechanics. `search/tt.rs` interprets TT entries for search.
`search/eval.rs` builds eval and stack contracts. `search/pruning.rs` contains pruning gates and
move-pruning predicates. `search/singular.rs` verifies TT-move singularity. `search/moves.rs`
searches ordered children. `search/reductions.rs` computes child-search depth policy.
`search/transition.rs` owns make/undo transition invariants. `search/history.rs` applies search
history feedback. `search/finalize.rs` completes full-width node results. `search/tablebase.rs`
handles interior Syzygy proofs.

[alpha-beta]: https://www.chessprogramming.org/Alpha-Beta
[aspiration window]: https://www.chessprogramming.org/Aspiration_Windows
[futility pruning]: https://www.chessprogramming.org/Futility_Pruning
[history]: https://www.chessprogramming.org/History_Heuristic
[iterative deepening]: https://www.chessprogramming.org/Iterative_Deepening
[late-move pruning]: https://www.chessprogramming.org/Futility_Pruning#Move_Count_Based_Pruning
[late-move reductions]: https://www.chessprogramming.org/Late_Move_Reductions
[MultiPV]: https://www.chessprogramming.org/Principal_Variation
[Null-move pruning]: https://www.chessprogramming.org/Null_Move_Pruning
[ProbCut]: https://www.chessprogramming.org/ProbCut
[PVS]: https://www.chessprogramming.org/Principal_Variation_Search
[quiescence search]: https://www.chessprogramming.org/Quiescence_Search
[Razoring]: https://www.chessprogramming.org/Razoring
[Reverse futility pruning]: https://www.chessprogramming.org/Reverse_Futility_Pruning
[SEE]: https://www.chessprogramming.org/Static_Exchange_Evaluation
[Singular search]: https://www.chessprogramming.org/Singular_Extensions
[tablebases]: https://www.chessprogramming.org/Endgame_Tablebases
[transposition table]: https://www.chessprogramming.org/Transposition_Table
