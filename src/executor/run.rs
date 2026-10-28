//! The `run` pipeline: orchestration of a benchmark run, the client threads
//! and the data collector that turns their messages into the logs and the
//! post-ramp-up counters. `Executor::aggregate_data`, `print_results` and
//! `generate_report` (the steps after the workload) stay in the parent module.

use std::collections::{BTreeSet, HashMap};
use std::fs::{create_dir_all, File};
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossbeam_channel::{unbounded, Receiver, Sender};
use indicatif::ProgressBar;
use postgres::Client;
use rand::prelude::*;
use tokio_postgres_rustls::MakeRustlsConnect;

use super::benchmark::{BenchmarkTransaction, Counter};
use super::error::{error_detail, Error};
use super::txmessage::{TXMessage, TXMessageKind};
use super::yaml_benchmark::YamlBenchmark;
use super::{redact_dsn, summary, terminal, Executor, ERROR_FILE, LOG_FILE, RUN_FILE};
use crate::args::RunArgs;

/// How the clients are driven; decided by the spec, see `Executor::engine`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    /// One std::thread and one blocking connection per client. The Lua
    /// database bridge is synchronous, so every spec with a script-mode
    /// transaction runs here.
    ThreadPerClient,
    /// One Tokio task per client on `threads` worker threads, for specs made
    /// of `simple` transactions only.
    Async { threads: usize },
}

impl Engine {
    fn describe(self) -> String {
        match self {
            Engine::ThreadPerClient => "one thread per client (script mode)".to_string(),
            Engine::Async { threads } => {
                format!("{threads} worker thread(s), one task per client")
            }
        }
    }
}

/// Worker threads of the async engine when `--threads` is 0: half the CPUs,
/// at least one, at most one per client. On a 4-core box shared with the
/// server, one or two workers did best for 8 clients while four competed
/// with the server; a dedicated client machine can afford
/// more, with `--threads`.
fn default_threads(clients: u16, cpus: usize) -> usize {
    (cpus / 2).clamp(1, clients.max(1) as usize)
}

/// What a client runs next, shared by both engines.
///
/// A timed run draws every transaction at random by weight for as long as the
/// run lasts. A `--once` run gives each client its own random permutation of
/// the transactions with a weight above 0 and runs it once, which is the
/// query-stream model of the TPC-H and TPC-DS power and throughput tests.
enum Schedule {
    Weighted {
        cumulative_weights: Vec<u32>,
        total_weight: u32,
    },
    Once {
        order: Vec<usize>,
        next: usize,
    },
}

struct TransactionPicker {
    transactions: Vec<BenchmarkTransaction>,
    schedule: Schedule,
}

impl TransactionPicker {
    fn new(transactions: Vec<BenchmarkTransaction>, once: bool, rng: &mut impl Rng) -> Self {
        let schedule = if once {
            let mut order: Vec<usize> = (0..transactions.len())
                .filter(|&i| transactions[i].weight > 0)
                .collect();
            order.shuffle(rng);
            Schedule::Once { order, next: 0 }
        } else {
            let total_weight = transactions.iter().map(|t| t.weight as u32).sum();
            let cumulative_weights = transactions
                .iter()
                .scan(0u32, |acc, t| {
                    *acc += t.weight as u32;
                    Some(*acc)
                })
                .collect();
            Schedule::Weighted {
                cumulative_weights,
                total_weight,
            }
        };
        Self {
            transactions,
            schedule,
        }
    }

    /// The next transaction to run, or `None` when a `--once` client is done.
    fn next(&mut self, rng: &mut impl Rng) -> Option<&BenchmarkTransaction> {
        match &mut self.schedule {
            Schedule::Weighted {
                cumulative_weights,
                total_weight,
            } => {
                let rand_val: u32 = rng.random_range(1..=*total_weight);
                Some(&self.transactions[pick_transaction(cumulative_weights, rand_val)])
            }
            Schedule::Once { order, next } => {
                let index = *order.get(*next)?;
                *next += 1;
                Some(&self.transactions[index])
            }
        }
    }
}

/// The progress bar or progress lines of the workload window and the thread
/// that drives them.
struct WorkloadMonitor {
    bar: ProgressBar,
    handle: JoinHandle<()>,
    stop: Arc<AtomicBool>,
}

/// Everything one client task of the async engine needs.
struct ClientTask {
    client_id: u16,
    dsn: String,
    tls: MakeRustlsConnect,
    benchmark: Arc<YamlBenchmark>,
    tx: Sender<TXMessage>,
    abort: Arc<AtomicBool>,
    interrupted: Arc<AtomicBool>,
    duration_ms: u64,
    once: bool,
}

