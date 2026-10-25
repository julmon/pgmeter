use std::collections::{BTreeMap, HashMap};
use std::env::current_dir;
use std::fs::{create_dir_all, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossbeam_channel::{unbounded, Receiver, Sender};
use postgres::{Client, NoTls};
use rand::prelude::*;

mod benchmark;
mod data_agg;
mod report;
mod summary;
mod terminal;
mod txmessage;
mod yaml_benchmark;
mod yaml_lua;
mod yaml_types;

use super::args::{OutputFormat, RunArgs};
use benchmark::{Benchmark, BenchmarkStmt, Counter, ReadWriteSync, ResponseTimeStatistics};
use txmessage::{TXMessage, TXMessageKind};

/// What the data collector hands back when it finishes: the post-ramp-up
/// per-transaction counters and the log timestamp (unix seconds, same clock
/// as `transaction.log`) at which ramp-up ended.
struct CollectorResult {
    counters: HashMap<u16, Counter>,
    rampup_end_ts: Option<i64>,
}
use yaml_benchmark::YamlBenchmark;

pub struct Executor {
    dsn: String,
    spec_file: String,
    counters: HashMap<u16, Counter>,
    /// Log timestamp at which ramp-up ended; samples before it are excluded
    /// from the statistics and the histogram (RELEASE_REVIEW B2).
    rampup_end_ts: Option<i64>,
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
const RUN_FILE: &str = "run.json";

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

// postgres::Error's Display prints only a terse category such as "db error";
// the actual server message (e.g. ERROR: relation "warehouse" already exists)
// lives in its source chain. Build a full message by appending each underlying
// cause so failures are actionable instead of opaque.
fn error_detail(error: &dyn std::error::Error) -> String {
    let mut msg = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !msg.contains(&text) {
            msg.push_str(": ");
            msg.push_str(&text);
        }
        source = cause.source();
    }
    msg
}

// Mask the password in a libpq URL so the connection target can be echoed in the
// run header without leaking the secret: postgresql://user:pass@host:port/db
// becomes postgresql://user:****@host:port/db.
fn redact_dsn(dsn: &str) -> String {
    if let Some((scheme, rest)) = dsn.split_once("://") {
        if let Some((cred, hostpart)) = rest.split_once('@') {
            let user = cred.split_once(':').map(|(u, _)| u).unwrap_or(cred);
            return format!("{}://{}:****@{}", scheme, user, hostpart);
        }
    }
    dsn.to_string()
}

// Forward the quiet switch to the terminal module (which is private to this
// module) so main can toggle it once at startup.
pub fn set_quiet(quiet: bool) {
    terminal::set_quiet(quiet);
}

pub fn get_target_dir_path() -> PathBuf {
    let current_dir = match current_dir() {
        Ok(current_dir) => current_dir,
        Err(error) => {
            terminal::err_msg(&error.to_string());
            std::process::exit(1);
        }
    };
    let now: DateTime<Utc> = Utc::now();
    // Basic ISO 8601 (no `:`), so the name is valid on every filesystem and
    // needs no quoting in a shell.
    let target_dir_path = current_dir.join(format!("pgmeter-{}", now.format("%Y%m%dT%H%M%SZ")));

    target_dir_path
}

impl Executor {
    pub fn new(dsn: String, spec_file: String) -> Executor {
        Executor {
            dsn,
            spec_file,
            counters: HashMap::new(),
            rampup_end_ts: None,
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
        let sleep_ms = if args.client > 0 {
            rampup_ms / args.client as u64
        } else {
            0
        };

        // Echo the run configuration up front so the output is self-documenting
        // and the run is reproducible from its own log.
        self.print_run_header(&args);

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
        match create_dir_all(&self.target_dir) {
            Ok(_) => (),
            Err(error) => {
                terminal::err_msg(error_detail(&error).as_str());
                std::process::exit(1);
            }
        };
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        // Start data collector
        let dc_tx_counters = tx_counters.clone();
        let data_collector = self.start_data_collector(
            rx,
            dc_tx_counters,
            Arc::clone(&live_commits),
            Arc::clone(&live_latency_us),
        );

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
                    terminal::err_msg(error_detail(error.as_ref()).as_str());
                    std::process::exit(1);
                }
            };
            terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);
            max_id
        };

        let message = format!(
            "Starting {} client(s) in {} seconds",
            args.client, args.rampup
        );
        terminal::start_msg(command, message.as_str());

        // One std::thread per client, each on its own blocking connection
        // (matches pgbench's load-generation model).
        self.run_benchmark_sync(
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
        );

        // Proceed total execution time
        self.total_time_ms = start.elapsed().as_millis();

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

        self.write_run_metadata();

        self
    }

    // Write `run.json` next to the logs: the run parameters and the ramp-up
    // boundary, so the raw `transaction.log` can be re-analysed with the same
    // steady-state window the statistics use.
    fn write_run_metadata(&self) {
        let doc = serde_json::json!({
            "clients": self.n_clients,
            "duration_s": self.duration_s,
            "rampup_s": self.rampup_s,
            "total_time_ms": self.total_time_ms,
            "rampup_time_ms": self.rampup_time_ms,
            "workload_secs": summary::workload_secs(self.total_time_ms, self.rampup_time_ms),
            "rampup_end_ts": self.rampup_end_ts,
        });
        let path = self.target_dir.join(RUN_FILE);
        if let Err(error) = std::fs::write(&path, format!("{:#}\n", doc)) {
            terminal::err_msg(&format!("cannot write {}: {}", path.display(), error));
        }
    }

    // Echo the run configuration to stderr so the output is self-documenting and
    // the run can be reproduced from its own log. Kept on stderr so stdout still
    // carries only the results (tables or --format json/csv).
    fn print_run_header(&mut self, args: &RunArgs) {
        if terminal::is_quiet() {
            return;
        }
        let spec_name = self.get_benchmark(0, 0, 0).spec.name.clone();
        let started: DateTime<Utc> = Utc::now();
        eprintln!();
        eprintln!("pgmeter {} - {}", env!("CARGO_PKG_VERSION"), spec_name);
        eprintln!("  spec:     {}", self.spec_file);
        eprintln!("  target:   {}", redact_dsn(&self.dsn));
        eprintln!("  clients:  {}", args.client);
        eprintln!("  duration: {} s", args.time);
        eprintln!("  rampup:   {} s", args.rampup);
        if args.progress > 0 {
            eprintln!("  progress: every {} s", args.progress);
        }
        eprintln!("  started:  {}", started.format("%Y-%m-%dT%H:%M:%SZ"));
        eprintln!();
    }

    /// Benchmark execution using one std::thread per client, each on its own
    /// blocking connection (matches pgbench's load-generation model).
    // TODO(RELEASE_REVIEW D3/D11): bundle the run parameters into a struct.
    #[allow(clippy::too_many_arguments)]
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
        live_commits: Arc<AtomicU64>,
        live_latency_us: Arc<AtomicU64>,
        progress: u16,
    ) {
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
                    terminal::err_msg(&format!("Connection failed: {}", error_detail(&err)));
                    std::process::exit(1);
                });

                // Get benchmark and prepare statements
                let benchmark = YamlBenchmark::from_file(&spec_file, 0, min_id, max_id);
                let prepared = benchmark
                    .prepare_statements(&mut client)
                    .unwrap_or_else(|err| {
                        terminal::err_msg(&format!(
                            "Failed to prepare statements: {}",
                            error_detail(&err)
                        ));
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
                        cumulative_weights
                            .iter()
                            .position(|&w| rand_val <= w)
                            .unwrap_or(0)
                    };
                    let transaction = &transactions[tx_idx];

                    // Execute transaction using prepared statements
                    match benchmark.execute_rw_transaction_sync(&mut client, &prepared, transaction)
                    {
                        Ok(duration) => {
                            let m =
                                TXMessage::committed(transaction.id, client_id as u32, duration);
                            let _ = tx_clone.send(m);
                        }
                        Err(error) => {
                            let m = TXMessage::error(
                                transaction.id,
                                client_id as u32,
                                error_detail(error.as_ref()),
                            );
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

        let start2 = Instant::now();

        // Drive a determinate bar (TTY) or periodic progress lines (non-TTY) over
        // the post-rampup workload window, fed by the live counters.
        let monitor_stop = Arc::new(AtomicBool::new(false));
        let (bar, monitor) = terminal::spawn_workload_monitor(
            time_ms / 1000,
            start2,
            live_commits,
            live_latency_us,
            Arc::clone(&monitor_stop),
            progress,
        );

        // Send end-of-rampup message
        tx.send(TXMessage::end_of_rampup()).unwrap();
        self.rampup_time_ms = start.elapsed().as_millis();

        // Wait for all client threads
        for handle in client_threads {
            handle.join().expect("Client thread panicked");
        }

        monitor_stop.store(true, Ordering::Relaxed);
        monitor
            .join()
            .expect("the workload monitor thread panicked");
        terminal::finish_run_bar(bar, start2.elapsed().as_micros() as f64 / 1000.0);
    }

    // Start the data collector thread. Data collector is in charge of storing transaction
    // informations into the log file and incrementing counters.
    // Once the data collector has received the shutdown order (message with id=0), then
    // the counters are sent back to the main process through the tx_counters channel.
    fn start_data_collector(
        &mut self,
        rx: Receiver<TXMessage>,
        tx_counters: Sender<CollectorResult>,
        live_commits: Arc<AtomicU64>,
        live_latency_us: Arc<AtomicU64>,
    ) -> JoinHandle<()> {
        let target_dir = self.target_dir.clone();
        thread::spawn(move || {
            // Create the file where transaction logs are written
            let log_file = match File::create(target_dir.join(LOG_FILE)) {
                Ok(f) => f,
                Err(e) => {
                    terminal::err_msg(&format!("Could not create {}: {}", LOG_FILE, e));
                    std::process::exit(1);
                }
            };
            // Use larger buffer for better I/O performance
            let mut log_file = BufWriter::with_capacity(64 * 1024, log_file);
            // Create the error log file
            let error_file = match File::create(target_dir.join(ERROR_FILE)) {
                Ok(f) => f,
                Err(e) => {
                    terminal::err_msg(&format!("Could not create {}: {}", ERROR_FILE, e));
                    std::process::exit(1);
                }
            };
            let mut error_file = BufWriter::with_capacity(8 * 1024, error_file);

            // Initialize the counters with expected capacity
            let mut counters: HashMap<u16, Counter> = HashMap::with_capacity(16);
            let mut client_ids = BTreeMap::new();

            let mut ramping_up: bool = true;
            let mut rampup_end_ts: Option<i64> = None;
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
                    TXMessageKind::Terminate => {
                        break;
                    }
                    // Committed transaction
                    TXMessageKind::Committed => {
                        // Keep a track of the client_ids and increment n_client if this is a new client_id
                        if let std::collections::btree_map::Entry::Vacant(e) =
                            client_ids.entry(msg.client_id)
                        {
                            e.insert(true);
                            n_client += 1;
                        }

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
                    }
                    TXMessageKind::Error => {
                        // Keep a track of the client_ids and increment n_client if this is a new client_id
                        if let std::collections::btree_map::Entry::Vacant(e) =
                            client_ids.entry(msg.client_id)
                        {
                            e.insert(true);
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
                        let error_str = escape_log_message(msg.error.as_deref().unwrap_or(""));

                        // Format and write the line to the error log file
                        let _ = error_file.write(buffer_i.format(tx_timestamp).as_bytes());
                        let _ = error_file.write(b" ");
                        let _ = error_file.write(buffer_i.format(n_client).as_bytes());
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
                    TXMessageKind::Default => {
                        // Should not happen
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
        })
    }

    // Open a new connection to the database and returns a Client
    fn connect(dsn: String) -> Client {
        Client::connect(&dsn, NoTls).unwrap_or_else(|err| {
            terminal::err_msg(error_detail(&err).as_str());
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
                terminal::err_msg(error_detail(&error).as_str());
                std::process::exit(1);
            }
        };
        let duration_ms = duration_us as f64 / 1000_f64;

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
                terminal::err_msg(error_detail(&error).as_str());
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
                terminal::err_msg(error.as_str());
                std::process::exit(1);
            }
        };
        let duration_ms = duration_us as f64 / 1000_f64;

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
                        terminal::err_msg(error.as_str());
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
        let duration_ms = start.elapsed().as_micros() as f64 / 1000_f64;
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
        for (n, stmt) in stmts.iter().enumerate() {
            rows[(n + 1) % n_jobs as usize].push(stmt.sql.clone());
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
                                terminal::err_msg(error_detail(&error).as_str());
                                std::process::exit(1);
                            }
                        };
                        match transaction.batch_execute(stmt) {
                            Ok(_) => (),
                            Err(error) => {
                                terminal::err_msg(error_detail(&error).as_str());
                                std::process::exit(1);
                            }
                        }
                        match transaction.commit() {
                            Ok(_) => (),
                            Err(error) => {
                                terminal::err_msg(error_detail(&error).as_str());
                                std::process::exit(1);
                            }
                        }
                    }
                    // No transaction
                    else {
                        match client.batch_execute(stmt) {
                            Ok(_) => (),
                            Err(error) => {
                                terminal::err_msg(error_detail(&error).as_str());
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
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        self
    }

    // Execute foreign keys DDLs serially.
    //
    // FK creation is intentionally not parallelized: ALTER TABLE ... ADD FOREIGN KEY
    // takes a ShareRowExclusiveLock on both the referencing and the referenced table,
    // so concurrent adds touching a shared parent serialize on its lock anyway and can
    // deadlock when two FKs reference each other's tables in opposite order across jobs.
    pub fn add_foreign_keys(&mut self, _n_jobs: u32, no_fkey: bool) -> &mut Self {
        // Don't do anything if no_fkey is true
        if no_fkey {
            return self;
        }

        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Foreign keys creation");
        self.exec_stmts(1, benchmark.get_fkey_ddls(), true);
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        self
    }

    // Execute additional indexes DDLs using multiple concurrent jobs
    pub fn add_indexes(&mut self, n_jobs: u32) -> &mut Self {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Additional indexes creation");
        self.exec_stmts(n_jobs, benchmark.get_index_ddls(), true);
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        self
    }

    // Execute VACUUM statementsusing multiple concurrent jobs
    pub fn vacuum(&mut self, n_jobs: u32) -> &mut Self {
        // Load the corresponding benchmark
        let benchmark = self.get_benchmark(0, 0, 0);
        let start = Instant::now();

        terminal::start_msg("INIT", "Vacuuming tables");
        self.exec_stmts(n_jobs, benchmark.get_vacuum_stmts(), false);
        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

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
                terminal::err_msg(error_detail(&error).as_str());
                std::process::exit(1);
            }
        }

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        self
    }

    // Perform data aggregation based on the log file
    pub fn aggregate_data(&mut self) -> &mut Self {
        let start = Instant::now();

        terminal::start_msg("RUN", "Aggregating data");

        let transactions = self.get_benchmark(0, 0, 0).get_transactions_rw();

        match data_agg::aggregate_transaction_data(
            LOG_FILE,
            &self.target_dir,
            &transactions,
            self.rampup_end_ts,
        ) {
            Ok(_) => (),
            Err(error) => {
                terminal::err_msg(error_detail(error.as_ref()).as_str());
                std::process::exit(1);
            }
        }

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        self
    }

    pub fn print_results(&mut self, format: OutputFormat) -> &mut Self {
        // Get transactions details and the headline throughput metric from the spec.
        let benchmark = self.get_benchmark(0, 0, 0);
        let transactions = benchmark.get_transactions_rw();
        let throughput_unit = benchmark.spec.metrics.throughput_unit.clone();
        let throughput_factor = benchmark.spec.metrics.per.factor();
        // Read statistics from the generated local files
        let stats_map = match data_agg::get_stats(&self.target_dir, &transactions) {
            Ok(stats_map) => stats_map,
            Err(error) => {
                terminal::err_msg(error_detail(error.as_ref()).as_str());
                std::process::exit(1);
            }
        };

        // Same computation as the HTML report, so every output agrees.
        let summary = summary::summarize(
            &transactions,
            &self.counters,
            self.total_time_ms,
            self.rampup_time_ms,
            throughput_factor,
        );

        let data_stats: Vec<ResponseTimeStatistics> = transactions
            .iter()
            .map(|transaction| {
                stats_map
                    .get(&transaction.id)
                    .cloned()
                    .unwrap_or_else(|| ResponseTimeStatistics {
                        name: transaction.name.clone(),
                        mean: 0.0,
                        min: 0.0,
                        max: 0.0,
                        std: 0.0,
                        percentile_95: 0.0,
                        percentile_99: 0.0,
                    })
            })
            .collect();

        match format {
            OutputFormat::Table => {
                terminal::kpi_banner(
                    summary.throughput.round() as u64,
                    &throughput_unit,
                    summary.total_commits,
                    summary.error_rate,
                    summary.avg_latency_ms,
                );

                data_agg::print_transactions_tables(
                    &summary.transactions,
                    &data_stats,
                    &throughput_unit,
                );
            }
            OutputFormat::Json => {
                print!(
                    "{}",
                    data_agg::results_json(
                        &summary.transactions,
                        &data_stats,
                        &throughput_unit,
                        summary.workload_secs,
                    )
                );
            }
            OutputFormat::Csv => {
                print!(
                    "{}",
                    data_agg::results_csv(&summary.transactions, &data_stats, &throughput_unit)
                );
            }
        }

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
                terminal::err_msg(error_detail(error.as_ref()).as_str());
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
            rampup_end_ts: self.rampup_end_ts,
            clients: self.n_clients,
            duration_s: self.duration_s,
            rampup_s: self.rampup_s,
        };

        match report::generate(&input, &mut client) {
            Ok(report_path) => {
                terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);
                terminal::artifact_path("report:", &report_path.display().to_string());
            }
            Err(error) => terminal::err_msg(error_detail(error.as_ref()).as_str()),
        }

        self
    }
}

#[cfg(test)]
mod tests {
    use super::escape_log_message;

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
