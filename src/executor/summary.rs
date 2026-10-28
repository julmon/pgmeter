//! Run summary shared by every output of `pgmeter run`: the terminal tables
//! and KPI banner, the JSON and CSV formats, and the HTML report. Computing
//! the numbers in one place guarantees that all of them agree.
//!
//! Throughput is `committed / workload_secs * factor`, where `workload_secs`
//! is the steady-state window: total elapsed time minus the ramp-up period.
//! The data collector only counts commits once ramp-up is over, so dividing
//! by the full elapsed time would understate the rate.

use std::collections::HashMap;

use serde::Serialize;

use super::benchmark::{BenchmarkTransaction, Counter, ResponseTimeStatistics, TransactionSummary};

/// Seconds of steady-state workload: total elapsed time minus ramp-up, with
/// millisecond precision. Floored at one second so a rate can always be
/// computed, even for a run cut short before ramp-up ended.
pub fn workload_secs(total_time_ms: u128, rampup_time_ms: u128) -> f64 {
    let ms = total_time_ms.saturating_sub(rampup_time_ms);
    (ms as f64 / 1000.0).max(1.0)
}

/// Headline numbers for a run plus one [`TransactionSummary`] per declared
/// transaction, in spec order.
pub struct RunSummary {
    pub workload_secs: f64,
    pub transactions: Vec<TransactionSummary>,
    pub total_commits: u64,
    pub total_errors: u64,
    /// Errors as a percentage of all attempted transactions.
    pub error_rate: f64,
    /// Overall throughput in the spec's unit (sum of commits over the workload window).
    pub throughput: f64,
    /// Commit-weighted mean response time in milliseconds, from the collector's
    /// post-ramp-up counters.
    pub avg_latency_ms: f64,
}

/// Build the run summary from the collector's per-transaction counters.
/// Transactions without a counter (never executed) get zeroed rows.
pub fn summarize(
    transactions: &[BenchmarkTransaction],
    counters: &HashMap<u16, Counter>,
    total_time_ms: u128,
    rampup_time_ms: u128,
    throughput_factor: f64,
) -> RunSummary {
    let secs = workload_secs(total_time_ms, rampup_time_ms);
    let zero = Counter {
        n_commits: 0,
        n_total: 0,
        total_duration_ms: 0.0,
    };

    let mut total_commits = 0u64;
    let mut total_errors = 0u64;
    let mut total_duration_ms = 0.0f64;
    let mut rows = Vec::with_capacity(transactions.len());

    for tx in transactions {
        let c = counters.get(&tx.id).unwrap_or(&zero);
        let errors = c.n_total.saturating_sub(c.n_commits);
        total_commits += c.n_commits;
        total_errors += errors;
        total_duration_ms += c.total_duration_ms;
        rows.push(TransactionSummary::new(
            tx.name.clone(),
            c.n_commits,
            errors,
            percentage(errors, c.n_total),
            c.n_commits as f64 / secs * throughput_factor,
        ));
    }

    RunSummary {
        workload_secs: secs,
        transactions: rows,
        total_commits,
        total_errors,
        error_rate: percentage(total_errors, total_commits + total_errors),
        throughput: total_commits as f64 / secs * throughput_factor,
        avg_latency_ms: if total_commits > 0 {
            total_duration_ms / total_commits as f64
        } else {
            0.0
        },
    }
}

/// One transaction of a stream run: how often it ran and how much of the
/// stream's time it took.
#[derive(Clone, Debug)]
pub struct StreamRow {
    pub name: String,
    pub executed: u64,
    pub errors: u64,
    /// Sum of the response times of its commits, in ms.
    pub total_ms: f64,
    /// `total_ms` as a percentage of every transaction's `total_ms`.
    pub share_pct: f64,
    pub mean_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

/// A stream run read as such: where its time went, transaction by
/// transaction, rather than a rate.
#[derive(Clone, Debug)]
pub struct StreamSummary {
    /// The measured window, in seconds.
    pub elapsed_secs: f64,
    pub executed: u64,
    pub errors: u64,
    /// Geometric mean of the mean response times of the transactions that
    /// committed, in ms: TPC-H's power metric is built the same way, so one
    /// slow query does not hide the others.
    pub geomean_ms: f64,
    /// One row per declared transaction, in spec order.
    pub rows: Vec<StreamRow>,
}

impl StreamSummary {
    /// The row that took the most time, if anything committed.
    pub fn slowest(&self) -> Option<&StreamRow> {
        self.rows
            .iter()
            .filter(|r| r.executed > 0)
            .max_by(|a, b| a.total_ms.total_cmp(&b.total_ms))
    }

