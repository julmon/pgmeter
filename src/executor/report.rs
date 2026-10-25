use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use postgres::Client;
use serde::Serialize;
use serde_json::json;

use super::benchmark::{BenchmarkTransaction, Counter, ResponseTimeStatistics};
use super::error::Error;
use super::summary;
use super::yaml_types::BenchmarkSpec;

const CHART_JS: &str = include_str!("assets/chart.umd.min.js");
const REPORT_FILE: &str = "report.html";

// Server settings surfaced in the report header.
const SERVER_SETTINGS: &[&str] = &[
    "server_version",
    "shared_buffers",
    "work_mem",
    "max_wal_size",
    "max_connections",
    "effective_cache_size",
];

// Everything passed from the executor to build the report.
pub struct ReportInput<'a> {
    pub target_dir: &'a PathBuf,
    pub log_file: &'a str,
    pub error_file: &'a str,
    pub spec: &'a BenchmarkSpec,
    pub transactions: &'a [BenchmarkTransaction],
    pub counters: &'a HashMap<u16, Counter>,
    pub stats: &'a HashMap<u16, ResponseTimeStatistics>,
    pub total_time_ms: u128,
    pub rampup_time_ms: u128,
    /// Log timestamp at which ramp-up ended; earlier samples are excluded from
    /// the histogram and shaded on the time-series charts.
    pub rampup_end_ts: Option<i64>,
    /// The workload was cut short by Ctrl-C; the figures describe the partial run.
    pub interrupted: bool,
    pub clients: u16,
    pub duration_s: u16,
    pub rampup_s: u16,
}

#[derive(Serialize)]
struct TxRow {
    name: String,
    /// The spec's free-text description, shown as the row's tooltip.
    description: String,
    committed: u64,
    errors: u64,
    error_rate: f64,
    throughput: u64,
    mean: f64,
    min: f64,
    max: f64,
    std: f64,
    p95: f64,
    p99: f64,
}

// Per-transaction time series aligned on a shared time axis.
struct Series {
    name: String,
    tpm: Vec<u64>,
    latency: Vec<f64>,
}

// Generate the self-contained HTML report into the run's target directory.
pub fn generate(input: &ReportInput, client: &mut Client) -> Result<PathBuf, Error> {
    let server = fetch_server_info(client);
    let data_info = fetch_data_info(client, input.spec);

    // Headline throughput metric: unit label + time base, configured per spec.
    let throughput_factor = input.spec.metrics.per.factor();
    let throughput_unit = input.spec.metrics.throughput_unit.clone();

    // Same computation as the terminal/JSON/CSV outputs, so every output agrees.
    let summary = summary::summarize(
        input.transactions,
        input.counters,
        input.total_time_ms,
        input.rampup_time_ms,
        throughput_factor,
    );

    // Per-transaction summary + statistics rows (post-rampup counters).
    let tx_rows: Vec<TxRow> = input
        .transactions
        .iter()
        .zip(summary.transactions.iter())
        .map(|(tx, s)| {
            let default_stats = ResponseTimeStatistics {
                name: tx.name.clone(),
                mean: 0.0,
                min: 0.0,
                max: 0.0,
                std: 0.0,
                percentile_95: 0.0,
                percentile_99: 0.0,
            };
            let stats = input.stats.get(&tx.id).unwrap_or(&default_stats);
            TxRow {
                name: tx.name.clone(),
                description: input
                    .spec
                    .transactions
                    .iter()
                    .find(|t| t.id == tx.id)
                    .map(|t| t.description.clone())
                    .unwrap_or_default(),
                committed: s.n_commits,
                errors: s.n_errors,
                error_rate: s.error_rate,
                throughput: s.throughput.round() as u64,
                mean: stats.mean,
                min: stats.min,
                max: stats.max,
                std: stats.std,
                p95: stats.percentile_95,
                p99: stats.percentile_99,
            }
        })
        .collect();

    // Time series and histogram parsed directly from the raw logs so that every
    // chart shares a single time origin.
    let ts = build_time_series(input, throughput_factor)?;
    let peak_throughput = ts.total_tpm.iter().copied().fold(0u64, u64::max);

    let palette = [
        "#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7",
        "#9c755f", "#bab0ac",
    ];

    let per_tx_throughput: Vec<_> = ts
        .series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "name": s.name,
                "color": palette[i % palette.len()],
                "data": s.tpm,
            })
        })
        .collect();
    let per_tx_latency: Vec<_> = ts
        .series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "name": s.name,
                "color": palette[i % palette.len()],
                "data": s.latency,
            })
        })
        .collect();

    let mix: Vec<_> = tx_rows
        .iter()
        .filter(|r| r.committed > 0)
        .map(|r| json!({"name": r.name, "value": r.committed}))
        .collect();
    let percentiles: Vec<_> = tx_rows
        .iter()
        .map(|r| json!({"name": r.name, "mean": r.mean, "p95": r.p95, "p99": r.p99, "max": r.max}))
        .collect();

    let generated_at: DateTime<Utc> = Utc::now();

    let data = json!({
        "meta": {
            "name": input.spec.name,
            "description": input.spec.description.clone().unwrap_or_default(),
            "generatedAt": generated_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            "clients": input.clients,
            "duration": input.duration_s,
            "rampup": input.rampup_s,
            "rampupEndS": ts.rampup_end_s,
            "interrupted": input.interrupted,
            "elapsedMs": input.total_time_ms,
            "scalingUnit": input.spec.scaling.unit,
            "throughputUnit": throughput_unit,
        },
        "server": server,
        "dataInfo": data_info,
        "kpis": {
            "throughput": summary.throughput.round() as u64,
            "peakThroughput": peak_throughput,
            "committed": summary.total_commits,
            "errors": summary.total_errors,
            "errorRate": summary.error_rate,
            "avgLatency": summary.avg_latency_ms,
        },
        "transactions": tx_rows,
        "timeAxis": ts.time_axis,
        "throughput": {
            "total": ts.total_tpm,
            "perTx": per_tx_throughput,
        },
        "latency": {
            "perTx": per_tx_latency,
        },
        "percentiles": percentiles,
        "histogram": {
            "labels": ts.hist_labels,
            "counts": ts.hist_counts,
        },
        "mix": mix,
        "errors": {
            "timeAxis": ts.error_time_axis,
            "counts": ts.error_counts,
        },
    });

    let json_blob = serde_json::to_string(&data)?;
    let html = render_html(&json_blob);

    let report_path = input.target_dir.join(REPORT_FILE);
    let mut file = File::create(&report_path)?;
    file.write_all(html.as_bytes())?;

    Ok(report_path)
}

#[derive(Default)]
struct TimeSeries {
    time_axis: Vec<u64>,
    /// Elapsed seconds (on `time_axis`) at which ramp-up ended, if any sample
    /// precedes it.
    rampup_end_s: Option<u64>,
    total_tpm: Vec<u64>,
    series: Vec<Series>,
    hist_labels: Vec<String>,
    hist_counts: Vec<u64>,
    error_time_axis: Vec<u64>,
    error_counts: Vec<u64>,
}

