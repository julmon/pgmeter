use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use super::benchmark::{BenchmarkTransaction, ResponseTimeStatistics, TransactionSummary};
use super::error::Error;
use tabled::{
    builder::Builder,
    settings::{
        object::{Columns, Object, Rows},
        style::HorizontalLine,
        Alignment, Format, Modify, Style, Width,
    },
    Table,
};

// Widest rendered line (in characters) of a table.
fn rendered_width(table: &Table) -> usize {
    table
        .to_string()
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
}

// Sanitize a transaction name for use as a filename component: any character
// that isn't alphanumeric, '-' or '_' (e.g. the '/' in "Debit/credit") becomes
// '_', so the per-transaction CSV paths stay valid instead of being treated as
// nested directories.
pub(super) fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Per-second accumulation for one transaction type (or for all of them).
#[derive(Default)]
struct PerSecond {
    count: u64,
    duration_sum_ms: f64,
}

/// Everything aggregated for one transaction type in a single pass over the log.
#[derive(Default)]
struct TxAggregate {
    /// Per log second, whole run (ramp-up included), for the time-series CSVs.
    per_second: BTreeMap<i64, PerSecond>,
    /// Steady-state response times (at or after the ramp-up boundary), for the statistics.
    steady_durations_ms: Vec<f64>,
}

/// Response-time statistics of a sample. Matches what the previous
/// implementation (polars `describe`) computed: sample standard deviation
/// (n - 1), percentiles by linear interpolation between the two nearest
/// order statistics. An empty sample yields all zeros.
fn statistics(name: &str, durations_ms: &mut [f64]) -> ResponseTimeStatistics {
    let mut stats = ResponseTimeStatistics {
        name: name.to_string(),
        mean: 0.0,
        min: 0.0,
        max: 0.0,
        std: 0.0,
        percentile_95: 0.0,
        percentile_99: 0.0,
    };
    let n = durations_ms.len();
    if n == 0 {
        return stats;
    }
    durations_ms.sort_by(|a, b| a.total_cmp(b));
    let sum: f64 = durations_ms.iter().sum();
    stats.mean = sum / n as f64;
    stats.min = durations_ms[0];
    stats.max = durations_ms[n - 1];
    if n > 1 {
        let var = durations_ms
            .iter()
            .map(|d| (d - stats.mean).powi(2))
            .sum::<f64>()
            / (n - 1) as f64;
        stats.std = var.sqrt();
    }
    stats.percentile_95 = percentile(durations_ms, 0.95);
    stats.percentile_99 = percentile(durations_ms, 0.99);
    stats
}

/// Linear-interpolation percentile of a sorted, non-empty sample.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    let position = q * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f64;
    sorted[lower] + (sorted[upper] - sorted[lower]) * fraction
}

/// Throughput in the spec's unit for one second with `count` commits.
fn rate(count: u64, throughput_factor: f64) -> u64 {
    (count as f64 * throughput_factor).round() as u64
}

fn create(path: &Path) -> Result<BufWriter<File>, Error> {
    File::create(path)
        .map(BufWriter::new)
        .map_err(|e| Error::io(format!("Cannot create {}", path.display()), e))
}

/// What one pass over the transaction log yields for the results and the report.
pub struct Aggregate {
    /// Steady-state response-time statistics per transaction id.
    pub stats: HashMap<u16, ResponseTimeStatistics>,
    /// The same statistics over every steady-state sample, all transactions together.
    pub overall: ResponseTimeStatistics,
    /// Commits per log second of the measured window, `(timestamp, count)`,
    /// with the seconds in which nothing committed included as 0; the input of
    /// [`super::summary::steady_state`].
    pub window: Vec<(i64, u64)>,
}

