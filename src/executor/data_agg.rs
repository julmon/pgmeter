use std::collections::HashMap;
use std::path::Path;

use super::benchmark::{BenchmarkTransaction, ResponseTimeStatistics, TransactionSummary};
use polars::prelude::*;
use tabled::{
    builder::Builder,
    object::{Columns, Object, Rows},
    style::{HorizontalLine, Line},
    width::Width,
    Alignment, ModifyObject, Style, Table,
};

// Widest rendered line (in characters) of a table.
fn rendered_width<R>(table: &Table<R>) -> usize
where
    Table<R>: std::fmt::Display,
{
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
fn safe_name(name: &str) -> String {
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

// Aggregates collected transaction data (response time, throughput) and saves it as CSV files.
//
// The per-second time series (`pgmeter-tpm-*.csv`, `pgmeter-response-time-*.csv`)
// cover the whole run, ramp-up included, so they line up with the report's
// charts. The response-time statistics (`pgmeter-stats-*.csv`) only use samples
// logged at or after `rampup_end_ts`, matching the committed counts and the
// throughput, which the collector also only counts once ramp-up is over.
pub fn aggregate_transaction_data(
    log_file: &str,
    target_dir: &Path,
    transactions: &[BenchmarkTransaction],
    rampup_end_ts: Option<i64>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Transaction log file parsing
    let df = LazyCsvReader::new(target_dir.join(log_file))
        .with_delimiter(b' ')
        .has_header(false)
        .finish()?;
    // Steady-state rows only, for the statistics.
    let steady_df = match rampup_end_ts {
        Some(ts) => df.clone().filter(col("column_1").gt_eq(lit(ts))),
        None => df.clone(),
    };

    // Produce aggregated data for each type of transaction
    for transaction in transactions {
        let safe = safe_name(&transaction.name);
        // Filter data for this transaction type
        let filtered_df = df.clone().filter(col("column_3").eq(transaction.id as i64));
        let steady_filtered_df = steady_df
            .clone()
            .filter(col("column_3").eq(transaction.id as i64));

        // Check if there's any data for this transaction
        let count_df = filtered_df
            .clone()
            .select([col("column_3").count()])
            .collect()?;
        let count: u32 = count_df
            .column("column_3")
            .ok()
            .and_then(|c| c.get(0).ok())
            .and_then(|v| v.try_extract::<u32>().ok())
            .unwrap_or(0);

        if count == 0 {
            // No data for this transaction - create empty files
            let empty_tpm = DataFrame::new(vec![
                Series::new("time_s", Vec::<i64>::new()),
                Series::new("tpm", Vec::<i64>::new()),
            ])?;
            let mut file =
                std::fs::File::create(target_dir.join(format!("pgmeter-tpm-{}.csv", safe)))?;
            CsvWriter::new(&mut file).finish(&mut empty_tpm.clone())?;

            let empty_rt = DataFrame::new(vec![
                Series::new("time_s", Vec::<i64>::new()),
                Series::new("response_time_ms", Vec::<f64>::new()),
            ])?;
            let mut file = std::fs::File::create(
                target_dir.join(format!("pgmeter-response-time-{}.csv", safe)),
            )?;
            CsvWriter::new(&mut file).finish(&mut empty_rt.clone())?;

            let empty_stats = DataFrame::new(vec![
                Series::new("describe", vec!["mean", "std", "min", "95%", "99%", "max"]),
                Series::new("response_time_ms", vec![0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ])?;
            let mut file =
                std::fs::File::create(target_dir.join(format!("pgmeter-stats-{}.csv", safe)))?;
            CsvWriter::new(&mut file).finish(&mut empty_stats.clone())?;

            continue;
        }

        // Calculate transaction throughput: number of transaction over a period of time
        let mut tpm_df = filtered_df
            .clone()
            // Group by timestamp (in second) and transaction_id
            .groupby([col("column_1"), col("column_3")])
            // We want to calculate the number of transaction per minute
            .agg([col("column_4").count() * lit(60)])
            .select([
                col("column_1").alias("time_s") - col("column_1").min(),
                col("column_4").alias("tpm"),
            ])
            .sort("time_s", Default::default())
            .collect()?;

        // Save data as a CSV file
        let mut file = std::fs::File::create(target_dir.join(format!("pgmeter-tpm-{}.csv", safe)))?;
        CsvWriter::new(&mut file).finish(&mut tpm_df)?;

        // Calculate the average (mean) response time over a period of time
        let mut response_time_df = filtered_df
            .clone()
            // Group by timestamp (in second) and transaction_id
            .groupby([col("column_1"), col("column_3")])
            .agg([col("column_4").mean()])
            .select([
                col("column_1").alias("time_s") - col("column_1").min(),
                col("column_4").alias("response_time_ms"),
            ])
            .sort("time_s", Default::default())
            .collect()?;

        // Response times statistics (post-ramp-up samples only)
        let stats_result = steady_filtered_df
            .select([col("column_4").alias("response_time_ms")])
            .collect();

        let mut stats = match stats_result {
            Ok(stats_df) => stats_df
                .describe(Some(&[0.95, 0.99]))
                .select(["describe", "response_time_ms"])
                .unwrap_or_else(|_| {
                    DataFrame::new(vec![
                        Series::new("describe", vec!["mean", "std", "min", "95%", "99%", "max"]),
                        Series::new("response_time_ms", vec![0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0]),
                    ])
                    .unwrap()
                }),
            Err(_) => DataFrame::new(vec![
                Series::new("describe", vec!["mean", "std", "min", "95%", "99%", "max"]),
                Series::new("response_time_ms", vec![0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ])?,
        };

        // Save data as a CSV file
        let mut file =
            std::fs::File::create(target_dir.join(format!("pgmeter-response-time-{}.csv", safe)))?;
        CsvWriter::new(&mut file).finish(&mut response_time_df)?;
        // Save statistics
        let mut file_stats =
            std::fs::File::create(target_dir.join(format!("pgmeter-stats-{}.csv", safe)))?;
        CsvWriter::new(&mut file_stats).finish(&mut stats)?;
    }

    // Produce total TPM data, including data from all transaction types
    let mut tpm_all_df = df
        .clone()
        // Group by timestamp (in second) only
        .groupby([col("column_1")])
        .agg([col("column_4").count() * lit(60)])
        .select([
            col("column_1").alias("time_s") - col("column_1").min(),
            col("column_4").alias("tpm"),
        ])
        .sort("time_s", Default::default())
        .collect()?;

    // Save data as a CSV file
    let mut file = std::fs::File::create(target_dir.join("pgmeter-tpm-all.csv"))?;
    CsvWriter::new(&mut file).finish(&mut tpm_all_df)?;

    Ok(())
}

// Reads the CSV file containing statistics and returns them as the following structure: HashMap<transaction_id: u16, ResponseTimeStatistices>
pub fn get_stats(
    target_dir: &Path,
    transactions: &[BenchmarkTransaction],
) -> Result<HashMap<u16, ResponseTimeStatistics>, Box<dyn std::error::Error>> {
    let labels = ["mean", "std", "min", "95%", "99%", "max"];

    let mut stats_map: HashMap<u16, ResponseTimeStatistics> = HashMap::new();

    for transaction in transactions {
        let safe = safe_name(&transaction.name);
        let mut stats = ResponseTimeStatistics {
            name: transaction.name.clone(),
            min: 0.0,
            mean: 0.0,
            max: 0.0,
            std: 0.0,
            percentile_95: 0.0,
            percentile_99: 0.0,
        };

        let stats_file = target_dir.join(format!("pgmeter-stats-{}.csv", safe));

        // Check if stats file exists and has data
        if !stats_file.exists() {
            stats_map.insert(transaction.id, stats);
            continue;
        }

        let df = match LazyCsvReader::new(&stats_file)
            .with_delimiter(b',')
            .has_header(true)
            .finish()
        {
            Ok(df) => df,
            Err(_) => {
                stats_map.insert(transaction.id, stats);
                continue;
            }
        };

        for label in labels.iter() {
            let row = match df
                .clone()
                .filter(col("describe").eq(lit(*label)))
                .first()
                .select([col("response_time_ms")])
                .collect()
            {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Try to extract the value, defaulting to 0.0 if it fails
            let value: f64 = row
                .column("response_time_ms")
                .ok()
                .and_then(|col| col.get(0).ok())
                .and_then(|v| v.try_extract::<f64>().ok())
                .unwrap_or(0.0);

            match *label {
                "mean" => {
                    stats.mean = value;
                }
                "min" => {
                    stats.min = value;
                }
                "max" => {
                    stats.max = value;
                }
                "std" => {
                    stats.std = value;
                }
                "95%" => {
                    stats.percentile_95 = value;
                }
                "99%" => {
                    stats.percentile_99 = value;
                }
                &_ => (),
            }
        }
        stats_map.insert(transaction.id, stats);
    }

    Ok(stats_map)
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
    let header_rule = || {
        Style::blank()
            .off_vertical()
            .horizontals([HorizontalLine::new(1, Line::filled('─'))])
    };

    // Summary table.
    let mut builder = Builder::default();
    builder.set_columns([
        "Transaction",
        "Committed",
        "Errors",
        "Error rate (%)",
        throughput_unit,
    ]);
    for row in summary {
        builder.add_record([
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
        .with(Rows::first().modify().with(Alignment::center()))
        .with(
            Columns::new(1..)
                .not(Rows::first())
                .modify()
                .with(Alignment::right()),
        );

    // Response-time stats table.
    let mut stats_table = Table::from_iter(stats);
    stats_table
        .with(header_rule())
        .with(Rows::first().modify().with(Alignment::center()));
    for col in 1..=6 {
        stats_table.with(
            Columns::single(col)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3, val = s.parse::<f64>().unwrap()))
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
            description: String::new(),
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
        let out = results_json(&summary, &stats, "TPS", 30.2);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["throughput_unit"], "TPS");
        assert_eq!(v["duration_s"], 30);
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
        let dir = std::env::temp_dir().join(format!("pgmeter-agg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Two slow ramp-up samples (t=100,101), then four steady ones (t=102..103).
        let mut f = File::create(dir.join("transaction.log")).unwrap();
        write!(
            f,
            "100 1 1 50.0\n101 1 1 50.0\n102 2 1 2.0\n102 2 1 4.0\n103 2 1 2.0\n103 2 1 4.0\n"
        )
        .unwrap();
        drop(f);
        let transactions = vec![tx(1, "only")];

        aggregate_transaction_data("transaction.log", &dir, &transactions, Some(102)).unwrap();

        let stats = get_stats(&dir, &transactions).unwrap();
        let only = &stats[&1];
        assert_eq!(only.mean, 3.0, "ramp-up samples must not inflate the mean");
        assert_eq!(only.max, 4.0);
        assert_eq!(only.min, 2.0);

        // The per-second series still start at the first ramp-up second.
        let tpm = std::fs::read_to_string(dir.join("pgmeter-tpm-only.csv")).unwrap();
        let lines: Vec<&str> = tpm.lines().collect();
        assert_eq!(lines[0], "time_s,tpm");
        assert_eq!(lines.len(), 5, "four seconds of data: {tpm}");
        assert_eq!(lines[1], "0,60");

        // Without a boundary, every sample counts.
        aggregate_transaction_data("transaction.log", &dir, &transactions, None).unwrap();
        let stats = get_stats(&dir, &transactions).unwrap();
        assert_eq!(stats[&1].max, 50.0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn get_stats_reads_csv_and_defaults_when_missing() {
        let dir = std::env::temp_dir().join(format!("pgmeter-stats-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // safe_name("tx/one") == "tx_one" -> file must use the sanitized name.
        let mut f = File::create(dir.join("pgmeter-stats-tx_one.csv")).unwrap();
        writeln!(f, "describe,response_time_ms").unwrap();
        writeln!(f, "mean,1.5").unwrap();
        writeln!(f, "std,0.5").unwrap();
        writeln!(f, "min,0.1").unwrap();
        writeln!(f, "95%,2.0").unwrap();
        writeln!(f, "99%,3.0").unwrap();
        writeln!(f, "max,9.9").unwrap();
        drop(f);

        let transactions = vec![tx(1, "tx/one"), tx(2, "missing")];
        let stats = get_stats(&dir, &transactions).unwrap();

        let one = &stats[&1];
        assert_eq!(one.mean, 1.5);
        assert_eq!(one.std, 0.5);
        assert_eq!(one.min, 0.1);
        assert_eq!(one.percentile_95, 2.0);
        assert_eq!(one.percentile_99, 3.0);
        assert_eq!(one.max, 9.9);

        // No file for tx 2 -> all-zero stats, but still present.
        let two = &stats[&2];
        assert_eq!(two.mean, 0.0);
        assert_eq!(two.max, 0.0);

        std::fs::remove_dir_all(&dir).ok();
    }
}
