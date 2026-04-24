use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    board::Board,
    history::{ContinuationCorrectionHistory, ContinuationHistory, CorrectionHistory, NoisyHistory, QuietHistory},
    nnue::{Network, ParametersHandle},
    numa::{NumaConfig, NumaReplicable, NumaReplicated, NumaReplicatedAccessToken, NumaReplicationContext},
    stack::Stack,
    threadpool::ThreadPool,
    time::{Limits, TimeManager},
    transposition::TranspositionTable,
    types::{MAX_MOVES, MAX_PLY, Move, Score, normalize_to_cp},
};

#[repr(align(64))]
struct AlignedAtomicU64 {
    inner: AtomicU64,
}

pub struct Counter {
    shards: Box<[AlignedAtomicU64]>,
}

unsafe impl Sync for Counter {}

impl Counter {
    pub fn aggregate(&self) -> u64 {
        self.shards.iter().map(|shard| shard.inner.load(Ordering::Relaxed)).sum()
    }

    pub fn get(&self, id: usize) -> u64 {
        self.shards[id].inner.load(Ordering::Relaxed)
    }

    pub fn increment(&self, id: usize) {
        self.shards[id].inner.store(self.shards[id].inner.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }

    pub fn reset(&self) {
        for shard in &self.shards {
            shard.inner.store(0, Ordering::Relaxed);
        }
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            shards: std::iter::from_fn(|| Some(AlignedAtomicU64 { inner: AtomicU64::new(0) }))
                .take(ThreadPool::available_threads())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }
}

pub struct Status {
    inner: AtomicUsize,
}

impl Status {
    pub const STOPPED: usize = 0;
    pub const RUNNING: usize = 1;

    pub fn get(&self) -> usize {
        self.inner.load(Ordering::Acquire)
    }

    pub fn set(&self, status: usize) {
        self.inner.store(status, Ordering::Release);
    }
}

impl Clone for Status {
    fn clone(&self) -> Self {
        Self { inner: AtomicUsize::new(self.inner.load(Ordering::Relaxed)) }
    }
}

impl Default for Status {
    fn default() -> Self {
        Self { inner: AtomicUsize::new(Self::STOPPED) }
    }
}

#[derive(Default)]
pub struct SharedCorrectionHistory {
    pub pawn: CorrectionHistory,
    pub non_pawn: [CorrectionHistory; 2],
}

impl NumaReplicable for SharedCorrectionHistory {
    fn allocate() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

pub struct SharedContext {
    pub tt: TranspositionTable,
    pub status: Status,
    pub nodes: Counter,
    pub tb_hits: Counter,
    pub stop_probing_tb: AtomicBool,
    pub root_in_tb: AtomicBool,
    pub soft_stop_votes: AtomicUsize,
    pub best_stats: [AtomicU32; MAX_MOVES],
    pub history: Arc<NumaReplicated<SharedCorrectionHistory>>,
    pub parameters: Arc<NumaReplicated<ParametersHandle>>,
    pub numa_context: Arc<NumaReplicationContext>,
}

impl Default for SharedContext {
    fn default() -> Self {
        let numa_context = Arc::new(NumaReplicationContext::new(NumaConfig::from_system()));

        Self {
            tt: TranspositionTable::default(),
            status: Status::default(),
            nodes: Counter::default(),
            tb_hits: Counter::default(),
            stop_probing_tb: AtomicBool::new(false),
            root_in_tb: AtomicBool::new(false),
            soft_stop_votes: AtomicUsize::new(0),
            best_stats: [const { AtomicU32::new(0) }; MAX_MOVES],
            history: NumaReplicated::new(numa_context.clone()),
            parameters: NumaReplicated::new(numa_context.clone()),
            numa_context,
        }
    }
}

pub struct ThreadData {
    pub id: usize,
    pub shared: Arc<SharedContext>,
    pub corrhist: Arc<SharedCorrectionHistory>,
    pub board: Board,
    pub time_manager: TimeManager,
    pub stack: Box<Stack>,
    pub nnue: Network,
    pub root_moves: Vec<RootMove>,
    pub pv_table: PrincipalVariationTable,
    pub noisy_history: NoisyHistory,
    pub quiet_history: QuietHistory,
    pub continuation_history: ContinuationHistory,
    pub continuation_corrhist: ContinuationCorrectionHistory,
    pub best_move_changes: usize,
    pub optimism: [i32; 2],
    pub root_depth: i32,
    pub root_delta: i32,
    pub sel_depth: i32,
    pub completed_depth: i32,
    pub nmp_min_ply: i32,
    pub previous_best_score: i32,
    pub multi_pv: usize,
    pub pv_index: usize,
    pub pv_start: usize,
    pub pv_end: usize,
}

impl ThreadData {
    pub fn new(shared: Arc<SharedContext>, numa_token: NumaReplicatedAccessToken) -> Self {
        let corrhist = shared.history.get(numa_token);
        let parameters = shared.parameters.get(numa_token);

        Self {
            id: 0,
            shared,
            corrhist,
            board: Board::starting_position(),
            time_manager: TimeManager::new(Limits::Infinite, 0, 0),
            stack: Stack::new(),
            nnue: Network::new(parameters),
            root_moves: Vec::new(),
            pv_table: PrincipalVariationTable::default(),
            noisy_history: NoisyHistory::default(),
            quiet_history: QuietHistory::default(),
            continuation_history: ContinuationHistory::default(),
            continuation_corrhist: ContinuationCorrectionHistory::default(),
            best_move_changes: 0,
            optimism: [0; 2],
            root_depth: 0,
            root_delta: 0,
            sel_depth: 0,
            completed_depth: 0,
            nmp_min_ply: 0,
            previous_best_score: 0,
            multi_pv: 1,
            pv_index: 0,
            pv_start: 0,
            pv_end: 0,
        }
    }

