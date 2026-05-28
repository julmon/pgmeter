use std::error::Error;
use std::fmt;
use std::io::Write;
use std::time::Instant;

use async_trait::async_trait;
use postgres::Client;
use sqlx::PgConnection;
use sqlx::Connection;
use rand::Rng;

use super::benchmark::{
    Benchmark,
    BenchmarkStmt,
    BenchmarkTransaction,
    PreparedStatements,
    ReadWrite,
    ReadWriteSync,
};

/// TPC-B benchmark for PostgreSQL
///
/// This benchmark implements the official TPC-B workload as defined in the
/// TPC-B Standard Specification. It simulates a banking system with branches,
/// tellers, and accounts performing debit/credit transactions.
///
/// Schema (per TPC-B specification):
/// - BRANCH: Bank branches (1 per TPS in scale factor)
/// - TELLER: Tellers belonging to branches (10 per branch)
/// - ACCOUNT: Customer accounts (100,000 per branch)
/// - HISTORY: Transaction history log (grows during benchmark)
///
/// Scaling Rules (per TPC-B specification):
/// - Scale factor N means N branches (targeting N TPS)
/// - 10 tellers per branch
/// - 100,000 accounts per branch
///
/// Workload:
/// - Single transaction type (debit/credit) that:
///   1. Updates account balance
///   2. Reads updated account balance
///   3. Updates teller balance
///   4. Updates branch balance
///   5. Inserts history record
#[allow(dead_code)]
pub struct TPCB {
    pub name: String,
    pub description: String,
    pub scalefactor: u32,
    pub min_id: u32,
    pub max_id: u32,
    pub transactions_rw: Vec<BenchmarkTransaction>,
    pub table_ddls: Vec<BenchmarkStmt>,
    pub pkey_ddls: Vec<BenchmarkStmt>,
    pub fkey_ddls: Vec<BenchmarkStmt>,
    pub index_ddls: Vec<BenchmarkStmt>,
    pub vacuum_stmts: Vec<BenchmarkStmt>,
}

#[derive(Debug)]
pub struct TPCBError(String);

impl fmt::Display for TPCBError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for TPCBError {}

// TPC-B scaling constants (per specification)
// Number of tellers per branch (TPS unit)
const TELLERS_PER_BRANCH: u32 = 10;
// Number of accounts per branch (TPS unit)
const ACCOUNTS_PER_BRANCH: u32 = 100_000;

