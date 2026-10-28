use std::collections::HashMap;
use std::env::current_dir;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use postgres::Client;
use tokio_postgres_rustls::MakeRustlsConnect;

mod benchmark;
mod data_agg;
pub mod error;
mod init;
mod report;
mod run;
mod summary;
mod terminal;
mod tls;
mod txmessage;
mod yaml_benchmark;
mod yaml_lua;
mod yaml_types;

use super::args::{OutputFormat, SslMode};
use benchmark::{Counter, ResponseTimeStatistics};
use error::Error;

use yaml_benchmark::YamlBenchmark;
use yaml_types::ReportKind;

pub struct Executor {
    dsn: String,
    /// TLS connector matching the requested sslmode, cloned into every
    /// connection (RELEASE_REVIEW C4).
    tls: MakeRustlsConnect,
    spec_file: String,
    /// The spec, its scripts and modules, parsed once at construction and
    /// shared by every pipeline step and client thread (RELEASE_REVIEW C3).
    benchmark: Arc<YamlBenchmark>,
    counters: HashMap<u16, Counter>,
    /// Response-time statistics per transaction, computed by `aggregate_data`
    /// and shared by the results and the report (RELEASE_REVIEW D5).
    stats: HashMap<u16, ResponseTimeStatistics>,
    /// The same statistics over all transactions together.
    overall_stats: ResponseTimeStatistics,
    /// Commits per second of the measured window, for the steady-state check.
    window: Vec<(i64, u64)>,
    /// Log timestamp at which ramp-up ended; samples before it are excluded
    /// from the statistics and the histogram (RELEASE_REVIEW B2).
    rampup_end_ts: Option<i64>,
    rampup_time_ms: u128,
    total_time_ms: u128,
    /// Set when Ctrl-C cut the workload short; the partial run is still
    /// aggregated and reported, flagged as interrupted (RELEASE_REVIEW C5).
    interrupted: bool,
    // Run parameters retained for reporting
    n_clients: u16,
    duration_s: u16,
    rampup_s: u16,
    /// `--once`: every client ran each transaction once instead of a timed
    /// random stream; `duration_s` is then the measured window that resulted.
    once: bool,
    /// Headline throughput unit and per-second factor from the spec's
    /// `metrics` block, shared by the live progress line, the per-second
    /// CSVs, run.json and the results.
    throughput_unit: String,
    throughput_factor: f64,
    // Target directory used to store collected and aggregated data
    target_dir: PathBuf,
}

const LOG_FILE: &str = "transaction.log";
const ERROR_FILE: &str = "error.log";
const RUN_FILE: &str = "run.json";

