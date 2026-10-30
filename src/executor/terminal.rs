use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant};

use chrono::Local;
use console::{style, Term};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use super::benchmark::BenchmarkTransaction;

// A phase that is currently in progress. On a TTY it owns an animated spinner;
// otherwise it just remembers its label so the closing line can be printed.
enum Step {
    Spinner { bar: ProgressBar, label: String },
    Plain { label: String },
}

thread_local! {
    static CURRENT: RefCell<Option<Step>> = const { RefCell::new(None) };
}

// Global quiet switch. When set, all decorative phase output (spinners,
// checkmarks, run bar, progress lines, KPI banner) is suppressed so only the
// result data reaches the terminal. Error reporting via err_msg is unaffected.
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

// True when stderr is an interactive terminal. Spinners and progress bars are
// only drawn in that case; piped/redirected output gets plain log lines so the
// captured logs stay clean.
pub fn is_tty() -> bool {
    Term::stderr().is_term()
}

// Human-friendly duration: sub-second stays in milliseconds, longer in seconds.
fn fmt_duration_ms(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{:.0} ms", ms)
    } else {
        format!("{:.1} s", ms / 1000.0)
    }
}

// Group a non-negative integer with thin spaces (e.g. 1234567 -> "1 234 567").
pub fn fmt_int(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let first = bytes.len() % 3;
    for (i, b) in bytes.iter().enumerate() {
        if i >= first && i != 0 && (i - first).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(*b as char);
    }
    out
}

// Begin a new phase. On a TTY this starts a spinner that animates until the
// matching done_msg/err_msg call; otherwise the closing call prints a single
// status line.
pub fn start_msg(command: &str, message: &str) {
    if is_quiet() {
        return;
    }
    let label = format!("{} {}", style(command).dim(), message);
    let step = if is_tty() {
        let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        bar.set_message(label.clone());
        bar.enable_steady_tick(Duration::from_millis(80));
        Step::Spinner { bar, label }
    } else {
        Step::Plain { label }
    };
    CURRENT.with(|c| *c.borrow_mut() = Some(step));
}

// Close the current phase as successful, showing its elapsed time.
pub fn done_msg(duration_ms: f64) {
    CURRENT.with(|c| {
        if let Some(step) = c.borrow_mut().take() {
            let label = match step {
                Step::Spinner { bar, label } => {
                    bar.finish_and_clear();
                    label
                }
                Step::Plain { label } => label,
            };
            eprintln!(
                "{} {}  {}",
                style("✔").green().bold(),
                label,
                style(fmt_duration_ms(duration_ms)).dim(),
            );
        }
    });
}

// Close the current phase as failed (if any) and print the error detail. May be
// called from a worker thread that never opened a phase, in which case only the
// error line is printed.
pub fn err_msg(error: &str) {
    CURRENT.with(|c| {
        if let Some(step) = c.borrow_mut().take() {
            let label = match step {
                Step::Spinner { bar, label } => {
                    bar.finish_and_clear();
                    label
                }
                Step::Plain { label } => label,
            };
            eprintln!("{} {}", style("✗").red().bold(), label);
        }
    });
    // Mirror clap's error style ("error: <message>" with a bold-red label) so
    // argument-parse failures and runtime failures look identical.
    eprintln!("{} {}", style("error:").red().bold(), error);
}

// Determinate bar tracking the workload's elapsed/total seconds of a timed
// run. The message suffix (live throughput/latency) is set by the caller.
// Hidden on a non-TTY so callers can fall back to periodic log lines.
pub fn run_bar(total_s: u64) -> ProgressBar {
    if !is_tty() || is_quiet() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::with_draw_target(Some(total_s), ProgressDrawTarget::stderr());
    // Two leading spaces shift the line right so "RUN" aligns with the icon-
    // prefixed status lines (✔/spinner + space). "Running workload" is rendered
    // in white while the "RUN" prefix stays dim.
    let template = format!(
        "  {{prefix:.dim}} {} {{bar:24.cyan/blue}} {{percent:>3}}% {{pos:>3}}/{{len}}s  {{msg}}",
        style("Running workload").white(),
    );
    bar.set_style(
        ProgressStyle::with_template(&template)
            .unwrap()
            .progress_chars("█▉░"),
    );
    bar.set_prefix("RUN");
    bar
}