/// One client of the async engine: connect, then run transactions until the
/// client's share of the run is over or the run is aborted or interrupted.
async fn run_client_task(task: ClientTask) -> Result<(), Error> {
    let ClientTask {
        client_id,
        dsn,
        tls,
        benchmark,
        tx,
        abort,
        interrupted,
        duration_ms,
        once,
    } = task;

    let (client, connection) = match tokio_postgres::connect(&dsn, tls).await {
        Ok(ready) => ready,
        Err(error) => {
            abort.store(true, Ordering::Relaxed);
            return Err(Error::Other(format!(
                "client {}: connection failed: {}",
                client_id,
                error_detail(&error)
            )));
        }
    };
    // The connection future moves the bytes; it ends when `client` is
    // dropped. A connection error resurfaces on the client's next statement,
    // which reports it like any other error.
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let mut rng = SmallRng::from_os_rng();
    let mut picker = TransactionPicker::new(benchmark.get_transactions_rw(), once, &mut rng);
    let loop_start = Instant::now();
    // No time limit in a `--once` run: the client stops when its list ends.
    let duration_limit = (!once).then_some(duration_ms as u128);

    while let Some(transaction) = picker.next(&mut rng) {
        match benchmark
            .execute_simple_transaction_async(&client, transaction)
            .await
        {
            Ok(duration) => {
                let _ = tx.send(TXMessage::committed(
                    transaction.id,
                    client_id as u32,
                    duration,
                ));
            }
            Err(error) => {
                let _ = tx.send(TXMessage::error(
                    transaction.id,
                    client_id as u32,
                    error.to_string(),
                ));
            }
        }

        if duration_limit.is_some_and(|limit| loop_start.elapsed().as_millis() >= limit)
            || abort.load(Ordering::Relaxed)
            || interrupted.load(Ordering::Relaxed)
        {
            break;
        }
    }
    Ok(())
}

/// Weighted transaction selection: the index of the first cumulative weight
/// that `rand_val` (drawn uniformly from `1..=total_weight`) does not exceed.
/// A transaction with weight 0 shares its cumulative value with the one before
/// it and is therefore never picked. A value beyond the total, which cannot
/// come from the draw, falls back to the first transaction.
fn pick_transaction(cumulative_weights: &[u32], rand_val: u32) -> usize {
    cumulative_weights
        .iter()
        .position(|&w| rand_val <= w)
        .unwrap_or(0)
}

/// What the data collector hands back when it finishes: the post-ramp-up
/// per-transaction counters and the log timestamp (unix seconds, same clock
/// as `transaction.log`) at which ramp-up ended.
struct CollectorResult {
    counters: HashMap<u16, Counter>,
    rampup_end_ts: Option<i64>,
}