// Number of points each time series is capped at. Longer runs are aggregated
// into wider buckets so charts stay readable and the embedded JSON stays small.
const TARGET_BUCKETS: u64 = 1000;

// Derive every time series from transaction.log in two streaming passes (so the
// whole log is never held in memory): pass 1 finds the time span, pass 2
// aggregates rows into at most TARGET_BUCKETS time buckets plus a
// bounded-memory latency histogram. All series share the earliest timestamp as
// their origin.
fn build_time_series(input: &ReportInput, factor: f64) -> Result<TimeSeries, Error> {
    let name_by_id: HashMap<u16, String> = input
        .transactions
        .iter()
        .map(|t| (t.id, t.name.clone()))
        .collect();
    let log_path = input.target_dir.join(input.log_file);

    // Pass 1: find the time span without materialising any rows.
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    if let Ok(f) = File::open(&log_path) {
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if let Some(ts) = line
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<i64>().ok())
            {
                min_ts = min_ts.min(ts);
                max_ts = max_ts.max(ts);
            }
        }
    }
    if min_ts == i64::MAX {
        return Ok(TimeSeries::default());
    }

    let total_secs = (max_ts - min_ts) as u64 + 1;
    let bucket_secs = total_secs.div_ceil(TARGET_BUCKETS).max(1);
    let n_buckets = total_secs.div_ceil(bucket_secs) as usize;
    let bw = bucket_secs as f64;

    let mut total_count = vec![0u64; n_buckets];
    let mut counts: HashMap<(u16, u64), u64> = HashMap::new();
    let mut lat_sum: HashMap<(u16, u64), f64> = HashMap::new();
    let mut tx_ids: BTreeSet<u16> = BTreeSet::new();
    let mut hist = LatencyHistogram::new();

    // Pass 2: aggregate into buckets and the histogram, still streaming.
    if let Ok(f) = File::open(&log_path) {
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let mut parts = line.split_whitespace();
            let ts = parts.next().and_then(|v| v.parse::<i64>().ok());
            let _client = parts.next();
            let tx_id = parts.next().and_then(|v| v.parse::<u16>().ok());
            let dur = parts.next().and_then(|v| v.parse::<f64>().ok());
            if let (Some(ts), Some(tx_id), Some(dur)) = (ts, tx_id, dur) {
                let bucket = ((ts - min_ts) as u64) / bucket_secs;
                total_count[bucket as usize] += 1;
                *counts.entry((tx_id, bucket)).or_insert(0) += 1;
                *lat_sum.entry((tx_id, bucket)).or_insert(0.0) += dur;
                tx_ids.insert(tx_id);
                // The histogram, like the statistics, only covers steady state.
                if input.rampup_end_ts.is_none_or(|end| ts >= end) {
                    hist.add(dur);
                }
            }
        }
    }

    // X axis is each bucket's start second, so "Elapsed (s)" stays meaningful.
    let time_axis: Vec<u64> = (0..n_buckets as u64).map(|b| b * bucket_secs).collect();
    // Throughput is a per-second rate (count / bucket width) scaled by the unit factor.
    let total_tpm: Vec<u64> = total_count
        .iter()
        .map(|c| (*c as f64 / bw * factor) as u64)
        .collect();

    // Build one aligned series per transaction, preserving spec order.
    let mut series: Vec<Series> = Vec::new();
    for tx in input.transactions {
        if !tx_ids.contains(&tx.id) {
            continue;
        }
        let mut tpm = vec![0u64; n_buckets];
        let mut latency = vec![0.0f64; n_buckets];
        for b in 0..n_buckets as u64 {
            let c = counts.get(&(tx.id, b)).copied().unwrap_or(0);
            tpm[b as usize] = (c as f64 / bw * factor) as u64;
            if c > 0 {
                latency[b as usize] = lat_sum.get(&(tx.id, b)).copied().unwrap_or(0.0) / c as f64;
            }
        }
        let name = name_by_id
            .get(&tx.id)
            .cloned()
            .unwrap_or_else(|| format!("tx-{}", tx.id));
        series.push(Series { name, tpm, latency });
    }

    let (hist_labels, hist_counts) = hist.finish();
    let (error_time_axis, error_counts) = build_error_series(input, min_ts, bucket_secs);
    let rampup_end_s = input
        .rampup_end_ts
        .filter(|end| *end > min_ts)
        .map(|end| (end - min_ts) as u64);

    Ok(TimeSeries {
        time_axis,
        rampup_end_s,
        total_tpm,
        series,
        hist_labels,
        hist_counts,
        error_time_axis,
        error_counts,
    })
}

// Bounded-memory latency histogram: durations are tallied into exponentially
// spaced bins (fixed count, independent of sample size). finish() caps the
// display range at the 99th percentile with a trailing overflow bucket so a
// few outliers don't flatten the distribution.
struct LatencyHistogram {
    counts: Vec<u64>,
    total: u64,
}
const HIST_MIN_MS: f64 = 0.05;
const HIST_MAX_MS: f64 = 600_000.0;
const HIST_GROWTH: f64 = 1.015_625; // 1 + 1/64
impl LatencyHistogram {
    fn n_bins() -> usize {
        ((HIST_MAX_MS / HIST_MIN_MS).ln() / HIST_GROWTH.ln()).ceil() as usize + 2
    }
    fn new() -> Self {
        LatencyHistogram {
            counts: vec![0; Self::n_bins()],
            total: 0,
        }
    }
    fn upper_edge(i: usize) -> f64 {
        if i == 0 {
            HIST_MIN_MS
        } else {
            HIST_MIN_MS * HIST_GROWTH.powi(i as i32)
        }
    }
    fn center(i: usize) -> f64 {
        if i == 0 {
            HIST_MIN_MS * 0.5
        } else {
            (Self::upper_edge(i - 1) * Self::upper_edge(i)).sqrt()
        }
    }
    fn add(&mut self, d: f64) {
        let last = self.counts.len() - 1;
        let bin = if d <= HIST_MIN_MS {
            0
        } else if d >= HIST_MAX_MS {
            last
        } else {
            (1 + ((d / HIST_MIN_MS).ln() / HIST_GROWTH.ln()).floor() as usize).min(last)
        };
        self.counts[bin] += 1;
        self.total += 1;
    }
    fn finish(self) -> (Vec<String>, Vec<u64>) {
        const BINS: usize = 30;
        if self.total == 0 {
            return (Vec::new(), Vec::new());
        }
        let target = (self.total as f64 * 0.99).ceil() as u64;
        let mut cum = 0u64;
        let mut cap = HIST_MIN_MS;
        for (i, c) in self.counts.iter().enumerate() {
            cum += c;
            if cum >= target {
                cap = Self::upper_edge(i).max(0.001);
                break;
            }
        }
        let width = cap / BINS as f64;
        let mut out = vec![0u64; BINS + 1];
        for (i, c) in self.counts.iter().enumerate() {
            if *c == 0 {
                continue;
            }
            let v = Self::center(i);
            let bin = if v >= cap {
                BINS
            } else {
                ((v / width) as usize).min(BINS - 1)
            };
            out[bin] += c;
        }
        let mut labels: Vec<String> = (0..BINS)
            .map(|i| format!("{:.1}", (i as f64 + 0.5) * width))
            .collect();
        labels.push(format!(">{:.1}", cap));
        (labels, out)
    }
}