/// The live view of a `--once` run. A stream run has a known length (every
/// client runs each eligible transaction once) and long, uneven transactions,
/// so its progress is shown as K/N done plus what each client is running,
/// and every completion is printed as it happens; a timed run shows a rate.
/// Updated by the clients, read by the workload monitor.
pub struct StreamProgress {
    names: HashMap<u16, String>,
    /// Per client (index client_id - 1): the transaction in flight as its id
    /// plus one (0 when idle) and when it started, in ms since `start`.
    slots: Vec<(AtomicU32, AtomicU64)>,
    done: AtomicU64,
    total: u64,
    start: Instant,
    /// The bar the completion lines are printed above. The monitor draws it
    /// once the clients are started; before that, and on a non-TTY, it is
    /// hidden and the lines go straight to stderr.
    bar: ProgressBar,
}

impl StreamProgress {
    pub fn new(transactions: &[BenchmarkTransaction], clients: u16) -> Self {
        let eligible = transactions.iter().filter(|t| t.weight > 0).count() as u64;
        let total = eligible * clients as u64;
        let bar = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::hidden());
        let template = format!(
            "  {{prefix:.dim}} {} {{bar:24.cyan/blue}} {{pos:>3}}/{{len}}  {{elapsed_precise}}  {{msg}}",
            style("Running workload").white(),
        );
        bar.set_style(
            ProgressStyle::with_template(&template)
                .expect("valid template")
                .progress_chars("█▉░"),
        );
        bar.set_prefix("RUN");
        Self {
            names: transactions
                .iter()
                .map(|t| (t.id, t.name.clone()))
                .collect(),
            slots: (0..clients)
                .map(|_| (AtomicU32::new(0), AtomicU64::new(0)))
                .collect(),
            done: AtomicU64::new(0),
            total,
            start: Instant::now(),
            bar,
        }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    fn slot(&self, client_id: u16) -> Option<&(AtomicU32, AtomicU64)> {
        self.slots.get((client_id as usize).wrapping_sub(1))
    }

    /// A client starts a transaction.
    pub fn begin(&self, client_id: u16, tx_id: u16) {
        if let Some((tx, since)) = self.slot(client_id) {
            since.store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
            tx.store(tx_id as u32 + 1, Ordering::Relaxed);
        }
    }

    /// A client finished a transaction: count it and print its line,
    /// `HH:MM:SS  name  duration  client N` (or the error).
    pub fn finished(&self, client_id: u16, tx_id: u16, outcome: Result<u128, &str>) {
        if let Some((tx, _)) = self.slot(client_id) {
            tx.store(0, Ordering::Relaxed);
        }
        self.done.fetch_add(1, Ordering::Relaxed);
        if is_quiet() {
            return;
        }
        let name = self.names.get(&tx_id).map_or("?", String::as_str);
        let when = Local::now().format("%H:%M:%S");
        let line = match outcome {
            Ok(duration_us) => format!(
                "  {}  {:<32} {:>10}  client {}",
                style(when).dim(),
                name,
                fmt_response_ms(duration_us as f64 / 1000.0),
                client_id
            ),
            Err(message) => format!(
                "  {}  {:<32} {:>10}  client {}: {}",
                style(when).dim(),
                name,
                style("ERROR").red(),
                client_id,
                message.replace('\n', " ")
            ),
        };
        if self.bar.is_hidden() {
            eprintln!("{line}");
        } else {
            self.bar.println(line);
        }
    }