/// `error.log` holds one record per line (`<ts> <clients> <tx_id> <message>`),
/// but Lua errors carry a multi-line traceback and PostgreSQL errors can have
/// `DETAIL:` / `HINT:` lines. Escape line breaks, tabs and the backslash itself
/// (so the escaping is reversible, as in PostgreSQL's text COPY format) to keep
/// one error per line for the report parser and for grep/awk users.
fn escape_log_message(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for c in message.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

impl Executor {
    // Execute read/write mixed workload
    pub fn run_benchmark(&mut self, args: RunArgs) -> Result<&mut Self, Error> {
        self.n_clients = args.client;
        self.duration_s = args.time;
        self.rampup_s = args.rampup;
        self.once = args.once;
        let rampup_ms = args.rampup as u64 * 1000;
        let time_ms = args.time as u64 * 1000;
        // Nap time before starting a new client
        let sleep_ms = if args.client > 0 {
            rampup_ms / args.client as u64
        } else {
            0
        };

        let engine = self.engine(&args);

        // Echo the run configuration up front so the output is self-documenting
        // and the run is reproducible from its own log.
        self.print_run_header(&args, engine)?;

        // Channels used to communicate transactions states: id, duration, committed?, etc..
        let (tx, rx): (Sender<TXMessage>, Receiver<TXMessage>) = unbounded();
        // Channels used to send back the counters once data collector has finished its work.
        let (tx_counters, rx_counters): (Sender<CollectorResult>, Receiver<CollectorResult>) =
            unbounded();

        // Track total execution time in ms
        let start = Instant::now();
        let command = "RUN";

        // Live counters shared with the data collector; sampled by the optional
        // progress reporter thread to emit periodic throughput/latency lines.
        let live_commits = Arc::new(AtomicU64::new(0));
        let live_latency_us = Arc::new(AtomicU64::new(0));

        // Create the target dir
        let target_dir_name = match self.target_dir.file_name() {
            Some(dir_name) => dir_name.to_str().unwrap(),
            None => "Should not happen",
        };
        let message = format!("Creating the target dir. {}", target_dir_name);
        terminal::start_msg(command, message.as_str());
        create_dir_all(&self.target_dir)
            .map_err(|e| Error::io(format!("Cannot create {}", self.target_dir.display()), e))?;
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        // The benchmark always targets the full object-id range: min is 1 and
        // max is auto-detected from the dataset at run time.
        let min_id: u32 = 1;
        terminal::start_msg(command, "Fetching maximum ID value");
        let benchmark_client = self.get_benchmark(0, 0, 0);
        self.throughput_unit = benchmark_client.spec.metrics.throughput_unit.clone();
        self.throughput_factor = benchmark_client.spec.metrics.per.factor();
        let max_id: u32 = {
            let mut client = self.connect()?;
            let max_id = benchmark_client.get_default_max_id(&mut client)?;
            terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);
            max_id
        };

        // Start the data collector only now that everything that can fail
        // before the clients start has succeeded.
        let dc_tx_counters = tx_counters.clone();
        let data_collector = self.start_data_collector(
            rx,
            dc_tx_counters,
            Arc::clone(&live_commits),
            Arc::clone(&live_latency_us),
        )?;

        let message = format!(
            "Starting {} client(s) in {} seconds",
            args.client, args.rampup
        );
        terminal::start_msg(command, message.as_str());

        // Ctrl-C stops the clients at the end of their current transaction
        // and lets the collector, aggregation and report run on the partial
        // data; a second Ctrl-C (flag already raised) exits with status 130.
        let interrupted = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register_conditional_shutdown(
            signal_hook::consts::SIGINT,
            130,
            Arc::clone(&interrupted),
        )
        .and_then(|_| {
            signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))
        })
        .map_err(|e| Error::io("Cannot install the Ctrl-C handler", e))?;

        // A client that cannot start is reported after the collector has
        // flushed the logs below.
        let workload = match engine {
            Engine::ThreadPerClient => self.run_benchmark_sync(
                args.once,
                args.client,
                min_id,
                max_id,
                time_ms,
                rampup_ms,
                sleep_ms,
                tx.clone(),
                start,
                Arc::clone(&live_commits),
                Arc::clone(&live_latency_us),
                args.progress,
                Arc::clone(&interrupted),
            ),
            Engine::Async { threads } => self.run_benchmark_async(
                threads,
                args.once,
                args.client,
                min_id,
                max_id,
                time_ms,
                rampup_ms,
                sleep_ms,
                tx.clone(),
                start,
                Arc::clone(&live_commits),
                Arc::clone(&live_latency_us),
                args.progress,
                Arc::clone(&interrupted),
            ),
        };

        // Proceed total execution time
        self.total_time_ms = start.elapsed().as_millis();
        if args.once {
            // The window a `--once` run took, so the results and the report
            // carry the real duration rather than the unused --time default.
            self.duration_s = summary::workload_secs(self.total_time_ms, self.rampup_time_ms)
                .round()
                .clamp(1.0, u16::MAX as f64) as u16;
        }
        self.interrupted = interrupted.load(Ordering::Relaxed);
        if self.interrupted {
            terminal::interrupted_msg();
        }

        // Send termination message to the data collector
        tx.send(TXMessage::terminate_data_collector()).unwrap();
        // Wait for the end of the data collection thread
        data_collector
            .join()
            .expect("the data collector thread panicked");

        // Receive counters from the data collector
        let result = rx_counters.recv().unwrap();
        self.counters = result.counters;
        self.rampup_end_ts = result.rampup_end_ts;

        workload?;
        self.write_run_metadata();

        Ok(self)
    }

    // Write `run.json` next to the logs: the run parameters and the ramp-up
    // boundary, so the raw `transaction.log` can be re-analysed with the same
    // steady-state window the statistics use.
    fn write_run_metadata(&self) {
        let doc = serde_json::json!({
            "clients": self.n_clients,
            "duration_s": self.duration_s,
            "rampup_s": self.rampup_s,
            "once": self.once,
            "total_time_ms": self.total_time_ms,
            "rampup_time_ms": self.rampup_time_ms,
            "workload_secs": summary::workload_secs(self.total_time_ms, self.rampup_time_ms),
            "rampup_end_ts": self.rampup_end_ts,
            "throughput_unit": self.throughput_unit,
            "throughput_factor": self.throughput_factor,
            "interrupted": self.interrupted,
        });
        let path = self.target_dir.join(RUN_FILE);
        if let Err(error) = std::fs::write(&path, format!("{:#}\n", doc)) {
            terminal::err_msg(&format!("cannot write {}: {}", path.display(), error));
        }
    }

    // Echo the run configuration to stderr so the output is self-documenting and
    // the run can be reproduced from its own log. Kept on stderr so stdout still
    // carries only the results (tables or --format json/csv).
    /// The engine this run uses: the thread engine as soon as the spec has a
    /// script-mode transaction, the async engine otherwise, with `--threads`
    /// workers or the default for this machine.
    fn engine(&self, args: &RunArgs) -> Engine {
        if self.benchmark.has_script_transactions() {
            return Engine::ThreadPerClient;
        }
        let threads = if args.threads == 0 {
            let cpus = thread::available_parallelism().map_or(1, |n| n.get());
            default_threads(args.client, cpus)
        } else {
            args.threads as usize
        };
        Engine::Async { threads }
    }

    fn print_run_header(&mut self, args: &RunArgs, engine: Engine) -> Result<(), Error> {
        if terminal::is_quiet() {
            return Ok(());
        }
        let spec_name = self.get_benchmark(0, 0, 0).spec.name.clone();
        let started: DateTime<Utc> = Utc::now();
        eprintln!();
        eprintln!("pgmeter {} - {}", env!("CARGO_PKG_VERSION"), spec_name);
        eprintln!("  spec:     {}", self.spec_file);
        eprintln!("  target:   {}", redact_dsn(&self.dsn));
        eprintln!("  clients:  {}", args.client);
        eprintln!("  engine:   {}", engine.describe());
        if args.once {
            eprintln!("  mode:     every transaction once per client (--once)");
        } else {
            eprintln!("  duration: {} s", args.time);
        }
        eprintln!("  rampup:   {} s", args.rampup);
        if args.progress > 0 {
            eprintln!("  progress: every {} s", args.progress);
        }
        eprintln!("  started:  {}", started.format("%Y-%m-%dT%H:%M:%SZ"));
        eprintln!();
        Ok(())
    }

    /// Benchmark execution using one std::thread per client, each on its own
    /// blocking connection. The Lua database bridge is synchronous, so this is
    /// the engine of every spec with a script-mode transaction.
    // TODO(RELEASE_REVIEW D3/D11): bundle the run parameters into a struct.
    #[allow(clippy::too_many_arguments)]
    fn run_benchmark_sync(
        &mut self,
        once: bool,
        n_clients: u16,
        min_id: u32,
        max_id: u32,
        time_ms: u64,
        rampup_ms: u64,
        sleep_ms: u64,
        tx: Sender<TXMessage>,
        start: Instant,
        live_commits: Arc<AtomicU64>,
        live_latency_us: Arc<AtomicU64>,
        progress: u16,
        interrupted: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        let mut client_threads: Vec<JoinHandle<Result<(), Error>>> =
            Vec::with_capacity(n_clients as usize);
        // Raised by a client that cannot start (connection, spec or prepare
        // failure) so the other clients stop instead of running out the clock.
        let abort = Arc::new(AtomicBool::new(false));

        // The parsed spec with this run's id range, shared by every client.
        let benchmark = Arc::new(self.benchmark.with_context(0, min_id, max_id));

        // Start the clients using std::thread
        for client_id in 1..=n_clients {
            let duration_ms = time_ms + rampup_ms - client_id as u64 * sleep_ms;
            sleep(Duration::from_millis(sleep_ms));
            if abort.load(Ordering::Relaxed) || interrupted.load(Ordering::Relaxed) {
                break;
            }

            let dsn = self.dsn.clone();
            let tls = self.tls.clone();
            let benchmark = Arc::clone(&benchmark);
            let tx_clone = tx.clone();
            let abort = Arc::clone(&abort);
            let interrupted = Arc::clone(&interrupted);

            let handle = thread::spawn(move || -> Result<(), Error> {
                let setup = (|| {
                    let client = Client::connect(&dsn, tls).map_err(|e| {
                        Error::Other(format!(
                            "client {}: connection failed: {}",
                            client_id,
                            error_detail(&e)
                        ))
                    })?;
                    Ok::<_, Error>(client)
                })();
                let mut client = match setup {
                    Ok(ready) => ready,
                    Err(error) => {
                        abort.store(true, Ordering::Relaxed);
                        return Err(error);
                    }
                };

                let mut rng = SmallRng::from_os_rng();
                let mut picker =
                    TransactionPicker::new(benchmark.get_transactions_rw(), once, &mut rng);
                let loop_start = Instant::now();
                // No time limit in a `--once` run: the client stops when its list ends.
                let duration_limit = (!once).then_some(duration_ms as u128);

                while let Some(transaction) = picker.next(&mut rng) {
                    // Execute the transaction
                    match benchmark.execute_rw_transaction_sync(&mut client, transaction) {
                        Ok(duration) => {
                            let m =
                                TXMessage::committed(transaction.id, client_id as u32, duration);
                            let _ = tx_clone.send(m);
                        }
                        Err(error) => {
                            let m = TXMessage::error(
                                transaction.id,
                                client_id as u32,
                                error.to_string(),
                            );
                            let _ = tx_clone.send(m);
                        }
                    }

                    if duration_limit.is_some_and(|limit| loop_start.elapsed().as_millis() >= limit)
                        || abort.load(Ordering::Relaxed)
                        || interrupted.load(Ordering::Relaxed)
                    {
                        break;
                    }
                }
                Ok(())
            });

            client_threads.push(handle);
        }

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);

        let start2 = Instant::now();
        let monitor = self.spawn_monitor(
            (!once).then_some(time_ms / 1000),
            start2,
            live_commits,
            live_latency_us,
            progress,
        );

        // Send end-of-rampup message
        tx.send(TXMessage::end_of_rampup()).unwrap();
        self.rampup_time_ms = start.elapsed().as_millis();

        // Wait for all client threads, keeping the first failure.
        let mut first_error = None;
        for handle in client_threads {
            if let Err(error) = handle.join().expect("Client thread panicked") {
                first_error.get_or_insert(error);
            }
        }

        Self::stop_monitor(monitor, start2);

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Benchmark execution on the async engine: a Tokio runtime with
    /// `threads` worker threads drives one task per client on its own
    /// `tokio-postgres` connection. A worker that finds a client waiting on
    /// the server moves on to the next one, so the process wakes up far less
    /// often than one thread per client does: on a 4-core box shared with the
    /// server this took PGM-B from 87% to 94% of pgbench's rate
    ///. Only `simple` transactions can run here.
    #[allow(clippy::too_many_arguments)]
    fn run_benchmark_async(
        &mut self,
        threads: usize,
        once: bool,
        n_clients: u16,
        min_id: u32,
        max_id: u32,
        time_ms: u64,
        rampup_ms: u64,
        sleep_ms: u64,
        tx: Sender<TXMessage>,
        start: Instant,
        live_commits: Arc<AtomicU64>,
        live_latency_us: Arc<AtomicU64>,
        progress: u16,
        interrupted: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads)
            .thread_name("pgmeter-client")
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| Error::io("Cannot start the client runtime", e))?;
        // Raised by a client that cannot connect so the other clients stop
        // instead of running out the clock.
        let abort = Arc::new(AtomicBool::new(false));
        let benchmark = Arc::new(self.benchmark.with_context(0, min_id, max_id));
        let dsn = self.dsn.clone();
        let tls = self.tls.clone();

        // Start the clients, staggered over the ramp-up like the thread engine.
        let tasks: Vec<tokio::task::JoinHandle<Result<(), Error>>> = runtime.block_on(async {
            let mut tasks = Vec::with_capacity(n_clients as usize);
            for client_id in 1..=n_clients {
                let duration_ms = time_ms + rampup_ms - client_id as u64 * sleep_ms;
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                if abort.load(Ordering::Relaxed) || interrupted.load(Ordering::Relaxed) {
                    break;
                }
                tasks.push(tokio::spawn(run_client_task(ClientTask {
                    client_id,
                    dsn: dsn.clone(),
                    tls: tls.clone(),
                    benchmark: Arc::clone(&benchmark),
                    tx: tx.clone(),
                    abort: Arc::clone(&abort),
                    interrupted: Arc::clone(&interrupted),
                    duration_ms,
                    once,
                })));
            }
            tasks
        });

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);

        let start2 = Instant::now();
        let monitor = self.spawn_monitor(
            (!once).then_some(time_ms / 1000),
            start2,
            live_commits,
            live_latency_us,
            progress,
        );

        // Send end-of-rampup message
        tx.send(TXMessage::end_of_rampup()).unwrap();
        self.rampup_time_ms = start.elapsed().as_millis();

        // Wait for all client tasks, keeping the first failure.
        let mut first_error = None;
        runtime.block_on(async {
            for task in tasks {
                match task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        first_error.get_or_insert(error);
                    }
                    Err(panic) => {
                        first_error
                            .get_or_insert(Error::Other(format!("client task panicked: {panic}")));
                    }
                }
            }
        });
        // The connections close with the runtime.
        drop(runtime);

        Self::stop_monitor(monitor, start2);

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Drive a determinate bar (TTY) or periodic progress lines (non-TTY)
    /// over the post-rampup workload window, fed by the live counters.
    fn spawn_monitor(
        &self,
        total_s: Option<u64>,
        start2: Instant,
        live_commits: Arc<AtomicU64>,
        live_latency_us: Arc<AtomicU64>,
        progress: u16,
    ) -> WorkloadMonitor {
        let stop = Arc::new(AtomicBool::new(false));
        let (bar, handle) = terminal::spawn_workload_monitor(
            total_s,
            start2,
            live_commits,
            live_latency_us,
            Arc::clone(&stop),
            progress,
            terminal::RateUnit {
                unit: self.throughput_unit.clone(),
                factor: self.throughput_factor,
            },
        );
        WorkloadMonitor { bar, handle, stop }
    }

    fn stop_monitor(monitor: WorkloadMonitor, start2: Instant) {
        monitor.stop.store(true, Ordering::Relaxed);
        monitor
            .handle
            .join()
            .expect("the workload monitor thread panicked");
        terminal::finish_run_bar(monitor.bar, start2.elapsed().as_micros() as f64 / 1000.0);
    }

    // Start the data collector thread. Data collector is in charge of storing transaction
    // informations into the log file and incrementing counters.
    // Once the data collector has received the Terminate message, then
    // the counters are sent back to the main process through the tx_counters channel.
    fn start_data_collector(
        &mut self,
        rx: Receiver<TXMessage>,
        tx_counters: Sender<CollectorResult>,
        live_commits: Arc<AtomicU64>,
        live_latency_us: Arc<AtomicU64>,
    ) -> Result<JoinHandle<()>, Error> {
        // Open both log files here, so a failure is reported before any
        // client starts rather than from inside the collector thread.
        let create = |name: &str| {
            let path = self.target_dir.join(name);
            File::create(&path)
                .map_err(|e| Error::io(format!("Cannot create {}", path.display()), e))
        };
        // Use larger buffer for better I/O performance
        let mut log_file = BufWriter::with_capacity(64 * 1024, create(LOG_FILE)?);
        let mut error_file = BufWriter::with_capacity(8 * 1024, create(ERROR_FILE)?);
        Ok(thread::spawn(move || {
            // Initialize the counters with expected capacity
            let mut counters: HashMap<u16, Counter> = HashMap::with_capacity(16);
            // Distinct clients seen so far; its size is the "active clients"
            // column of both logs.
            let mut client_ids: BTreeSet<u32> = BTreeSet::new();

            let mut ramping_up: bool = true;
            let mut rampup_end_ts: Option<i64> = None;
            let mut buffer_i = itoa::Buffer::new();
            let mut buffer_f = ryu::Buffer::new();

            // Track start time for timestamp generation
            let start_time = Utc::now().timestamp();
            let start_instant = Instant::now();

            // Wait for messages from the clients. A closed channel means the
            // run was abandoned before Terminate could be sent (every sender
            // dropped): flush and finish like a normal shutdown instead of
            // panicking.
            while let Ok(msg) = rx.recv() {
                // Exit thread
                match msg.kind {
                    // Terminate data collector
                    TXMessageKind::Terminate => {
                        break;
                    }
                    // Committed transaction
                    TXMessageKind::Committed => {
                        // Keep a track of the client_ids and increment n_client if this is a new client_id
                        client_ids.insert(msg.client_id);

                        let duration_ms = msg.tx_duration_us as f64 / 1000.0;

                        // Feed the live counters for the progress reporter. These
                        // count every commit (rampup included) so the live view
                        // reflects real activity; the headline metrics below stay
                        // rampup-aware.
                        live_commits.fetch_add(1, Ordering::Relaxed);
                        live_latency_us.fetch_add(msg.tx_duration_us as u64, Ordering::Relaxed);

                        // Counters calculation - update only if rampup stage is over
                        if !ramping_up {
                            counters
                                .entry(msg.tx_id)
                                .and_modify(|c| {
                                    c.n_commits += 1;
                                    c.n_total += 1;
                                    c.total_duration_ms += duration_ms;
                                })
                                .or_insert(Counter {
                                    n_commits: 1,
                                    n_total: 1,
                                    total_duration_ms: duration_ms,
                                });
                        }

                        // Timestamps are taken here rather than passed through
                        // the channel. The commit was just received, so its start
                        // is now minus its duration, in ms since the collector
                        // started: the stream report's timeline needs it.
                        let elapsed_us = start_instant.elapsed().as_micros();
                        let tx_timestamp = start_time + (elapsed_us / 1_000_000) as i64;
                        let start_ms = elapsed_us.saturating_sub(msg.tx_duration_us) / 1000;

                        // `<ts> <active clients> <tx id> <ms> <client id> <start ms>`
                        let _ = log_file.write(buffer_i.format(tx_timestamp).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(client_ids.len()).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(msg.tx_id).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_f.format(duration_ms).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(msg.client_id).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(start_ms).as_bytes());
                        let _ = log_file.write(b"\n");
                    }
                    TXMessageKind::Error => {
                        // Keep a track of the client_ids and increment n_client if this is a new client_id
                        client_ids.insert(msg.client_id);

                        // Counters calculation
                        if !ramping_up {
                            counters
                                .entry(msg.tx_id)
                                .and_modify(|c| {
                                    c.n_total += 1;
                                })
                                .or_insert(Counter {
                                    n_commits: 0,
                                    n_total: 1,
                                    total_duration_ms: 0.0,
                                });
                        }

                        // Generate timestamp locally
                        let tx_timestamp = start_time + start_instant.elapsed().as_secs() as i64;
                        let error_str = escape_log_message(msg.error.as_deref().unwrap_or(""));

                        // Format and write the line to the error log file
                        let _ = error_file.write(buffer_i.format(tx_timestamp).as_bytes());
                        let _ = error_file.write(b" ");
                        let _ = error_file.write(buffer_i.format(client_ids.len()).as_bytes());
                        let _ = error_file.write(b" ");
                        let _ = error_file.write(buffer_i.format(msg.tx_id).as_bytes());
                        let _ = error_file.write(b" ");
                        let _ = error_file.write(error_str.as_bytes());
                        let _ = error_file.write(b"\n");
                    }
                    TXMessageKind::EndOfRampup => {
                        ramping_up = false;
                        rampup_end_ts = Some(start_time + start_instant.elapsed().as_secs() as i64);
                    }
                }
            }
            // Flush buffers before sending counters
            let _ = log_file.flush();
            let _ = error_file.flush();
            // Send counters
            tx_counters
                .send(CollectorResult {
                    counters,
                    rampup_end_ts,
                })
                .unwrap();
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{default_threads, escape_log_message, pick_transaction, TransactionPicker};
    use crate::executor::benchmark::BenchmarkTransaction;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    // The async engine's default worker count: half the CPUs, never 0, never
    // more than one per client.
    #[test]
    fn default_threads_is_half_the_cpus_bounded_by_the_clients() {
        assert_eq!(default_threads(8, 4), 2);
        assert_eq!(default_threads(8, 16), 8);
        assert_eq!(default_threads(1, 16), 1);
        assert_eq!(default_threads(8, 1), 1);
        assert_eq!(default_threads(0, 4), 1);
    }

    fn tx(id: u16, weight: u16) -> BenchmarkTransaction {
        BenchmarkTransaction {
            id,
            name: format!("tx{id}"),
            weight,
        }
    }

    // The picker shared by both engines draws each transaction in proportion
    // to its weight and never a zero-weight one, for as long as it is asked.
    #[test]
    fn transaction_picker_follows_the_weights() {
        let mut rng = SmallRng::seed_from_u64(7);
        let mut picker =
            TransactionPicker::new(vec![tx(1, 90), tx(2, 0), tx(3, 10)], false, &mut rng);
        let mut counts = [0u32; 4];
        for _ in 0..10_000 {
            counts[picker.next(&mut rng).unwrap().id as usize] += 1;
        }
        assert_eq!(counts[2], 0, "{counts:?}");
        assert!((8_700..=9_300).contains(&counts[1]), "{counts:?}");
        assert!((700..=1_300).contains(&counts[3]), "{counts:?}");
    }

    // A `--once` schedule hands out every transaction with a weight above 0
    // exactly once, in an order that differs between clients, then ends.
    #[test]
    fn once_schedule_runs_each_transaction_once_then_ends() {
        let transactions: Vec<BenchmarkTransaction> = (1..=20)
            .map(|id| tx(id, if id == 7 { 0 } else { 1 }))
            .collect();
        let mut orders = Vec::new();
        for seed in 0..4 {
            let mut rng = SmallRng::seed_from_u64(seed);
            let mut picker = TransactionPicker::new(transactions.clone(), true, &mut rng);
            let mut order = Vec::new();
            while let Some(t) = picker.next(&mut rng) {
                order.push(t.id);
            }
            assert!(
                picker.next(&mut rng).is_none(),
                "a finished client stays finished"
            );
            let mut sorted = order.clone();
            sorted.sort_unstable();
            let expected: Vec<u16> = (1..=20).filter(|&id| id != 7).collect();
            assert_eq!(sorted, expected, "seed {seed}: {order:?}");
            orders.push(order);
        }
        assert!(
            orders.windows(2).any(|w| w[0] != w[1]),
            "clients must not all run the same order: {orders:?}"
        );
    }

    // RELEASE_REVIEW F2: the weighted selection the client loop runs on every
    // transaction. Weights 70/30 give cumulative [70, 100]; each draw in
    // 1..=70 must select the first transaction, 71..=100 the second.
    #[test]
    fn pick_transaction_honours_the_cumulative_boundaries() {
        let cumulative = [70, 100];
        assert_eq!(pick_transaction(&cumulative, 1), 0);
        assert_eq!(pick_transaction(&cumulative, 70), 0);
        assert_eq!(pick_transaction(&cumulative, 71), 1);
        assert_eq!(pick_transaction(&cumulative, 100), 1);

        // Exhaustively: the share of draws per transaction is exactly its weight.
        let picks: Vec<usize> = (1..=100)
            .map(|v| pick_transaction(&cumulative, v))
            .collect();
        assert_eq!(picks.iter().filter(|&&i| i == 0).count(), 70);
        assert_eq!(picks.iter().filter(|&&i| i == 1).count(), 30);

        // A single transaction always wins.
        assert_eq!(pick_transaction(&[5], 5), 0);
        // A value beyond the total cannot be drawn; it falls back to index 0
        // rather than panicking on an out-of-range index.
        assert_eq!(pick_transaction(&cumulative, 101), 0);
    }

    // A transaction with weight 0 is valid (the spec only requires the total
    // to be above 0) and must never run: it shares its cumulative value with
    // its predecessor, which `position` finds first.
    #[test]
    fn pick_transaction_never_selects_a_zero_weight_transaction() {
        let cumulative = [10, 10, 20]; // weights 10, 0, 10
        let picks: Vec<usize> = (1..=20).map(|v| pick_transaction(&cumulative, v)).collect();
        assert!(!picks.contains(&1), "{picks:?}");
        assert_eq!(picks.iter().filter(|&&i| i == 0).count(), 10);
        assert_eq!(picks.iter().filter(|&&i| i == 2).count(), 10);

        // Leading zero weight: index 0 has cumulative 0, never matched by v >= 1.
        let leading = [0, 10]; // weights 0, 10
        assert!((1..=10).all(|v| pick_transaction(&leading, v) == 1));
    }

    // Regression for RELEASE_REVIEW B11: a Lua traceback or a multi-line
    // PostgreSQL error used to be written raw, breaking the one-record-per-line
    // layout of error.log that the report parser and shell tools rely on.
    #[test]
    fn error_log_messages_stay_on_one_line() {
        let lua = "runtime error: boom\nstack traceback:\n\t[C]: in function 'error'";
        let escaped = escape_log_message(lua);
        assert!(
            !escaped.contains('\n') && !escaped.contains('\t'),
            "{escaped}"
        );
        assert_eq!(
            escaped,
            "runtime error: boom\\nstack traceback:\\n\\t[C]: in function 'error'"
        );

        let pg = "ERROR: division by zero\r\nDETAIL: path C:\\data";
        assert_eq!(
            escape_log_message(pg),
            "ERROR: division by zero\\r\\nDETAIL: path C:\\\\data"
        );
        // Plain messages pass through untouched.
        assert_eq!(escape_log_message("plain message"), "plain message");
    }
}
