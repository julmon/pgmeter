use std::error::Error;
use std::fmt;
use std::io::Write;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use postgres::Client;
use sqlx::PgConnection;
use sqlx::Connection;
use rand::{distributions::Alphanumeric, Rng, seq::SliceRandom};

use super::benchmark::{
    Benchmark,
    BenchmarkStmt,
    BenchmarkTransaction,
    PreparedStatements,
    ReadWrite,
    ReadWriteSync,
};

// TPC-C like benchmark
pub struct TPCC {
    pub name: String,
    pub description: String,
    pub scalefactor: u32,
    pub min_id: u32,
    pub max_id: u32,
    // Vector of the read and write transactions that will be executed for this benchmark
    pub transactions_rw: Vec<BenchmarkTransaction>,
    // Tables DDLs
    pub table_ddls: Vec<BenchmarkStmt>,
    // Primary keys DDLs
    pub pkey_ddls: Vec<BenchmarkStmt>,
    // Foreign keys DDLs
    pub fkey_ddls: Vec<BenchmarkStmt>,
    // Additional index DDLs
    pub index_ddls: Vec<BenchmarkStmt>,
    // Vacuum table statememts
    pub vacuum_stmts: Vec<BenchmarkStmt>,
}

#[derive(Debug)]
pub struct TPCCError(String);

impl fmt::Display for TPCCError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for TPCCError {}