    /// Indexes into `rows`, the most time-consuming first.
    pub fn by_total_desc(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        order.sort_by(|a, b| self.rows[*b].total_ms.total_cmp(&self.rows[*a].total_ms));
        order
    }
}

/// Build the stream view of a run from the collector's counters and the
/// steady-state statistics (for the minimum and maximum).
pub fn stream_summary(
    transactions: &[BenchmarkTransaction],
    counters: &HashMap<u16, Counter>,
    stats: &HashMap<u16, ResponseTimeStatistics>,
    total_time_ms: u128,
    rampup_time_ms: u128,
) -> StreamSummary {
    let mut rows: Vec<StreamRow> = transactions
        .iter()
        .map(|tx| {
            let (executed, errors, total_ms) = counters.get(&tx.id).map_or((0, 0, 0.0), |c| {
                (
                    c.n_commits,
                    c.n_total.saturating_sub(c.n_commits),
                    c.total_duration_ms,
                )
            });
            let (min_ms, max_ms) = stats.get(&tx.id).map_or((0.0, 0.0), |s| (s.min, s.max));
            StreamRow {
                name: tx.name.clone(),
                executed,
                errors,
                total_ms,
                share_pct: 0.0,
                mean_ms: if executed > 0 {
                    total_ms / executed as f64
                } else {
                    0.0
                },
                min_ms,
                max_ms,
            }
        })
        .collect();
    let all_ms: f64 = rows.iter().map(|r| r.total_ms).sum();
    if all_ms > 0.0 {
        for row in &mut rows {
            row.share_pct = row.total_ms / all_ms * 100.0;
        }
    }
    let means: Vec<f64> = rows
        .iter()
        .filter(|r| r.executed > 0 && r.mean_ms > 0.0)
        .map(|r| r.mean_ms)
        .collect();
    let geomean_ms = if means.is_empty() {
        0.0
    } else {
        (means.iter().map(|m| m.ln()).sum::<f64>() / means.len() as f64).exp()
    };
    StreamSummary {
        elapsed_secs: workload_secs(total_time_ms, rampup_time_ms),
        executed: rows.iter().map(|r| r.executed).sum(),
        errors: rows.iter().map(|r| r.errors).sum(),
        geomean_ms,
        rows,
    }
}

/// Below this many full seconds the window is too short to judge.
const STEADY_MIN_SECONDS: usize = 10;
/// Steady when the per-second throughput varies less than this (coefficient
/// of variation, %) ...
pub const STEADY_MAX_CV_PCT: f64 = 10.0;
/// ... and the second half of the window is within this many % of the first.
pub const STEADY_MAX_DRIFT_PCT: f64 = 5.0;

/// How the throughput behaved over the measured window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Little variation and no drift: the headline rate describes the window.
    Steady,
    /// No drift, but the rate swings from second to second.
    Unstable,
    /// The second half ran at a different rate from the first: the headline
    /// is an average of two regimes, and a longer run may settle elsewhere.
    Drifted,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Steady => "steady",
            Verdict::Unstable => "unstable",
            Verdict::Drifted => "drifted",
        }
    }
}

/// Per-second throughput statistics of the measured window, in the spec's unit.
#[derive(Clone, Debug, Serialize)]
pub struct SteadyState {
    /// Full seconds judged: the window minus its first and last, partial, seconds.
    pub seconds: usize,
    pub mean: f64,
    /// Coefficient of variation of the per-second rate, in %.
    pub cv_pct: f64,
    pub first_half: f64,
    pub second_half: f64,
    /// Second half against the first, in %; negative when throughput fell.
    pub drift_pct: f64,
    pub verdict: Verdict,
    /// Log timestamps of the first judged second, of the first second of
    /// the second half, and of the last judged second.
    pub start_ts: i64,
    pub mid_ts: i64,
    pub end_ts: i64,
}

