use async_trait::async_trait;
use postgres::Client;
use sqlx::PgConnection;

use super::benchmark::{
    Benchmark,
    BenchmarkStmt,
    BenchmarkTransaction,
    PreparedStatements,
    ReadWrite,
    ReadWriteSync,
};
use super::tpcc::TPCC;
use super::tpcb::TPCB;
use super::tpciot::TPCIoT;
use super::tpch::TPCH;

/// Enum wrapper that holds different benchmark implementations
/// This allows runtime dispatch to different benchmark types
pub enum BenchmarkWrapper {
    Tpcc(TPCC),
    Tpcb(TPCB),
    TpcIot(TPCIoT),
    Tpch(TPCH),
}

impl BenchmarkWrapper {
    pub fn new(benchmark_type: &str, scalefactor: u32, min_id: u32, max_id: u32) -> BenchmarkWrapper {
        match benchmark_type {
            "tpcb" | "tpc-b" => {
                BenchmarkWrapper::Tpcb(TPCB::new(scalefactor, min_id, max_id))
            },
            "tpciot" | "tpc-iot" | "iot" => {
                BenchmarkWrapper::TpcIot(TPCIoT::new(scalefactor, min_id, max_id))
            },
            "tpch" | "tpc-h" => {
                BenchmarkWrapper::Tpch(TPCH::new(scalefactor, min_id, max_id))
            },
            _ => {
                BenchmarkWrapper::Tpcc(TPCC::new(scalefactor, min_id, max_id))
            },
        }
    }
}

#[async_trait]
impl ReadWrite for BenchmarkWrapper {
    async fn execute_rw_transaction(&self, conn: &mut PgConnection, transaction: &BenchmarkTransaction) -> Result<u128, Box<dyn std::error::Error>> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.execute_rw_transaction(conn, transaction).await,
            BenchmarkWrapper::Tpcb(b) => b.execute_rw_transaction(conn, transaction).await,
            BenchmarkWrapper::TpcIot(b) => b.execute_rw_transaction(conn, transaction).await,
            BenchmarkWrapper::Tpch(b) => b.execute_rw_transaction(conn, transaction).await,
        }
    }
}

impl Benchmark for BenchmarkWrapper {
    fn initialize_schema(&self, client: &mut Client) -> Result<u128, postgres::Error> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.initialize_schema(client),
            BenchmarkWrapper::Tpcb(b) => b.initialize_schema(client),
            BenchmarkWrapper::TpcIot(b) => b.initialize_schema(client),
            BenchmarkWrapper::Tpch(b) => b.initialize_schema(client),
        }
    }

    fn pre_load_data(&self, client: &mut Client) -> Result<u128, String> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.pre_load_data(client),
            BenchmarkWrapper::Tpcb(b) => b.pre_load_data(client),
            BenchmarkWrapper::TpcIot(b) => b.pre_load_data(client),
            BenchmarkWrapper::Tpch(b) => b.pre_load_data(client),
        }
    }

    fn load_data(&self, client: &mut Client, ids: Vec<u32>) -> Result<u128, String> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.load_data(client, ids),
            BenchmarkWrapper::Tpcb(b) => b.load_data(client, ids),
            BenchmarkWrapper::TpcIot(b) => b.load_data(client, ids),
            BenchmarkWrapper::Tpch(b) => b.load_data(client, ids),
        }
    }

    fn get_default_max_id(&self, client: &mut Client) -> Result<u32, postgres::Error> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.get_default_max_id(client),
            BenchmarkWrapper::Tpcb(b) => b.get_default_max_id(client),
            BenchmarkWrapper::TpcIot(b) => b.get_default_max_id(client),
            BenchmarkWrapper::Tpch(b) => b.get_default_max_id(client),
        }
    }

    fn get_transactions_rw(&self) -> Vec<BenchmarkTransaction> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.get_transactions_rw(),
            BenchmarkWrapper::Tpcb(b) => b.get_transactions_rw(),
            BenchmarkWrapper::TpcIot(b) => b.get_transactions_rw(),
            BenchmarkWrapper::Tpch(b) => b.get_transactions_rw(),
        }
    }

    fn get_table_ddls(&self) -> Vec<BenchmarkStmt> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.get_table_ddls(),
            BenchmarkWrapper::Tpcb(b) => b.get_table_ddls(),
            BenchmarkWrapper::TpcIot(b) => b.get_table_ddls(),
            BenchmarkWrapper::Tpch(b) => b.get_table_ddls(),
        }
    }

    fn get_pkey_ddls(&self) -> Vec<BenchmarkStmt> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.get_pkey_ddls(),
            BenchmarkWrapper::Tpcb(b) => b.get_pkey_ddls(),
            BenchmarkWrapper::TpcIot(b) => b.get_pkey_ddls(),
            BenchmarkWrapper::Tpch(b) => b.get_pkey_ddls(),
        }
    }

    fn get_fkey_ddls(&self) -> Vec<BenchmarkStmt> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.get_fkey_ddls(),
            BenchmarkWrapper::Tpcb(b) => b.get_fkey_ddls(),
            BenchmarkWrapper::TpcIot(b) => b.get_fkey_ddls(),
            BenchmarkWrapper::Tpch(b) => b.get_fkey_ddls(),
        }
    }

    fn get_index_ddls(&self) -> Vec<BenchmarkStmt> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.get_index_ddls(),
            BenchmarkWrapper::Tpcb(b) => b.get_index_ddls(),
            BenchmarkWrapper::TpcIot(b) => b.get_index_ddls(),
            BenchmarkWrapper::Tpch(b) => b.get_index_ddls(),
        }
    }

    fn get_vacuum_stmts(&self) -> Vec<BenchmarkStmt> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.get_vacuum_stmts(),
            BenchmarkWrapper::Tpcb(b) => b.get_vacuum_stmts(),
            BenchmarkWrapper::TpcIot(b) => b.get_vacuum_stmts(),
            BenchmarkWrapper::Tpch(b) => b.get_vacuum_stmts(),
        }
    }
}

impl ReadWriteSync for BenchmarkWrapper {
    fn supports_sync(&self) -> bool {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.supports_sync(),
            BenchmarkWrapper::Tpcb(b) => b.supports_sync(),
            BenchmarkWrapper::TpcIot(b) => b.supports_sync(),
            BenchmarkWrapper::Tpch(b) => b.supports_sync(),
        }
    }

    fn prepare_statements(&self, client: &mut Client) -> Result<PreparedStatements, postgres::Error> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.prepare_statements(client),
            BenchmarkWrapper::Tpcb(b) => b.prepare_statements(client),
            BenchmarkWrapper::TpcIot(b) => b.prepare_statements(client),
            BenchmarkWrapper::Tpch(b) => b.prepare_statements(client),
        }
    }

    fn execute_rw_transaction_sync(
        &self,
        client: &mut Client,
        prepared: &PreparedStatements,
        transaction: &BenchmarkTransaction,
    ) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        match self {
            BenchmarkWrapper::Tpcc(b) => b.execute_rw_transaction_sync(client, prepared, transaction),
            BenchmarkWrapper::Tpcb(b) => b.execute_rw_transaction_sync(client, prepared, transaction),
            BenchmarkWrapper::TpcIot(b) => b.execute_rw_transaction_sync(client, prepared, transaction),
            BenchmarkWrapper::Tpch(b) => b.execute_rw_transaction_sync(client, prepared, transaction),
        }
    }
}