impl TPCB {
    pub fn new(scalefactor: u32, min_id: u32, max_id: u32) -> TPCB {
        TPCB {
            name: "TPC-B benchmark".to_string(),
            description: "TPC-B benchmark implementation for PostgreSQL per official TPC-B specification.".to_string(),
            scalefactor,
            min_id,
            max_id,
            transactions_rw: Vec::from([
                BenchmarkTransaction {
                    id: 1,
                    weight: 100,
                    name: "TPC-B".to_string(),
                    description: "TPC-B debit/credit transaction".to_string(),
                },
            ]),
            table_ddls: Vec::from([
                // Drop tables if they exist (per TPC-B specification naming)
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS history CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS account CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS teller CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS branch CASCADE".to_string(),
                },
                // Create tables per TPC-B specification
                // BRANCH table: 1 row per TPS (scale factor)
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE branch (
                            branch_id INTEGER NOT NULL,
                            branch_balance INTEGER,
                            filler CHAR(88)
                        )".to_string(),
                },
                // TELLER table: 10 rows per branch
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE teller (
                            teller_id INTEGER NOT NULL,
                            branch_id INTEGER,
                            teller_balance INTEGER,
                            filler CHAR(84)
                        )".to_string(),
                },
                // ACCOUNT table: 100,000 rows per branch
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE account (
                            account_id INTEGER NOT NULL,
                            branch_id INTEGER,
                            account_balance INTEGER,
                            filler CHAR(84)
                        )".to_string(),
                },
                // HISTORY table: grows during benchmark execution
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE history (
                            account_id INTEGER,
                            teller_id INTEGER,
                            branch_id INTEGER,
                            delta INTEGER,
                            timestamp TIMESTAMP,
                            filler CHAR(22)
                        )".to_string(),
                },
            ]),
            pkey_ddls: Vec::from([
                BenchmarkStmt {
                    sql: "ALTER TABLE branch ADD PRIMARY KEY (branch_id)".to_string(),
                },
                BenchmarkStmt {
                    sql: "ALTER TABLE teller ADD PRIMARY KEY (teller_id)".to_string(),
                },
                BenchmarkStmt {
                    sql: "ALTER TABLE account ADD PRIMARY KEY (account_id)".to_string(),
                },
            ]),
            fkey_ddls: Vec::from([
                BenchmarkStmt {
                    sql: r"
                        ALTER TABLE teller
                        ADD CONSTRAINT fk_teller_branch
                        FOREIGN KEY (branch_id)
                        REFERENCES branch (branch_id)
                    ".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        ALTER TABLE account
                        ADD CONSTRAINT fk_account_branch
                        FOREIGN KEY (branch_id)
                        REFERENCES branch (branch_id)
                    ".to_string(),
                },
            ]),
            index_ddls: Vec::from([
                // Index on teller by branch_id
                BenchmarkStmt {
                    sql: "CREATE INDEX idx_teller_branch ON teller (branch_id)".to_string(),
                },
                // Index on account by branch_id
                BenchmarkStmt {
                    sql: "CREATE INDEX idx_account_branch ON account (branch_id)".to_string(),
                },
            ]),
            vacuum_stmts: Vec::from([
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE branch".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE teller".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE account".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE history".to_string() },
            ]),
        }
    }

    /// TPC-B debit/credit transaction (per official specification)
    ///
    /// The transaction simulates a bank debit/credit operation:
    /// 1. Updates account balance (debit or credit) and returns updated balance
    /// 2. Updates teller balance
    /// 3. Updates branch balance
    /// 4. Inserts history record
    ///
    /// Optimized: Combined UPDATE + SELECT using RETURNING clause to reduce round-trips
    pub async fn tpcb_transaction(conn: &mut PgConnection, branch_id: i32, min_id: u32, max_id: u32) -> Result<u128, Box<dyn std::error::Error>> {
        let start = Instant::now();

        // Calculate the range of valid IDs based on branch range
        let min_branch = min_id as i32;
        let max_branch = max_id as i32;

        // Generate random values for the transaction
        // Account ID: random across all accounts in the branch range
        let account_id: i32 = rand::thread_rng().gen_range(
            ((min_branch - 1) * ACCOUNTS_PER_BRANCH as i32 + 1)..=
            (max_branch * ACCOUNTS_PER_BRANCH as i32)
        );

        // Teller ID: random across all tellers in the branch range
        let teller_id: i32 = rand::thread_rng().gen_range(
            ((min_branch - 1) * TELLERS_PER_BRANCH as i32 + 1)..=
            (max_branch * TELLERS_PER_BRANCH as i32)
        );

        // Branch ID: use the passed branch_id (already random)
        let bid: i32 = branch_id;

        // Delta: random amount between -5000 and 5000 (debit or credit)
        let delta: i32 = rand::thread_rng().gen_range(-5000..=5000);

        let mut transaction = conn.begin().await?;

        // 1 & 2. Update account balance and return updated value (combined using RETURNING)
        let _row: (i32,) = sqlx::query_as(
            "UPDATE account SET account_balance = account_balance + $1 WHERE account_id = $2 RETURNING account_balance"
        )
        .bind(delta)
        .bind(account_id)
        .fetch_one(&mut transaction)
        .await?;

        // 3. Update teller balance
        sqlx::query(
            "UPDATE teller SET teller_balance = teller_balance + $1 WHERE teller_id = $2"
        )
        .bind(delta)
        .bind(teller_id)
        .execute(&mut transaction)
        .await?;

        // 4. Update branch balance
        sqlx::query(
            "UPDATE branch SET branch_balance = branch_balance + $1 WHERE branch_id = $2"
        )
        .bind(delta)
        .bind(bid)
        .execute(&mut transaction)
        .await?;

        // 5. Insert history record
        sqlx::query(
            "INSERT INTO history (account_id, teller_id, branch_id, delta, timestamp) VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP)"
        )
        .bind(account_id)
        .bind(teller_id)
        .bind(bid)
        .bind(delta)
        .execute(&mut transaction)
        .await?;

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    /// Populate the branch table
    pub fn populate_branch(client: &mut Client, branch_id: u32) -> Result<(), String> {
        let mut writer = match client.copy_in("COPY branch (branch_id, branch_balance, filler) FROM stdin") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        // Initial balance is 0, filler is empty (will be padded by PostgreSQL)
        let line = format!("{}\t0\t\n", branch_id);

        match writer.write_all(line.as_bytes()) {
            Ok(_) => (),
            Err(e) => return Err(e.to_string()),
        }

        match writer.finish() {
            Ok(_) => (),
            Err(e) => return Err(e.to_string()),
        }

        Ok(())
    }

    /// Populate the teller table for a branch
    pub fn populate_teller(client: &mut Client, branch_id: u32) -> Result<(), String> {
        let mut writer = match client.copy_in("COPY teller (teller_id, branch_id, teller_balance, filler) FROM stdin") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        let teller_base = (branch_id - 1) * TELLERS_PER_BRANCH;

        for i in 1..=TELLERS_PER_BRANCH {
            let teller_id = teller_base + i;
            // Initial balance is 0, filler is empty
            let line = format!("{}\t{}\t0\t\n", teller_id, branch_id);

            match writer.write_all(line.as_bytes()) {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }

        match writer.finish() {
            Ok(_) => (),
            Err(e) => return Err(e.to_string()),
        }

        Ok(())
    }

    /// Populate the account table for a branch
    pub fn populate_account(client: &mut Client, branch_id: u32) -> Result<(), String> {
        let batch_size = 10_000u32;
        let n_accounts = ACCOUNTS_PER_BRANCH;
        let n_batch = ((n_accounts as f64 / batch_size as f64) as f64).ceil() as u32;

        let account_base = (branch_id - 1) * ACCOUNTS_PER_BRANCH;

        for b in 1..=n_batch {
            let mut writer = match client.copy_in("COPY account (account_id, branch_id, account_balance, filler) FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let batch_start = (b - 1) * batch_size + 1;
            let mut batch_end = b * batch_size;
            if batch_end > n_accounts {
                batch_end = n_accounts;
            }

            for i in batch_start..=batch_end {
                let account_id = account_base + i;
                // Initial balance is 0, filler is empty
                let line = format!("{}\t{}\t0\t\n", account_id, branch_id);

                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }
            }

            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }

        Ok(())
    }
}

#[async_trait]
impl ReadWrite for TPCB {
    async fn execute_rw_transaction(&self, conn: &mut PgConnection, transaction: &BenchmarkTransaction) -> Result<u128, Box<dyn std::error::Error>> {
        // Generate a random branch ID within the range
        let branch_id: i32 = rand::thread_rng()
            .gen_range(self.min_id..=self.max_id) as i32;

        match transaction.id {
            1 => {
                match TPCB::tpcb_transaction(conn, branch_id, self.min_id, self.max_id).await {
                    Ok(duration) => Ok(duration),
                    Err(e) => Err(Box::new(TPCBError(e.to_string()))),
                }
            },
            0 | 2..=u16::MAX => todo!(),
        }
    }
}

impl Benchmark for TPCB {
    fn initialize_schema(&self, client: &mut Client) -> Result<u128, postgres::Error> {
        let start = Instant::now();

        let mut transaction = client.transaction()?;
        for table_ddl in self.get_table_ddls().iter() {
            transaction.batch_execute(&table_ddl.sql)?;
        }
        transaction.commit()?;

        Ok(start.elapsed().as_micros())
    }

    fn pre_load_data(&self, _client: &mut Client) -> Result<u128, String> {
        // No pre-load operations needed for TPC-B
        Ok(0)
    }

    fn load_data(&self, client: &mut Client, branch_ids: Vec<u32>) -> Result<u128, String> {
        let start = Instant::now();

        for branch_id in branch_ids {
            // Populate tables for this branch
            TPCB::populate_branch(client, branch_id)?;
            TPCB::populate_teller(client, branch_id)?;
            TPCB::populate_account(client, branch_id)?;
        }

        Ok(start.elapsed().as_micros())
    }

    fn get_default_max_id(&self, client: &mut Client) -> Result<u32, postgres::Error> {
        let row = client.query("SELECT MAX(branch_id) AS max_branch_id FROM branch", &[])?;
        let max_branch_id: i32 = row[0].get("max_branch_id");

        Ok(max_branch_id as u32)
    }

    fn get_transactions_rw(&self) -> Vec<BenchmarkTransaction> {
        self.transactions_rw.clone()
    }

    fn get_table_ddls(&self) -> Vec<BenchmarkStmt> {
        self.table_ddls.clone()
    }

    fn get_pkey_ddls(&self) -> Vec<BenchmarkStmt> {
        self.pkey_ddls.clone()
    }

    fn get_fkey_ddls(&self) -> Vec<BenchmarkStmt> {
        self.fkey_ddls.clone()
    }

    fn get_index_ddls(&self) -> Vec<BenchmarkStmt> {
        self.index_ddls.clone()
    }

    fn get_vacuum_stmts(&self) -> Vec<BenchmarkStmt> {
        self.vacuum_stmts.clone()
    }
}

/// Synchronous implementation for high-performance TPC-B execution
/// Uses simple protocol (like pgbench) - sends entire transaction as single string
/// This minimizes round-trips to just 1 per transaction
impl ReadWriteSync for TPCB {
    fn supports_sync(&self) -> bool {
        true
    }

    fn prepare_statements(&self, _client: &mut Client) -> Result<PreparedStatements, postgres::Error> {
        // Simple protocol doesn't use prepared statements
        Ok(PreparedStatements::new())
    }

    fn execute_rw_transaction_sync(
        &self,
        client: &mut Client,
        _prepared: &PreparedStatements,
        _transaction: &BenchmarkTransaction,
    ) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        // Generate random values
        let min_branch = self.min_id as i32;
        let max_branch = self.max_id as i32;

        let branch_id: i32 = rand::thread_rng().gen_range(min_branch..=max_branch);

        let account_id: i32 = rand::thread_rng().gen_range(
            ((min_branch - 1) * ACCOUNTS_PER_BRANCH as i32 + 1)..=
            (max_branch * ACCOUNTS_PER_BRANCH as i32)
        );

        let teller_id: i32 = rand::thread_rng().gen_range(
            ((min_branch - 1) * TELLERS_PER_BRANCH as i32 + 1)..=
            (max_branch * TELLERS_PER_BRANCH as i32)
        );

        let delta: i32 = rand::thread_rng().gen_range(-5000..=5000);

        let start = Instant::now();

        // Simple protocol: Send entire transaction as a single string (1 round-trip)
        // This matches pgbench's simple protocol mode for maximum performance
        let sql = format!(
            "BEGIN;\
             UPDATE account SET account_balance = account_balance + {0} WHERE account_id = {1};\
             SELECT account_balance FROM account WHERE account_id = {1};\
             UPDATE teller SET teller_balance = teller_balance + {0} WHERE teller_id = {2};\
             UPDATE branch SET branch_balance = branch_balance + {0} WHERE branch_id = {3};\
             INSERT INTO history (account_id, teller_id, branch_id, delta, timestamp) VALUES ({1}, {2}, {3}, {0}, CURRENT_TIMESTAMP);\
             COMMIT;",
            delta, account_id, teller_id, branch_id
        );

        client.batch_execute(&sql)?;

        Ok(start.elapsed().as_micros())
    }
}