// Count errors per time bucket relative to the transaction-log origin.
fn build_error_series(input: &ReportInput, min_ts: i64, bucket_secs: u64) -> (Vec<u64>, Vec<u64>) {
    let path = input.target_dir.join(input.error_file);
    let mut by_bucket: HashMap<u64, u64> = HashMap::new();
    let mut max_bucket: u64 = 0;
    let mut any = false;
    if let Ok(f) = File::open(&path) {
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if let Some(ts) = line
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<i64>().ok())
            {
                let sec = (ts - min_ts).max(0) as u64;
                let bucket = sec / bucket_secs;
                *by_bucket.entry(bucket).or_insert(0) += 1;
                max_bucket = max_bucket.max(bucket);
                any = true;
            }
        }
    }
    if !any {
        return (Vec::new(), Vec::new());
    }
    let axis: Vec<u64> = (0..=max_bucket).map(|b| b * bucket_secs).collect();
    let counts: Vec<u64> = (0..=max_bucket)
        .map(|b| by_bucket.get(&b).copied().unwrap_or(0))
        .collect();
    (axis, counts)
}

// Query PostgreSQL version and a handful of relevant tuning settings.
fn fetch_server_info(client: &mut Client) -> serde_json::Value {
    let mut info = serde_json::Map::new();

    match client.query_one("SELECT version()", &[]) {
        Ok(row) => {
            let v: String = row.get(0);
            info.insert("version".to_string(), json!(v));
        }
        Err(_) => {
            info.insert("version".to_string(), json!("unknown"));
        }
    }

    let mut settings = serde_json::Map::new();
    for name in SERVER_SETTINGS {
        let value = client
            .query_one("SELECT current_setting($1, true)", &[name])
            .ok()
            .and_then(|row| row.get::<_, Option<String>>(0))
            .unwrap_or_else(|| "-".to_string());
        settings.insert(name.to_string(), json!(value));
    }
    info.insert("settings".to_string(), serde_json::Value::Object(settings));

    let dbname: String = client
        .query_one("SELECT current_database()", &[])
        .ok()
        .map(|r| r.get(0))
        .unwrap_or_default();
    info.insert("database".to_string(), json!(dbname));

    serde_json::Value::Object(info)
}

// Inspect the loaded dataset from the catalog: scale factor (via the spec's own
// scaling query), per-table size / access method / storage options, and the
// size of every index on those tables.
fn fetch_data_info(client: &mut Client, spec: &BenchmarkSpec) -> serde_json::Value {
    let mut info = serde_json::Map::new();

    // The scaling query yields the dataset's max_id, i.e. its size in the
    // configured scaling unit (warehouses, scalefactor, ...).
    let scale: Option<i64> = match client.query_one(&spec.scaling.max_id_query, &[]) {
        Ok(row) => row
            .try_get::<_, i64>(0)
            .ok()
            .or_else(|| row.try_get::<_, i32>(0).ok().map(|v| v as i64)),
        Err(_) => None,
    };
    info.insert("scaleFactor".to_string(), json!(scale));
    info.insert("scalingUnit".to_string(), json!(spec.scaling.unit));

    let table_names: Vec<String> = spec.schema.tables.iter().map(|t| t.name.clone()).collect();

    // Both queries restrict the match to the schemas on the connection's
    // search_path (what the benchmark's unqualified table names resolve to),
    // so a same-named table in another schema is not listed.
    let tables_sql = "SELECT c.relname, \
            pg_table_size(c.oid)::int8, \
            pg_indexes_size(c.oid)::int8, \
            coalesce(am.amname, 'heap'), \
            coalesce(array_to_string(c.reloptions, ', '), ''), \
            c.reltuples::int8 \
        FROM pg_class c \
        JOIN pg_namespace n ON n.oid = c.relnamespace \
        LEFT JOIN pg_am am ON am.oid = c.relam \
        WHERE c.relkind IN ('r', 'p') AND c.relname = ANY($1) \
          AND n.nspname = ANY(current_schemas(false)) \
        ORDER BY pg_table_size(c.oid) DESC";
    let mut tables = Vec::new();
    if let Ok(rows) = client.query(tables_sql, &[&table_names]) {
        for row in rows {
            tables.push(json!({
                "name": row.get::<_, String>(0),
                "tableBytes": row.get::<_, i64>(1),
                "indexBytes": row.get::<_, i64>(2),
                "accessMethod": row.get::<_, String>(3),
                "storage": row.get::<_, String>(4),
                "rows": row.get::<_, i64>(5),
            }));
        }
    }
    info.insert("tables".to_string(), json!(tables));

    let indexes_sql = "SELECT i.relname, t.relname, \
            pg_relation_size(i.oid)::int8, \
            coalesce(am.amname, ''), \
            idx.indisunique, idx.indisprimary \
        FROM pg_index idx \
        JOIN pg_class i ON i.oid = idx.indexrelid \
        JOIN pg_class t ON t.oid = idx.indrelid \
        JOIN pg_namespace n ON n.oid = t.relnamespace \
        LEFT JOIN pg_am am ON am.oid = i.relam \
        WHERE t.relname = ANY($1) \
          AND n.nspname = ANY(current_schemas(false)) \
        ORDER BY pg_relation_size(i.oid) DESC";
    let mut indexes = Vec::new();
    if let Ok(rows) = client.query(indexes_sql, &[&table_names]) {
        for row in rows {
            indexes.push(json!({
                "name": row.get::<_, String>(0),
                "table": row.get::<_, String>(1),
                "bytes": row.get::<_, i64>(2),
                "method": row.get::<_, String>(3),
                "unique": row.get::<_, bool>(4),
                "primary": row.get::<_, bool>(5),
            }));
        }
    }
    info.insert("indexes".to_string(), json!(indexes));

    serde_json::Value::Object(info)
}