    /// The transactions in flight, the longest-running first, at most three
    /// named: "running Q16 for 42 s" for one client, "running Q16 (c1, 42 s),
    /// Q4 (c2, 3 s), +2" for several; empty when every client is idle.
    pub fn running(&self) -> String {
        let now_ms = self.start.elapsed().as_millis() as u64;
        let mut live: Vec<(u64, usize, u16)> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, (tx, since))| {
                let tx = tx.load(Ordering::Relaxed);
                (tx > 0).then(|| {
                    let ms = now_ms.saturating_sub(since.load(Ordering::Relaxed));
                    (ms, i + 1, (tx - 1) as u16)
                })
            })
            .collect();
        if live.is_empty() {
            return String::new();
        }
        live.sort_by_key(|&(ms, _, _)| std::cmp::Reverse(ms));
        let named: Vec<String> = live
            .iter()
            .take(3)
            .map(|(ms, client, tx)| {
                let name = self.names.get(tx).map_or("?", String::as_str);
                if self.slots.len() == 1 {
                    format!("{name} for {}", fmt_elapsed(*ms))
                } else {
                    format!("{name} (c{client}, {})", fmt_elapsed(*ms))
                }
            })
            .collect();
        let mut out = format!("running {}", named.join(", "));
        if live.len() > 3 {
            out.push_str(&format!(", +{}", live.len() - 3));
        }
        out
    }
}

// An elapsed time for the in-flight list: "42 s", "12m 03s", "1h 02m".
fn fmt_elapsed(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, s % 3600 / 60)
    }
}

/// What the workload monitor follows: the window of a timed run, in
/// seconds, or the stream progress of a `--once` run.
pub enum Window {
    Timed(u64),
    Stream(Arc<StreamProgress>),
}

// Create the workload bar and a background ticker that drives it. On a TTY the
// ticker advances the determinate bar and refreshes the suffix: the live
// throughput/latency over a ~1s rolling window of a timed run, the in-flight
// transactions of a stream run (whose bar counts transactions, not seconds).
// On a non-TTY the bar is hidden, and instead the ticker emits a
// pgbench-style `progress:` line every `progress_s` seconds (suppressed
// entirely when progress_s is 0). The caller flips `stop` once the clients are
// done, then joins the handle and calls finish_run_bar on the returned bar.
/// How the live rate is displayed: the spec's throughput unit label and the
/// per-second factor that turns commits/s into it (60 for a per-minute unit).
pub struct RateUnit {
    pub unit: String,
    pub factor: f64,
}

pub fn spawn_workload_monitor(
    window: Window,
    start: Instant,
    live_commits: Arc<AtomicU64>,
    live_latency_us: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    progress_s: u16,
    rate: RateUnit,
) -> (ProgressBar, JoinHandle<()>) {
    let tty = is_tty();
    let quiet = is_quiet();
    let (bar, stream) = match window {
        Window::Timed(total_s) => (run_bar(total_s), None),
        Window::Stream(stream) => {
            let bar = stream.bar.clone();
            if tty && !quiet {
                bar.set_draw_target(ProgressDrawTarget::stderr());
            }
            (bar, Some(stream))
        }
    };
    let pb = bar.clone();
    let handle = thread::spawn(move || {
        // Rolling window of (elapsed_s, commits, latency_us) samples spanning ~1s,
        // used to derive an instantaneous rate that isn't dominated by startup.
        let mut window: VecDeque<(f64, u64, u64)> = VecDeque::new();
        let mut next_progress = progress_s as f64;

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            let elapsed = start.elapsed().as_secs_f64();
            if let Some(stream) = &stream {
                if tty && !quiet {
                    pb.set_position(stream.done());
                    pb.set_message(stream.running());
                } else if !quiet && progress_s > 0 && elapsed >= next_progress {
                    eprintln!(
                        "progress: {:.1} s, {}/{} done, {}",
                        elapsed,
                        stream.done(),
                        stream.total(),
                        stream.running()
                    );
                    next_progress += progress_s as f64;
                }
                sleep(Duration::from_millis(120));
                continue;
            }
            let commits = live_commits.load(Ordering::Relaxed);
            let latency_us = live_latency_us.load(Ordering::Relaxed);
            window.push_back((elapsed, commits, latency_us));
            // Keep at least two samples so a delta is always available.
            while window.len() > 2 && elapsed - window.front().unwrap().0 > 1.0 {
                window.pop_front();
            }
            let (t0, c0, l0) = *window.front().unwrap();
            let dt = (elapsed - t0).max(1e-6);
            let dc = commits - c0;
            // Instantaneous rate in the spec's unit (commits/s x time-base factor).
            let rate_now = dc as f64 / dt * rate.factor;
            let lat_ms = if dc > 0 {
                (latency_us - l0) as f64 / dc as f64 / 1000.0
            } else {
                0.0
            };

            if tty {
                pb.set_position(elapsed as u64);
                pb.set_message(format!(
                    "{} {} · lat {:.2} ms",
                    fmt_int(rate_now.round() as u64),
                    rate.unit,
                    lat_ms
                ));
            } else if !quiet && progress_s > 0 && elapsed >= next_progress {
                eprintln!(
                    "progress: {:.1} s, {:.1} {}, lat {:.3} ms",
                    elapsed, rate_now, rate.unit, lat_ms
                );
                next_progress += progress_s as f64;
            }

            sleep(Duration::from_millis(120));
        }
    });
    (bar, handle)
}