// Aggregates the transaction log in a single pass and writes the per-transaction
// CSV files; returns the response-time statistics for the results and the report.
//
// The per-second time series (`pgmeter-throughput-*.csv`, `pgmeter-response-time-*.csv`)
// cover the whole run, ramp-up included, so they line up with the report's
// charts. The response-time statistics (returned, and written to
// `pgmeter-stats-*.csv` for users) only use samples logged at or after
// `rampup_end_ts`, matching the committed counts and the throughput, which the
// collector also only counts once ramp-up is over.
pub fn aggregate_transaction_data(
    log_file: &str,
    target_dir: &Path,
    transactions: &[BenchmarkTransaction],
    rampup_end_ts: Option<i64>,
    throughput_factor: f64,
) -> Result<Aggregate, Error> {
    let log_path = target_dir.join(log_file);
    let log = File::open(&log_path)
        .map_err(|e| Error::io(format!("Cannot read {}", log_path.display()), e))?;

    // One pass: `<ts> <clients> <tx_id> <duration_ms>` per line.
    let mut per_tx: HashMap<u16, TxAggregate> = HashMap::new();
    let mut all: BTreeMap<i64, PerSecond> = BTreeMap::new();
    for line in BufReader::new(log).lines().map_while(Result::ok) {
        let mut fields = line.split_whitespace();
        let ts = fields.next().and_then(|v| v.parse::<i64>().ok());
        let _clients = fields.next();
        let tx_id = fields.next().and_then(|v| v.parse::<u16>().ok());
        let duration_ms = fields.next().and_then(|v| v.parse::<f64>().ok());
        let (Some(ts), Some(tx_id), Some(duration_ms)) = (ts, tx_id, duration_ms) else {
            continue;
        };
        let agg = per_tx.entry(tx_id).or_default();
        let second = agg.per_second.entry(ts).or_default();
        second.count += 1;
        second.duration_sum_ms += duration_ms;
        let total = all.entry(ts).or_default();
        total.count += 1;
        total.duration_sum_ms += duration_ms;
        if rampup_end_ts.is_none_or(|end| ts >= end) {
            agg.steady_durations_ms.push(duration_ms);
        }
    }

    let mut stats_map = HashMap::new();
    // Every steady-state sample, moved out of the per-transaction vectors once
    // their own statistics are done, so memory does not double.
    let mut all_steady: Vec<f64> = Vec::new();
    for transaction in transactions {
        let safe = safe_name(&transaction.name);
        let mut agg = per_tx.remove(&transaction.id).unwrap_or_default();
        let origin = agg.per_second.keys().next().copied().unwrap_or(0);

        let mut throughput = create(&target_dir.join(format!("pgmeter-throughput-{safe}.csv")))?;
        writeln!(throughput, "time_s,throughput")?;
        let mut response_time =
            create(&target_dir.join(format!("pgmeter-response-time-{safe}.csv")))?;
        writeln!(response_time, "time_s,response_time_ms")?;
        for (ts, second) in &agg.per_second {
            writeln!(
                throughput,
                "{},{}",
                ts - origin,
                rate(second.count, throughput_factor)
            )?;
            writeln!(
                response_time,
                "{},{}",
                ts - origin,
                second.duration_sum_ms / second.count as f64
            )?;
        }

        let stats = statistics(&transaction.name, &mut agg.steady_durations_ms);
        let mut stats_file = create(&target_dir.join(format!("pgmeter-stats-{safe}.csv")))?;
        writeln!(stats_file, "describe,response_time_ms")?;
        writeln!(stats_file, "count,{}", agg.steady_durations_ms.len())?;
        for (label, value) in [
            ("mean", stats.mean),
            ("std", stats.std),
            ("min", stats.min),
            ("95%", stats.percentile_95),
            ("99%", stats.percentile_99),
            ("max", stats.max),
        ] {
            writeln!(stats_file, "{label},{value}")?;
        }
        stats_map.insert(transaction.id, stats);
        all_steady.append(&mut agg.steady_durations_ms);
    }
    let overall = statistics("all", &mut all_steady);

    // Total throughput per second across all transaction types, same unit.
    let origin = all.keys().next().copied().unwrap_or(0);
    let mut total = create(&target_dir.join("pgmeter-throughput-all.csv"))?;
    writeln!(total, "time_s,throughput")?;
    for (ts, second) in &all {
        writeln!(
            total,
            "{},{}",
            ts - origin,
            rate(second.count, throughput_factor)
        )?;
    }

    // The measured window, gaps filled: a second without a commit is a stall
    // the steady-state check must see.
    let window_start = rampup_end_ts.unwrap_or(i64::MIN);
    let mut window: Vec<(i64, u64)> = Vec::new();
    for (ts, second) in all.range(window_start..) {
        if let Some(&(last, _)) = window.last() {
            window.extend((last + 1..*ts).map(|gap| (gap, 0)));
        }
        window.push((*ts, second.count));
    }

    Ok(Aggregate {
        stats: stats_map,
        overall,
        window,
    })
}

