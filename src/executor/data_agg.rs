use std::path::PathBuf;
use std::collections::HashMap;

use polars::prelude::*;
use super::benchmark::{BenchmarkTransaction, ResponseTimeStatistics, TransactionSummary};
use tabled::{
    object::{Rows, Object, Columns},
    Alignment,
    ModifyObject,
    Style,
    Table,
};

// Sanitize a transaction name for use as a filename component: any character
// that isn't alphanumeric, '-' or '_' (e.g. the '/' in "Debit/credit") becomes
// '_', so the per-transaction CSV paths stay valid instead of being treated as
// nested directories.
fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

// Aggregates collected transaction data (response time, throughput) and saves it as CSV files.
pub fn aggregate_tpcc_data(log_file: &str, target_dir: &PathBuf, transactions: &Vec<BenchmarkTransaction>) -> Result<(), Box<dyn std::error::Error>> {
    // Transaction log file parsing
    let df = LazyCsvReader::new(target_dir.join(log_file))
        .with_delimiter(b' ')
        .has_header(false)
        .finish()?;

    // Produce aggregated data for each type of transaction
    for transaction in transactions {
        let safe = safe_name(&transaction.name);
        // Filter data for this transaction type
        let filtered_df = df
            .clone()
            .filter(col("column_3").eq(transaction.id as i64));

        // Check if there's any data for this transaction
        let count_df = filtered_df.clone().select([col("column_3").count()]).collect()?;
        let count: u32 = count_df.column("column_3")
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
            let mut file = std::fs::File::create(target_dir.join(format!("pgmtr-tpm-{}.csv", safe)))?;
            CsvWriter::new(&mut file).finish(&mut empty_tpm.clone())?;

            let empty_rt = DataFrame::new(vec![
                Series::new("time_s", Vec::<i64>::new()),
                Series::new("response_time_ms", Vec::<f64>::new()),
            ])?;
            let mut file = std::fs::File::create(target_dir.join(format!("pgmtr-response-time-{}.csv", safe)))?;
            CsvWriter::new(&mut file).finish(&mut empty_rt.clone())?;

            let empty_stats = DataFrame::new(vec![
                Series::new("describe", vec!["mean", "std", "min", "95%", "99%", "max"]),
                Series::new("response_time_ms", vec![0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ])?;
            let mut file = std::fs::File::create(target_dir.join(format!("pgmtr-stats-{}.csv", safe)))?;
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
        let mut file = std::fs::File::create(target_dir.join(format!("pgmtr-tpm-{}.csv", safe)))?;
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

        // Response times statistics
        let stats_result = filtered_df
            .clone()
            .select([col("column_4").alias("response_time_ms")])
            .collect();

        let mut stats = match stats_result {
            Ok(stats_df) => {
                stats_df.describe(Some(&[0.95, 0.99]))
                    .select(["describe", "response_time_ms"])
                    .unwrap_or_else(|_| DataFrame::new(vec![
                        Series::new("describe", vec!["mean", "std", "min", "95%", "99%", "max"]),
                        Series::new("response_time_ms", vec![0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0]),
                    ]).unwrap())
            },
            Err(_) => DataFrame::new(vec![
                Series::new("describe", vec!["mean", "std", "min", "95%", "99%", "max"]),
                Series::new("response_time_ms", vec![0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ])?,
        };

        // Save data as a CSV file
        let mut file = std::fs::File::create(target_dir.join(format!("pgmtr-response-time-{}.csv", safe)))?;
        CsvWriter::new(&mut file).finish(&mut response_time_df)?;
        // Save statistics
        let mut file_stats = std::fs::File::create(target_dir.join(format!("pgmtr-stats-{}.csv", safe)))?;
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
    let mut file = std::fs::File::create(target_dir.join("pgmtr-tpm-all.csv"))?;
    CsvWriter::new(&mut file).finish(&mut tpm_all_df)?;

    Ok(())
}

// Reads the CSV file containing statistics and returns them as the following structure: HashMap<transaction_id: u16, ResponseTimeStatistices>
pub fn get_stats(target_dir: &PathBuf, transactions: &Vec<BenchmarkTransaction>) -> Result<HashMap<u16, ResponseTimeStatistics>, Box<dyn std::error::Error>> {
    let labels = vec!["mean", "std", "min", "95%", "99%", "max"];

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

        let stats_file = target_dir.join(format!("pgmtr-stats-{}.csv", safe));

        // Check if stats file exists and has data
        if !stats_file.exists() {
            stats_map.insert(transaction.id, stats);
            continue;
        }

        let df = match LazyCsvReader::new(&stats_file)
            .with_delimiter(b',')
            .has_header(true)
            .finish() {
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
                .collect() {
                    Ok(r) => r,
                    Err(_) => continue,
                };

            // Try to extract the value, defaulting to 0.0 if it fails
            let value: f64 = row.column("response_time_ms")
                .ok()
                .and_then(|col| col.get(0).ok())
                .and_then(|v| v.try_extract::<f64>().ok())
                .unwrap_or(0.0);

            match *label {
                "mean" => {
                    stats.mean = value;
                },
                "min" => {
                    stats.min = value;
                },
                "max" => {
                    stats.max = value;
                },
                "std" => {
                    stats.std = value;
                },
                "95%" => {
                    stats.percentile_95 = value;
                },
                "99%" => {
                    stats.percentile_99 = value;
                },
                &_ => (),
            }
        }
        stats_map.insert(transaction.id, stats);
    }

    Ok(stats_map)
}

pub fn print_transactions_summary(data: &Vec<TransactionSummary>) {
    let mut table = Table::from_iter(data);
    let style = Style::rounded();

    table
        .with(style)
        .with(
            Rows::first()
                .modify()
                .with(Alignment::center())
        )
        .with(
            Columns::single(1)
                .not(Rows::first())
                .modify()
                .with(Alignment::right())
        )
        .with(
            Columns::single(2)
                .not(Rows::first())
                .modify()
                .with(Alignment::right())
        )
        .with(
            Columns::single(3)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3,  val=s.parse::<f64>().unwrap()))
                .with(Alignment::right())
        )
        .with(
            Columns::single(4)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{}", s.parse::<u32>().unwrap()))
                .with(Alignment::right())
        )
        .with(
            Columns::single(5)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{}", s.parse::<u32>().unwrap()))
                .with(Alignment::right())
        );

        println!("{}", table);
}

pub fn print_transactions_stats(data: &Vec<ResponseTimeStatistics>) {
    let mut table = Table::from_iter(data);
    let style = Style::rounded();

    table
        .with(style)
        .with(
            Rows::first()
                .modify()
                .with(Alignment::center())
        )
        .with(
            Columns::single(1)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3,  val=s.parse::<f64>().unwrap()))
                .with(Alignment::right())
        )
        .with(
            Columns::single(2)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3,  val=s.parse::<f64>().unwrap()))
                .with(Alignment::right())
        )
        .with(
            Columns::single(3)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3,  val=s.parse::<f64>().unwrap()))
                .with(Alignment::right())
        )
        .with(
            Columns::single(4)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3, val=s.parse::<f64>().unwrap()))
                .with(Alignment::right())
        )
        .with(
            Columns::single(5)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3, val=s.parse::<f64>().unwrap()))
                .with(Alignment::right())
        )
        .with(
            Columns::single(6)
                .not(Rows::first())
                .modify()
                .with(|s: &str| format!("{val:.*}", 3, val=s.parse::<f64>().unwrap()))
                .with(Alignment::right())
        );

        println!("{}", table);
}
