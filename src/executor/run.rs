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
use postgres::Client;
use rand::prelude::*;

use super::benchmark::Counter;
use super::error::{error_detail, Error};
use super::txmessage::{TXMessage, TXMessageKind};
use super::{redact_dsn, summary, terminal, Executor, ERROR_FILE, LOG_FILE, RUN_FILE};
use crate::args::RunArgs;

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
        self.print_run_header(&args)?;

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

        // One std::thread per client, each on its own blocking connection
        // (matches pgbench's load-generation model). A client that cannot
        // start is reported after the collector has flushed the logs below.
        let workload = self.run_benchmark_sync(
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
        );

        // Proceed total execution time
        self.total_time_ms = start.elapsed().as_millis();
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
    fn print_run_header(&mut self, args: &RunArgs) -> Result<(), Error> {
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
        eprintln!("  duration: {} s", args.time);
        eprintln!("  rampup:   {} s", args.rampup);
        if args.progress > 0 {
            eprintln!("  progress: every {} s", args.progress);
        }
        eprintln!("  started:  {}", started.format("%Y-%m-%dT%H:%M:%SZ"));
        eprintln!();
        Ok(())
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
                        let rand_val: u32 = rand::rng().random_range(1..=total_weight);
                        cumulative_weights
                            .iter()
                            .position(|&w| rand_val <= w)
                            .unwrap_or(0)
                    };
                    let transaction = &transactions[tx_idx];

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

                    if loop_start.elapsed().as_millis() >= duration_limit
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
            terminal::RateUnit {
                unit: self.throughput_unit.clone(),
                factor: self.throughput_factor,
            },
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

        monitor_stop.store(true, Ordering::Relaxed);
        monitor
            .join()
            .expect("the workload monitor thread panicked");
        terminal::finish_run_bar(bar, start2.elapsed().as_micros() as f64 / 1000.0);

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
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

                        // Generate timestamp locally (more efficient than passing through channel)
                        let tx_timestamp = start_time + start_instant.elapsed().as_secs() as i64;

                        // Format and write the line to the log file
                        let _ = log_file.write(buffer_i.format(tx_timestamp).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(client_ids.len()).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_i.format(msg.tx_id).as_bytes());
                        let _ = log_file.write(b" ");
                        let _ = log_file.write(buffer_f.format(duration_ms).as_bytes());
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
