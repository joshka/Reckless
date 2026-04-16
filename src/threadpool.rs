use std::{
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender},
    },
    thread::JoinHandle,
};

use crate::{
    board::Board,
    search::{self, Report},
    thread::{HelperSearchResult, SearchSnapshot, SharedContext, Status, ThreadData},
    time::TimeManager,
};

#[derive(Clone)]
pub struct SearchRequest {
    pub board: Arc<Board>,
    pub time_manager: TimeManager,
    pub report: Report,
    pub thread_count: usize,
    pub multi_pv: usize,
}

pub struct SearchResults {
    pub main: SearchSnapshot,
    pub helpers: Vec<HelperSearchResult>,
}

pub struct ThreadPool {
    main: ThreadData,
    workers: Vec<WorkerThread>,
}

impl ThreadPool {
    pub fn available_threads() -> usize {
        const MINIMUM_THREADS: usize = 512;

        match std::thread::available_parallelism() {
            Ok(threads) => (4 * threads.get()).max(MINIMUM_THREADS),
            Err(_) => MINIMUM_THREADS,
        }
    }

    pub fn new(shared: Arc<SharedContext>) -> Self {
        let board = Arc::new(Board::starting_position());
        let main = make_main_thread_data(shared.clone(), board.clone());
        let workers = make_worker_threads(1, shared, board);

        Self { main, workers }
    }

    pub fn set_count(&mut self, threads: usize) {
        let threads = threads.clamp(1, ThreadPool::available_threads());
        let shared = self.main.shared.clone();
        let board = Arc::new(self.main.board.clone());

        self.workers.drain(..).for_each(WorkerThread::join);
        self.main = make_main_thread_data(shared.clone(), board.clone());
        self.workers = make_worker_threads(threads, shared, board);
    }

    pub fn main_thread(&mut self) -> &mut ThreadData {
        &mut self.main
    }

    pub fn board(&self) -> &Board {
        &self.main.board
    }

    pub const fn len(&self) -> usize {
        self.workers.len() + 1
    }

    pub fn set_board(&mut self, board: Board) {
        self.main.board = board;
    }

    pub fn clear(&mut self) {
        let shared = self.main.shared.clone();
        let threads = self.len();
        let board = Arc::new(Board::starting_position());

        self.workers.drain(..).for_each(WorkerThread::join);
        self.main = make_main_thread_data(shared.clone(), board.clone());
        self.workers = make_worker_threads(threads, shared, board);
    }

    pub fn execute_searches(&mut self, request: SearchRequest, shared: &Arc<SharedContext>) -> SearchResults {
        shared.tt.increment_age();

        shared.nodes.reset();
        shared.tb_hits.reset();
        shared.soft_stop_votes.store(0, std::sync::atomic::Ordering::Release);
        shared.status.set(Status::RUNNING);
        shared.best_stats.iter().for_each(|x| {
            x.store((self.main.previous_best_score + 32768) as u32, std::sync::atomic::Ordering::Release);
        });

        for worker in &self.workers {
            worker.start_search(request.clone()).expect("Failed to send function to worker thread");
        }

        self.main.time_manager = request.time_manager;
        self.main.multi_pv = request.multi_pv;
        search::start(&mut self.main, request.report, request.thread_count);
        shared.status.set(Status::STOPPED);

        let mut helpers = Vec::with_capacity(self.workers.len());

        for worker in &self.workers {
            helpers.push(worker.recv_result());
        }

        SearchResults { main: SearchSnapshot::from(&self.main), helpers }
    }
}

struct WorkerThread {
    handle: JoinHandle<()>,
    commands: SyncSender<WorkerCommand>,
    results: Receiver<HelperSearchResult>,
}

impl WorkerThread {
    fn start_search(&self, request: SearchRequest) -> Result<(), std::sync::mpsc::SendError<WorkerCommand>> {
        self.commands.send(WorkerCommand::Search(request))
    }

    fn recv_result(&self) -> HelperSearchResult {
        self.results.recv().expect("Worker thread failed to report search results")
    }

    fn join(self) {
        let _ = self.commands.send(WorkerCommand::Exit);
        self.handle.join().expect("Worker thread panicked");
    }
}

enum WorkerCommand {
    Search(SearchRequest),
    Exit,
}

struct WorkerState {
    td: ThreadData,
}

impl WorkerState {
    fn new(id: usize, shared: Arc<SharedContext>, board: Arc<Board>) -> Self {
        let mut td = make_main_thread_data(shared, board);
        td.id = id;
        Self { td }
    }

    fn run_search(&mut self, request: SearchRequest) -> HelperSearchResult {
        self.td.time_manager = request.time_manager;
        self.td.multi_pv = request.multi_pv;
        self.td.board = (*request.board).clone();
        search::start(&mut self.td, Report::None, request.thread_count);
        HelperSearchResult::from(&self.td)
    }
}

fn make_worker_thread(id: usize, shared: Arc<SharedContext>, board: Arc<Board>) -> WorkerThread {
    let (commands, command_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, results) = std::sync::mpsc::sync_channel(0);

    let handle = std::thread::spawn(move || {
        #[cfg(feature = "numa")]
        crate::numa::bind_thread(id - 1);

        let mut state = WorkerState::new(id, shared, board);

        while let Ok(command) = command_rx.recv() {
            match command {
                WorkerCommand::Search(request) => {
                    result_tx.send(state.run_search(request)).expect("Main thread dropped worker result receiver");
                }
                WorkerCommand::Exit => break,
            }
        }
    });

    WorkerThread { handle, commands, results }
}

fn make_worker_threads(num_threads: usize, shared: Arc<SharedContext>, board: Arc<Board>) -> Vec<WorkerThread> {
    (1..num_threads).map(|id| make_worker_thread(id, shared.clone(), board.clone())).collect()
}

fn make_main_thread_data(shared: Arc<SharedContext>, board: Arc<Board>) -> ThreadData {
    let mut td = ThreadData::new(shared);
    td.board = (*board).clone();
    td
}
