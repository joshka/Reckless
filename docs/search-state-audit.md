# Search State And Node-Kind Audit

## ThreadData Field Groups

`ThreadData` is not one concept. Full-width search uses it as several state
views:

- Board and NNUE: `board`, `nnue`, make/undo, null move, hash, threats, SEE,
  direct-check tests, and static evaluation.
- Stack: current/parent/child entries, excluded move, eval, TT move, TT-PV,
  move count, cutoff count, reduction, continuation-history pointers.
- Shared state: TT, status, node counters, tablebase counters and flags,
  best-stats atomics, shared correction histories.
- Local histories: quiet, noisy, continuation, continuation-correction.
- Root state: root moves, PV table, root depth, root delta, selected depth,
  MultiPV indices, best-move-change count.
- Time state: thread id, time manager, null-move verification minimum ply.

New helpers should avoid taking `&mut ThreadData` just because it is convenient.
When broad access remains necessary, the helper should document why the phase is
cross-cutting.

## NodeType Usage

`NODE::ROOT` currently controls:

- root-only PV semantics and root move filtering;
- skipping non-root draw/mate-distance guards;
- excluding root from repetition adjustment and tablebase probes;
- root result updates and MultiPV TT write suppression;
- no-legal-move behavior;
- history/finalization differences.

`NODE::PV` currently controls:

- window invariant checks;
- PV table clear/update and selected-depth tracking;
- early TT cutoff restrictions;
- razoring exclusion;
- singular-extension margins;
- LMR and FDS reduction formulas;
- PV re-search;
- qsearch TT cutoff and stand-pat adjustment restrictions;
- final TT write PV flag.

This split suggests two future directions:

- root behavior can probably become a separate entry/result-update concern;
- PV behavior is deeply tied to alpha-beta window semantics and may need to
  remain explicit near child-search planning.

## Near-Term Guidance

- Do not spread `NodeType` into new helpers by default.
- Prefer helper arguments that name the exact distinction they need, such as
  `pv`, `root`, or `cut_node`.
- If replacing `NodeType` with a runtime `NodeKind`, benchmark it directly;
  current monomorphization may be carrying useful codegen.
- Treat board/NNUE and stack mutation as hot-path contracts.