// Mask the password in a libpq URL so the connection target can be echoed in the
// run header without leaking the secret: postgresql://user:pass@host:port/db
// becomes postgresql://user:****@host:port/db.
/// The DSN as printed in the run header: the password is masked in both
/// forms the driver accepts, `scheme://user:password@host...` and the
/// keyword form `host=... password=...`. A DSN without a password is
/// returned unchanged.
fn redact_dsn(dsn: &str) -> String {
    if let Some((scheme, rest)) = dsn.split_once("://") {
        if let Some((cred, hostpart)) = rest.split_once('@') {
            let user = cred.split_once(':').map(|(u, _)| u).unwrap_or(cred);
            return format!("{}://{}:****@{}", scheme, user, hostpart);
        }
        return dsn.to_string();
    }
    dsn.split_whitespace()
        .map(|kv| match kv.split_once('=') {
            Some(("password", _)) => "password=****".to_string(),
            _ => kv.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// Forward the quiet switch to the terminal module (which is private to this
// module) so main can toggle it once at startup.
pub fn set_quiet(quiet: bool) {
    terminal::set_quiet(quiet);
}

pub fn get_target_dir_path() -> Result<PathBuf, Error> {
    let current_dir =
        current_dir().map_err(|e| Error::io("Cannot determine the current directory", e))?;
    let now: DateTime<Utc> = Utc::now();
    // Basic ISO 8601 (no `:`), so the name is valid on every filesystem and
    // needs no quoting in a shell.
    Ok(current_dir.join(format!("pgmeter-{}", now.format("%Y%m%dT%H%M%SZ"))))
}

/// Print an error the way every other failure is printed and exit non-zero.
/// This is the only place the executor's callers should terminate the
/// process from; everything inside the executor returns `Error`.
pub fn exit_with_error(error: &Error) -> ! {
    terminal::err_msg(&error.to_string());
    std::process::exit(1);
}

impl Executor {
    pub fn new(dsn: String, sslmode: SslMode, spec_file: String) -> Result<Executor, Error> {
        let benchmark = Arc::new(YamlBenchmark::from_file(&spec_file, 0, 0, 0)?);
        let tls = tls::connector(sslmode)?;
        Ok(Executor {
            dsn,
            tls,
            spec_file,
            benchmark,
            counters: HashMap::new(),
            stats: HashMap::new(),
            overall_stats: ResponseTimeStatistics::zero("all"),
            window: Vec::new(),
            rampup_end_ts: None,
            interrupted: false,
            total_time_ms: 0,
            rampup_time_ms: 0,
            n_clients: 0,
            duration_s: 0,
            rampup_s: 0,
            once: false,
            throughput_unit: "TPM".to_string(),
            throughput_factor: 60.0,
            target_dir: get_target_dir_path()?,
        })
    }

    // Open a new connection to the database and returns a Client
    fn connect(&self) -> Result<Client, Error> {
        Client::connect(&self.dsn, self.tls.clone()).map_err(Error::from)
    }

    /// The parsed benchmark with the given scale factor and id range applied.
    fn get_benchmark(&self, scalefactor: u32, min_id: u32, max_id: u32) -> YamlBenchmark {
        self.benchmark.with_context(scalefactor, min_id, max_id)
    }

    // Perform data aggregation based on the log file
    pub fn aggregate_data(&mut self) -> Result<&mut Self, Error> {
        let start = Instant::now();

        terminal::start_msg("RUN", "Aggregating data");

        let transactions = self.get_benchmark(0, 0, 0).get_transactions_rw();

        let aggregate = data_agg::aggregate_transaction_data(
            LOG_FILE,
            &self.target_dir,
            &transactions,
            self.rampup_end_ts,
            self.throughput_factor,
        )?;
        self.stats = aggregate.stats;
        self.overall_stats = aggregate.overall;
        self.window = aggregate.window;

        terminal::done_msg(start.elapsed().as_micros() as f64 / 1000_f64);

        Ok(self)
    }

    pub fn print_results(&mut self, format: OutputFormat) -> Result<&mut Self, Error> {
        // Get transactions details and the headline throughput metric from the spec.
        let benchmark = self.get_benchmark(0, 0, 0);
        let transactions = benchmark.get_transactions_rw();
        let throughput_unit = benchmark.spec.metrics.throughput_unit.clone();
        let throughput_factor = benchmark.spec.metrics.per.factor();
        let stats_map = &self.stats;

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
                    .unwrap_or_else(|| ResponseTimeStatistics::zero(&transaction.name))
            })
            .collect();
        let steady = self.steady_state();
        let stream = self.stream_summary(&transactions);

        match format {
            OutputFormat::Table if stream.is_some() => {
                let stream = stream.as_ref().unwrap();
                terminal::stream_banner(stream);
                data_agg::print_stream_table(stream);
            }
            OutputFormat::Table => {
                terminal::kpi_banner(
                    summary.throughput.round() as u64,
                    &throughput_unit,
                    summary.total_commits,
                    summary.error_rate,
                    summary.avg_latency_ms,
                    self.overall_stats.percentile_99,
                );
                terminal::steady_state_line(steady.as_ref());

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
                        self.interrupted,
                        steady.as_ref(),
                        stream.as_ref(),
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

        Ok(self)
    }

    /// The report layout: the spec's `metrics.report`, or a stream for a
    /// `--once` run, which is one by construction.
    fn report_kind(&self) -> ReportKind {
        if self.once {
            ReportKind::Stream
        } else {
            self.benchmark.spec.metrics.report
        }
    }

    /// The stream view of the run when it is reported as a stream.
    fn stream_summary(
        &self,
        transactions: &[benchmark::BenchmarkTransaction],
    ) -> Option<summary::StreamSummary> {
        (self.report_kind() == ReportKind::Stream).then(|| {
            summary::stream_summary(
                transactions,
                &self.counters,
                &self.stats,
                self.total_time_ms,
                self.rampup_time_ms,
            )
        })
    }

    /// The steady-state check of a timed run; `None` for a `--once` run,
    /// whose clients stop one by one, and for a window too short to judge.
    fn steady_state(&self) -> Option<summary::SteadyState> {
        if self.report_kind() == ReportKind::Stream {
            return None;
        }
        summary::steady_state(&self.window, self.throughput_factor)
    }

    // Generate a self-contained HTML report into the run's target directory.
    // A report failure is reported but does not fail the run: the results and
    // the raw logs are already on disk and get printed next.
    pub fn generate_report(&mut self) -> Result<&mut Self, Error> {
        let start = Instant::now();
        terminal::start_msg("RUN", "Generating HTML report");

        let benchmark = self.get_benchmark(0, 0, 0);
        let transactions = benchmark.get_transactions_rw();
        let spec = benchmark.spec.clone();

        let stats = &self.stats;
        let steady = self.steady_state();
        let kind = self.report_kind();
        let stream = self.stream_summary(&transactions);

        let mut client = match self.connect() {
            Ok(client) => client,
            Err(error) => {
                terminal::err_msg(&error.to_string());
                return Ok(self);
            }
        };

        let input = report::ReportInput {
            target_dir: &self.target_dir,
            log_file: LOG_FILE,
            error_file: ERROR_FILE,
            spec: &spec,
            transactions: &transactions,
            counters: &self.counters,
            stats,
            overall: &self.overall_stats,
            steady: steady.as_ref(),
            kind,
            stream: stream.as_ref(),
            total_time_ms: self.total_time_ms,
            rampup_time_ms: self.rampup_time_ms,
            rampup_end_ts: self.rampup_end_ts,
            interrupted: self.interrupted,
            clients: self.n_clients,
            duration_s: self.duration_s,
            rampup_s: self.rampup_s,
        };

        match report::generate(&input, &mut client) {
            Ok(report_path) => {
                terminal::done_msg(start.elapsed().as_micros() as f64 / 1000.0);
                terminal::artifact_path("report:", &report_path.display().to_string());
            }
            Err(error) => terminal::err_msg(&error.to_string()),
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{redact_dsn, Executor};

    // RELEASE_REVIEW F2: the run header prints the target DSN; the password
    // must never reach the terminal, in either DSN syntax the driver accepts.
    #[test]
    fn redact_dsn_masks_the_password_only() {
        assert_eq!(
            redact_dsn("postgresql://alice:s3cr:et@db.example:5432/app?sslmode=require"),
            "postgresql://alice:****@db.example:5432/app?sslmode=require"
        );
        // A user without a password still gets the mask: the header cannot
        // tell whether one was given, and nothing is lost by hiding it.
        assert_eq!(
            redact_dsn("postgres://alice@localhost/app"),
            "postgres://alice:****@localhost/app"
        );
        // No credentials at all: untouched.
        assert_eq!(
            redact_dsn("postgresql://localhost:5432/app"),
            "postgresql://localhost:5432/app"
        );
        // Keyword form: only the password value is replaced.
        assert_eq!(
            redact_dsn("host=localhost user=alice password=s3cret dbname=app"),
            "host=localhost user=alice password=**** dbname=app"
        );
        assert_eq!(
            redact_dsn("host=localhost dbname=app"),
            "host=localhost dbname=app"
        );
    }

    // Regression for RELEASE_REVIEW C3: the spec is read and parsed once, at
    // construction. Before, every pipeline step and every client thread
    // re-read the YAML and the Lua files, so removing the file here would
    // have failed the first `get_benchmark`.
    #[test]
    fn spec_is_parsed_once_at_construction() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let spec_path = dir.join("spec.yaml");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/samples/custom-simple/spec.yaml"
            ),
            &spec_path,
        )
        .unwrap();

        let executor = Executor::new(
            "postgresql://u@localhost:5432/db".to_string(),
            crate::args::SslMode::Disable,
            spec_path.to_string_lossy().into_owned(),
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let benchmark = executor.get_benchmark(3, 1, 7);
        assert_eq!(
            (benchmark.scalefactor, benchmark.min_id, benchmark.max_id),
            (3, 1, 7)
        );
        assert_eq!(benchmark.spec.name, "my-app-benchmark");
        assert_eq!(
            executor.get_benchmark(0, 0, 0).get_transactions_rw().len(),
            3
        );
    }
}