// Render both result tables with a light, frame-less style and at an identical
// width so they line up visually. `throughput_unit` is the headline metric
// label from the spec's `metrics` block (e.g. TPS, txns/min, queries/hr); it becomes the
// summary table's throughput column header. The two tables have different
// column counts (hence different natural widths), so the narrower one is grown
// to match via `Width::increase`.
pub fn print_transactions_tables(
    summary: &[TransactionSummary],
    stats: &[ResponseTimeStatistics],
    throughput_unit: &str,
) {
    // Frame-less style with a single rule under the header row.
    let header_rule =
        || Style::blank().horizontals([(1, HorizontalLine::new('─').intersection('─'))]);

    // Summary table.
    let mut builder = Builder::default();
    builder.push_record([
        "Transaction",
        "Committed",
        "Errors",
        "Error rate (%)",
        throughput_unit,
    ]);
    for row in summary {
        builder.push_record([
            row.name.clone(),
            super::terminal::fmt_int(row.n_commits),
            super::terminal::fmt_int(row.n_errors),
            format!("{:.3}", row.error_rate),
            super::terminal::fmt_int(row.throughput.round() as u64),
        ]);
    }
    let mut summary_table = builder.build();
    summary_table
        .with(header_rule())
        .with(Modify::new(Rows::first()).with(Alignment::center()))
        .with(Modify::new(Columns::new(1..).not(Rows::first())).with(Alignment::right()));

    // Response-time stats table.
    let mut stats_table = Table::new(stats);
    stats_table
        .with(header_rule())
        .with(Modify::new(Rows::first()).with(Alignment::center()));
    for col in 1..=6 {
        stats_table.with(
            Modify::new(Columns::new(col..=col).not(Rows::first()))
                .with(Format::content(|s: &str| {
                    format!("{val:.*}", 3, val = s.parse::<f64>().unwrap())
                }))
                .with(Alignment::right()),
        );
    }

    // Equalize widths, then print.
    let target = rendered_width(&summary_table).max(rendered_width(&stats_table));
    summary_table.with(Width::increase(target));
    stats_table.with(Width::increase(target));

    println!("Results:");
    println!("{}", summary_table);
    println!();
    println!("Response times:");
    println!("{}", stats_table);
}

// Render the results as a pretty-printed JSON document for machine consumption.
// `summary` and `stats` are aligned by index (same transaction order).
// `workload_secs` is the steady-state window (elapsed minus ramp-up) that the
// throughput figures are computed over; it is emitted as `duration_s`.
pub fn results_json(
    summary: &[TransactionSummary],
    stats: &[ResponseTimeStatistics],
    throughput_unit: &str,
    workload_secs: f64,
    interrupted: bool,
    steady: Option<&super::summary::SteadyState>,
) -> String {
    let transactions: Vec<serde_json::Value> = summary
        .iter()
        .zip(stats.iter())
        .map(|(s, st)| {
            serde_json::json!({
                "name": s.name,
                "committed": s.n_commits,
                "errors": s.n_errors,
                "error_rate": s.error_rate,
                "throughput": s.throughput.round() as u64,
                "latency_ms": {
                    "avg": st.mean,
                    "min": st.min,
                    "max": st.max,
                    "stddev": st.std,
                    "p95": st.percentile_95,
                    "p99": st.percentile_99,
                },
            })
        })
        .collect();

    let doc = serde_json::json!({
        "throughput_unit": throughput_unit,
        "duration_s": workload_secs.round() as u64,
        "interrupted": interrupted,
        "steady_state": steady.map(|s| serde_json::json!({
            "verdict": s.verdict,
            "seconds": s.seconds,
            "mean": s.mean,
            "cv_pct": s.cv_pct,
            "first_half": s.first_half,
            "second_half": s.second_half,
            "drift_pct": s.drift_pct,
        })),
        "transactions": transactions,
    });

    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap())
}

// Render the results as CSV (one row per transaction). The throughput column is
// labeled with the spec's configured unit, matching the table output.
pub fn results_csv(
    summary: &[TransactionSummary],
    stats: &[ResponseTimeStatistics],
    throughput_unit: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "transaction,committed,errors,error_rate,{},avg_ms,min_ms,max_ms,stddev_ms,p95_ms,p99_ms\n",
        csv_field(throughput_unit)
    ));
    for (s, st) in summary.iter().zip(stats.iter()) {
        out.push_str(&format!(
            "{},{},{},{:.3},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}\n",
            csv_field(&s.name),
            s.n_commits,
            s.n_errors,
            s.error_rate,
            s.throughput.round() as u64,
            st.mean,
            st.min,
            st.max,
            st.std,
            st.percentile_95,
            st.percentile_99,
        ));
    }
    out
}