    pub fn nodes(&self) -> u64 {
        self.shared.nodes.get(self.id)
    }

    /// Whether shared search control has requested this worker to stop.
    ///
    /// Search phases should use this instead of reaching through `shared.status`
    /// when they only need the stop contract. Thread-pool lifecycle code still
    /// owns the lower-level RUNNING/STOPPED state transitions.
    #[inline]
    pub fn is_stopped(&self) -> bool {
        self.shared.status.get() == Status::STOPPED
    }

    /// Request that all workers stop at their next polling point.
    #[inline]
    pub fn stop_search(&self) {
        self.shared.status.set(Status::STOPPED);
    }

    pub fn corrhist(&self) -> &SharedCorrectionHistory {
        &self.corrhist
    }

    pub fn conthist(&self, ply: isize, index: isize, mv: Move) -> i32 {
        self.continuation_history.get(self.stack[ply - index].conthist, self.board.piece_on(mv.from()), mv.to())
    }

    pub fn print_uci_info(&self, depth: i32) {
        let elapsed = self.time_manager.elapsed();
        let nps = self.shared.nodes.aggregate() as f64 / elapsed.as_secs_f64();
        let ms = elapsed.as_millis();

        let root_in_tb = self.shared.root_in_tb.load(Ordering::Relaxed);

        for pv_index in 0..self.multi_pv {
            let root_move = &self.root_moves[pv_index];
            let Some(report) = root_move.uci_report(depth, pv_index, root_in_tb) else {
                continue;
            };

            print!(
                "info depth {} seldepth {} multipv {} score {} nodes {} time {ms} nps {nps:.0} hashfull {} tbhits {} pv",
                report.depth,
                root_move.sel_depth,
                pv_index + 1,
                report.formatted_score(&self.board),
                self.shared.nodes.aggregate(),
                self.shared.tt.hashfull(),
                self.shared.tb_hits.aggregate(),
            );

            print!(" {}", root_move.mv.to_uci(&self.board));
            for mv in root_move.pv.line() {
                print!(" {}", mv.to_uci(&self.board));
            }

            println!();
        }
    }
}

/// Root-search state for one legal move at ply zero.
///
/// This type is intentionally physical rather than perfectly conceptual: root search, tablebase
/// setup, UCI reporting, and move sorting all mutate the same move list. Treat the field groups as
/// separate concepts even though they share one cache-friendly record.
#[derive(Clone)]
pub struct RootMove {
    /// Legal root move identity. This is the stable key used by root filtering and reporting.
    pub mv: Move,

    /// Current search score used for root sorting and best-move selection.
    pub score: i32,

    /// Previous completed-depth score, used to preserve root ordering between iterations.
    pub previous_score: i32,

    /// UCI-facing score after root display shaping and aspiration-bound handling.
    pub display_score: i32,

    /// Whether `display_score` should be reported as an upper bound.
    pub upperbound: bool,

    /// Whether `display_score` should be reported as a lower bound.
    pub lowerbound: bool,

    /// Selected depth reached by the root move, reported with the UCI line.
    pub sel_depth: i32,

    /// Node count spent under this root move during the current search.
    pub nodes: u64,

    /// Principal variation currently attached to this root move.
    pub pv: PrincipalVariationTable,

    /// Root tablebase rank used to group MultiPV searches without mixing proven classes.
    pub tb_rank: i32,

    /// Root tablebase score associated with `tb_rank`.
    pub tb_score: i32,
}

impl Default for RootMove {
    fn default() -> Self {
        Self {
            mv: Move::NULL,
            score: -Score::INFINITE,
            previous_score: -Score::INFINITE,
            display_score: -Score::INFINITE,
            upperbound: false,
            lowerbound: false,
            sel_depth: 0,
            nodes: 0,
            pv: PrincipalVariationTable::default(),
            tb_rank: 0,
            tb_score: 0,
        }
    }
}

impl RootMove {
    /// Carry the current score into `previous_score` at the start of a new root depth.
    pub fn start_depth(&mut self) {
        self.previous_score = self.score;
    }

    /// Whether two root moves belong to the same root tablebase rank group.
    pub fn same_tablebase_group(&self, other: &Self) -> bool {
        self.tb_rank == other.tb_rank
    }

