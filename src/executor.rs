use std::time::{Duration, Instant};
use std::thread::{JoinHandle, sleep};
use std::thread;
use std::fs::{File, create_dir_all};
use std::io::{BufWriter, Write};
use std::collections::{HashMap, BTreeMap};
use std::env::current_dir;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use crossbeam_channel::{Sender, Receiver, unbounded};
use num_cpus;
use postgres::{Client, NoTls};
use rand::prelude::*;
use tokio::runtime::Builder;
use sqlx::PgConnection;
use sqlx::Connection;
use itoa;
use ryu;

mod benchmark;
mod txmessage;
mod terminal;
mod data_agg;
mod report;
mod yaml_types;
mod yaml_benchmark;
mod yaml_lua;

use benchmark::{
    Benchmark,
    BenchmarkStmt,
    Counter,
    ReadWrite,
    ReadWriteSync,
    ResponseTimeStatistics,
    TransactionSummary,
};
use yaml_benchmark::YamlBenchmark;
use txmessage::{TXMessage, TXMessageKind};
use super::args::{RunArgs};

pub struct Executor {
    dsn: String,
    spec_file: String,
    counters: HashMap<u16, Counter>,
    rampup_time_ms: u128,
    total_time_ms: u128,
    // Run parameters retained for reporting
    n_clients: u16,
    duration_s: u16,
    rampup_s: u16,
    // Target directory used to store collected and aggregated data
    target_dir: PathBuf,
}

const LOG_FILE: &str = "transaction.log";
const ERROR_FILE: &str = "error.log";

pub fn get_target_dir_path() -> PathBuf {
    let current_dir = match current_dir() {
        Ok(current_dir) => current_dir,
        Err(error) => {
            eprintln!("ERROR: {}", error);
            std::process::exit(1);
         }
    };
    let now: DateTime<Utc> = Utc::now();
    let target_dir_path = current_dir.join(format!("pgmtr-{}", now.format("%Y-%m-%dT%H:%M:%S")));

    target_dir_path
}

impl Executor {
    pub fn new(dsn: String, spec_file: String) -> Executor {
        Executor {
            dsn: dsn,
            spec_file: spec_file,
            counters: HashMap::new(),
            total_time_ms: 0,
            rampup_time_ms: 0,
            n_clients: 0,
            duration_s: 0,
            rampup_s: 0,
            target_dir: get_target_dir_path(),
        }
    }

    // Execute read/write mixed workload
    pub fn run_benchmark(&mut self, args: RunArgs) -> &mut Self {
        self.n_clients = args.client;
        self.duration_s = args.time;
        self.rampup_s = args.rampup;
        let rampup_ms = args.rampup as u64 * 1000;
        let time_ms = args.time as u64 * 1000;
        // Nap time before starting a new client
        let sleep_ms = if args.client > 0 { rampup_ms / args.client as u64 } else { 0 };

        // Channels used to communicate transactions states: id, duration, committed?, etc..
        let (tx, rx): (Sender<TXMessage>, Receiver<TXMessage>) = unbounded();
        // Channels used to send back the counters once data collector has finished its work.
        let (tx_counters, rx_counters): (Sender<HashMap<u16, Counter>>, Receiver<HashMap<u16, Counter>>) = unbounded();

        // Track total execution time in ms
        let start = Instant::now();
        let command = "RUN";

        // Create the target dir
        let target_dir_name = match self.target_dir.file_name() {
            Some(dir_name) => dir_name.to_str().unwrap(),
            None => "Should not happen",
        };
        let message = format!("Creating the target dir. {}", target_dir_name);
        terminal::start_msg(command, message.as_str());
        match create_dir_all(&self.target_dir) {
            Ok(_) => (),
            Err(error) => {
                terminal::err_msg(format!("{}", error).as_str());
                std::process::exit(1);
            }
        };
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);

        // Start data collector
        let dc_tx_counters = tx_counters.clone();
        let data_collector = self.start_data_collector(rx, dc_tx_counters);