// One-line headline summary printed (to stdout) above the results tables so the
// eye lands on the figures that matter. The throughput KPI carries the single
// accent colour; the remaining figures are emphasized with bold only. Counts use
// thin-space thousands grouping. Goes to stdout so it travels with the tables and
// is auto-decolourized when the output is piped.
pub fn kpi_banner(
    throughput: u64,
    unit: &str,
    committed: u64,
    error_rate: f64,
    avg_latency_ms: f64,
    p99_latency_ms: f64,
) {
    if is_quiet() {
        return;
    }
    let dot = || style("  ·  ").dim();
    println!();
    println!(
        "  {} {}{}{} committed{}{} errors{}{} avg{}{} p99",
        style(fmt_int(throughput)).cyan().bold(),
        style(unit).cyan(),
        dot(),
        style(fmt_int(committed)).bold(),
        dot(),
        style(format!("{:.2}%", error_rate)).bold(),
        dot(),
        style(format!("{:.2} ms", avg_latency_ms)).bold(),
        dot(),
        style(format!("{:.2} ms", p99_latency_ms)).bold(),
    );
    println!();
}

/// A response time for a reader: seconds from one second up, else ms, with
/// two decimals below 10 ms where they still mean something.
fn fmt_response_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms >= 10.0 {
        format!("{:.0} ms", ms)
    } else {
        format!("{:.2} ms", ms)
    }
}

// The headline of a stream run: how long it took, how many transactions
// ran, their geometric mean, and the one that took the most time.
pub fn stream_banner(s: &super::summary::StreamSummary) {
    if is_quiet() {
        return;
    }
    let dot = || style("  ·  ").dim();
    let slowest = s.slowest().map_or(String::new(), |r| {
        format!(
            "{}slowest {} {} ({:.1}%)",
            dot(),
            style(&r.name).bold(),
            style(fmt_response_ms(r.total_ms)).bold(),
            r.share_pct
        )
    });
    println!();
    println!(
        "  {} elapsed{}{} executed{}{} geomean{}{}{} errors",
        style(format!("{:.1} s", s.elapsed_secs)).cyan().bold(),
        dot(),
        style(fmt_int(s.executed)).bold(),
        dot(),
        style(fmt_response_ms(s.geomean_ms)).bold(),
        slowest,
        dot(),
        style(fmt_int(s.errors)).bold(),
    );
    println!();
}

// One line under the banner on how the throughput behaved over the window:
// the verdict, the drift between its halves and the per-second variation.
pub fn steady_state_line(steady: Option<&super::summary::SteadyState>) {
    if is_quiet() {
        return;
    }
    match steady {
        Some(s) => {
            let verdict = match s.verdict {
                super::summary::Verdict::Steady => style(s.verdict.as_str()).green().bold(),
                _ => style(s.verdict.as_str()).yellow().bold(),
            };
            println!(
                "  steady state: {}{}{:+.1}% second half{}{:.1}% variation per second",
                verdict,
                style("  ·  ").dim(),
                s.drift_pct,
                style("  ·  ").dim(),
                s.cv_pct,
            );
        }
        None => println!(
            "  steady state: {}",
            style("not judged: under 10 full seconds measured, or nothing committed").dim()
        ),
    }
    println!();
}