fn render_html(json_blob: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>pgmeter report</title>
<style>
  :root {{
    --bg: #0f172a; --panel: #1e293b; --panel2: #273449; --text: #e2e8f0;
    --muted: #94a3b8; --accent: #38bdf8; --border: #334155; --good: #4ade80; --bad: #f87171;
  }}
  :root[data-theme="light"] {{
    --bg: #f8fafc; --panel: #ffffff; --panel2: #eef2f7; --text: #1e293b;
    --muted: #64748b; --accent: #0284c7; --border: #d8dee9; --good: #16a34a; --bad: #dc2626;
  }}
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; background: var(--bg); color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }}
  .wrap {{ max-width: 1200px; margin: 0 auto; padding: 32px 20px 64px; }}
  h1 {{ font-size: 26px; margin: 0 0 4px; }}
  h2 {{ font-size: 18px; margin: 36px 0 14px; border-bottom: 1px solid var(--border); padding-bottom: 8px; }}
  .desc {{ color: var(--muted); margin: 0 0 20px; }}
  .grid {{ display: grid; gap: 16px; }}
  .meta-grid {{ grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); }}
  .kpi-grid {{ grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); }}
  .chart-grid {{ grid-template-columns: repeat(auto-fit, minmax(440px, 1fr)); }}
  .card {{ background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 16px; }}
  .kpi .label {{ color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: .04em; }}
  .kpi .value {{ font-size: 28px; font-weight: 700; margin-top: 6px; }}
  .meta .k {{ color: var(--muted); font-size: 12px; }}
  .meta .v {{ font-size: 14px; margin-top: 2px; word-break: break-word; }}
  table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}
  th, td {{ padding: 8px 10px; text-align: right; border-bottom: 1px solid var(--border); }}
  th:first-child, td:first-child {{ text-align: left; }}
  thead th {{ color: var(--muted); font-weight: 600; text-transform: uppercase; font-size: 11px; letter-spacing: .04em; }}
  tbody tr:hover {{ background: var(--panel2); }}
  .chart-card {{ position: relative; height: 320px; }}
  .chart-card canvas {{ max-height: 280px; }}
  /* Line/pie charts get a taller card and a scrollable external legend so many
     series (e.g. PGM-H's 22 queries) don't crowd the plot area. The plot lives
     in a fixed-height wrapper so populating the legend can't shrink the canvas
     after Chart.js has rendered (which would scale and crush the bitmap). */
  .line-card {{ height: 420px; display: flex; flex-direction: column; }}
  .line-card .cwrap {{ position: relative; height: 300px; flex: 0 0 auto; }}
  .line-card .cwrap canvas {{ position: absolute; inset: 0; max-height: none; }}
  .legend {{ flex: 1 1 auto; min-height: 0; max-height: 92px; overflow-y: auto; display: flex;
    flex-wrap: wrap; gap: 4px 14px; padding-top: 10px; margin-top: 6px;
    border-top: 1px solid var(--border); }}
  .legend .item {{ display: flex; align-items: center; gap: 6px; font-size: 12px;
    cursor: pointer; user-select: none; }}
  .legend .item.hidden {{ opacity: .4; text-decoration: line-through; }}
  .legend .swatch {{ width: 12px; height: 12px; border-radius: 2px; flex: 0 0 auto; }}
  .dlbtn {{ position: absolute; top: 8px; right: 8px; z-index: 5; background: var(--panel2);
    color: var(--muted); border: 1px solid var(--border); border-radius: 6px; font-size: 11px;
    padding: 3px 8px; cursor: pointer; opacity: 0; transition: opacity .15s; }}
  .chart-card:hover .dlbtn {{ opacity: 1; }}
  .dlbtn:hover {{ color: var(--text); border-color: var(--accent); }}
  .page-head {{ display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }}
  .theme-toggle {{ color: var(--muted); font-size: 12px; display: flex; align-items: center; gap: 6px;
    flex: 0 0 auto; }}
  .theme-toggle select {{ background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    border-radius: 6px; padding: 5px 8px; font-size: 12px; }}
  .charts-head {{ display: flex; align-items: center; justify-content: space-between; gap: 16px; }}
  .charts-head button {{ background: var(--panel2); color: var(--text); border: 1px solid var(--border);
    border-radius: 6px; font-size: 12px; padding: 6px 12px; cursor: pointer; }}
  .charts-head button:hover {{ border-color: var(--accent); }}
  .settings {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; }}
  .settings td:last-child {{ color: var(--accent); }}
  footer {{ margin-top: 40px; color: var(--muted); font-size: 12px; text-align: center; }}
</style>
</head>
<body>
<div class="wrap">
  <div class="page-head">
    <div><h1 id="title"></h1><p class="desc" id="description"></p></div>
    <label class="theme-toggle">Theme
      <select id="theme-select"><option value="dark">Dark</option><option value="light">Light</option></select>
    </label>
  </div>

  <h2>Environment</h2>
  <div class="grid meta-grid" id="meta"></div>
  <div class="card" style="margin-top:16px">
    <table class="settings"><tbody id="settings"></tbody></table>
  </div>

  <h2>Data</h2>
  <div class="grid kpi-grid" id="data-kpis"></div>
  <div class="card" style="overflow-x:auto; margin-top:16px">
    <table>
      <thead><tr>
        <th>Table</th><th>Access method</th><th>Storage</th>
        <th>Rows (est.)</th><th>Table size</th><th>Index size</th>
      </tr></thead>
      <tbody id="tables-body"></tbody>
    </table>
  </div>
  <div class="card" style="overflow-x:auto; margin-top:16px">
    <table>
      <thead><tr>
        <th>Index</th><th>Table</th><th>Method</th><th>Properties</th><th>Size</th>
      </tr></thead>
      <tbody id="indexes-body"></tbody>
    </table>
  </div>

  <h2>Summary</h2>
  <div class="grid kpi-grid" id="kpis"></div>

  <h2>Per-transaction results</h2>
  <div class="card" style="overflow-x:auto">
    <table>
      <thead><tr>
        <th>Transaction</th><th>Committed</th><th>Errors</th><th>Error %</th>
        <th id="tx-throughput-head">TPM</th><th>Avg ms</th><th>p95 ms</th><th>p99 ms</th><th>Max ms</th>
      </tr></thead>
      <tbody id="txbody"></tbody>
    </table>
  </div>

  <div class="charts-head"><h2>Charts</h2><button id="dl-all">Download all charts (PNG)</button></div>
  <div class="grid chart-grid">
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="throughput"></canvas></div><div class="legend" id="throughput-legend"></div></div>
    <div class="card chart-card line-card"><div class="cwrap"><canvas id="latency"></canvas></div><div class="legend" id="latency-legend"></div></div>
    <div class="card chart-card" id="card-percentiles"><canvas id="percentiles"></canvas></div>
    <div class="card chart-card"><canvas id="histogram"></canvas></div>
    <div class="card chart-card line-card" id="card-mix"><div class="cwrap"><canvas id="mix"></canvas></div><div class="legend" id="mix-legend"></div></div>
    <div class="card chart-card"><canvas id="errors"></canvas></div>
  </div>

  <footer>Generated by pgmeter</footer>
</div>

<script>{chart_js}</script>
<script>
const DATA = {data};
{app_js}
</script>
</body>
</html>
"##,
        chart_js = CHART_JS,
        data = json_blob,
        app_js = APP_JS,
    )
}

const APP_JS: &str = r##"
const fmt = (n, d = 0) => Number(n).toLocaleString(undefined, {minimumFractionDigits: d, maximumFractionDigits: d});
// Every string that comes from the spec or the server (names, settings,
// storage options) goes through innerHTML templates below; escape it so a
// table named `<img src=x onerror=...>` renders as text instead of running.
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'}[c]));
const humanBytes = (b) => {
  b = Number(b);
  if (!isFinite(b)) return '-';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
  return b.toFixed(i === 0 ? 0 : 1) + ' ' + u[i];
};