        // The benchmark always targets the full object-id range: min is 1 and
        // max is auto-detected from the dataset at run time.
        let min_id: u32 = 1;
        terminal::start_msg(command, "Fetching maximum ID value");
        let max_id: u32 = {
            let mut client = Executor::connect(self.dsn.clone());
            let benchmark_client = self.get_benchmark(0, 0, 0);

            let max_id = match benchmark_client.get_default_max_id(&mut client) {
                Ok(max_id) => max_id,
                Err(error) => {
                    terminal::err_msg(format!("{}", error).as_str());
                    std::process::exit(1);
                }
            };
            terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);
            max_id
        };

        // Check if benchmark supports synchronous execution (faster path)
        let benchmark_check = self.get_benchmark(0, min_id, max_id);
        let use_sync = benchmark_check.supports_sync();

        let message = format!("Starting {} client(s) in {} seconds{}",
            args.client, args.rampup,
            if use_sync { " (sync mode)" } else { "" });
        terminal::start_msg(command, message.as_str());

        if use_sync {
            // Use synchronous execution with std::thread (faster, matches pgbench)
            self.run_benchmark_sync(args.client, min_id, max_id, time_ms, rampup_ms, sleep_ms, tx.clone(), start);
        } else {
            // Use async execution with tokio (for benchmarks that don't support sync)
            self.run_benchmark_async(args.client, min_id, max_id, time_ms, rampup_ms, sleep_ms, tx.clone(), start);
        }

        // Proceed total execution time
        self.total_time_ms = start.elapsed().as_millis();

        // Send termination message to the data collector
        tx.send(TXMessage::terminate_data_collector()).unwrap();
        // Wait for the end of the data collection thread
        data_collector.join().expect("the data collector thread panicked");

        // Receive counters from the data collector
        self.counters = rx_counters.recv().unwrap();

        self
    }

    /// Synchronous benchmark execution using std::thread and prepared statements
    /// This path is significantly faster than async for simple benchmarks like TPC-B
    fn run_benchmark_sync(
        &mut self,
        n_clients: u16,
        min_id: u32,
        max_id: u32,
        time_ms: u64,
        rampup_ms: u64,
        sleep_ms: u64,
        tx: Sender<TXMessage>,
        start: Instant,
    ) {
        let command = "RUN";
        let mut client_threads: Vec<JoinHandle<()>> = Vec::with_capacity(n_clients as usize);

        // Start the clients using std::thread
        for client_id in 1..=n_clients {
            let duration_ms = time_ms + rampup_ms - client_id as u64 * sleep_ms;
            sleep(Duration::from_millis(sleep_ms));

            let dsn = self.dsn.clone();
            let spec_file = self.spec_file.clone();
            let tx_clone = tx.clone();

            let handle = thread::spawn(move || {
                // Create synchronous postgres connection
                let mut client = Client::connect(&dsn, NoTls).unwrap_or_else(|err| {
                    eprintln!("ERROR: Connection failed: {}", err);
                    std::process::exit(1);
                });

                // Get benchmark and prepare statements
                let benchmark = YamlBenchmark::from_file(&spec_file, 0, min_id, max_id);
                let prepared = benchmark.prepare_statements(&mut client).unwrap_or_else(|err| {
                    eprintln!("ERROR: Failed to prepare statements: {}", err);
                    std::process::exit(1);
                });

                let transactions = benchmark.get_transactions_rw();

                // Pre-compute cumulative weights for fast weighted selection
                let total_weight: u32 = transactions.iter().map(|t| t.weight as u32).sum();
                let cumulative_weights: Vec<u32> = transactions
                    .iter()
                    .scan(0u32, |acc, t| {
                        *acc += t.weight as u32;
                        Some(*acc)
                    })
                    .collect();

                let loop_start = Instant::now();
                let duration_limit = duration_ms as u128;

                loop {
                    // Fast weighted random selection
                    let tx_idx = {
                        let rand_val: u32 = thread_rng().gen_range(1..=total_weight);
                        cumulative_weights.iter().position(|&w| rand_val <= w).unwrap_or(0)
                    };
                    let transaction = &transactions[tx_idx];

                    // Execute transaction using prepared statements
                    match benchmark.execute_rw_transaction_sync(&mut client, &prepared, transaction) {
                        Ok(duration) => {
                            let m = TXMessage::committed(transaction.id, client_id as u32, duration);
                            let _ = tx_clone.send(m);
                        }
                        Err(error) => {
                            let m = TXMessage::error(transaction.id, client_id as u32, format!("{}", error));
                            let _ = tx_clone.send(m);
                        }
                    }

                    if loop_start.elapsed().as_millis() >= duration_limit {
                        break;
                    }
                }
            });

            client_threads.push(handle);
        }

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);

        let message2 = format!("Running the workload for {} seconds", time_ms / 1000);
        terminal::start_msg(command, message2.as_str());
        let start2 = Instant::now();

        // Send end-of-rampup message
        tx.send(TXMessage::end_of_rampup()).unwrap();
        self.rampup_time_ms = start.elapsed().as_millis();

        // Wait for all client threads
        for handle in client_threads {
            handle.join().expect("Client thread panicked");
        }

        terminal::done_msg(start2.elapsed().as_micros() as f64 / 1000.0);
    }

    /// Async benchmark execution using tokio (for complex benchmarks)
    fn run_benchmark_async(
        &mut self,
        n_clients: u16,
        min_id: u32,
        max_id: u32,
        time_ms: u64,
        rampup_ms: u64,
        sleep_ms: u64,
        tx: Sender<TXMessage>,
        start: Instant,
    ) {
        let command = "RUN";
        let worker_threads = std::cmp::min(n_clients as usize, num_cpus::get());
        let rt = Builder::new_multi_thread()
            .worker_threads(worker_threads)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let mut benchmark_clients = Vec::new();

            for client_id in 1..=n_clients {
                let duration_ms = time_ms + rampup_ms - client_id as u64 * sleep_ms;
                sleep(Duration::from_millis(sleep_ms));

                let benchmark_client = self.start_rw_client(
                    duration_ms,
                    self.dsn.clone(),
                    min_id,
                    max_id,
                    tx.clone(),
                    client_id as u32,
                ).await;

                benchmark_clients.push(benchmark_client);
            }

            terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);

            let message2 = format!("Running the workload for {} seconds", time_ms / 1000);
            terminal::start_msg(command, message2.as_str());
            let start2 = Instant::now();

            tx.send(TXMessage::end_of_rampup()).unwrap();
            self.rampup_time_ms = start.elapsed().as_millis();

            for benchmark_client in benchmark_clients {
                benchmark_client.await.expect("Client task panicked");
            }

            terminal::done_msg(start2.elapsed().as_micros() as f64 / 1000.0);
        });
    }

    // Start a new read/write benchmark client in its own thread
    async fn start_rw_client(&mut self, duration_ms: u64, dsn: String, min_id: u32, max_id: u32, tx: Sender<TXMessage>, client_id: u32) -> tokio::task::JoinHandle<()>
    {
        // Create a new benchmark object by thread because we don't want to share a such
        // complex structure between all the client threads
        let benchmark_client = self.get_benchmark(0, min_id, max_id);

        tokio::spawn(async move {
            // New database connection
            let mut connection = match PgConnection::connect(&dsn).await {
                Ok(connection) => connection,
                Err(error) => {
                    terminal::err_msg(format!("{}", error).as_str());
                    std::process::exit(1);
                }
            };
            let transactions = benchmark_client.get_transactions_rw();

            // Pre-compute cumulative weights for faster weighted selection
            let total_weight: u32 = transactions.iter().map(|t| t.weight as u32).sum();
            let cumulative_weights: Vec<u32> = transactions
                .iter()
                .scan(0u32, |acc, t| {
                    *acc += t.weight as u32;
                    Some(*acc)
                })
                .collect();

            // Used for tracking client execution time
            let start = Instant::now();
            let duration_limit = duration_ms as u128;

            loop {
                // Generate random value before await (ThreadRng is not Send)
                // Fast weighted random selection using pre-computed cumulative weights
                let tx_idx = {
                    let rand_val: u32 = thread_rng().gen_range(1..=total_weight);
                    cumulative_weights
                        .iter()
                        .position(|&w| rand_val <= w)
                        .unwrap_or(0)
                };
                let transaction = &transactions[tx_idx];

                // Execute the database transactions
                match benchmark_client.execute_rw_transaction(&mut connection, &transaction).await {
                    Ok(duration) => {
                        // Send committed message (avoid timestamp computation in hot path)
                        let m = TXMessage::committed(transaction.id, client_id, duration);
                        tx.send(m).unwrap();
                    },
                    Err(error) => {
                        // Send error message
                        let m = TXMessage::error(transaction.id, client_id, format!("{}", error));
                        tx.send(m).unwrap();
                    },
                }
                // Break the loop if we reach the time limit
                if start.elapsed().as_millis() >= duration_limit {
                    break;
                }
            }
        })
    }

    // Start the data collector thread. Data collector is in charge of storing transaction
    // informations into the log file and incrementing counters.
    // Once the data collector has received the shutdown order (message with id=0), then
    // the counters are sent back to the main process through the tx_counters channel.
    fn start_data_collector(&mut self, rx: Receiver<TXMessage>, tx_counters: Sender<HashMap<u16, Counter>>) -> JoinHandle<()> {
        let target_dir = self.target_dir.clone();
        thread::spawn(move || {
            // Create the file where transaction logs are written
            let log_file = match File::create(target_dir.join(LOG_FILE)) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("ERROR: Could not create {}: {}", LOG_FILE, e);
                    std::process::exit(1);
                },
            };
            // Use larger buffer for better I/O performance
            let mut log_file = BufWriter::with_capacity(64 * 1024, log_file);
            // Create the error log file
            let error_file = match File::create(target_dir.join(ERROR_FILE)) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("ERROR: Could not create {}: {}", ERROR_FILE, e);
                    std::process::exit(1);
                },
            };
            let mut error_file = BufWriter::with_capacity(8 * 1024, error_file);

            // Initialize the counters with expected capacity
            let mut counters: HashMap<u16, Counter> = HashMap::with_capacity(16);
            let mut client_ids = BTreeMap::new();

            let mut ramping_up: bool = true;
            let mut buffer_i = itoa::Buffer::new();
            let mut buffer_f = ryu::Buffer::new();

            // Track start time for timestamp generation
            let start_time = Utc::now().timestamp();
            let start_instant = Instant::now();

            let mut n_client: u32 = 0;
            loop {
                // Wait for a new message coming from the clients
                let msg = rx.recv().unwrap();
                // Exit thread
                match msg.kind {
                    // Terminate data collector
                    TXMessageKind::TERMINATE => {
                        break;
                    },
                    // Committed transaction
                    TXMessageKind::COMMITTED => {
                        // Keep a track of the client_ids and increment n_client if this is a new client_id
                        if !client_ids.contains_key(&msg.client_id) {
                            client_ids.insert(msg.client_id, true);
                            n_client += 1;
                        }

                        let duration_ms = msg.tx_duration_us as f64 / 1000.0;

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

                        // Generate timestamp locally (more efficient than passing through channel)
                        let tx_timestamp = start_time + start_instant.elapsed().as_secs() as i64;

                        // Format and write the line to the log file
                        let _ = log_file.write(buffer_i.format(tx_timestamp).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(n_client).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(msg.tx_id).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_f.format(duration_ms).as_bytes());
                        let _ = log_file.write(b"\n");
                    },
                    TXMessageKind::ERROR => {
                        // Keep a track of the client_ids and increment n_client if this is a new client_id
                        if !client_ids.contains_key(&msg.client_id) {
                            client_ids.insert(msg.client_id, true);
                            n_client += 1;
                        }

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
                        let error_str = msg.error.as_deref().unwrap_or("");

                        // Format and write the line to the error log file
                        let _ = error_file.write(buffer_i.format(tx_timestamp).as_bytes());
                        let _ = error_file.write(b" ");
                        let _ = error_file.write(buffer_i.format(n_client).as_bytes());
                        let _ = error_file.write(b" ");
                        let _ = error_file.write(buffer_i.format(msg.tx_id).as_bytes());
                        let _ = error_file.write(b" ");
                        let _ = error_file.write(error_str.as_bytes());
                        let _ = error_file.write(b"\n");
                    },
                    TXMessageKind::ENDOFRAMPUP => {
                        ramping_up = false;
                    },
                    TXMessageKind::DEFAULT => {
                        // Should not happen
                    },
                }
            }
            // Flush buffers before sending counters
            let _ = log_file.flush();
            let _ = error_file.flush();
            // Send counters
            tx_counters.send(counters).unwrap();
        })
    }

    // Open a new connection to the database and returns a Client
    fn connect(dsn: String) -> Client {
        Client::connect(&dsn, NoTls).unwrap_or_else(|err| {
            terminal::err_msg(format!("{}", err).as_str());
            std::process::exit(1);
        })
    }

    // Initialize database schemabenchmark: create tables
    pub fn init_db_schema(&mut self) -> &mut Self {
        let command = "INIT";
        let message = "Executing database DDLs";

        terminal::start_msg(command, message);

        // New database connection
        let mut client = Executor::connect(self.dsn.clone());

        // Load the corresponding benchmark client
        let benchmark_client = self.get_benchmark(0, 0, 0);

        // Initialize the database model/schema
        let duration_us = match benchmark_client.initialize_schema(&mut client) {
            Ok(duration) => duration,
            Err(error) => {
                terminal::err_msg(format!("{}", error).as_str());
                std::process::exit(1);
            }
        };
        let duration_ms = duration_us as f64 / 1000 as f64;

        terminal::done_msg(duration_ms);

        self
    }

    // Execute a post-create-table SQL script if provided
    pub fn exec_post_create_table_script(&mut self, script_path: Option<String>) -> &mut Self {
        let path = match script_path {
            Some(p) => p,
            None => return self,
        };

        let command = "INIT";
        let message = format!("Executing post-create-table script: {}", path);

        terminal::start_msg(command, message.as_str());
        let start = std::time::Instant::now();

        // Read the SQL script file
        let sql = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                terminal::err_msg(format!("Failed to read script file: {}", error).as_str());
                std::process::exit(1);
            }
        };

        // Execute the script
        let mut client = Executor::connect(self.dsn.clone());
        match client.batch_execute(&sql) {
            Ok(_) => (),
            Err(error) => {
                terminal::err_msg(format!("{}", error).as_str());
                std::process::exit(1);
            }
        }

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);

        self
    }

    // Generate benchmark data
    pub fn load_data(&mut self, scalefactor: u32, n_jobs: u32) -> &mut Self {
        // Load the corresponding benchmark client
        let benchmark_client = self.get_benchmark(scalefactor, 0, 0);

        let command = "INIT";
        let message = "Pre-loading operations";

        terminal::start_msg(command, message);

        // New database connection
        let mut client = Executor::connect(self.dsn.clone());

        // Execute PreLoadData
        let duration_us = match benchmark_client.pre_load_data(&mut client) {
            Ok(duration) => duration,
            Err(error) => {
                terminal::err_msg(format!("{}", error).as_str());
                std::process::exit(1);
            }
        };
        let duration_ms = duration_us as f64 / 1000 as f64;

        terminal::done_msg(duration_ms);

        // Execute LoadData using multiple concurrent jobs
        let mut jobs = Vec::new();

        // Build the scalefactor ids matrix as follow (considering 3 jobs and 12 ids):
        // [1, 4, 7, 10]
        // [2, 5, 8, 11]
        // [3, 6, 9, 12]
        // We want to get one line per jobs and the ids balanced across the lines.
        let mut ids = Vec::with_capacity(n_jobs as usize);
        for _ in 0..n_jobs {
            ids.push(Vec::new());
        }
        for n in 1..=scalefactor {
            ids[(n % n_jobs) as usize].push(n);
        }

        let message2 = format!("Data loading using {} jobs", n_jobs);
        terminal::start_msg(command, message2.as_str());

        let start = Instant::now();

        for j in 1..=n_jobs {
            // Cloning values before passing them to the thread
            let job_ids = ids[(j - 1) as usize].clone();
            let dsn = self.dsn.clone();

            // Load the corresponding benchmark client
            let job_benchmark_client = self.get_benchmark(scalefactor, 0, 0);

            // Starting a new job into its dedicated thread
            let job = thread::spawn(move || {
                // New database connection
                let mut job_client = Executor::connect(dsn);

                let _duration_us = match job_benchmark_client.load_data(&mut job_client, job_ids) {
                    Ok(duration) => duration,
                    Err(error) => {
                        terminal::err_msg(format!("{}", error).as_str());
                        std::process::exit(1);
                    }
                };
            });

            jobs.push(job);
        }

        // Wait for the end of all jobs
        for job in jobs {
            job.join().expect("the client thread panicked");
        }
        let duration_ms = start.elapsed().as_micros() as f64 / 1000 as f64;
        terminal::done_msg(duration_ms);

        self
    }

    // Execute database multiple statements (DDLs, admin query, etc..) using n_jobs threads.
    pub fn exec_stmts(&mut self, n_jobs: u32, stmts: Vec<BenchmarkStmt>, use_transaction: bool) {
        // We want to get one row per job and the ids balanced across the rowss.
        let mut rows = Vec::with_capacity(n_jobs as usize);
        for _ in 0..n_jobs {
            rows.push(Vec::new());
        }
        let mut n = 1;
        for stmt in stmts.iter() {
            rows[(n % n_jobs) as usize].push(stmt.sql.clone());
            n += 1;
        }
        let mut jobs = Vec::new();

        for j in 1..=n_jobs {
            // Cloning values before passing them to the thread
            let job_stmts = rows[(j - 1) as usize].clone();
            let dsn = self.dsn.clone();

            // Starting a new job into its dedicated thread
            let job = thread::spawn(move || {
                // New database connection
                let mut client = Executor::connect(dsn);

                for stmt in job_stmts.iter() {
                    if use_transaction {
                        let mut transaction = match client.transaction() {
                            Ok(t) => t,
                            Err(error) => {
                                terminal::err_msg(format!("{}", error).as_str());
                                std::process::exit(1);
                            }
                        };
                        match transaction.batch_execute(stmt) {
                            Ok(_) => (),
                            Err(error) => {
                                terminal::err_msg(format!("{}", error).as_str());
                                std::process::exit(1);
                            }
                        }
                        match transaction.commit() {
                            Ok(_) => (),
                            Err(error) => {
                                terminal::err_msg(format!("{}", error).as_str());
                                std::process::exit(1);
                            }
                        }
                    }
                    // No transaction
                    else {
                        match client.batch_execute(stmt) {
                            Ok(_) => (),
                            Err(error) => {
                                terminal::err_msg(format!("{}", error).as_str());
                                std::process::exit(1);
                            }
                        }
                    }
                }
            });

            jobs.push(job);
        }

        // Wait for the end of all jobs
        for job in jobs {
            job.join().expect("the client thread panicked");
        }
    }

    fn get_benchmark(&mut self, scalefactor: u32, min_id: u32, max_id: u32) -> YamlBenchmark {
        YamlBenchmark::from_file(&self.spec_file, scalefactor, min_id, max_id)
    }

    // Execute primary keys DDLs using multiple concurrent jobs
    pub fn add_primary_keys(&mut self, n_jobs: u32) -> &mut Self {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Primary keys creation");
        self.exec_stmts(n_jobs, benchmark.get_pkey_ddls(), true);
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);

        self
    }

    // Execute foreign keys DDLs using multiple concurrent jobs
    pub fn add_foreign_keys(&mut self, n_jobs: u32, no_fkey: bool) -> &mut Self {
        // Don't do anything if no_fkey is true
        if no_fkey {
            return self;
        }

        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Foreign keys creation");
        self.exec_stmts(n_jobs, benchmark.get_fkey_ddls(), true);
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);

        self
    }

    // Execute additional indexes DDLs using multiple concurrent jobs
    pub fn add_indexes(&mut self, n_jobs: u32) -> &mut Self {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Additional indexes creation");
        self.exec_stmts(n_jobs, benchmark.get_index_ddls(), true);
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);

        self
    }

    // Execute VACUUM statementsusing multiple concurrent jobs
    pub fn vacuum(&mut self, n_jobs: u32) -> &mut Self {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Vacuuming tables");
        self.exec_stmts(n_jobs, benchmark.get_vacuum_stmts(), false);
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);

        self
    }

    // Force a checkpoint
    pub fn checkpoint(&mut self) -> &mut Self {
        let start = Instant::now();

        terminal::start_msg("INIT", "Forcing checkpoint");
        let mut client = Executor::connect(self.dsn.clone());

        match client.batch_execute("CHECKPOINT") {
            Ok(_) => (),
            Err(error) => {
                terminal::err_msg(format!("{}", error).as_str());
                std::process::exit(1);
            }
        }

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);

        self
    }

    // Perform data aggregation based on the log file
    pub fn aggregate_data(&mut self) -> &mut Self {
        let start = Instant::now();

        terminal::start_msg("RUN", "Aggregating data");

        let transactions = self.get_benchmark(0, 0, 0)
            .get_transactions_rw();

        match data_agg::aggregate_tpcc_data(LOG_FILE, &self.target_dir, &transactions) {
            Ok(_) => (),
            Err(error) => {
                terminal::err_msg(format!("{}", error).as_str());
                std::process::exit(1);
            }
        }

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000 as f64);

        self
    }

    pub fn print_results(&mut self) -> &mut Self {
        let duration_ms = Duration::from_millis(self.total_time_ms as u64);
        // Get transactions details
        let transactions = self.get_benchmark(0, 0, 0)
            .get_transactions_rw();
        // Read statistics from the generated local files
        let stats_map = match data_agg::get_stats(&self.target_dir, &transactions) {
            Ok(stats_map) => stats_map,
            Err(error) => {
                eprintln!("ERROR: {}", error);
                std::process::exit(1);
            }
        };

        let mut data_stats: Vec<ResponseTimeStatistics>  = Vec::new();
        let mut data_summary: Vec<TransactionSummary>  = Vec::new();

        for transaction in transactions {
            let default_stats = ResponseTimeStatistics {
                name: transaction.name.clone(),
                mean: 0.0, min: 0.0, max: 0.0, std: 0.0,
                percentile_95: 0.0, percentile_99: 0.0,
            };
            let stats = stats_map.get(&transaction.id).unwrap_or(&default_stats);
            let default_counters = Counter { n_commits: 0, n_total: 0, total_duration_ms: 0.0 };
            let counters = self.counters.get(&transaction.id).unwrap_or(&default_counters);

            data_stats.push(stats.clone());
            data_summary.push(
                TransactionSummary::new(
                    transaction.name,
                    // Number of commits
                    counters.n_commits,
                    // Number of errors
                    counters.n_total - counters.n_commits,
                    // Error rate
                    if counters.n_total > 0 { (counters.n_total - counters.n_commits) as f64 / counters.n_total as f64 * 100.0 } else { 0.0 },
                    // Transactions per minute
                    (counters.n_commits as f64 / duration_ms.as_secs() as f64 * 60.0) as u32,
                    // Transactions per second
                    (counters.n_commits as f64 / duration_ms.as_secs() as f64) as u32,
                )
            );
        }

        println!("");
        // Print summary
        println!("Results:");
        data_agg::print_transactions_summary(&data_summary);
        // Print stats
        println!("Response times:");
        data_agg::print_transactions_stats(&data_stats);

        self
    }

    // Generate a self-contained HTML report into the run's target directory.
    pub fn generate_report(&mut self) -> &mut Self {
        let start = Instant::now();
        terminal::start_msg("RUN", "Generating HTML report");

        let benchmark = self.get_benchmark(0, 0, 0);
        let transactions = benchmark.get_transactions_rw();
        let spec = benchmark.spec.clone();

        let stats = match data_agg::get_stats(&self.target_dir, &transactions) {
            Ok(stats) => stats,
            Err(error) => {
                terminal::err_msg(format!("{}", error).as_str());
                return self;
            }
        };

        let mut client = Executor::connect(self.dsn.clone());

        let input = report::ReportInput {
            target_dir: &self.target_dir,
            log_file: LOG_FILE,
            error_file: ERROR_FILE,
            spec: &spec,
            transactions: &transactions,
            counters: &self.counters,
            stats: &stats,
            total_time_ms: self.total_time_ms,
            rampup_time_ms: self.rampup_time_ms,
            clients: self.n_clients,
            duration_s: self.duration_s,
            rampup_s: self.rampup_s,
        };

        match report::generate(&input, &mut client) {
            Ok(_) => terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0),
            Err(error) => terminal::err_msg(format!("{}", error).as_str()),
        }

        self
    }
}