/// Judge the measured window from its per-second commit counts (see
/// `data_agg::Aggregate::window`). The first and last seconds are dropped
/// because clients start and stop inside them. `None` when fewer than
/// [`STEADY_MIN_SECONDS`] full seconds remain or nothing committed.
pub fn steady_state(window: &[(i64, u64)], factor: f64) -> Option<SteadyState> {
    if window.len() < STEADY_MIN_SECONDS + 2 {
        return None;
    }
    let full = &window[1..window.len() - 1];
    let rates: Vec<f64> = full.iter().map(|(_, c)| *c as f64 * factor).collect();
    let mean_of = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
    let mean = mean_of(&rates);
    if mean <= 0.0 {
        return None;
    }
    let variance = rates.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rates.len() as f64;
    let cv_pct = variance.sqrt() / mean * 100.0;
    let half = rates.len() / 2;
    let first_half = mean_of(&rates[..half]);
    let second_half = mean_of(&rates[half..]);
    let drift_pct = if first_half > 0.0 {
        (second_half - first_half) / first_half * 100.0
    } else {
        100.0
    };
    let verdict = if drift_pct.abs() >= STEADY_MAX_DRIFT_PCT {
        Verdict::Drifted
    } else if cv_pct >= STEADY_MAX_CV_PCT {
        Verdict::Unstable
    } else {
        Verdict::Steady
    };
    Some(SteadyState {
        seconds: rates.len(),
        mean,
        cv_pct,
        first_half,
        second_half,
        drift_pct,
        verdict,
        start_ts: full[0].0,
        mid_ts: full[half].0,
        end_ts: full[full.len() - 1].0,
    })
}