    /// Record the UCI-facing result of searching this root move.
    ///
    /// This updates the root move's search score, display score, bound flags, selected depth, PV,
    /// and node accounting together because UCI reporting and root sorting consume them as one
    /// result concept.
    pub fn record_search_result(&mut self, result: RootSearchResult<'_>) -> bool {
        self.nodes += result.nodes;

        if result.move_count != 1 && result.score <= result.alpha {
            self.mark_unsearched_for_sorting();
            return false;
        }

        self.upperbound = false;
        self.lowerbound = false;
        match result.score {
            v if v <= result.alpha => {
                self.display_score = result.alpha;
                self.upperbound = true;
            }
            v if v >= result.beta => {
                self.display_score = result.beta;
                self.lowerbound = true;
            }
            _ => {
                self.display_score = result.score;
            }
        }

        self.score = result.score;
        self.sel_depth = result.sel_depth;
        self.pv.commit_full_root_pv(result.pv, result.start_ply);

        result.move_count > 1
    }

    /// Mark this move as not currently competitive for root sorting.
    pub fn mark_unsearched_for_sorting(&mut self) {
        self.score = -Score::INFINITE;
    }

    /// Build the UCI reporting view for this root move.
    ///
    /// Root search may report a previous-depth value when a move has not been searched at the
    /// current depth. Tablebase root mode replaces ordinary search display scores for TB-winning
    /// values because the root TB rank, not aspiration-window bounds, owns the displayed proof.
    fn uci_report(&self, depth: i32, pv_index: usize, root_in_tb: bool) -> Option<RootMoveReport> {
        let updated = self.score != -Score::INFINITE;

        if depth == 1 && !updated && pv_index > 0 {
            return None;
        }

        let mut report = RootMoveReport {
            depth: if updated { depth } else { (depth - 1).max(1) },
            score: if updated { self.display_score } else { self.previous_score },
            upperbound: self.upperbound,
            lowerbound: self.lowerbound,
        };

        if root_in_tb && report.score.abs() <= Score::TB_WIN {
            report.score = self.tb_score;
            report.upperbound = false;
            report.lowerbound = false;
        }

        Some(report)
    }
}

/// UCI-facing score and bound view for one root move.
struct RootMoveReport {
    /// Depth to print for this move.
    depth: i32,

    /// Score after root display shaping and tablebase replacement.
    score: i32,

    /// Whether this score is an aspiration upper bound.
    upperbound: bool,

    /// Whether this score is an aspiration lower bound.
    lowerbound: bool,
}

impl RootMoveReport {
    /// Format the score and optional bound flag for a UCI `info` line.
    fn formatted_score(&self, board: &Board) -> String {
        let mut formatted = match self.score.abs() {
            s if s < Score::TB_WIN_IN_MAX => {
                format!("cp {}", normalize_to_cp(self.score, board))
            }
            s if s <= Score::TB_WIN => {
                let cp = 20_000 - Score::TB_WIN + self.score.abs();
                format!("cp {}", if self.score.is_positive() { cp } else { -cp })
            }
            _ => {
                let mate = (Score::MATE - self.score.abs() + self.score.is_positive() as i32) / 2;
                format!("mate {}", if self.score.is_positive() { mate } else { -mate })
            }
        };

        if self.upperbound {
            formatted.push_str(" upperbound");
        } else if self.lowerbound {
            formatted.push_str(" lowerbound");
        }

        formatted
    }
}

/// Root move search result applied after one candidate search.
pub struct RootSearchResult<'a> {
    /// Score returned by the root child search.
    pub score: i32,

    /// Root alpha before accepting this score.
    pub alpha: i32,

    /// Root beta for display bound shaping.
    pub beta: i32,

    /// One-based root move count in the ordered move loop.
    pub move_count: i32,

    /// Selected depth reached while searching this move.
    pub sel_depth: i32,

    /// Principal variation table to copy from.
    pub pv: &'a PrincipalVariationTable,

    /// Source ply in `pv` for the root line.
    pub start_ply: usize,

    /// Nodes spent under this root move during the just-finished child search.
    pub nodes: u64,
}

#[derive(Clone)]
pub struct PrincipalVariationTable {
    table: Box<[[Move; MAX_PLY + 1]]>,
    len: [usize; MAX_PLY + 1],
}

impl PrincipalVariationTable {
    pub fn line(&self) -> &[Move] {
        &self.table[0][..self.len[0]]
    }

    pub const fn clear(&mut self, ply: usize) {
        self.len[ply] = 0;
    }

    pub fn update(&mut self, ply: usize, mv: Move) {
        self.table[ply][0] = mv;
        self.len[ply] = self.len[ply + 1] + 1;

        for i in 0..self.len[ply + 1] {
            self.table[ply][i + 1] = self.table[ply + 1][i];
        }
    }

    pub fn commit_full_root_pv(&mut self, src: &Self, start_ply: usize) {
        let len = src.len[start_ply].min(MAX_PLY + 1);
        self.len[0] = len;
        self.table[0][..len].copy_from_slice(&src.table[start_ply][..len]);
    }
}

impl Default for PrincipalVariationTable {
    fn default() -> Self {
        Self {
            table: vec![[Move::NULL; MAX_PLY + 1]; MAX_PLY + 1].into_boxed_slice(),
            len: [0; MAX_PLY + 1],
        }
    }
}
