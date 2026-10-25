use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant};

use console::{style, Term};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

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
        if i >= first && i != 0 && (i - first) % 3 == 0 {
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

// Determinate bar tracking the workload's elapsed/total seconds. The message
// suffix (live throughput/latency) is set by the caller. Hidden on a non-TTY so
// callers can fall back to periodic log lines.
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

// Create the workload bar and a background ticker that drives it. On a TTY the
// ticker advances the determinate bar and refreshes the suffix with live
// throughput/latency computed over a ~1s rolling window. On a non-TTY the bar
// is hidden, and instead the ticker emits a pgbench-style `progress:` line every
// `progress_s` seconds (suppressed entirely when progress_s is 0). The caller
// flips `stop` once the clients are done, then joins the handle and calls
// finish_run_bar on the returned bar.
pub fn spawn_workload_monitor(
    total_s: u64,
    start: Instant,
    live_commits: Arc<AtomicU64>,
    live_latency_us: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    progress_s: u16,
) -> (ProgressBar, JoinHandle<()>) {
    let bar = run_bar(total_s);
    let pb = bar.clone();
    let tty = is_tty();
    let quiet = is_quiet();
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
            let tps = dc as f64 / dt;
            let lat_ms = if dc > 0 {
                (latency_us - l0) as f64 / dc as f64 / 1000.0
            } else {
                0.0
            };

            if tty {
                pb.set_position(elapsed.min(total_s as f64) as u64);
                pb.set_message(format!(
                    "{} tps · lat {:.2} ms",
                    fmt_int(tps as u64),
                    lat_ms
                ));
            } else if !quiet && progress_s > 0 && elapsed >= next_progress {
                eprintln!(
                    "progress: {:.1} s, {:.1} tps, lat {:.3} ms",
                    elapsed, tps, lat_ms
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
) {
    if is_quiet() {
        return;
    }
    let dot = || style("  ·  ").dim();
    println!();
    println!(
        "  {} {}{}{} committed{}{} errors{}{} avg",
        style(fmt_int(throughput)).cyan().bold(),
        style(unit).cyan(),
        dot(),
        style(fmt_int(committed)).bold(),
        dot(),
        style(format!("{:.2}%", error_rate)).bold(),
        dot(),
        style(format!("{:.2} ms", avg_latency_ms)).bold(),
    );
    println!();
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
        "{} {} {}  {}",
        style("✔").green().bold(),
        style("RUN").dim(),
        "Workload complete",
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

    #[test]
    fn fmt_duration_switches_unit_at_one_second() {
        assert_eq!(fmt_duration_ms(0.3), "0 ms");
        assert_eq!(fmt_duration_ms(999.0), "999 ms");
        assert_eq!(fmt_duration_ms(1500.0), "1.5 s");
    }
}