// Renders a Chart.js legend into a scrollable external container so charts with
// many series stay readable. Clicking an entry toggles that series.
const htmlLegendPlugin = {
  id: 'htmlLegend',
  afterUpdate(chart, _args, opts) {
    const box = document.getElementById(opts.containerID);
    if (!box) return;
    box.innerHTML = '';
    const isPie = chart.config.type === 'doughnut' || chart.config.type === 'pie';
    const items = chart.options.plugins.legend.labels.generateLabels(chart);
    items.forEach(item => {
      const el = document.createElement('div');
      el.className = 'item' + (item.hidden ? ' hidden' : '');
      const sw = document.createElement('span');
      sw.className = 'swatch';
      // Pie/doughnut: each entry is a segment (fill color). Line/bar: a dataset (line color).
      sw.style.background = isPie ? item.fillStyle : (item.strokeStyle || item.fillStyle);
      const txt = document.createElement('span');
      txt.textContent = item.text;
      el.appendChild(sw);
      el.appendChild(txt);
      el.onclick = () => {
        if (isPie) {
          chart.toggleDataVisibility(item.index);
        } else {
          chart.setDatasetVisibility(item.datasetIndex, !chart.isDatasetVisible(item.datasetIndex));
        }
        chart.update();
      };
      box.appendChild(el);
    });
  }
};

Chart.defaults.color = '#94a3b8';
Chart.defaults.borderColor = '#334155';
Chart.defaults.font.family = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif";

document.getElementById('title').textContent = DATA.meta.name || 'pgmeter report';
document.getElementById('description').textContent = DATA.meta.description || '';

const meta = document.getElementById('meta');
const metaItems = [
  ['Generated', DATA.meta.generatedAt],
  ['Database', DATA.server.database || '-'],
  ['Clients', DATA.meta.clients],
  ['Duration (s)', DATA.meta.duration],
  ['Ramp-up (s)', DATA.meta.rampup],
  ['Status', DATA.meta.interrupted ? 'interrupted by Ctrl-C (partial run)' : 'completed'],
  ['Elapsed (s)', (DATA.meta.elapsedMs / 1000).toFixed(1)],
  ['Scaling unit', DATA.meta.scalingUnit || '-'],
  ['PostgreSQL', DATA.server.version || '-'],
];
for (const [k, v] of metaItems) {
  const el = document.createElement('div');
  el.className = 'card meta';
  el.innerHTML = `<div class="k">${esc(k)}</div><div class="v">${esc(v)}</div>`;
  meta.appendChild(el);
}

const settings = document.getElementById('settings');
for (const [k, v] of Object.entries(DATA.server.settings || {})) {
  const tr = document.createElement('tr');
  tr.innerHTML = `<td style="text-align:left">${esc(k)}</td><td>${esc(v)}</td>`;
  settings.appendChild(tr);
}

const di = DATA.dataInfo || {};
const diTables = di.tables || [];
const diIndexes = di.indexes || [];
const totalTableBytes = diTables.reduce((a, t) => a + Number(t.tableBytes || 0), 0);
const totalIndexBytes = diTables.reduce((a, t) => a + Number(t.indexBytes || 0), 0);
const dataKpis = document.getElementById('data-kpis');
const dataKpiItems = [
  ['Scale factor', di.scaleFactor != null ? fmt(di.scaleFactor) : '-'],
  ['Scaling unit', di.scalingUnit || '-'],
  ['Tables', fmt(diTables.length)],
  ['Indexes', fmt(diIndexes.length)],
  ['Total table size', humanBytes(totalTableBytes)],
  ['Total index size', humanBytes(totalIndexBytes)],
];
for (const [k, v] of dataKpiItems) {
  const el = document.createElement('div');
  el.className = 'card kpi';
  el.innerHTML = `<div class="label">${esc(k)}</div><div class="value" style="font-size:20px">${esc(v)}</div>`;
  dataKpis.appendChild(el);
}

const tablesBody = document.getElementById('tables-body');
for (const t of diTables) {
  const tr = document.createElement('tr');
  tr.innerHTML = `<td style="text-align:left">${esc(t.name)}</td>`
    + `<td>${esc(t.accessMethod || '-')}</td>`
    + `<td style="text-align:left">${esc(t.storage || '-')}</td>`
    + `<td>${fmt(t.rows)}</td>`
    + `<td>${humanBytes(t.tableBytes)}</td>`
    + `<td>${humanBytes(t.indexBytes)}</td>`;
  tablesBody.appendChild(tr);
}

const indexesBody = document.getElementById('indexes-body');
for (const x of diIndexes) {
  const props = x.primary ? 'PRIMARY' : (x.unique ? 'UNIQUE' : '-');
  const tr = document.createElement('tr');
  tr.innerHTML = `<td style="text-align:left">${esc(x.name)}</td>`
    + `<td style="text-align:left">${esc(x.table)}</td>`
    + `<td>${esc(x.method || '-')}</td>`
    + `<td>${props}</td>`
    + `<td>${humanBytes(x.bytes)}</td>`;
  indexesBody.appendChild(tr);
}

const kpis = document.getElementById('kpis');
const tpUnit = DATA.meta.throughputUnit || 'TPM';
const kpiItems = [
  [tpUnit, fmt(DATA.kpis.throughput)],
  ['Peak ' + tpUnit, fmt(DATA.kpis.peakThroughput)],
  ['Committed', fmt(DATA.kpis.committed)],
  ['Errors', fmt(DATA.kpis.errors)],
  ['Error rate', fmt(DATA.kpis.errorRate, 2) + ' %'],
  ['Avg latency', fmt(DATA.kpis.avgLatency, 2) + ' ms'],
];
for (const [k, v] of kpiItems) {
  const el = document.createElement('div');
  el.className = 'card kpi';
  el.innerHTML = `<div class="label">${esc(k)}</div><div class="value">${esc(v)}</div>`;
  kpis.appendChild(el);
}

document.getElementById('tx-throughput-head').textContent = tpUnit;
const txbody = document.getElementById('txbody');
for (const t of DATA.transactions) {
  const tr = document.createElement('tr');
  tr.innerHTML = `<td>${esc(t.name)}</td><td>${fmt(t.committed)}</td><td>${fmt(t.errors)}</td>` +
    `<td>${fmt(t.error_rate, 2)}</td><td>${fmt(t.throughput)}</td>` +
    `<td>${fmt(t.mean, 2)}</td><td>${fmt(t.p95, 2)}</td><td>${fmt(t.p99, 2)}</td><td>${fmt(t.max, 2)}</td>`;
  // The spec's description, if any, as a tooltip (a property, not markup).
  if (t.description) tr.title = t.description;
  txbody.appendChild(tr);
}

const lineOpts = (yTitle) => ({
  responsive: true, maintainAspectRatio: false,
  interaction: {mode: 'index', intersect: false},
  plugins: {legend: {position: 'bottom', labels: {boxWidth: 12}}},
  scales: {
    x: {title: {display: true, text: 'Elapsed (s)'}, grid: {display: false}},
    y: {title: {display: true, text: yTitle}, beginAtZero: true}
  },
  elements: {point: {radius: 0}, line: {borderWidth: 1.5, tension: 0.2}}
});