// Tell the user the workload was cut short by Ctrl-C and that the partial
// run is being aggregated. Goes to stderr like the other status lines.
pub fn interrupted_msg() {
    if is_quiet() {
        return;
    }
    eprintln!(
        "{} {} Interrupted (Ctrl-C): aggregating the partial run; press Ctrl-C again to abort",
        style("!").yellow().bold(),
        style("RUN").dim(),
    );
}

// Print the path of a generated artifact (e.g. the HTML report) on stderr,
// indented to match the run header. Suppressed when quiet.
pub fn artifact_path(label: &str, path: &str) {
    if is_quiet() {
        return;
    }
    eprintln!("  {} {}", style(label).dim(), path);
}

// Clear the workload bar and print a final success line with the elapsed time.
pub fn finish_run_bar(bar: ProgressBar, duration_ms: f64) {
    bar.finish_and_clear();
    if is_quiet() {
        return;
    }
    eprintln!(
        "{} {} Workload complete  {}",
        style("✔").green().bold(),
        style("RUN").dim(),
        style(fmt_duration_ms(duration_ms)).dim(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_int_groups_thousands() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(42), "42");
        assert_eq!(fmt_int(1234), "1 234");
        assert_eq!(fmt_int(1234567), "1 234 567");
    }

    fn tx(id: u16, weight: u16) -> BenchmarkTransaction {
        BenchmarkTransaction {
            id,
            name: format!("Q{id}"),
            weight,
        }
    }

    // The stream view counts the eligible transactions of every client, and
    // names what is in flight, the longest-running first, three at most.
    #[test]
    fn stream_progress_counts_and_names_the_transactions_in_flight() {
        set_quiet(true);
        let stream = StreamProgress::new(&[tx(1, 1), tx(2, 0), tx(3, 1)], 5);
        assert_eq!(stream.total(), 10);
        assert_eq!(stream.running(), "");
        stream.begin(2, 3);
        sleep(Duration::from_millis(5));
        for client in [1, 3, 4, 5] {
            stream.begin(client, 1);
        }
        let running = stream.running();
        assert!(
            running.starts_with("running Q3 (c2, 0 s), Q1 ("),
            "{running}"
        );
        assert!(running.ends_with(", +2"), "{running}");
        stream.finished(2, 3, Ok(1_500_000));
        stream.finished(4, 1, Err("boom"));
        assert_eq!(stream.done(), 2);
        assert!(!stream.running().contains("c2"), "{}", stream.running());

        let one = StreamProgress::new(&[tx(1, 1)], 1);
        one.begin(1, 1);
        assert!(
            one.running().starts_with("running Q1 for "),
            "{}",
            one.running()
        );
        // An unknown client id is ignored rather than indexed.
        one.begin(0, 1);
        one.begin(7, 1);
        set_quiet(false);
    }

    #[test]
    fn fmt_elapsed_switches_units() {
        assert_eq!(fmt_elapsed(42_000), "42 s");
        assert_eq!(fmt_elapsed(723_000), "12m 03s");
        assert_eq!(fmt_elapsed(3_720_000), "1h 02m");
    }

    #[test]
    fn fmt_duration_switches_unit_at_one_second() {
        assert_eq!(fmt_duration_ms(0.3), "0 ms");
        assert_eq!(fmt_duration_ms(999.0), "999 ms");
        assert_eq!(fmt_duration_ms(1500.0), "1.5 s");
        assert_eq!(fmt_response_ms(0.476), "0.48 ms");
        assert_eq!(fmt_response_ms(476.2), "476 ms");
        assert_eq!(fmt_response_ms(48_522.0), "48.52 s");
    }
}
