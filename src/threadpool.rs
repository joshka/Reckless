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
    thread::{SearchSnapshot, SharedContext, Status, ThreadData},
    time::TimeManager,
};

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

    pub fn execute_searches(
        &mut self,
        time_manager: TimeManager,
        report: Report,
        shared: &Arc<SharedContext>,
    ) -> Vec<SearchSnapshot> {
        shared.tt.increment_age();

        shared.nodes.reset();
        shared.tb_hits.reset();
        shared.soft_stop_votes.store(0, std::sync::atomic::Ordering::Release);
        shared.status.set(Status::RUNNING);
        shared.best_stats.iter().for_each(|x| {
            x.store((self.main.previous_best_score + 32768) as u32, std::sync::atomic::Ordering::Release);
        });

        let thread_count = self.len();
        let board = Arc::new(self.main.board.clone());

        for worker in &self.workers {
            worker
                .start_search(board.clone(), time_manager.clone(), thread_count)
                .expect("Failed to send function to worker thread");
        }

        self.main.time_manager = time_manager;
        search::start(&mut self.main, report, thread_count);
        shared.status.set(Status::STOPPED);

        let mut results = Vec::with_capacity(thread_count);
        results.push(SearchSnapshot::from(&self.main));

        for worker in &self.workers {
            results.push(worker.recv_result());
        }

        results
    }
}

struct WorkerThread {
    handle: JoinHandle<()>,
    commands: SyncSender<WorkerCommand>,
    results: Receiver<SearchSnapshot>,
}

impl WorkerThread {
    fn start_search(
        &self,
        board: Arc<Board>,
        time_manager: TimeManager,
        thread_count: usize,
    ) -> Result<(), std::sync::mpsc::SendError<WorkerCommand>> {
        self.commands.send(WorkerCommand::Search { board, time_manager, thread_count })
    }

    fn recv_result(&self) -> SearchSnapshot {
        self.results.recv().expect("Worker thread failed to report search results")
    }

    fn join(self) {
        let _ = self.commands.send(WorkerCommand::Exit);
        self.handle.join().expect("Worker thread panicked");
    }
}

enum WorkerCommand {
    Search { board: Arc<Board>, time_manager: TimeManager, thread_count: usize },
    Exit,
}

fn make_worker_thread(id: usize, shared: Arc<SharedContext>, board: Arc<Board>) -> WorkerThread {
    let (commands, command_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, results) = std::sync::mpsc::sync_channel(0);

    let handle = std::thread::spawn(move || {
        #[cfg(feature = "numa")]
        crate::numa::bind_thread(id - 1);

        let mut td = make_main_thread_data(shared, board);
        td.id = id;

        while let Ok(command) = command_rx.recv() {
            match command {
                WorkerCommand::Search { board, time_manager, thread_count } => {
                    td.time_manager = time_manager;
                    td.board = (*board).clone();
                    search::start(&mut td, Report::None, thread_count);
                    result_tx.send(SearchSnapshot::from(&td)).expect("Main thread dropped worker result receiver");
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
