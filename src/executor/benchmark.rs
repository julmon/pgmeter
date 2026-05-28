use async_trait::async_trait;
use postgres::Client;
use sqlx::PgConnection;
use tabled::Tabled;

// Transaction specifications
#[derive(Clone)]
pub struct BenchmarkTransaction {
    // Must be unique and greater than 0
    pub id: u16,
    // Transaction name
    pub name: String,
    // Probability of the transaction to be executed. From 0 (never) to 100 (alaways executed).
    pub weight: u16,
    // Description of the transaction, useful for the report
    #[allow(dead_code)]
    pub description: String,
}

/// Prepared statements holder for synchronous benchmark execution
/// This allows reusing prepared statements across multiple transactions
pub struct PreparedStatements {
    #[allow(dead_code)]
    pub statements: Vec<postgres::Statement>,
}

impl PreparedStatements {
    pub fn new() -> Self {
        PreparedStatements {
            statements: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct Counter {
    pub n_commits: u64,
    pub n_total: u64,
    pub total_duration_ms: f64,
}

#[derive(Clone)]
pub struct BenchmarkStmt {
    pub sql: String,
}

#[derive(Tabled,Clone,Debug)]
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

// ReadWrite trait for all benchmarks implementing read/write workload (async version)
#[async_trait]
pub trait ReadWrite {
    async fn execute_rw_transaction(&self, conn: &mut PgConnection, transaction :&BenchmarkTransaction) -> Result<u128, Box<dyn std::error::Error>>;
}

/// Synchronous ReadWrite trait for high-performance benchmark execution
/// Uses prepared statements and synchronous I/O (matches pgbench architecture)
pub trait ReadWriteSync: Send + Sync {
    /// Prepare all statements needed for transactions (called once per connection)
    fn prepare_statements(&self, client: &mut Client) -> Result<PreparedStatements, postgres::Error>;

    /// Execute a transaction using prepared statements (hot path)
    fn execute_rw_transaction_sync(
        &self,
        client: &mut Client,
        prepared: &PreparedStatements,
        transaction: &BenchmarkTransaction,
    ) -> Result<u128, Box<dyn std::error::Error + Send + Sync>>;

    /// Check if this benchmark supports synchronous execution
    fn supports_sync(&self) -> bool {
        false
    }
}

#[derive(Tabled)]
pub struct TransactionSummary {
    #[tabled(rename = "Transaction")]
    name: String,
    #[tabled(rename = "Committed")]
    n_commits: u64,
    #[tabled(rename = "Errors")]
    n_errors: u64,
    #[tabled(rename = "Error rate (%)")]
    error_rate: f64,
    #[tabled(rename = "TPM")]
    tpm: u32,
    #[tabled(rename = "TPS")]
    tps: u32,
}

impl TransactionSummary {
    pub fn new(name: String, n_commits: u64, n_errors: u64, error_rate: f64, tpm: u32, tps: u32) -> TransactionSummary {
        TransactionSummary {
            name: name,
            n_commits: n_commits,
            n_errors: n_errors,
            error_rate: error_rate,
            tpm: tpm,
            tps: tps,
        }
    }
}

pub trait Benchmark:ReadWrite {
    fn initialize_schema(&self, client: &mut Client) -> Result<u128, postgres::Error>;
    fn pre_load_data(&self, client: &mut Client) -> Result<u128, String>;
    fn load_data(&self, client: &mut Client, ids: Vec<u32>) -> Result<u128, String>;
    fn get_default_max_id(&self, client: &mut Client) -> Result<u32, postgres::Error>;
    fn get_transactions_rw(&self) -> Vec<BenchmarkTransaction>;
    fn get_table_ddls(&self) -> Vec<BenchmarkStmt>;
    fn get_pkey_ddls(&self) -> Vec<BenchmarkStmt>;
    fn get_fkey_ddls(&self) -> Vec<BenchmarkStmt>;
    fn get_index_ddls(&self) -> Vec<BenchmarkStmt>;
    fn get_vacuum_stmts(&self) -> Vec<BenchmarkStmt>;
}
