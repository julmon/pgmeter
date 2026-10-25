//! Run summary shared by every output of `pgmeter run`: the terminal tables
//! and KPI banner, the JSON and CSV formats, and the HTML report. Computing
//! the numbers in one place guarantees that all of them agree.
//!
//! Throughput is `committed / workload_secs * factor`, where `workload_secs`
//! is the steady-state window: total elapsed time minus the ramp-up period.
//! The data collector only counts commits once ramp-up is over, so dividing
//! by the full elapsed time would understate the rate.

use std::collections::HashMap;

use super::benchmark::{BenchmarkTransaction, Counter, TransactionSummary};

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