// Shades the ramp-up window on time-series charts. Samples in that window are
// drawn but excluded from the KPIs, statistics and histogram.
const rampupPlugin = {
  id: 'rampup',
  beforeDatasetsDraw(chart) {
    const end = DATA.meta.rampupEndS;
    if (end == null || end <= 0) return;
    const {ctx, chartArea: {top, bottom, left}, scales: {x}} = chart;
    // Category axis: find the last label at or before the boundary.
    let idx = -1;
    for (let i = 0; i < DATA.timeAxis.length && DATA.timeAxis[i] < end; i++) idx = i;
    if (idx < 0) return;
    const right = Math.min(x.getPixelForValue(idx + 1), x.right);
    ctx.save();
    ctx.fillStyle = 'rgba(148, 163, 184, 0.18)';
    ctx.fillRect(left, top, right - left, bottom - top);
    ctx.fillStyle = 'rgba(148, 163, 184, 0.9)';
    ctx.font = '11px sans-serif';
    ctx.textBaseline = 'top';
    ctx.fillText('ramp-up', left + 4, top + 4);
    ctx.restore();
  }
};

// Registry of live Chart.js instances, keyed by canvas id, so we can export each as PNG.
const charts = {};

// Throughput over time
charts.throughput = new Chart(document.getElementById('throughput'), {
  type: 'line',
  data: {
    labels: DATA.timeAxis,
    datasets: [
      {label: 'Total', data: DATA.throughput.total, borderColor: '#e2e8f0', borderWidth: 2},
      ...DATA.throughput.perTx.map(s => ({label: s.name, data: s.data, borderColor: s.color}))
    ]
  },
  options: {...lineOpts(tpUnit), plugins: {
    legend: {display: false},
    htmlLegend: {containerID: 'throughput-legend'},
    title: {display: true, text: 'Throughput over time'}
  }},
  plugins: [htmlLegendPlugin, rampupPlugin]
});

// Mean latency over time per transaction
charts.latency = new Chart(document.getElementById('latency'), {
  type: 'line',
  data: {
    labels: DATA.timeAxis,
    datasets: DATA.latency.perTx.map(s => ({label: s.name, data: s.data, borderColor: s.color}))
  },
  options: {...lineOpts('Mean response time (ms)'), plugins: {
    legend: {display: false},
    htmlLegend: {containerID: 'latency-legend'},
    title: {display: true, text: 'Latency over time'}
  }},
  plugins: [htmlLegendPlugin, rampupPlugin]
});

// Per-transaction panels (percentiles, mix) are only meaningful with more than
// one transaction type; hide them for single-transaction workloads like PGM-B.
const multiTx = DATA.transactions.length > 1;
if (!multiTx) {
  for (const id of ['card-percentiles', 'card-mix']) {
    const el = document.getElementById(id);
    if (el) el.style.display = 'none';
  }
}

// Latency percentiles grouped bars
if (multiTx) charts.percentiles = new Chart(document.getElementById('percentiles'), {
  type: 'bar',
  data: {
    labels: DATA.percentiles.map(p => p.name),
    datasets: [
      {label: 'Avg', data: DATA.percentiles.map(p => p.mean), backgroundColor: '#4e79a7'},
      {label: 'p95', data: DATA.percentiles.map(p => p.p95), backgroundColor: '#f28e2b'},
      {label: 'p99', data: DATA.percentiles.map(p => p.p99), backgroundColor: '#e15759'},
      {label: 'Max', data: DATA.percentiles.map(p => p.max), backgroundColor: '#76b7b2'}
    ]
  },
  options: {
    responsive: true, maintainAspectRatio: false,
    plugins: {legend: {position: 'bottom'}, title: {display: true, text: 'Response-time percentiles (ms)'}},
    scales: {y: {beginAtZero: true, title: {display: true, text: 'ms'}}}
  }
});

// Latency distribution histogram
charts.histogram = new Chart(document.getElementById('histogram'), {
  type: 'bar',
  data: {
    labels: DATA.histogram.labels,
    datasets: [{label: 'Transactions', data: DATA.histogram.counts, backgroundColor: '#38bdf8'}]
  },
  options: {
    responsive: true, maintainAspectRatio: false,
    plugins: {legend: {display: false}, title: {display: true, text: 'Response-time distribution'}},
    scales: {
      x: {title: {display: true, text: 'Response time (ms)'}, grid: {display: false}},
      y: {beginAtZero: true, title: {display: true, text: 'Count'}}
    }
  }
});

// Transaction mix donut
if (multiTx) charts.mix = new Chart(document.getElementById('mix'), {
  type: 'doughnut',
  data: {
    labels: DATA.mix.map(m => m.name),
    datasets: [{data: DATA.mix.map(m => m.value),
      backgroundColor: ['#4e79a7','#f28e2b','#e15759','#76b7b2','#59a14f','#edc948','#b07aa1','#ff9da7','#9c755f','#bab0ac']}]
  },
  options: {
    responsive: true, maintainAspectRatio: false,
    plugins: {
      legend: {display: false},
      htmlLegend: {containerID: 'mix-legend'},
      title: {display: true, text: 'Committed transaction mix'}
    }
  },
  plugins: [htmlLegendPlugin]
});

// Errors over time
const errEl = document.getElementById('errors');
if (DATA.errors.counts.length) {
  charts.errors = new Chart(errEl, {
    type: 'bar',
    data: {labels: DATA.errors.timeAxis, datasets: [{label: 'Errors', data: DATA.errors.counts, backgroundColor: '#f87171'}]},
    options: {
      responsive: true, maintainAspectRatio: false,
      plugins: {legend: {display: false}, title: {display: true, text: 'Errors over time'}},
      scales: {x: {title: {display: true, text: 'Elapsed (s)'}, grid: {display: false}}, y: {beginAtZero: true}}
    }
  });
} else {
  errEl.parentElement.innerHTML = '<div style="display:flex;height:100%;align-items:center;justify-content:center;color:var(--good)">No errors recorded</div>';
}

// --- Chart PNG export -------------------------------------------------------
// Chart.js draws to a canvas whose backing store is (CSS size x devicePixelRatio).
// To export at high resolution regardless of the viewer's screen, we bump the
// chart's devicePixelRatio, re-render, snapshot, then restore. The snapshot is
// composited onto an opaque background (the live canvas is transparent) so the
// PNG looks intentional instead of showing the page through it.
const EXPORT_DPR = 4;
const slug = (s) => (s || 'chart').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
const runStamp = slug(DATA.meta.generatedAt || '');
const panelColor = (getComputedStyle(document.body).getPropertyValue('--panel') || '#1e293b').trim();

// Two export palettes. 'dark' is WYSIWYG (matches the on-screen report). 'light'
// recolours text/grid/background for embedding figures in a white document.
const EXPORT_THEMES = {
  dark:  {bg: panelColor || '#1e293b', text: '#e2e8f0', muted: '#94a3b8', grid: '#334155', swapLightLine: null},
  light: {bg: '#ffffff', text: '#1e293b', muted: '#475569', grid: '#e2e8f0', swapLightLine: '#0f172a'},
};