// TPC-C-like implementation
impl TPCC {
    pub fn new(scalefactor: u32, min_id: u32, max_id: u32) -> TPCC {
        TPCC {
            name: "TPC-C-like benchmark".to_string(),
            description: "TPC-C-like benchmark implementation.".to_string(),
            scalefactor: scalefactor,
            min_id: min_id,
            max_id: max_id,
            transactions_rw: Vec::from(
                [
                    BenchmarkTransaction {
                        id: 1,
                        weight: 4,
                        name: "Delivery".to_string(),
                        description: "The Delivery transaction".to_string(),
                    },
                    BenchmarkTransaction {
                        id: 2,
                        weight: 45,
                        name: "New-Order".to_string(),
                        description: "The New-Order transaction".to_string(),
                    },
                    BenchmarkTransaction {
                        id: 3,
                        weight: 43,
                        name: "Payment".to_string(),
                        description: "The Payment transaction".to_string(),
                    },
                    BenchmarkTransaction {
                        id: 4,
                        weight: 4,
                        name: "Order-Status".to_string(),
                        description: "The Order-Status transaction".to_string(),
                    },
                    BenchmarkTransaction {
                        id: 5,
                        weight: 4,
                        name: "Stock-Level".to_string(),
                        description: "The Stock-Level transaction".to_string(),
                    },
                ]
            ),
            table_ddls: Vec::from(
                [
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS warehouse CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE warehouse (
                                w_id INTEGER,
                                w_name VARCHAR(10),
                                w_street_1 VARCHAR(20),
                                w_street_2 VARCHAR(20),
                                w_city VARCHAR(20),
                                w_state CHAR(2),
                                w_zip CHAR(9),
                                w_tax REAL,
                                w_ytd NUMERIC(24, 12)
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS district CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE district (
                                d_id INTEGER,
                                d_w_id INTEGER,
                                d_name VARCHAR(10),
                                d_street_1 VARCHAR(20),
                                d_street_2 VARCHAR(20),
                                d_city VARCHAR(20),
                                d_state CHAR(2),
                                d_zip CHAR(9),
                                d_tax REAL,
                                d_ytd NUMERIC(24, 12),
                                d_next_o_id INTEGER
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS customer CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE customer (
                                c_id INTEGER,
                                c_d_id INTEGER,
                                c_w_id INTEGER,
                                c_first VARCHAR(16),
                                c_middle CHAR(2),
                                c_last VARCHAR(16),
                                c_street_1 VARCHAR(20),
                                c_street_2 VARCHAR(20),
                                c_city VARCHAR(20),
                                c_state CHAR(2),
                                c_zip CHAR(9),
                                c_phone CHAR(16),
                                c_since TIMESTAMP,
                                c_credit CHAR(2),
                                c_credit_lim NUMERIC(24, 12),
                                c_discount REAL,
                                c_balance NUMERIC(24, 12),
                                c_ytd_payment NUMERIC(24, 12),
                                c_payment_cnt REAL,
                                c_delivery_cnt REAL,
                                c_data VARCHAR(500)
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS history CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE history (
                                h_c_id INTEGER,
                                h_c_d_id INTEGER,
                                h_c_w_id INTEGER,
                                h_d_id INTEGER,
                                h_w_id INTEGER,
                                h_date TIMESTAMP,
                                h_amount REAL,
                                h_data VARCHAR(24)
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS new_order CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE new_order (
                                no_o_id INTEGER,
                                no_d_id INTEGER,
                                no_w_id INTEGER
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS orders CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE orders (
                                o_id INTEGER,
                                o_d_id INTEGER,
                                o_w_id INTEGER,
                                o_c_id INTEGER,
                                o_entry_d TIMESTAMP,
                                o_carrier_id INTEGER,
                                o_ol_cnt INTEGER,
                                o_all_local INTEGER
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS order_line CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE order_line (
                                ol_o_id INTEGER,
                                ol_d_id INTEGER,
                                ol_w_id INTEGER,
                                ol_number INTEGER,
                                ol_i_id INTEGER,
                                ol_supply_w_id INTEGER,
                                ol_delivery_d TIMESTAMP,
                                ol_quantity INTEGER,
                                ol_amount REAL,
                                ol_dist_info VARCHAR(24)
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS item CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE item (
                                i_id INTEGER,
                                i_im_id INTEGER,
                                i_name VARCHAR(24),
                                i_price REAL,
                                i_data VARCHAR(50)
                            );".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "DROP TABLE IF EXISTS stock CASCADE".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            CREATE TABLE stock (
                                s_i_id INTEGER,
                                s_w_id INTEGER,
                                s_quantity INTEGER,
                                s_dist_01 VARCHAR(24),
                                s_dist_02 VARCHAR(24),
                                s_dist_03 VARCHAR(24),
                                s_dist_04 VARCHAR(24),
                                s_dist_05 VARCHAR(24),
                                s_dist_06 VARCHAR(24),
                                s_dist_07 VARCHAR(24),
                                s_dist_08 VARCHAR(24),
                                s_dist_09 VARCHAR(24),
                                s_dist_10 VARCHAR(24),
                                s_ytd NUMERIC(16, 8),
                                s_order_cnt REAL,
                                s_remote_cnt REAL,
                                s_data VARCHAR(50)
                            );".to_string(),
                    },
                ]
            ),
            pkey_ddls: Vec::from(
                [
                    BenchmarkStmt {
                        sql: "ALTER TABLE warehouse ADD PRIMARY KEY (w_id)".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "ALTER TABLE district ADD PRIMARY KEY (d_w_id, d_id)".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "ALTER TABLE customer ADD PRIMARY KEY (c_w_id, c_d_id, c_id)".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "ALTER TABLE new_order ADD PRIMARY KEY (no_w_id, no_d_id, no_o_id)".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "ALTER TABLE orders ADD PRIMARY KEY (o_w_id, o_d_id, o_id)".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "ALTER TABLE order_line ADD PRIMARY KEY (ol_w_id, ol_d_id, ol_o_id, ol_number)".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "ALTER TABLE stock ADD PRIMARY KEY (s_w_id, s_i_id)".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "ALTER TABLE item ADD PRIMARY KEY (i_id)".to_string(),
                    },
                ]
            ),
            fkey_ddls: Vec::from(
                [
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE district
                            ADD CONSTRAINT fk_district_warehouse
                            FOREIGN KEY (d_w_id)
                            REFERENCES warehouse (w_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE customer
                            ADD CONSTRAINT fk_customer_district
                            FOREIGN KEY (c_w_id, c_d_id)
                            REFERENCES district (d_w_id, d_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE history
                            ADD CONSTRAINT fk_history_customer
                            FOREIGN KEY (h_c_w_id, h_c_d_id, h_c_id)
                            REFERENCES customer (c_w_id, c_d_id, c_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE history
                            ADD CONSTRAINT fk_history_district
                            FOREIGN KEY (h_w_id, h_d_id)
                            REFERENCES district (d_w_id, d_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE new_order
                            ADD CONSTRAINT fk_new_order_orders
                            FOREIGN KEY (no_w_id, no_d_id, no_o_id)
                            REFERENCES orders (o_w_id, o_d_id, o_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE orders
                            ADD CONSTRAINT fk_orders_customer
                            FOREIGN KEY (o_w_id, o_d_id, o_c_id)
                            REFERENCES customer (c_w_id, c_d_id, c_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE order_line
                            ADD CONSTRAINT fk_order_line_orders
                            FOREIGN KEY (ol_w_id, ol_d_id, ol_o_id)
                            REFERENCES orders (o_w_id, o_d_id, o_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE order_line
                            ADD CONSTRAINT fk_order_line_stock
                            FOREIGN KEY (ol_supply_w_id, ol_i_id)
                            REFERENCES stock (s_w_id, s_i_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE stock
                            ADD CONSTRAINT fk_stock_warehouse
                            FOREIGN KEY (s_w_id)
                            REFERENCES warehouse (w_id)
                            ".to_string(),
                    },
                    BenchmarkStmt {
                        sql: r"
                            ALTER TABLE stock
                            ADD CONSTRAINT fk_stock_item
                            FOREIGN KEY (s_i_id)
                            REFERENCES item (i_id)
                            ".to_string(),
                    },
                ]
            ),
            index_ddls: Vec::from(
                [
                     BenchmarkStmt {
                        sql: "CREATE UNIQUE INDEX i_customer_last_first ON customer (c_w_id, c_d_id, c_last, c_first, c_id);".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "CREATE UNIQUE INDEX i_orders ON orders USING BTREE (o_w_id, o_d_id, o_c_id, o_id);".to_string(),
                    },
                    BenchmarkStmt {
                        sql: "CREATE INDEX i_stock_quantity ON stock (s_w_id, s_i_id, s_quantity)".to_string(),
                    },
                ]
            ),
            vacuum_stmts: Vec::from(
                [
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE warehouse".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE district".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE customer".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE history".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE new_order".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE orders".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE order_line".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE item".to_string() },
                    BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE stock".to_string() },
                ]
            ),
        }
    }

    // The Delivery business transaction
    pub async fn delivery(conn: &mut PgConnection, warehouse_id :i32, _min_id :u32, _max_id :u32) -> Result<u128, Box<dyn std::error::Error>> {
        let start = Instant::now();

        let carrier_id :i32 = rand::thread_rng()
            .gen_range(1..=10);

        let mut transaction = conn.begin().await?;

        for district_id in 1..=10 {
            let row: (i32,) = sqlx::query_as(r"
                SELECT no_o_id
                FROM new_order
                WHERE
                    no_w_id = $1
                    AND no_d_id = $2
                ORDER BY no_o_id ASC
                LIMIT 1
                ")
                .bind(&warehouse_id)
                .bind(&district_id)
                .fetch_one(&mut transaction)
                .await?;

            let order_id: i32 = row.0;

            sqlx::query(r"
                DELETE FROM new_order
                WHERE
                    no_o_id = $1
                    AND no_w_id = $2
                    AND no_d_id = $3
                ")
                .bind(&order_id)
                .bind(&warehouse_id)
                .bind(&district_id)
                .execute(&mut transaction)
                .await?;

            let row_orders: (i32,) = sqlx::query_as(r"
                UPDATE orders
                SET
                    o_carrier_id = $1
                WHERE
                    o_id = $2
                    AND o_w_id = $3
                    AND o_d_id = $4
                RETURNING o_c_id
                ")
                .bind(&carrier_id)
                .bind(&order_id)
                .bind(&warehouse_id)
                .bind(&district_id)
                .fetch_one(&mut transaction)
                .await?;

            let customer_id: i32 = row_orders.0;

            sqlx::query(r"
                UPDATE order_line
                SET
                    ol_delivery_d = current_timestamp
                WHERE
                    ol_o_id = $1
                    AND ol_w_id = $2
                    AND ol_d_id = $3
                ")
                .bind(&order_id)
                .bind(&warehouse_id)
                .bind(&district_id)
                .execute(&mut transaction)
                .await?;

            let row_amount: (f64,)= sqlx::query_as(r"
                SELECT SUM(ol_amount * ol_quantity) AS total_ol_amount
                FROM order_line
                WHERE
                    ol_o_id = $1
                    AND ol_w_id = $2
                    AND ol_d_id = $3
                ")
                .bind(&order_id)
                .bind(&warehouse_id)
                .bind(&district_id)
                .fetch_one(&mut transaction)
                .await?;

            let total_ol_amount :f64 = row_amount.0;

            sqlx::query(r"
                UPDATE customer
                SET
                    c_delivery_cnt = c_delivery_cnt + 1,
                    c_balance = c_balance + $1
                WHERE
                    c_id = $2
                    AND c_w_id = $3
                    AND c_d_id = $4;
                ")
                .bind(&total_ol_amount)
                .bind(&customer_id)
                .bind(&warehouse_id)
                .bind(&district_id)
                .execute(&mut transaction)
                .await?;
        }
        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    // The New-Order business transaction
    pub async fn new_order(conn: &mut PgConnection, warehouse_id :i32, min_id :u32, max_id :u32) -> Result<u128, Box<dyn std::error::Error>> {
        let district_id :i32 = rand::thread_rng()
            .gen_range(1..=10);
        let customer_id :i32 = rand::thread_rng()
            .gen_range(1..=3000);

        // Number of order_line entries
        let ol_cnt :i32 = rand::thread_rng()
            .gen_range(5..=15);
        let mut ol_all_local :i32 = 1;

        // Build order_lines
        let mut order_line_data = Vec::new();
        let mut item_ids = Vec::new();

        let mut ol_i_id :i32;

        // Generate 1% of rollback by setting up an invalid item id
        let mut rbk = rand::thread_rng()
            .gen_range(1..=100);

        for ol_number in 1..=ol_cnt {
            let mut ol_supply_w_id :i32 = warehouse_id;
            let ol_quantity :i32 = rand::thread_rng()
                .gen_range(1..=10);

            // Generate ol_i_id / item id
            loop {
                ol_i_id = rand::thread_rng()
                    .gen_range(1..=100_000);
                if !item_ids.contains(&ol_i_id) {
                    item_ids.push(ol_i_id.clone());
                    break;
                }
            }
            if rbk == 1 {
                ol_i_id = 999_999;
                rbk = 0;
            }

            // If we have more than one warehouse, then ol_supply_w_id can be different from
            // warehouse_id
            if (max_id - min_id) > 0 {
                let x :u8 = rand::thread_rng()
                    .gen_range(1..=100);
                if x == 1 {
                    ol_all_local = 0;
                    // Pickup random warehouse id different from warehouse_id
                    while ol_supply_w_id == warehouse_id {
                        ol_supply_w_id = rand::thread_rng()
                            .gen_range(min_id as i32..=max_id as i32);
                    }
                }
            }
            order_line_data.push((ol_number, ol_supply_w_id, ol_quantity, ol_i_id));
        }

        // Starting database transaction
        let start = Instant::now();
        let mut transaction = conn.begin().await?;

        sqlx::query(r"
            SELECT w_tax FROM warehouse WHERE w_id = $1
            ")
            .bind(&warehouse_id)
            .execute(&mut transaction)
            .await?;

        let row_district: (f32, i32,) = sqlx::query_as(r"
             UPDATE district
             SET d_next_o_id = d_next_o_id + 1
             WHERE
                d_w_id = $1
                AND d_id = $2
            RETURNING d_tax, d_next_o_id AS o_id
            ")
            .bind(&warehouse_id)
            .bind(&district_id)
            .fetch_one(&mut transaction)
            .await?;

        let mut o_id :i32 = row_district.1;
        o_id -= 1;

        sqlx::query(r"
            SELECT c_discount, c_last, c_credit
            FROM customer
            WHERE
                c_w_id = $1
                AND c_d_id = $2
                AND c_id = $3
            ")
            .bind(&warehouse_id)
            .bind(&district_id)
            .bind(&customer_id)
            .execute(&mut transaction)
            .await?;

        // Inserting one new row into orders and new_order
        sqlx::query(r"
            INSERT INTO orders (o_id, o_d_id, o_w_id, o_c_id, o_entry_d, o_ol_cnt, o_all_local)
            VALUES ($1, $2, $3, $4, NOW(), $5, $6)
            ")
            .bind(&o_id)
            .bind(&district_id)
            .bind(&warehouse_id)
            .bind(&customer_id)
            .bind(&ol_cnt)
            .bind(&ol_all_local)
            .execute(&mut transaction)
            .await?;

        sqlx::query(r"
            INSERT INTO new_order (no_o_id, no_d_id, no_w_id)
            VALUES ($1, $2, $3)
            ")
            .bind(&o_id)
            .bind(&district_id)
            .bind(&warehouse_id)
            .execute(&mut transaction)
            .await?;

        let stock_query = format!("SELECT s_quantity, s_dist_{:0>2} AS s_dist, s_data FROM stock WHERE s_i_id = $1 AND s_w_id = $2", district_id);

        for (ol_number, ol_supply_w_id, ol_quantity, ol_i_id) in order_line_data {
            let row_item: Vec<(f32, String, String)> = sqlx::query_as(r"
                SELECT i_price, i_name, i_data FROM item WHERE i_id = $1
            ")
            .bind(&ol_i_id)
            .fetch_all(&mut transaction)
            .await?;

            if row_item.len() == 0 {
                // Item not found then we must rollback the transaction
                transaction.rollback().await?;
                return Err(Box::new(TPCCError("New-order transaction rollbacked. Item not found.".into())));
            }

            let i_price :f32 = row_item[0].0;
            let ol_amount :f32 = i_price * ol_quantity as f32;

            // Execute stock query
            let row_stock :(i32, String, String) = sqlx::query_as(&stock_query)
                .bind(&ol_i_id)
                .bind(&ol_supply_w_id)
                .fetch_one(&mut transaction)
                .await?;

            let mut s_quantity :i32 = row_stock.0;
            let s_dist :String = row_stock.1;

            // Update stock
            if (s_quantity - ol_quantity) > 10 {
                s_quantity = s_quantity - ol_quantity;
            }
            else {
                s_quantity = s_quantity - ol_quantity + 91;
            }
            let mut s_remote_cnt_inc :f32 = 0.0;
            if ol_supply_w_id != warehouse_id {
                s_remote_cnt_inc = 1.0;
            }
            sqlx::query(r"
                UPDATE stock SET
                    s_quantity = $3,
                    s_ytd = s_ytd + $4::FLOAT,
                    s_order_cnt = s_order_cnt + 1,
                    s_remote_cnt = s_remote_cnt + $5
                WHERE
                    s_i_id = $1
                    AND s_w_id = $2
                ")
                .bind(&ol_i_id)
                .bind(&ol_supply_w_id)
                .bind(&s_quantity)
                .bind(&ol_quantity)
                .bind(&s_remote_cnt_inc)
                .execute(&mut transaction)
                .await?;

            // Insert into order_line
            sqlx::query(r"
                INSERT INTO order_line (
                    ol_o_id, ol_d_id, ol_w_id, ol_number, ol_i_id, ol_supply_w_id, ol_quantity,
                    ol_amount, ol_dist_info
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9
                )
                ")
                .bind(&o_id)
                .bind(&district_id)
                .bind(&warehouse_id)
                .bind(&ol_number)
                .bind(&ol_i_id)
                .bind(&ol_supply_w_id)
                .bind(&ol_quantity)
                .bind(&ol_amount)
                .bind(&s_dist)
                .execute(&mut transaction)
                .await?;
        }

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    // The Payment business transaction
    pub async fn payment(conn: &mut PgConnection, warehouse_id :i32, min_id :u32, max_id :u32) -> Result<u128, Box<dyn std::error::Error>> {
        let x :u8 = rand::thread_rng()
            .gen_range(1..=100);
        let y :u8 = rand::thread_rng()
            .gen_range(1..=100);

        let district_id :i32 = rand::thread_rng()
            .gen_range(1..=10);

        let c_d_id :i32;
        let mut c_w_id :i32;

        if x <= 85 {
            c_d_id = district_id;
            c_w_id = warehouse_id;
        }
        else {
            c_d_id = rand::thread_rng()
                .gen_range(1..=10);
             if (max_id - min_id) > 0 {
                // Pickup random warehouse id different from warehouse_id
                loop {
                    c_w_id = rand::thread_rng()
                        .gen_range(min_id as i32..=max_id as i32);
                    if c_w_id != warehouse_id {
                        break;
                    }
                }
            }
            else {
                c_w_id = warehouse_id;
            }
        }
        let mut c_last: String = "".to_string();
        let mut c_id :i32 = rand::thread_rng()
            .gen_range(1..=3_000);

        if y <= 60 {
            let t :u32 = rand::thread_rng()
                .gen_range(1..=1_000);
            c_last = TPCC::gen_last(t);
        }
        let h_amount :f32 = rand::thread_rng()
            .gen_range(1.00..=5_000.00);

        let start = Instant::now();
        let mut transaction = conn.begin().await?;

        let row_warehouse: (String,) = sqlx::query_as(r"
            UPDATE warehouse
            SET w_ytd = w_ytd + $1::FLOAT
            WHERE w_id = $2
            RETURNING w_name
            ")
            .bind(&h_amount)
            .bind(&warehouse_id)
            .fetch_one(&mut transaction)
            .await?;

        let w_name: String = row_warehouse.0;

        let row_district: (String,) = sqlx::query_as(r"
            UPDATE district
            SET d_ytd = d_ytd + $1::FLOAT
            WHERE
                d_w_id = $2
                AND d_id = $3
            RETURNING d_name
            ")
            .bind(&h_amount)
            .bind(&warehouse_id)
            .bind(&district_id)
            .fetch_one(&mut transaction)
            .await?;

        let d_name: String = row_district.0;

        if y <= 60 {
            let row_c_id: Vec<(i32,)> = sqlx::query_as(r"
                SELECT c_id FROM customer
                WHERE c_w_id = $1 AND c_d_id = $2 AND c_last = $3
                ORDER BY c_first ASC
                ")
                .bind(&c_w_id)
                .bind(&c_d_id)
                .bind(&c_last)
                .fetch_all(&mut transaction)
                .await?;

            if row_c_id.len() == 0 {
                transaction.rollback().await?;
                return Err(Box::new(TPCCError("Payment transaction rollbacked. Customer not found (c_last).".into())));
            }

            let n = row_c_id.len();
            c_id = row_c_id[n / 2].0;
        }

        let row_customer: (String,) = sqlx::query_as(r"
            SELECT
                c_credit
            FROM customer
            WHERE
                c_w_id = $1
                AND c_d_id = $2
                AND c_id = $3
            ")
            .bind(&c_w_id)
            .bind(&c_d_id)
            .bind(&c_id)
            .fetch_one(&mut transaction)
            .await?;

        let c_credit: String = row_customer.0;

        if c_credit == "BC".to_string() {
            let pre_c_data = format!("{} {} {} {} {} {}", c_id, c_d_id, c_w_id, district_id, warehouse_id, h_amount);
            sqlx::query(r"
                UPDATE customer
                SET
                    c_balance = c_balance - $1::FLOAT,
                    c_ytd_payment = c_ytd_payment + 1,
                    c_data = substring($5||' '||c_data, 1, 500)
                WHERE
                    c_id = $2 AND c_d_id = $3 AND c_w_id = $4
            ")
            .bind(&h_amount)
            .bind(&c_id)
            .bind(&c_d_id)
            .bind(&c_w_id)
            .bind(&pre_c_data)
            .execute(&mut transaction)
            .await?;
        }
        else {
            sqlx::query(r"
                UPDATE customer
                SET
                    c_balance = c_balance - $1::FLOAT,
                    c_ytd_payment = c_ytd_payment + 1
                WHERE
                    c_id = $2 AND c_d_id = $3 AND c_w_id = $4
            ")
            .bind(&h_amount)
            .bind(&c_id)
            .bind(&c_d_id)
            .bind(&c_w_id)
            .execute(&mut transaction)
            .await?;
        }
        sqlx::query(r"
            INSERT INTO history
                (h_c_id, h_c_d_id, h_c_w_id, h_d_id, h_w_id, h_date, h_amount, h_data)
            VALUES
                ($1, $2, $3, $4, $5, NOW(), $6, substring($7||'    '||$8, 1, 24))
            ")
            .bind(&c_id)
            .bind(&c_d_id)
            .bind(&c_w_id)
            .bind(&district_id)
            .bind(&warehouse_id)
            .bind(&h_amount)
            .bind(&w_name)
            .bind(&d_name)
            .execute(&mut transaction)
            .await?;

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    // The Order-Status business transaction
    pub async fn order_status(conn: &mut PgConnection, warehouse_id :i32, _min_id :u32, _max_id :u32) -> Result<u128, Box<dyn std::error::Error>> {
        let y :u8 = rand::thread_rng()
            .gen_range(1..=100);

        let district_id :i32 = rand::thread_rng()
            .gen_range(1..=10);

        let mut c_last: String = "".to_string();
        let mut c_id :i32 = rand::thread_rng()
            .gen_range(1..=3_000);

        if y <= 60 {
            let t :u32 = rand::thread_rng()
                .gen_range(1..=999);
            c_last = TPCC::gen_last(t);
        }

        let start = Instant::now();
        let mut transaction = conn.begin().await?;

        if y <= 60 {
            let row_c_id: Vec<(i32,)> = sqlx::query_as(r"
                SELECT c_id FROM customer
                WHERE c_w_id = $1 AND c_d_id = $2 AND c_last = $3
                ORDER BY c_first ASC
                ")
                .bind(&warehouse_id)
                .bind(&district_id)
                .bind(&c_last)
                .fetch_all(&mut transaction)
                .await?;

            if row_c_id.len() == 0 {
                transaction.rollback().await?;
                return Err(Box::new(TPCCError("Payment transaction rollbacked. Customer not found (c_last).".into())));
            }

            let n = row_c_id.len();
            c_id = row_c_id[n / 2].0;
        }

        sqlx::query(r"
            SELECT
                c_balance, c_first, c_middle, c_last
            FROM customer
            WHERE
                c_w_id = $1
                AND c_d_id = $2
                AND c_id = $3
            ")
            .bind(&warehouse_id)
            .bind(&district_id)
            .bind(&c_id)
            .execute(&mut transaction)
            .await?;

        let row_order: (i32,) = sqlx::query_as(r"
            SELECT
                o_id
            FROM orders
            WHERE
                o_w_id = $1
                AND o_d_id = $2
                AND o_c_id = $3
            ORDER BY o_entry_d DESC LIMIT 1
            ")
            .bind(&warehouse_id)
            .bind(&district_id)
            .bind(&c_id)
            .fetch_one(&mut transaction)
            .await?;

        let o_id :i32 = row_order.0;

        sqlx::query(r"
            SELECT
                ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_delivery_d
            FROM order_line
            WHERE
                ol_w_id = $1
                AND ol_d_id = $2
                AND ol_o_id = $3
            ")
            .bind(&warehouse_id)
            .bind(&district_id)
            .bind(&o_id)
            .execute(&mut transaction)
            .await?;

        transaction.commit().await?;
        Ok(start.elapsed().as_micros())
    }

    pub async fn stock_level(conn: &mut PgConnection, warehouse_id :i32, _min_id :u32, _max_id :u32) -> Result<u128, Box<dyn std::error::Error>> {
        let district_id :i32 = rand::thread_rng()
            .gen_range(1..=10);
        let threshold :i32 = rand::thread_rng()
            .gen_range(10..=20);

        let start = Instant::now();
        let mut transaction = conn.begin().await?;

        let row_district: (i32,) = sqlx::query_as(r"
            SELECT d_next_o_id
            FROM district
            WHERE d_w_id = $1 AND d_id = $2
            ")
            .bind(&warehouse_id)
            .bind(&district_id)
            .fetch_one(&mut transaction)
            .await?;

        let d_next_o_id :i32 = row_district.0;

        let rows_order_line: Vec<(i32,)> = sqlx::query_as(r"
            SELECT
                DISTINCT ol_i_id
            FROM order_line
            WHERE
                ol_w_id = $1
                AND ol_d_id = $2
                AND ol_o_id < $3
                AND ol_o_id >= ($3 - 20)
            ")
            .bind(&warehouse_id)
            .bind(&district_id)
            .bind(&d_next_o_id)
            .fetch_all(&mut transaction)
            .await?;

        for row in rows_order_line {
            let ol_i_id :i32 = row.0;
            sqlx::query(r"
                SELECT s_quantity
                FROM stock
                WHERE
                    s_w_id = $1
                    AND s_i_id = $2
                    AND s_quantity < $3
            ")
            .bind(&warehouse_id)
            .bind(&ol_i_id)
            .bind(&threshold)
            .execute(&mut transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(start.elapsed().as_micros())
    }

    // Returns a randomly generated alphanumeric string of length between min_length and max_length
    fn random_alpha_string(min_length :usize, max_length: usize) -> String {
        let mut string_length: usize = max_length;
        if min_length < max_length {
            string_length = rand::thread_rng()
                .gen_range(min_length..=max_length);
        }
        let string_val :String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(string_length)
            .map(char::from)
            .collect();

        string_val
    }

    // Returns a randomly generated Zip code
    fn random_zip() -> String {
        let part1 :u16 = rand::thread_rng()
            .gen_range(1..10000);

        format!("{:0>4}11111", part1)
    }

    // Returns a randomly generated state code (2 capital letters)
    fn random_state() -> String {
        let mut rng = &mut rand::thread_rng();
        let sample = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".as_bytes();
        let val: Vec<u8> = sample.choose_multiple(&mut rng, 2).cloned().collect();

        String::from_utf8(val).unwrap()
    }

    // Generate customer's last name
    fn gen_last(customer_id :u32) -> String {
        let mut f_customer_id :u32 = customer_id;
        if customer_id >= 1000 {
            f_customer_id = rand::thread_rng()
                .gen_range(0..=999);
        }
        let syllables = vec!["BAR", "OUGHT", "ABLE", "PRIS", "PRES", "ESE", "ANTI",
            "CALLY", "ATION", "EING"];

        let f_customer_id_fmt = format!("{:0>3}", f_customer_id);

        let mut last :String = "".to_string();
        for c in f_customer_id_fmt.chars() {
            let i :u32 = c.to_digit(10).unwrap();
            last = format!("{}{}", last, syllables[i as usize]);
        }

        last
    }

    pub fn populate_item(client: &mut Client) -> Result<(), String> {
        // Populate the item table with 100_000 items
        let n_items = 100_000 as u32;
        // Number of lines submitted for each COPY operation
        let batch_size = 500 as u32;
        // Number of batch, based on the total number of items and batch size
        let n_batch = ((n_items as f64 / batch_size as f64) as f64).ceil() as u32;

        for b in 1..=n_batch {
            // Start a new copy from stdin op.
            let mut writer = match client.copy_in("COPY item FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            // Calculate items interval
            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_items {
                batch_end = n_items;
            }

            // Fill the write buffer batch_size items
            for i_id in batch_start..=batch_end {
                // Generate random data
                // Column i_name
                let i_name :String = TPCC::random_alpha_string(14, 24);
                // Column i_price
                let i_price :f64 = rand::thread_rng()
                    .gen_range(1.00..=100.00);
                // Column i_im_id
                let i_im_id :i32 = rand::thread_rng()
                    .gen_range(1..=10_000);
                // Column i_data
                let mut i_data :String = TPCC::random_alpha_string(26, 50);
                let i_data_length = i_data.len();
                let orig :u32 = rand::thread_rng()
                    .gen_range(1..=100);
                if orig <= 10 {
                    let pos :usize = rand::thread_rng()
                        .gen_range(1..(i_data_length - 8));
                    i_data.replace_range(pos..=(pos + 8), "ORIGINAL");
                }

                let line = format!("{}\t{}\t{}\t{:.2}\t{}\n", i_id, i_im_id, i_name, i_price, i_data);
                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }
            }

            // Finishing the copy order for the current batch
            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }

        Ok(())
    }

    pub fn populate_warehouse(client: &mut Client, warehouse_id: u32) -> Result<(), String> {
        // Start a new copy from stdin op.
        let mut writer = match client.copy_in("COPY warehouse FROM stdin") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        // Generate random data
        let w_name :String = TPCC::random_alpha_string(6, 10);
        let w_street1 :String = TPCC::random_alpha_string(10, 20);
        let w_street2 :String = TPCC::random_alpha_string(10, 20);
        let w_city :String = TPCC::random_alpha_string(10, 20);
        let w_state: String = TPCC::random_state();
        let w_zip: String = TPCC::random_zip();
        let w_tax: f64 = rand::thread_rng()
            .gen_range(0.10..0.20);

        let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t300000.00\n", warehouse_id, w_name, w_street1, w_street2, w_city, w_state, w_zip, w_tax);

        match writer.write_all(line.as_bytes()) {
            Ok(_) => (),
            Err(e) => return Err(e.to_string()),
        }
        // Finishing the copy order
        match writer.finish() {
            Ok(_) => (),
            Err(e) => return Err(e.to_string()),
        }

        Ok(())
    }

    pub fn populate_district(client: &mut Client, warehouse_id: u32) -> Result<(), String> {
        // Calculate district ids interval
        let district_start = 1;
        let district_end = 10;

        // Start a new copy from stdin op.
        let mut writer = match client.copy_in("COPY district FROM stdin") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        // Fill the write buffer
        for district_id in district_start..=district_end {
            // Generate random data
            let d_name :String = TPCC::random_alpha_string(6, 10);
            let d_street1 :String = TPCC::random_alpha_string(10, 20);
            let d_street2 :String = TPCC::random_alpha_string(10, 20);
            let d_city :String = TPCC::random_alpha_string(10, 20);
            let d_state: String = TPCC::random_state();
            let d_zip: String = TPCC::random_zip();
            let d_tax: f64 = rand::thread_rng()
                .gen_range(0.10..0.20);

            let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t30000.00\t3001\n", district_id, warehouse_id, d_name, d_street1, d_street2, d_city, d_state, d_zip, d_tax);

            match writer.write_all(line.as_bytes()) {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }

        // Finishing the copy order
        match writer.finish() {
            Ok(_) => (),
            Err(e) => return Err(e.to_string()),
        }

        Ok(())
    }

    pub fn populate_customer(client: &mut Client, warehouse_id: u32) -> Result<(), String> {
        // Populate the customer table with 30_000 items per warehouse
        let n_items = 30_000 as u32;
        let n_customer_per_district = 3_000 as u32;

        let mut customer_id :u32 = 1;
        let mut district_id :u32 = 1;

        // Number of lines submitted for each COPY operation
        let batch_size = 500 as u32;
        // Number of batch, based on the number of items and batch size
        let n_batch = ((n_items as f64 / batch_size as f64) as f64).ceil() as u32;

        // Column since
        let c_since: String = format!("{}", Utc::now().format("%Y-%m-%d %H:%M:%S"));

        for b in 1..=n_batch {
            // Start a new copy from stdin op.
            let mut writer = match client.copy_in("COPY customer FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            // Calculate items interval
            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_items {
                batch_end = n_items;
            }

            // Fill the write buffer batch_size items
            for _ in batch_start..=batch_end {
                let c_first :String = TPCC::random_alpha_string(8, 16);
                let c_middle: String = "OE".to_string();
                let c_last: String = TPCC::gen_last(customer_id);
                let c_street1 :String = TPCC::random_alpha_string(10, 20);
                let c_street2 :String = TPCC::random_alpha_string(10, 20);
                let c_city :String = TPCC::random_alpha_string(10, 20);
                let c_state: String = TPCC::random_state();
                let c_zip: String = TPCC::random_zip();
                let c_phone: u64 = rand::thread_rng()
                    .gen_range(1000000000000000..=9999999999999999);
                let c_discount: f64 = rand::thread_rng()
                    .gen_range(0.00..=0.50);
                let c_data: String = TPCC::random_alpha_string(300, 500);
                // Column c_credit
                let mut c_credit: String = "GC".to_string();
                let i = rand::thread_rng()
                    .gen_range(1..=10);
                if i == 1 {
                    c_credit = "BC".to_string();
                }

                let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t50000.00\t{:.2}\t-10.00\t10.00\t1\t0\t{}\n", customer_id, district_id, warehouse_id, c_first, c_middle, c_last, c_street1, c_street2, c_city, c_state, c_zip, c_phone, c_since, c_credit, c_discount, c_data);

                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }
                // Increment customer_id
                customer_id += 1;

                // Increment district_id and reset customer_id if we have populated
                // n_customer_per_district customers.
                if customer_id > n_customer_per_district {
                    district_id += 1;
                    customer_id = 1;
                }
            }
            // Finishing the copy order for the current batch
            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    pub fn populate_orders(client: &mut Client, warehouse_id: u32, o_entry_d: String) -> Result<(), String> {
        // Populate the orders table with 30_000 items per warehouse (1 per customer)
        let n_items = 30_000 as u32;
        // Number of lines submitted for each COPY operation
        let batch_size = 500 as u32;
        // Number of batch, based on the number of items and batch size
        let n_batch = ((n_items as f64 / batch_size as f64) as f64).ceil() as u32;

        let n_orders_per_district :u32 = 3_000;
        let mut orders_id :u32 = 1;
        let mut customer_id: u32 = 1;
        let mut district_id: u32 = 1;

        for b in 1..=n_batch {
            // Start a new copy from stdin op.
            let mut writer = match client.copy_in("COPY orders FROM stdin NULL AS ''") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            // Calculate items interval
            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_items {
                batch_end = n_items;
            }

            // Fill the write buffer batch_size items
            for _ in batch_start..=batch_end {
                // Column o_carrier_id
                let carrier_id :u32 = rand::thread_rng()
                    .gen_range(1..=10);
                let mut o_carrier_id :String = "".to_string();
                if orders_id < 2_101 {
                    o_carrier_id = format!("{}", carrier_id);
                }
                // Generate the number of order_line entries
                let o_ol_cnt :u32 = (orders_id * (orders_id + district_id + warehouse_id)) % 11 + 5;

                let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t1\n", orders_id, district_id, warehouse_id, customer_id, o_entry_d, o_carrier_id, o_ol_cnt);

                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }
                // Increment orders_id and customer_id
                orders_id += 1;
                customer_id += 1;
                // Increment district_id and reset orders_id and customer_id if we have populated
                // n_orders_per_district orders.
                if orders_id > n_orders_per_district {
                    district_id += 1;
                    orders_id = 1;
                    customer_id = 1;
                }
            }
            // Finishing the copy order for the current batch
            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    pub fn populate_history(client: &mut Client, warehouse_id: u32) -> Result<(), String> {
        // Populate the history table with 30_000 items per warehouse
        let n_items = 30_000 as u32;
        // Number of lines submitted for each COPY operation
        let batch_size = 500 as u32;
        // Number of batch, based on the number of items and batch size
        let n_batch = ((n_items as f64 / batch_size as f64) as f64).ceil() as u32;

        let n_customer_per_district :u32 = 3_000;
        let mut customer_id: u32 = 1;
        let mut district_id: u32 = 1;

        // Column date
        let h_date: String = format!("{}", Utc::now().format("%Y-%m-%d %H:%M:%S"));

        for b in 1..=n_batch {
            // Start a new copy from stdin op.
            let mut writer = match client.copy_in("COPY history FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            // Calculate items interval
            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_items {
                batch_end = n_items;
            }

            // Fill the write buffer batch_size items
            for _ in batch_start..=batch_end {
                // Column h_data
                let h_data :String = TPCC::random_alpha_string(12, 24);
                let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t10.0\t{}\n", customer_id, district_id, warehouse_id, district_id, warehouse_id, h_date, h_data);

                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }
                // Increment customer_id
                customer_id += 1;
                // Increment district_id and reset customer_id if we have populated
                // n_customer_per_district history.
                if customer_id > n_customer_per_district {
                    district_id += 1;
                    customer_id = 1;
                }
            }
            // Finishing the copy order for the current batch
            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    pub fn populate_stock(client: &mut Client, warehouse_id: u32) -> Result<(), String> {
        // Populate the stock table with 100_000 items per warehouse
        let n_items = 100_000 as u32;

        // Number of lines submitted for each COPY operation
        let batch_size = 500 as u32;
        // Number of batch, based on the number of items and batch size
        let n_batch = ((n_items as f64 / batch_size as f64) as f64).ceil() as u32;

        let mut item_id :u32 = 1;

        for b in 1..=n_batch {
            // Start a new copy from stdin op.
            let mut writer = match client.copy_in("COPY stock FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            // Calculate items interval
            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_items {
                batch_end = n_items;
            }

            // Fill the write buffer batch_size items
            for _ in batch_start..=batch_end {
                let s_dist_01 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_02 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_03 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_04 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_05 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_06 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_07 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_08 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_09 :String = TPCC::random_alpha_string(24, 24);
                let s_dist_10 :String = TPCC::random_alpha_string(24, 24);
                let s_quantity: u32 = rand::thread_rng()
                    .gen_range(10..=100);
                // Column s_data
                let mut s_data :String = TPCC::random_alpha_string(26, 50);
                let s_data_length = s_data.len();
                let orig :u32 = rand::thread_rng()
                    .gen_range(1..=100);
                if orig <= 10 {
                    let pos :usize = rand::thread_rng()
                        .gen_range(1..(s_data_length - 8));
                    s_data.replace_range(pos..=(pos + 8), "ORIGINAL");
                }

                let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t0\t0\t0\t{}\n", item_id, warehouse_id, s_quantity, s_dist_01, s_dist_02, s_dist_03, s_dist_04, s_dist_05, s_dist_06, s_dist_07, s_dist_08, s_dist_09, s_dist_10, s_data);

                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }
                // Increment item_id
                item_id += 1;
            }
            // Finishing the copy order for the current batch
            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    pub fn populate_new_order(client: &mut Client, warehouse_id: u32) -> Result<(), String> {
        // Populate the new_order table with 9_000 items per warehouse
        let n_items = 9_000 as u32;

        let orders_start = 2101;
        let orders_end = 3000;

        // Number of lines submitted for each COPY operation
        let batch_size = 500 as u32;
        // Number of batch, based on the number of items and batch size
        let n_batch = ((n_items as f64 / batch_size as f64) as f64).ceil() as u32;

        // Initialize orders_id and district_id
        let mut orders_id = orders_start;
        let mut district_id = 1;

        for b in 1..=n_batch {
            // Start a new copy from stdin op.
            let mut writer = match client.copy_in("COPY new_order FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            // Calculate items interval
            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_items {
                batch_end = n_items;
            }

            // Fill the write buffer batch_size items
            for _ in batch_start..=batch_end {
                let line = format!("{}\t{}\t{}\n", orders_id, district_id, warehouse_id);
                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }
                // Increment orders_id
                orders_id += 1;

                // Check if we have reached the "orders" limit and then move to the next district
                if orders_id > orders_end {
                    orders_id = orders_start;
                    district_id += 1;
                }
            }
            // Finishing the copy order for the current batch
            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    pub fn populate_order_line(client: &mut Client, warehouse_id: u32, ol_delivery_d :String) -> Result<(), String> {
        // Populate the order_line table for 30_000 orders per warehouse
        // Each orders has between 5 and 15 order_line entries
        let n_orders = 30_000 as u32;
        // Number of lines submitted for each COPY operation
        // This is set to a low value because we expect to get *~10 more lines at each iteration of
        // the main loop. Batch size will be actually growing to around 500 lines.
        let batch_size = 50 as u32;
        // Number of batch, based on the total number of orders and batch size
        let n_batch = ((n_orders as f64 / batch_size as f64) as f64).ceil() as u32;

        let n_orders_per_district :u32 = 3_000;

        let mut orders_id :u32 = 1;
        let mut district_id: u32 = 1;

        for b in 1..=n_batch {
            // Start a new copy from stdin op.
            let mut writer = match client.copy_in("COPY order_line FROM stdin NULL AS ''") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            // Calculate items interval
            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_orders {
                batch_end = n_orders;
            }

            // Fill the write buffer batch_size items
            for _ in batch_start..=batch_end {
                // Generate the number of order_line entries
                let ol_cnt :u32 = (orders_id * (orders_id + district_id + warehouse_id)) % 11 + 5;

                // Generate the list of item ids we will need
                let mut rng = rand::thread_rng();
                let item_ids = rand::seq::index::sample(&mut rng, 100_000, ol_cnt as usize).into_vec();

                let mut cur_ol_amount: f64;
                let mut cur_ol_delivery_d: String;
                // Build one line per item id.
                for i in 1..=ol_cnt {
                    let item_id = item_ids[(i - 1) as usize] + 1;
                    // Build ol_amount and ol_delivery_d
                    if orders_id >= 2101 {
                        cur_ol_amount = rand::thread_rng()
                            .gen_range(0.01..9999.99);
                        cur_ol_delivery_d = "".to_string();
                    }
                    else {
                        cur_ol_amount = 0.00;
                        cur_ol_delivery_d = ol_delivery_d.clone();
                    }
                    // Column ol_dist_info
                    let ol_dist_info :String = TPCC::random_alpha_string(24, 24);

                    let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t5\t{}\t{}\n", orders_id, district_id, warehouse_id, i, item_id, warehouse_id, cur_ol_delivery_d, cur_ol_amount, ol_dist_info);

                    match writer.write_all(line.as_bytes()) {
                        Ok(_) => (),
                        Err(e) => return Err(e.to_string()),
                    }
                }
                // Increment orders_id
                orders_id += 1;
                // Increment district_id and reset orders_id if we have populated n_orders_per_district
                // orders.
                if orders_id > n_orders_per_district {
                    district_id += 1;
                    orders_id = 1;
                }
            }
            // Finishing the copy order for the current batch
            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ReadWrite for TPCC {
    async fn execute_rw_transaction(&self, conn :&mut PgConnection, transaction :&BenchmarkTransaction) -> Result<u128, Box<dyn std::error::Error>> {
        // Generate the warehouse id we are going to hit
        // The used type is i32 because it matches with Postgres' int4 type.
        let warehouse_id :i32 = rand::thread_rng()
            .gen_range(self.min_id..=self.max_id) as i32;

        match transaction.id {
            1 => {
                match TPCC::delivery(conn, warehouse_id, self.min_id, self.max_id).await {
                    Ok(duration) => return Ok(duration),
                    Err(e) => return Err(Box::new(TPCCError(e.to_string()))),
                }
            },
            2 => {
                match TPCC::new_order(conn, warehouse_id, self.min_id, self.max_id).await {
                    Ok(duration) => return Ok(duration),
                    Err(e) => return Err(Box::new(TPCCError(e.to_string()))),
                }
            },
            3 => {
                match TPCC::payment(conn, warehouse_id, self.min_id, self.max_id).await {
                    Ok(duration) => return Ok(duration),
                    Err(e) => return Err(Box::new(TPCCError(e.to_string()))),
                }
            },
            4 => {
                match TPCC::order_status(conn, warehouse_id, self.min_id, self.max_id).await {
                    Ok(duration) => return Ok(duration),
                    Err(e) => return Err(Box::new(TPCCError(e.to_string()))),
                }
            },
            5 => {
                match TPCC::stock_level(conn, warehouse_id, self.min_id, self.max_id).await {
                    Ok(duration) => return Ok(duration),
                    Err(e) => return Err(Box::new(TPCCError(e.to_string()))),
                }
            },
            0 | 6..=u16::MAX => todo!(),
        };
    }
}

impl Benchmark for TPCC {
    fn initialize_schema(&self, client: &mut Client) -> Result<u128, postgres::Error> {
        let start = Instant::now();

        let mut transaction = client.transaction()?;
        // Sequentially create tables
        for table_ddl in self.get_table_ddls().iter() {
            transaction.batch_execute(&table_ddl.sql)?;
        }
        transaction.commit()?;

        Ok(start.elapsed().as_micros())
    }

    // On TPC-C-like benchmark, we need to:
    // - populate the item table with 100k randomly generated rows
    fn pre_load_data(&self, client: &mut Client) -> Result<u128, String> {
        let start = Instant::now();

        // Populate the item table
        TPCC::populate_item(client)?;

        Ok(start.elapsed().as_micros())
    }

    fn load_data(&self, client: &mut Client, warehouse_ids: Vec<u32>) -> Result<u128, String> {
        let start = Instant::now();
        for warehouse_id in warehouse_ids {
            // Orders entry date
            let o_entry_d: String = format!("{}", Utc::now().format("%Y-%m-%d %H:%M:%S"));
            // Populate tables
            TPCC::populate_warehouse(client, warehouse_id)?;
            TPCC::populate_district(client, warehouse_id)?;
            TPCC::populate_stock(client, warehouse_id)?;
            TPCC::populate_customer(client, warehouse_id)?;
            TPCC::populate_history(client, warehouse_id)?;
            TPCC::populate_orders(client, warehouse_id, o_entry_d.clone())?;
            TPCC::populate_new_order(client, warehouse_id)?;
            TPCC::populate_order_line(client, warehouse_id, o_entry_d.clone())?;
        }
        Ok(start.elapsed().as_micros())
    }

    fn get_default_max_id(&self, client: &mut Client) -> Result<u32, postgres::Error> {
        let row_max_w_id = client.query(r"SELECT MAX(w_id) AS max_w_id FROM warehouse", &[])?;
        let max_w_id :i32 = row_max_w_id[0].get("max_w_id");

        Ok(max_w_id as u32)
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

/// Synchronous implementation for high-performance TPC-C execution
impl ReadWriteSync for TPCC {
    fn supports_sync(&self) -> bool {
        false  // Temporarily disabled - postgres crate type serialization issues
    }

    fn prepare_statements(&self, _client: &mut Client) -> Result<PreparedStatements, postgres::Error> {
        // TPC-C uses simple protocol for fixed queries
        Ok(PreparedStatements::new())
    }

    fn execute_rw_transaction_sync(
        &self,
        client: &mut Client,
        _prepared: &PreparedStatements,
        transaction: &BenchmarkTransaction,
    ) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let warehouse_id: i32 = rand::thread_rng()
            .gen_range(self.min_id..=self.max_id) as i32;

        match transaction.id {
            1 => Self::delivery_sync(client, warehouse_id),
            2 => Self::new_order_sync(client, warehouse_id, self.min_id, self.max_id),
            3 => Self::payment_sync(client, warehouse_id, self.min_id, self.max_id),
            4 => Self::order_status_sync(client, warehouse_id),
            5 => Self::stock_level_sync(client, warehouse_id),
            _ => Err("Unknown transaction".into()),
        }
    }
}

impl TPCC {
    /// Synchronous Delivery transaction
    fn delivery_sync(client: &mut Client, warehouse_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let carrier_id: i32 = rand::thread_rng().gen_range(1..=10);
        let start = Instant::now();

        let mut tx = client.transaction()?;

        for district_id in 1..=10 {
            // Get oldest new order
            let rows = tx.query(
                "SELECT no_o_id FROM new_order WHERE no_w_id = $1 AND no_d_id = $2 ORDER BY no_o_id ASC LIMIT 1",
                &[&warehouse_id, &district_id]
            )?;

            if rows.is_empty() {
                continue;
            }
            let order_id: i32 = rows[0].get(0);

            // Delete from new_order
            tx.execute(
                "DELETE FROM new_order WHERE no_o_id = $1 AND no_w_id = $2 AND no_d_id = $3",
                &[&order_id, &warehouse_id, &district_id]
            )?;

            // Update orders and get customer_id
            let rows = tx.query(
                "UPDATE orders SET o_carrier_id = $1 WHERE o_id = $2 AND o_w_id = $3 AND o_d_id = $4 RETURNING o_c_id",
                &[&carrier_id, &order_id, &warehouse_id, &district_id]
            )?;
            let customer_id: i32 = rows[0].get(0);

            // Update order_line delivery date
            tx.execute(
                "UPDATE order_line SET ol_delivery_d = current_timestamp WHERE ol_o_id = $1 AND ol_w_id = $2 AND ol_d_id = $3",
                &[&order_id, &warehouse_id, &district_id]
            )?;

            // Get total amount
            let rows = tx.query(
                "SELECT SUM(ol_amount * ol_quantity) FROM order_line WHERE ol_o_id = $1 AND ol_w_id = $2 AND ol_d_id = $3",
                &[&order_id, &warehouse_id, &district_id]
            )?;
            let total_amount: f64 = rows[0].get(0);

            // Update customer - cast to NUMERIC for c_balance column
            let total_amount_str = format!("{:.2}", total_amount);
            tx.execute(
                "UPDATE customer SET c_delivery_cnt = c_delivery_cnt + 1, c_balance = c_balance + $1::NUMERIC WHERE c_id = $2 AND c_w_id = $3 AND c_d_id = $4",
                &[&total_amount_str, &customer_id, &warehouse_id, &district_id]
            )?;
        }

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous New-Order transaction
    fn new_order_sync(client: &mut Client, warehouse_id: i32, min_id: u32, max_id: u32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let district_id: i32 = rand::thread_rng().gen_range(1..=10);
        let customer_id: i32 = rand::thread_rng().gen_range(1..=3000);
        let ol_cnt: i32 = rand::thread_rng().gen_range(5..=15);
        let mut ol_all_local: i32 = 1;

        // Build order lines data
        let mut order_lines: Vec<(i32, i32, i32, i32)> = Vec::with_capacity(ol_cnt as usize);
        let mut item_ids: Vec<i32> = Vec::with_capacity(ol_cnt as usize);

        let rbk: i32 = rand::thread_rng().gen_range(1..=100);

        for ol_number in 1..=ol_cnt {
            let mut ol_supply_w_id = warehouse_id;
            let ol_quantity: i32 = rand::thread_rng().gen_range(1..=10);

            // Generate unique item id
            let mut ol_i_id: i32;
            loop {
                ol_i_id = rand::thread_rng().gen_range(1..=100_000);
                if !item_ids.contains(&ol_i_id) {
                    item_ids.push(ol_i_id);
                    break;
                }
            }

            // 1% rollback with invalid item
            if rbk == 1 && ol_number == ol_cnt {
                ol_i_id = 999_999;
            }

            // Remote warehouse 1% of the time
            if (max_id - min_id) > 0 && rand::thread_rng().gen_range(1..=100) == 1 {
                ol_all_local = 0;
                while ol_supply_w_id == warehouse_id {
                    ol_supply_w_id = rand::thread_rng().gen_range(min_id as i32..=max_id as i32);
                }
            }

            order_lines.push((ol_number, ol_supply_w_id, ol_quantity, ol_i_id));
        }

        let start = Instant::now();
        let mut tx = client.transaction()?;

        // Get warehouse tax
        tx.query("SELECT w_tax FROM warehouse WHERE w_id = $1", &[&warehouse_id])?;

        // Get next order id and district tax
        let rows = tx.query(
            "UPDATE district SET d_next_o_id = d_next_o_id + 1 WHERE d_w_id = $1 AND d_id = $2 RETURNING d_tax, d_next_o_id",
            &[&warehouse_id, &district_id]
        )?;
        let o_id: i32 = rows[0].get::<_, i32>(1) - 1;

        // Get customer info
        tx.query(
            "SELECT c_discount, c_last, c_credit FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_id = $3",
            &[&warehouse_id, &district_id, &customer_id]
        )?;

        // Insert order
        tx.execute(
            "INSERT INTO orders (o_id, o_d_id, o_w_id, o_c_id, o_entry_d, o_ol_cnt, o_all_local) VALUES ($1, $2, $3, $4, NOW(), $5, $6)",
            &[&o_id, &district_id, &warehouse_id, &customer_id, &ol_cnt, &ol_all_local]
        )?;

        // Insert new_order
        tx.execute(
            "INSERT INTO new_order (no_o_id, no_d_id, no_w_id) VALUES ($1, $2, $3)",
            &[&o_id, &district_id, &warehouse_id]
        )?;

        // Build stock query for this district
        let stock_query = format!(
            "SELECT s_quantity, s_dist_{:02}, s_data FROM stock WHERE s_i_id = $1 AND s_w_id = $2",
            district_id
        );

        for (ol_number, ol_supply_w_id, ol_quantity, ol_i_id) in order_lines {
            // Get item info
            let rows = tx.query("SELECT i_price, i_name, i_data FROM item WHERE i_id = $1", &[&ol_i_id])?;

            if rows.is_empty() {
                tx.rollback()?;
                return Err("New-order rollback: item not found".into());
            }

            let i_price: f32 = rows[0].get(0);
            let ol_amount = i_price * ol_quantity as f32;

            // Get stock info
            let rows = tx.query(&stock_query, &[&ol_i_id, &ol_supply_w_id])?;
            let mut s_quantity: i32 = rows[0].get(0);
            let s_dist: String = rows[0].get(1);

            // Update stock quantity
            if s_quantity - ol_quantity > 10 {
                s_quantity -= ol_quantity;
            } else {
                s_quantity = s_quantity - ol_quantity + 91;
            }

            let s_remote_cnt_inc: f32 = if ol_supply_w_id != warehouse_id { 1.0 } else { 0.0 };
            let ol_quantity_str = ol_quantity.to_string();

            tx.execute(
                "UPDATE stock SET s_quantity = $3, s_ytd = s_ytd + $4::NUMERIC, s_order_cnt = s_order_cnt + 1, s_remote_cnt = s_remote_cnt + $5 WHERE s_i_id = $1 AND s_w_id = $2",
                &[&ol_i_id, &ol_supply_w_id, &s_quantity, &ol_quantity_str, &s_remote_cnt_inc]
            )?;

            // Insert order_line
            tx.execute(
                "INSERT INTO order_line (ol_o_id, ol_d_id, ol_w_id, ol_number, ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_dist_info) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[&o_id, &district_id, &warehouse_id, &ol_number, &ol_i_id, &ol_supply_w_id, &ol_quantity, &ol_amount, &s_dist]
            )?;
        }

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous Payment transaction
    fn payment_sync(client: &mut Client, warehouse_id: i32, min_id: u32, max_id: u32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let x: u8 = rand::thread_rng().gen_range(1..=100);
        let y: u8 = rand::thread_rng().gen_range(1..=100);
        let district_id: i32 = rand::thread_rng().gen_range(1..=10);

        let (c_d_id, c_w_id) = if x <= 85 {
            (district_id, warehouse_id)
        } else {
            let c_d_id = rand::thread_rng().gen_range(1..=10);
            let c_w_id = if (max_id - min_id) > 0 {
                let mut w = warehouse_id;
                while w == warehouse_id {
                    w = rand::thread_rng().gen_range(min_id as i32..=max_id as i32);
                }
                w
            } else {
                warehouse_id
            };
            (c_d_id, c_w_id)
        };

        let mut c_id: i32 = rand::thread_rng().gen_range(1..=3000);
        let c_last = if y <= 60 {
            Some(TPCC::gen_last(rand::thread_rng().gen_range(1..=1000)))
        } else {
            None
        };

        let h_amount: f64 = rand::thread_rng().gen_range(1.00..=5000.00);
        let h_amount_str = format!("{:.2}", h_amount);

        let start = Instant::now();
        let mut tx = client.transaction()?;

        // Update warehouse - cast to NUMERIC for w_ytd column
        let rows = tx.query(
            "UPDATE warehouse SET w_ytd = w_ytd + $1::NUMERIC WHERE w_id = $2 RETURNING w_name",
            &[&h_amount_str, &warehouse_id]
        )?;
        let w_name: String = rows[0].get(0);

        // Update district - cast to NUMERIC for d_ytd column
        let rows = tx.query(
            "UPDATE district SET d_ytd = d_ytd + $1::NUMERIC WHERE d_w_id = $2 AND d_id = $3 RETURNING d_name",
            &[&h_amount_str, &warehouse_id, &district_id]
        )?;
        let d_name: String = rows[0].get(0);

        // Find customer by last name if needed
        if let Some(ref last) = c_last {
            let rows = tx.query(
                "SELECT c_id FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_last = $3 ORDER BY c_first ASC",
                &[&c_w_id, &c_d_id, last]
            )?;
            if rows.is_empty() {
                tx.rollback()?;
                return Err("Payment rollback: customer not found".into());
            }
            c_id = rows[rows.len() / 2].get(0);
        }

        // Get customer credit
        let rows = tx.query(
            "SELECT c_credit FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_id = $3",
            &[&c_w_id, &c_d_id, &c_id]
        )?;
        let c_credit: String = rows[0].get(0);

        // Update customer based on credit type - cast to NUMERIC for c_balance column
        if c_credit == "BC" {
            let pre_c_data = format!("{} {} {} {} {} {}", c_id, c_d_id, c_w_id, district_id, warehouse_id, h_amount);
            tx.execute(
                "UPDATE customer SET c_balance = c_balance - $1::NUMERIC, c_ytd_payment = c_ytd_payment + 1, c_data = substring($5||' '||c_data, 1, 500) WHERE c_id = $2 AND c_d_id = $3 AND c_w_id = $4",
                &[&h_amount_str, &c_id, &c_d_id, &c_w_id, &pre_c_data]
            )?;
        } else {
            tx.execute(
                "UPDATE customer SET c_balance = c_balance - $1::NUMERIC, c_ytd_payment = c_ytd_payment + 1 WHERE c_id = $2 AND c_d_id = $3 AND c_w_id = $4",
                &[&h_amount_str, &c_id, &c_d_id, &c_w_id]
            )?;
        }

        // Insert history - h_amount is REAL, so use f32
        let h_amount_f32 = h_amount as f32;
        tx.execute(
            "INSERT INTO history (h_c_id, h_c_d_id, h_c_w_id, h_d_id, h_w_id, h_date, h_amount, h_data) VALUES ($1, $2, $3, $4, $5, NOW(), $6, substring($7||'    '||$8, 1, 24))",
            &[&c_id, &c_d_id, &c_w_id, &district_id, &warehouse_id, &h_amount_f32, &w_name, &d_name]
        )?;

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous Order-Status transaction
    fn order_status_sync(client: &mut Client, warehouse_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let y: u8 = rand::thread_rng().gen_range(1..=100);
        let district_id: i32 = rand::thread_rng().gen_range(1..=10);

        let mut c_id: i32 = rand::thread_rng().gen_range(1..=3000);
        let c_last = if y <= 60 {
            Some(TPCC::gen_last(rand::thread_rng().gen_range(1..=999)))
        } else {
            None
        };

        let start = Instant::now();
        let mut tx = client.transaction()?;

        // Find customer by last name if needed
        if let Some(ref last) = c_last {
            let rows = tx.query(
                "SELECT c_id FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_last = $3 ORDER BY c_first ASC",
                &[&warehouse_id, &district_id, last]
            )?;
            if rows.is_empty() {
                tx.rollback()?;
                return Err("Order-status rollback: customer not found".into());
            }
            c_id = rows[rows.len() / 2].get(0);
        }

        // Get customer info
        tx.query(
            "SELECT c_balance, c_first, c_middle, c_last FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_id = $3",
            &[&warehouse_id, &district_id, &c_id]
        )?;

        // Get last order
        let rows = tx.query(
            "SELECT o_id FROM orders WHERE o_w_id = $1 AND o_d_id = $2 AND o_c_id = $3 ORDER BY o_entry_d DESC LIMIT 1",
            &[&warehouse_id, &district_id, &c_id]
        )?;
        let o_id: i32 = rows[0].get(0);

        // Get order lines
        tx.query(
            "SELECT ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_delivery_d FROM order_line WHERE ol_w_id = $1 AND ol_d_id = $2 AND ol_o_id = $3",
            &[&warehouse_id, &district_id, &o_id]
        )?;

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous Stock-Level transaction
    fn stock_level_sync(client: &mut Client, warehouse_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let district_id: i32 = rand::thread_rng().gen_range(1..=10);
        let threshold: i32 = rand::thread_rng().gen_range(10..=20);

        let start = Instant::now();
        let mut tx = client.transaction()?;

        // Get next order id
        let rows = tx.query(
            "SELECT d_next_o_id FROM district WHERE d_w_id = $1 AND d_id = $2",
            &[&warehouse_id, &district_id]
        )?;
        let d_next_o_id: i32 = rows[0].get(0);

        // Get distinct items from recent orders and check stock in one query
        tx.query(
            "SELECT COUNT(DISTINCT ol.ol_i_id) FROM order_line ol \
             JOIN stock s ON s.s_w_id = $1 AND s.s_i_id = ol.ol_i_id AND s.s_quantity < $3 \
             WHERE ol.ol_w_id = $1 AND ol.ol_d_id = $2 AND ol.ol_o_id < $4 AND ol.ol_o_id >= ($4 - 20)",
            &[&warehouse_id, &district_id, &threshold, &d_next_o_id]
        )?;

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }
}