fn percentage(part: u64, whole: u64) -> f64 {
    if whole > 0 {
        part as f64 / whole as f64 * 100.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(id: u16, name: &str) -> BenchmarkTransaction {
        BenchmarkTransaction {
            id,
            name: name.to_string(),
            weight: 1,
        }
    }

    fn counter(n_commits: u64, n_total: u64, total_duration_ms: f64) -> Counter {
        Counter {
            n_commits,
            n_total,
            total_duration_ms,
        }
    }

    fn window(counts: &[u64]) -> Vec<(i64, u64)> {
        counts
            .iter()
            .enumerate()
            .map(|(i, c)| (100 + i as i64, *c))
            .collect()
    }

    #[test]
    fn steady_state_needs_ten_full_seconds() {
        // 11 seconds = 9 full ones once both ends are dropped.
        assert!(steady_state(&window(&[10; 11]), 1.0).is_none());
        assert!(steady_state(&window(&[0; 20]), 1.0).is_none());
        let s = steady_state(&window(&[10; 12]), 60.0).unwrap();
        assert_eq!(s.seconds, 10);
        assert_eq!(s.mean, 600.0);
        assert_eq!((s.cv_pct, s.drift_pct), (0.0, 0.0));
        assert_eq!(s.verdict, Verdict::Steady);
        assert_eq!((s.start_ts, s.mid_ts, s.end_ts), (101, 106, 110));
    }

    #[test]
    fn steady_state_drops_the_partial_end_seconds() {
        // The ends (1 and 3) would wreck the variation if they counted.
        let s = steady_state(
            &window(&[1, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 3]),
            1.0,
        )
        .unwrap();
        assert_eq!(s.verdict, Verdict::Steady);
        assert_eq!(s.mean, 100.0);
    }

    #[test]
    fn steady_state_tells_drift_from_noise() {
        // Alternating 80/120 over 12 full seconds: no drift, 20% variation.
        let noisy = [0, 80, 120, 80, 120, 80, 120, 80, 120, 80, 120, 80, 120, 0];
        let s = steady_state(&window(&noisy), 1.0).unwrap();
        assert_eq!(s.verdict, Verdict::Unstable);
        assert!((s.cv_pct - 20.0).abs() < 1e-9, "{}", s.cv_pct);
        assert_eq!(s.drift_pct, 0.0);

        // 100 then 90: the second half fell 10%, under 10% variation.
        let falling = [0, 100, 100, 100, 100, 100, 90, 90, 90, 90, 90, 0];
        let s = steady_state(&window(&falling), 1.0).unwrap();
        assert_eq!(s.verdict, Verdict::Drifted);
        assert!((s.drift_pct + 10.0).abs() < 1e-9, "{}", s.drift_pct);
        assert_eq!((s.first_half, s.second_half), (100.0, 90.0));
    }

    #[test]
    fn stream_summary_shares_time_and_takes_the_geometric_mean() {
        let txs = [tx(1, "q1"), tx(2, "q2"), tx(3, "never")];
        // q1: two runs of 1 s and 3 s; q2: one run of 4 s.
        let counters = HashMap::from([(1, counter(2, 3, 4000.0)), (2, counter(1, 1, 4000.0))]);
        let mut stats = HashMap::new();
        let mut q1 = ResponseTimeStatistics::zero("q1");
        (q1.min, q1.max) = (1000.0, 3000.0);
        stats.insert(1, q1);
        let s = stream_summary(&txs, &counters, &stats, 9_000, 1_000);

        assert_eq!(s.elapsed_secs, 8.0);
        assert_eq!((s.executed, s.errors), (3, 1));
        assert_eq!(s.rows[0].share_pct, 50.0);
        assert_eq!(s.rows[0].mean_ms, 2000.0);
        assert_eq!((s.rows[0].min_ms, s.rows[0].max_ms), (1000.0, 3000.0));
        // Means 2000 and 4000; the transaction that never ran is left out.
        assert!(
            (s.geomean_ms - 8_000_000f64.sqrt()).abs() < 1e-6,
            "{}",
            s.geomean_ms
        );
        assert_eq!(s.rows[2].share_pct, 0.0);
        // Ties go either way; the never-run row is last.
        assert_eq!(s.by_total_desc()[2], 2);
        assert!(s.slowest().is_some_and(|r| r.total_ms == 4000.0));
    }

    #[test]
    fn stream_summary_of_an_empty_run_is_all_zeros() {
        let s = stream_summary(&[tx(1, "q1")], &HashMap::new(), &HashMap::new(), 500, 0);
        assert_eq!((s.executed, s.geomean_ms), (0, 0.0));
        assert!(s.slowest().is_none());
    }

    #[test]
    fn workload_window_excludes_rampup() {
        assert_eq!(workload_secs(70_000, 10_000), 60.0);
        assert_eq!(workload_secs(1_500, 0), 1.5);
    }

    #[test]
    fn workload_window_is_floored_at_one_second() {
        assert_eq!(workload_secs(400, 0), 1.0);
        // Run shorter than its ramp-up (interrupted): never divide by zero.
        assert_eq!(workload_secs(5_000, 10_000), 1.0);
    }

    // Regression for RELEASE_REVIEW B1: the terminal, JSON and CSV outputs
    // divided post-ramp-up commits by the *total* elapsed time, so with
    // `-r 10 -T 60` and 6 000 commits they printed 85 TPS while the HTML
    // report printed 100.
    #[test]
    fn throughput_divides_by_the_workload_window_not_total_time() {
        let txs = [tx(1, "only")];
        let counters = HashMap::from([(1, counter(6_000, 6_000, 0.0))]);
        let s = summarize(&txs, &counters, 70_000, 10_000, 1.0);
        assert_eq!(s.workload_secs, 60.0);
        assert_eq!(s.transactions[0].throughput, 100.0);
        assert_eq!(s.throughput, 100.0);
    }

    // The old code also truncated the elapsed time to whole seconds
    // (`Duration::as_secs`) and the rate to `u32`.
    #[test]
    fn throughput_keeps_millisecond_precision_and_fractions() {
        let txs = [tx(1, "only")];
        let counters = HashMap::from([(1, counter(3, 3, 0.0))]);
        // 3 commits over 1.5 s = 2 TPS; whole-second truncation would say 3.
        let s = summarize(&txs, &counters, 1_500, 0, 1.0);
        assert_eq!(s.throughput, 2.0);
        // Per-minute unit: 3 / 1.5 * 60 = 120.
        let s = summarize(&txs, &counters, 1_500, 0, 60.0);
        assert_eq!(s.transactions[0].throughput, 120.0);
    }

    #[test]
    fn totals_error_rate_and_weighted_latency() {
        let txs = [tx(1, "a"), tx(2, "b"), tx(3, "never-ran")];
        let counters = HashMap::from([
            // 90 commits, 10 errors, 90 ms total -> 1 ms mean
            (1, counter(90, 100, 90.0)),
            // 10 commits, 0 errors, 110 ms total -> 11 ms mean
            (2, counter(10, 10, 110.0)),
        ]);
        let s = summarize(&txs, &counters, 10_000, 0, 1.0);

        assert_eq!(s.total_commits, 100);
        assert_eq!(s.total_errors, 10);
        assert!((s.error_rate - 10.0 / 110.0 * 100.0).abs() < 1e-9);
        assert_eq!(s.throughput, 10.0);
        // (90*1 + 10*11) / 100 = 2 ms, weighted by commits not by transaction.
        assert_eq!(s.avg_latency_ms, 2.0);

        assert_eq!(s.transactions.len(), 3);
        assert_eq!(s.transactions[0].error_rate, 10.0);
        assert_eq!(s.transactions[0].throughput, 9.0);
        let never = &s.transactions[2];
        assert_eq!(never.name, "never-ran");
        assert_eq!((never.n_commits, never.n_errors), (0, 0));
        assert_eq!(never.error_rate, 0.0);
        assert_eq!(never.throughput, 0.0);
    }
}