function chartTitle(chart) {
  const t = chart.options.plugins && chart.options.plugins.title;
  return (t && t.text) || chart.canvas.id;
}

function selectedTheme() {
  const sel = document.getElementById('theme-select');
  return (sel && EXPORT_THEMES[sel.value]) ? sel.value : 'dark';
}

// Builds a standalone Chart.js config for export: a JSON clone of the live
// chart's config (drops functions, which we don't rely on) re-coloured for the
// chosen theme, with the on-canvas legend forced on. Cloning means we render a
// throwaway chart and never mutate the live one (mutating Chart.js' option
// proxies in place triggers infinite setter recursion).
function buildExportConfig(chart, t) {
  const src = chart.config;
  const data = JSON.parse(JSON.stringify(src.data));
  const options = JSON.parse(JSON.stringify(src.options || {}));
  options.animation = false;
  options.responsive = true;
  options.maintainAspectRatio = false;
  options.devicePixelRatio = EXPORT_DPR;
  options.plugins = options.plugins || {};
  delete options.plugins.htmlLegend; // external-legend hook is irrelevant offscreen
  const isPie = src.type === 'doughnut' || src.type === 'pie';
  options.plugins.legend = options.plugins.legend || {};
  options.plugins.legend.display = true;
  options.plugins.legend.position = isPie ? 'right' : 'bottom';
  options.plugins.legend.labels = Object.assign({}, options.plugins.legend.labels, {color: t.text, boxWidth: 12});
  if (options.plugins.title) options.plugins.title.color = t.text;
  for (const key of Object.keys(options.scales || {})) {
    const sc = options.scales[key];
    sc.ticks = sc.ticks || {}; sc.ticks.color = t.muted;
    sc.grid = sc.grid || {}; sc.grid.color = t.grid;
    if (sc.title) sc.title.color = t.text;
  }
  // The throughput "Total" series is near-white and would vanish on white.
  if (t.swapLightLine) {
    data.datasets.forEach(ds => { if (ds.borderColor === '#e2e8f0') ds.borderColor = t.swapLightLine; });
  }
  return {type: src.type, data, options};
}

function chartToPng(chart, themeName) {
  const t = EXPORT_THEMES[themeName] || EXPORT_THEMES.dark;
  const w = chart.width || chart.canvas.clientWidth || 640;
  const h = chart.height || chart.canvas.clientHeight || 320;

  // Render the export chart in a hidden, fixed-size holder so it lays out
  // exactly like the on-screen card without disturbing it.
  const holder = document.createElement('div');
  holder.style.cssText = `position:fixed; left:-99999px; top:0; width:${w}px; height:${h}px;`;
  const cv = document.createElement('canvas');
  holder.appendChild(cv);
  document.body.appendChild(holder);

  const tmp = new Chart(cv, buildExportConfig(chart, t));
  tmp.draw();

  const src = tmp.canvas;
  const out = document.createElement('canvas');
  out.width = src.width;
  out.height = src.height;
  const ctx = out.getContext('2d');
  ctx.fillStyle = t.bg; // canvas is transparent; paint an opaque background
  ctx.fillRect(0, 0, out.width, out.height);
  ctx.drawImage(src, 0, 0);
  const url = out.toDataURL('image/png');

  tmp.destroy();
  holder.remove();
  return url;
}

function downloadChart(chart, themeName) {
  const a = document.createElement('a');
  a.href = chartToPng(chart, themeName || selectedTheme());
  const name = runStamp ? `${runStamp}-${slug(chartTitle(chart))}` : slug(chartTitle(chart));
  a.download = name + '.png';
  document.body.appendChild(a);
  a.click();
  a.remove();
}

// Add a hover "Download PNG" button to each rendered chart's card.
for (const chart of Object.values(charts)) {
  const card = chart.canvas.closest('.chart-card');
  if (!card) continue;
  const btn = document.createElement('button');
  btn.className = 'dlbtn';
  btn.textContent = 'Download PNG';
  btn.onclick = () => downloadChart(chart);
  card.appendChild(btn);
}

// "Download all" fires the per-chart downloads sequentially with a small gap so
// the browser doesn't suppress the rapid succession of download prompts.
document.getElementById('dl-all').onclick = () => {
  const theme = selectedTheme();
  const list = Object.values(charts);
  list.forEach((chart, i) => setTimeout(() => downloadChart(chart, theme), i * 300));
};

// --- Document theme switch --------------------------------------------------
// The document palette is driven by CSS variables on <html data-theme>. Chart.js
// canvases can't read CSS vars, so we mirror the theme onto its global defaults
// and recolour the one near-white series ("Total"), then redraw every chart.
const CHART_PALETTES = {
  dark:  {tick: '#94a3b8', grid: '#334155', totalLine: '#e2e8f0'},
  light: {tick: '#475569', grid: '#e2e8f0', totalLine: '#0f172a'},
};

function applyTheme(name) {
  const p = CHART_PALETTES[name] || CHART_PALETTES.dark;
  document.documentElement.setAttribute('data-theme', name);
  Chart.defaults.color = p.tick;
  Chart.defaults.borderColor = p.grid;
  for (const chart of Object.values(charts)) {
    chart.data.datasets.forEach(ds => { if (ds.label === 'Total') ds.borderColor = p.totalLine; });
    chart.update();
  }
}

