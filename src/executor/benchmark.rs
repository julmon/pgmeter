use tabled::Tabled;

/// A transaction of the workload, as the executor sees it: the id used in
/// the logs and counters, the display name and the selection weight.
#[derive(Clone)]
pub struct BenchmarkTransaction {
    // Must be unique and greater than 0
    pub id: u16,
    // Transaction name
    pub name: String,
    // Probability of the transaction to be executed. From 0 (never) to 100 (always executed).
    pub weight: u16,
}

/// Post-ramp-up counters kept by the data collector for one transaction.
#[derive(Clone)]
pub struct Counter {
    pub n_commits: u64,
    pub n_total: u64,
    pub total_duration_ms: f64,
}

#[derive(Tabled, Clone, Debug)]
pub struct ResponseTimeStatistics {
    #[tabled(rename = "Transaction")]
    pub name: String,
    #[tabled(rename = "Avg. (ms)")]
    pub mean: f64,
    #[tabled(rename = "Min. (ms)")]
    pub min: f64,
    #[tabled(rename = "Max. (ms)")]
    pub max: f64,
    #[tabled(rename = "Std. Dev. (ms)")]
    pub std: f64,
    #[tabled(rename = "95% (ms)")]
    pub percentile_95: f64,
    #[tabled(rename = "99% (ms)")]
    pub percentile_99: f64,
}

impl ResponseTimeStatistics {
    /// The statistics of an empty sample: every figure 0.
    pub fn zero(name: &str) -> ResponseTimeStatistics {
        ResponseTimeStatistics {
            name: name.to_string(),
            mean: 0.0,
            min: 0.0,
            max: 0.0,
            std: 0.0,
            percentile_95: 0.0,
            percentile_99: 0.0,
        }
    }
}

// Per-transaction headline results. The throughput is expressed in the unit
// configured by the spec's `metrics` block (e.g. TPS, txns/min, queries/hr); the column
// is rendered with a dynamic header rather than the tabled derive.
pub struct TransactionSummary {
    pub name: String,
    pub n_commits: u64,
    pub n_errors: u64,
    pub error_rate: f64,
    /// Throughput in the spec's unit over the workload window (see `summary::summarize`).
    pub throughput: f64,
}

impl TransactionSummary {
    pub fn new(
        name: String,
        n_commits: u64,
        n_errors: u64,
        error_rate: f64,
        throughput: f64,
    ) -> TransactionSummary {
        TransactionSummary {
            name,
            n_commits,
            n_errors,
            error_rate,
            throughput,
        }
    }
}