// Quote a CSV field if it contains a comma, double-quote or newline.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::benchmark::BenchmarkTransaction;
    use std::fs::File;
    use std::io::Write;

    // RELEASE_REVIEW D5: the hand-rolled statistics reproduce what polars'
    // `describe` produced: sample standard deviation and linearly
    // interpolated percentiles.
    #[test]
    fn statistics_match_the_previous_definitions() {
        let mut sample = vec![4.0, 1.0, 3.0, 2.0, 5.0];
        let s = statistics("t", &mut sample);
        assert_eq!(s.mean, 3.0);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 5.0);
        // sample variance of 1..5 is 2.5
        assert!((s.std - 2.5f64.sqrt()).abs() < 1e-12, "{}", s.std);
        // position 0.95 * 4 = 3.8 -> 4 + 0.8 * (5 - 4)
        assert!((s.percentile_95 - 4.8).abs() < 1e-12, "{}", s.percentile_95);
        assert!(
            (s.percentile_99 - 4.96).abs() < 1e-12,
            "{}",
            s.percentile_99
        );

        let one = statistics("one", &mut [7.5]);
        assert_eq!((one.mean, one.min, one.max, one.std), (7.5, 7.5, 7.5, 0.0));
        assert_eq!((one.percentile_95, one.percentile_99), (7.5, 7.5));

        let none = statistics("none", &mut []);
        assert_eq!((none.mean, none.max, none.percentile_99), (0.0, 0.0, 0.0));
    }

    #[test]
    fn aggregate_writes_zero_stats_and_empty_series_for_an_idle_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("transaction.log"), "100 1 1 2.0\n").unwrap();
        let transactions = vec![tx(1, "busy"), tx(2, "idle")];
        let stats = aggregate_transaction_data("transaction.log", &dir, &transactions, None, 1.0)
            .unwrap()
            .stats;
        assert_eq!(stats[&2].mean, 0.0);
        assert_eq!(stats[&1].mean, 2.0);
        let idle = std::fs::read_to_string(dir.join("pgmeter-stats-idle.csv")).unwrap();
        assert_eq!(idle.lines().nth(1), Some("count,0"));
        assert_eq!(idle.lines().nth(2), Some("mean,0"));
        let series = std::fs::read_to_string(dir.join("pgmeter-throughput-idle.csv")).unwrap();
        assert_eq!(series, "time_s,throughput\n");
    }

    // A stalled second inside the window counts as 0 commits, and the
    // overall statistics pool every transaction's samples.
    #[test]
    fn aggregate_fills_stalled_seconds_and_pools_the_overall_statistics() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(
            dir.join("transaction.log"),
            "100 1 1 1.0 1 0\n100 1 2 3.0 1 2\n103 1 1 5.0 1 3000\n",
        )
        .unwrap();
        let transactions = vec![tx(1, "a"), tx(2, "b")];
        let agg =
            aggregate_transaction_data("transaction.log", &dir, &transactions, None, 1.0).unwrap();
        assert_eq!(agg.window, vec![(100, 2), (101, 0), (102, 0), (103, 1)]);
        assert_eq!(agg.overall.mean, 3.0);
        assert_eq!(agg.overall.max, 5.0);
    }

    #[test]
    fn safe_name_replaces_path_unsafe_chars() {
        assert_eq!(safe_name("Debit/credit"), "Debit_credit");
        assert_eq!(safe_name("New-Order"), "New-Order");
        assert_eq!(safe_name("a b:c\\d"), "a_b_c_d");
        assert_eq!(safe_name("plain_123"), "plain_123");
    }

    fn tx(id: u16, name: &str) -> BenchmarkTransaction {
        BenchmarkTransaction {
            id,
            name: name.to_string(),
            weight: 1,
        }
    }

    fn sample_results() -> (Vec<TransactionSummary>, Vec<ResponseTimeStatistics>) {
        let summary = vec![TransactionSummary::new(
            "Debit/credit".to_string(),
            100,
            2,
            1.961,
            3150.4,
        )];
        let stats = vec![ResponseTimeStatistics {
            name: "Debit/credit".to_string(),
            mean: 0.633,
            min: 0.533,
            max: 6.633,
            std: 0.202,
            percentile_95: 0.811,
            percentile_99: 1.747,
        }];
        (summary, stats)
    }

    #[test]
    fn results_json_includes_unit_and_latency() {
        let (summary, stats) = sample_results();
        let out = results_json(&summary, &stats, "TPS", 30.2, false, None);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["throughput_unit"], "TPS");
        assert_eq!(v["duration_s"], 30);
        assert_eq!(v["interrupted"], false);
        assert!(v["steady_state"].is_null());
        let steady = crate::executor::summary::steady_state(
            &(0..12).map(|i| (i, 10)).collect::<Vec<_>>(),
            1.0,
        );
        let v: serde_json::Value = serde_json::from_str(&results_json(
            &summary,
            &stats,
            "TPS",
            1.0,
            true,
            steady.as_ref(),
        ))
        .unwrap();
        assert_eq!(v["interrupted"], true);
        assert_eq!(v["steady_state"]["verdict"], "steady");
        assert_eq!(v["steady_state"]["seconds"], 10);
        let t = &v["transactions"][0];
        assert_eq!(t["name"], "Debit/credit");
        assert_eq!(t["committed"], 100);
        assert_eq!(t["errors"], 2);
        assert_eq!(t["throughput"], 3150);
        assert_eq!(t["latency_ms"]["p99"], 1.747);
    }

    #[test]
    fn results_csv_has_header_and_quotes_commas() {
        let (mut summary, mut stats) = sample_results();
        summary.push(TransactionSummary::new("a,b".to_string(), 1, 0, 0.0, 5.0));
        stats.push(ResponseTimeStatistics {
            name: "a,b".to_string(),
            mean: 0.0,
            min: 0.0,
            max: 0.0,
            std: 0.0,
            percentile_95: 0.0,
            percentile_99: 0.0,
        });
        let out = results_csv(&summary, &stats, "TPS");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "transaction,committed,errors,error_rate,TPS,avg_ms,min_ms,max_ms,stddev_ms,p95_ms,p99_ms"
        );
        assert_eq!(
            lines[1],
            "Debit/credit,100,2,1.961,3150,0.633,0.533,6.633,0.202,0.811,1.747"
        );
        // A name with a comma must be quoted.
        assert!(lines[2].starts_with("\"a,b\",1,0,"));
    }

    // Regression for RELEASE_REVIEW B2: the statistics must only cover the
    // steady-state window, while the per-second series keep the whole run.
    #[test]
    fn aggregate_excludes_rampup_samples_from_stats_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();

        // Two slow ramp-up samples (t=100,101), then four steady ones (t=102..103).
        let mut f = File::create(dir.join("transaction.log")).unwrap();
        write!(
            f,
            "100 1 1 50.0\n101 1 1 50.0\n102 2 1 2.0\n102 2 1 4.0\n103 2 1 2.0\n103 2 1 4.0\n"
        )
        .unwrap();
        drop(f);
        let transactions = vec![tx(1, "only")];

        let agg =
            aggregate_transaction_data("transaction.log", &dir, &transactions, Some(102), 60.0)
                .unwrap();
        let only = &agg.stats[&1];
        assert_eq!(only.mean, 3.0, "ramp-up samples must not inflate the mean");
        assert_eq!(only.max, 4.0);
        assert_eq!(only.min, 2.0);
        // The window starts at the boundary; all transactions together give
        // the same statistics here as the only one.
        assert_eq!(agg.window, vec![(102, 2), (103, 2)]);
        assert_eq!(agg.overall.mean, 3.0);

        // The per-second series still start at the first ramp-up second, and
        // carry the spec's unit: one commit in second 0 x factor 60 = 60.
        let tpm = std::fs::read_to_string(dir.join("pgmeter-throughput-only.csv")).unwrap();
        let lines: Vec<&str> = tpm.lines().collect();
        assert_eq!(lines[0], "time_s,throughput");
        assert_eq!(lines.len(), 5, "four seconds of data: {tpm}");
        assert_eq!(lines[1], "0,60");

        // Without a boundary, every sample counts. RELEASE_REVIEW C7: with a
        // per-second unit (factor 1) the series is a plain count, not x60.
        let stats = aggregate_transaction_data("transaction.log", &dir, &transactions, None, 1.0)
            .unwrap()
            .stats;
        assert_eq!(stats[&1].max, 50.0);
        let tps = std::fs::read_to_string(dir.join("pgmeter-throughput-only.csv")).unwrap();
        assert_eq!(tps.lines().nth(1), Some("0,1"));
        assert_eq!(tps.lines().nth(3), Some("2,2"));
        let all = std::fs::read_to_string(dir.join("pgmeter-throughput-all.csv")).unwrap();
        assert_eq!(all.lines().nth(3), Some("2,2"));
    }
}