const themeSelect = document.getElementById('theme-select');
themeSelect.addEventListener('change', () => applyTheme(themeSelect.value));
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn minimal_spec() -> BenchmarkSpec {
        let yaml = r#"
name: test-bench
description: a tiny spec
scaling:
  unit: tenant
  max_id_query: "SELECT 1"
schema:
  tables: []
transactions:
  - id: 1
    name: alpha
    weight: 50
  - id: 2
    name: beta
    weight: 50
"#;
        serde_yaml::from_str(yaml).expect("valid spec")
    }

    fn write(path: &PathBuf, contents: &str) {
        let mut f = File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn histogram_caps_at_p99_with_overflow_bucket() {
        // 99 values at 1.0 and one huge outlier; the outlier must land in the
        // trailing overflow bucket rather than stretching every bin.
        let mut hist = LatencyHistogram::new();
        for _ in 0..99 {
            hist.add(1.0);
        }
        hist.add(1000.0);
        let (labels, counts) = hist.finish();
        assert_eq!(labels.len(), 31);
        assert_eq!(counts.len(), 31);
        assert!(labels.last().unwrap().starts_with('>'));
        assert_eq!(counts.iter().sum::<u64>(), 100);
        assert_eq!(*counts.last().unwrap(), 1);
    }

    #[test]
    fn time_series_align_on_shared_origin() {
        let dir = std::env::temp_dir().join(format!("pgmeter-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // tx 1 at t=100,101; tx 2 at t=101,102. Shared origin = 100 -> 3 seconds.
        write(
            &dir.join("transaction.log"),
            "100 1 1 5.0\n100 1 1 7.0\n101 1 2 3.0\n102 2 1 9.0\n102 2 2 4.0\n",
        );
        write(&dir.join("error.log"), "101 1 2 boom\n");

        let spec = minimal_spec();
        let transactions = vec![
            BenchmarkTransaction {
                id: 1,
                name: "alpha".into(),
                weight: 50,
            },
            BenchmarkTransaction {
                id: 2,
                name: "beta".into(),
                weight: 50,
            },
        ];
        let counters: HashMap<u16, Counter> = HashMap::new();
        let stats: HashMap<u16, ResponseTimeStatistics> = HashMap::new();
        let input = ReportInput {
            target_dir: &dir,
            log_file: "transaction.log",
            error_file: "error.log",
            spec: &spec,
            transactions: &transactions,
            counters: &counters,
            stats: &stats,
            total_time_ms: 3000,
            rampup_time_ms: 0,
            rampup_end_ts: None,
            interrupted: false,
            clients: 2,
            duration_s: 3,
            rampup_s: 0,
        };

        let ts = build_time_series(&input, 60.0).unwrap();
        assert_eq!(ts.time_axis, vec![0, 1, 2]);
        assert_eq!(ts.rampup_end_s, None);
        // Total per-second counts *60: sec0=2, sec1=1, sec2=2.
        assert_eq!(ts.total_tpm, vec![120, 60, 120]);
        assert_eq!(ts.series.len(), 2);

        let alpha = ts.series.iter().find(|s| s.name == "alpha").unwrap();
        assert_eq!(alpha.tpm, vec![120, 0, 60]);
        assert_eq!(alpha.latency[0], 6.0); // mean of 5.0 and 7.0

        // One error at second 1 relative to origin.
        assert_eq!(ts.error_time_axis, vec![0, 1]);
        assert_eq!(ts.error_counts, vec![0, 1]);

        // Regression for RELEASE_REVIEW B2: with ramp-up ending at t=101 the
        // time series keep every sample but the histogram drops the two at
        // t=100, and the boundary is exposed as an elapsed-seconds offset.
        let input = ReportInput {
            rampup_end_ts: Some(101),
            ..input
        };
        let ts = build_time_series(&input, 60.0).unwrap();
        assert_eq!(ts.total_tpm, vec![120, 60, 120]);
        assert_eq!(ts.hist_counts.iter().sum::<u64>(), 3);
        assert_eq!(ts.rampup_end_s, Some(1));

        std::fs::remove_dir_all(&dir).ok();
    }

    // Regression for RELEASE_REVIEW B13: strings from the spec and the server
    // are inserted through innerHTML templates. Every interpolation of such a
    // string must go through `esc(...)`; numbers go through `fmt(...)`.
    #[test]
    fn report_js_escapes_every_string_interpolated_into_html() {
        assert!(APP_JS.contains("const esc = "));
        for line in APP_JS
            .lines()
            .filter(|l| l.contains("innerHTML = `") || l.trim_start().starts_with("+ `"))
        {
            let mut rest = line;
            while let Some(pos) = rest.find("${") {
                let expr_end = rest[pos..].find('}').expect("unterminated ${") + pos;
                let expr = &rest[pos + 2..expr_end];
                assert!(
                    expr.starts_with("esc(")
                        || expr.starts_with("fmt(")
                        || expr.starts_with("humanBytes(")
                        || expr == "props",
                    "unescaped interpolation `${{{expr}}}` in: {line}"
                );
                rest = &rest[expr_end + 1..];
            }
        }
    }

    // Regression for RELEASE_REVIEW B14 (needs PGMETER_TEST_DSN): a table with
    // the same name in another schema must not appear in the Data section.
    #[test]
    fn data_info_only_lists_tables_on_the_search_path() {
        use postgres::{Client, NoTls};

        let Ok(dsn) = std::env::var("PGMETER_TEST_DSN") else {
            eprintln!("PGMETER_TEST_DSN not set, skipping");
            return;
        };
        let mut client = Client::connect(&dsn, NoTls).expect("connect to PGMETER_TEST_DSN");
        client
            .batch_execute(
                "DROP SCHEMA IF EXISTS b14_other CASCADE; \
                 DROP TABLE IF EXISTS b14_t; \
                 CREATE TABLE b14_t (id int PRIMARY KEY); \
                 CREATE SCHEMA b14_other; \
                 CREATE TABLE b14_other.b14_t (id int PRIMARY KEY); \
                 CREATE INDEX b14_other_ix ON b14_other.b14_t (id);",
            )
            .unwrap();

        let mut spec = minimal_spec();
        spec.schema
            .tables
            .push(crate::executor::yaml_types::TableSpec {
                name: "b14_t".into(),
                columns: vec![],
                primary_key: vec![],
                access_method: None,
                storage: HashMap::new(),
            });
        let info = fetch_data_info(&mut client, &spec);

        let tables = info["tables"].as_array().unwrap();
        assert_eq!(tables.len(), 1, "one b14_t expected, got {tables:?}");
        let indexes = info["indexes"].as_array().unwrap();
        let names: Vec<&str> = indexes
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["b14_t_pkey"], "{indexes:?}");

        client
            .batch_execute("DROP SCHEMA b14_other CASCADE; DROP TABLE b14_t")
            .unwrap();
    }

    #[test]
    fn rendered_html_embeds_data_and_canvases() {
        let html = render_html(r#"{"meta":{"name":"x"}}"#);
        assert!(html.contains("const DATA = {\"meta\":{\"name\":\"x\"}};"));
        assert!(html.contains("id=\"throughput\""));
        assert!(html.contains("id=\"histogram\""));
        assert!(html.contains("Chart")); // inlined chart.js present
    }

    // Returns the version string from the vendored Chart.js license header,
    // e.g. "4.4.1" from " * Chart.js v4.4.1".
    fn chart_js_version() -> &'static str {
        let marker = "Chart.js v";
        let start = CHART_JS.find(marker).expect("Chart.js header missing") + marker.len();
        let rest = &CHART_JS[start..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        &rest[..end]
    }

    // Chart.js is MIT licensed: its copyright and permission notice must
    // survive in every generated report, which inlines the file verbatim.
    #[test]
    fn inlined_chart_js_keeps_license_header_in_reports() {
        let html = render_html("{}");
        for needle in [
            "Chart.js v",
            "Chart.js Contributors",
            "Released under the MIT License",
        ] {
            assert!(CHART_JS.contains(needle), "vendored asset lacks {needle:?}");
            assert!(html.contains(needle), "rendered report lacks {needle:?}");
        }
    }

    // THIRD_PARTY_NOTICES.md must exist (include_str! fails to compile
    // otherwise) and name the exact Chart.js version that is vendored, so a
    // future asset bump also updates the notice.
    #[test]
    fn third_party_notices_cover_embedded_components() {
        let notices = include_str!("../../THIRD_PARTY_NOTICES.md");
        let version = chart_js_version();
        assert!(!version.is_empty());
        assert!(
            notices.contains(&format!("Chart.js {version}")),
            "THIRD_PARTY_NOTICES.md does not mention Chart.js {version}"
        );
        assert!(notices.contains("Chart.js Contributors"));
        assert!(notices.contains("## Luau"));
        assert!(notices.contains("Roblox Corporation"));
        assert!(notices.contains("Lua.org, PUC-Rio"));
    }
}
