use std::error::Error;
use std::fmt;
use std::io::Write;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{NaiveDate, Duration};
use postgres::Client;
use sqlx::PgConnection;
use rand::{distributions::Alphanumeric, Rng, seq::SliceRandom};

use super::benchmark::{
    Benchmark,
    BenchmarkStmt,
    BenchmarkTransaction,
    PreparedStatements,
    ReadWrite,
    ReadWriteSync,
};

/// TPC-H Decision Support Benchmark for PostgreSQL
///
/// TPC-H is a decision support benchmark consisting of:
/// - 8 tables (REGION, NATION, PART, SUPPLIER, PARTSUPP, CUSTOMER, ORDERS, LINEITEM)
/// - 22 read-only analytical queries (Q1-Q22)
/// - 2 refresh functions (RF1: insert, RF2: delete) - optional
///
/// This implementation focuses on the 22 queries for measuring analytical query performance.
pub struct TPCH {
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
pub struct TPCHError(String);

impl fmt::Display for TPCHError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for TPCHError {}

// Data sizing constants per scale factor
const PARTS_PER_SF: u32 = 200_000;
const SUPPLIERS_PER_SF: u32 = 10_000;
const CUSTOMERS_PER_SF: u32 = 150_000;
const ORDERS_PER_SF: u32 = 1_500_000;
const PARTSUPPS_PER_PART: u32 = 4;

// Reference data
const REGIONS: [&str; 5] = ["AFRICA", "AMERICA", "ASIA", "EUROPE", "MIDDLE EAST"];
const NATIONS: [(&str, usize); 25] = [
    ("ALGERIA", 0), ("ARGENTINA", 1), ("BRAZIL", 1), ("CANADA", 1), ("EGYPT", 4),
    ("ETHIOPIA", 0), ("FRANCE", 3), ("GERMANY", 3), ("INDIA", 2), ("INDONESIA", 2),
    ("IRAN", 4), ("IRAQ", 4), ("JAPAN", 2), ("JORDAN", 4), ("KENYA", 0),
    ("MOROCCO", 0), ("MOZAMBIQUE", 0), ("PERU", 1), ("CHINA", 2), ("ROMANIA", 3),
    ("SAUDI ARABIA", 4), ("VIETNAM", 2), ("RUSSIA", 3), ("UNITED KINGDOM", 3), ("UNITED STATES", 1),
];

const SEGMENTS: [&str; 5] = ["AUTOMOBILE", "BUILDING", "FURNITURE", "HOUSEHOLD", "MACHINERY"];
const PRIORITIES: [&str; 5] = ["1-URGENT", "2-HIGH", "3-MEDIUM", "4-NOT SPECIFIED", "5-LOW"];
const SHIP_MODES: [&str; 7] = ["REG AIR", "AIR", "RAIL", "SHIP", "TRUCK", "MAIL", "FOB"];
const SHIP_INSTRUCT: [&str; 4] = ["DELIVER IN PERSON", "COLLECT COD", "NONE", "TAKE BACK RETURN"];
const CONTAINERS: [&str; 40] = [
    "SM CASE", "SM BOX", "SM PACK", "SM PKG", "SM JAR", "SM BAG", "SM CAN", "SM DRUM",
    "MED CASE", "MED BOX", "MED PACK", "MED PKG", "MED JAR", "MED BAG", "MED CAN", "MED DRUM",
    "LG CASE", "LG BOX", "LG PACK", "LG PKG", "LG JAR", "LG BAG", "LG CAN", "LG DRUM",
    "JUMBO CASE", "JUMBO BOX", "JUMBO PACK", "JUMBO PKG", "JUMBO JAR", "JUMBO BAG", "JUMBO CAN", "JUMBO DRUM",
    "WRAP CASE", "WRAP BOX", "WRAP PACK", "WRAP PKG", "WRAP JAR", "WRAP BAG", "WRAP CAN", "WRAP DRUM",
];
const TYPES_SYLLABLE1: [&str; 5] = ["STANDARD", "SMALL", "MEDIUM", "LARGE", "ECONOMY"];
const TYPES_SYLLABLE2: [&str; 5] = ["ANODIZED", "BURNISHED", "PLATED", "POLISHED", "BRUSHED"];
const TYPES_SYLLABLE3: [&str; 6] = ["TIN", "NICKEL", "BRASS", "STEEL", "COPPER", "IRON"];

impl TPCH {
    pub fn new(scalefactor: u32, min_id: u32, max_id: u32) -> TPCH {
        // For TPC-H, max_id represents the scale factor during run phase
        // Use max_id as scalefactor if scalefactor is 0 (run phase)
        let effective_sf = if scalefactor > 0 { scalefactor } else if max_id > 0 { max_id } else { 1 };
        TPCH {
            name: "TPC-H Decision Support Benchmark".to_string(),
            description: "TPC-H-like benchmark for analytical query performance.".to_string(),
            scalefactor: effective_sf,
            min_id,
            max_id,
            transactions_rw: Vec::from([
                // Group queries by approximate complexity/weight
                // Lighter queries (higher frequency)
                BenchmarkTransaction { id: 1, weight: 8, name: "Q1-Pricing-Summary".to_string(), description: "Pricing summary report".to_string() },
                BenchmarkTransaction { id: 6, weight: 8, name: "Q6-Revenue-Forecast".to_string(), description: "Forecasting revenue change".to_string() },
                BenchmarkTransaction { id: 14, weight: 6, name: "Q14-Promotion-Effect".to_string(), description: "Promotion effect".to_string() },
                BenchmarkTransaction { id: 12, weight: 6, name: "Q12-Shipping-Modes".to_string(), description: "Shipping modes and order priority".to_string() },
                // Medium queries
                BenchmarkTransaction { id: 3, weight: 5, name: "Q3-Shipping-Priority".to_string(), description: "Shipping priority".to_string() },
                BenchmarkTransaction { id: 4, weight: 5, name: "Q4-Order-Priority".to_string(), description: "Order priority checking".to_string() },
                BenchmarkTransaction { id: 5, weight: 5, name: "Q5-Local-Supplier".to_string(), description: "Local supplier volume".to_string() },
                BenchmarkTransaction { id: 10, weight: 5, name: "Q10-Returned-Items".to_string(), description: "Returned item reporting".to_string() },
                BenchmarkTransaction { id: 11, weight: 5, name: "Q11-Important-Stock".to_string(), description: "Important stock identification".to_string() },
                BenchmarkTransaction { id: 13, weight: 5, name: "Q13-Customer-Dist".to_string(), description: "Customer distribution".to_string() },
                // Heavier queries (lower frequency)
                BenchmarkTransaction { id: 2, weight: 4, name: "Q2-Min-Cost-Supplier".to_string(), description: "Minimum cost supplier".to_string() },
                BenchmarkTransaction { id: 7, weight: 4, name: "Q7-Volume-Shipping".to_string(), description: "Volume shipping".to_string() },
                BenchmarkTransaction { id: 8, weight: 4, name: "Q8-Market-Share".to_string(), description: "National market share".to_string() },
                BenchmarkTransaction { id: 9, weight: 4, name: "Q9-Product-Profit".to_string(), description: "Product type profit measure".to_string() },
                BenchmarkTransaction { id: 15, weight: 4, name: "Q15-Top-Supplier".to_string(), description: "Top supplier".to_string() },
                BenchmarkTransaction { id: 16, weight: 4, name: "Q16-Parts-Supplier".to_string(), description: "Parts/supplier relationship".to_string() },
                BenchmarkTransaction { id: 17, weight: 3, name: "Q17-Small-Quantity".to_string(), description: "Small-quantity-order revenue".to_string() },
                BenchmarkTransaction { id: 18, weight: 3, name: "Q18-Large-Volume".to_string(), description: "Large volume customer".to_string() },
                BenchmarkTransaction { id: 19, weight: 3, name: "Q19-Discounted-Rev".to_string(), description: "Discounted revenue".to_string() },
                BenchmarkTransaction { id: 20, weight: 3, name: "Q20-Potential-Promo".to_string(), description: "Potential part promotion".to_string() },
                BenchmarkTransaction { id: 21, weight: 3, name: "Q21-Waiting-Supplier".to_string(), description: "Suppliers who kept orders waiting".to_string() },
                BenchmarkTransaction { id: 22, weight: 3, name: "Q22-Global-Sales".to_string(), description: "Global sales opportunity".to_string() },
            ]),
            table_ddls: Vec::from([
                // REGION table (fixed 5 rows)
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS region CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE region (
                        r_regionkey INTEGER NOT NULL,
                        r_name CHAR(25) NOT NULL,
                        r_comment VARCHAR(152)
                    )".to_string() },
                // NATION table (fixed 25 rows)
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS nation CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE nation (
                        n_nationkey INTEGER NOT NULL,
                        n_name CHAR(25) NOT NULL,
                        n_regionkey INTEGER NOT NULL,
                        n_comment VARCHAR(152)
                    )".to_string() },
                // PART table
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS part CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE part (
                        p_partkey INTEGER NOT NULL,
                        p_name VARCHAR(55) NOT NULL,
                        p_mfgr CHAR(25) NOT NULL,
                        p_brand CHAR(10) NOT NULL,
                        p_type VARCHAR(25) NOT NULL,
                        p_size INTEGER NOT NULL,
                        p_container CHAR(10) NOT NULL,
                        p_retailprice DECIMAL(15,2) NOT NULL,
                        p_comment VARCHAR(23) NOT NULL
                    )".to_string() },
                // SUPPLIER table
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS supplier CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE supplier (
                        s_suppkey INTEGER NOT NULL,
                        s_name CHAR(25) NOT NULL,
                        s_address VARCHAR(40) NOT NULL,
                        s_nationkey INTEGER NOT NULL,
                        s_phone CHAR(15) NOT NULL,
                        s_acctbal DECIMAL(15,2) NOT NULL,
                        s_comment VARCHAR(101) NOT NULL
                    )".to_string() },
                // PARTSUPP table
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS partsupp CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE partsupp (
                        ps_partkey INTEGER NOT NULL,
                        ps_suppkey INTEGER NOT NULL,
                        ps_availqty INTEGER NOT NULL,
                        ps_supplycost DECIMAL(15,2) NOT NULL,
                        ps_comment VARCHAR(199) NOT NULL
                    )".to_string() },
                // CUSTOMER table
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS customer CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE customer (
                        c_custkey INTEGER NOT NULL,
                        c_name VARCHAR(25) NOT NULL,
                        c_address VARCHAR(40) NOT NULL,
                        c_nationkey INTEGER NOT NULL,
                        c_phone CHAR(15) NOT NULL,
                        c_acctbal DECIMAL(15,2) NOT NULL,
                        c_mktsegment CHAR(10) NOT NULL,
                        c_comment VARCHAR(117) NOT NULL
                    )".to_string() },
                // ORDERS table
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS orders CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE orders (
                        o_orderkey INTEGER NOT NULL,
                        o_custkey INTEGER NOT NULL,
                        o_orderstatus CHAR(1) NOT NULL,
                        o_totalprice DECIMAL(15,2) NOT NULL,
                        o_orderdate DATE NOT NULL,
                        o_orderpriority CHAR(15) NOT NULL,
                        o_clerk CHAR(15) NOT NULL,
                        o_shippriority INTEGER NOT NULL,
                        o_comment VARCHAR(79) NOT NULL
                    )".to_string() },
                // LINEITEM table
                BenchmarkStmt { sql: "DROP TABLE IF EXISTS lineitem CASCADE".to_string() },
                BenchmarkStmt { sql: r"
                    CREATE TABLE lineitem (
                        l_orderkey INTEGER NOT NULL,
                        l_partkey INTEGER NOT NULL,
                        l_suppkey INTEGER NOT NULL,
                        l_linenumber INTEGER NOT NULL,
                        l_quantity DECIMAL(15,2) NOT NULL,
                        l_extendedprice DECIMAL(15,2) NOT NULL,
                        l_discount DECIMAL(15,2) NOT NULL,
                        l_tax DECIMAL(15,2) NOT NULL,
                        l_returnflag CHAR(1) NOT NULL,
                        l_linestatus CHAR(1) NOT NULL,
                        l_shipdate DATE NOT NULL,
                        l_commitdate DATE NOT NULL,
                        l_receiptdate DATE NOT NULL,
                        l_shipinstruct CHAR(25) NOT NULL,
                        l_shipmode CHAR(10) NOT NULL,
                        l_comment VARCHAR(44) NOT NULL
                    )".to_string() },
            ]),
            pkey_ddls: Vec::from([
                BenchmarkStmt { sql: "ALTER TABLE region ADD PRIMARY KEY (r_regionkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE nation ADD PRIMARY KEY (n_nationkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE part ADD PRIMARY KEY (p_partkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE supplier ADD PRIMARY KEY (s_suppkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE partsupp ADD PRIMARY KEY (ps_partkey, ps_suppkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE customer ADD PRIMARY KEY (c_custkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE orders ADD PRIMARY KEY (o_orderkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE lineitem ADD PRIMARY KEY (l_orderkey, l_linenumber)".to_string() },
            ]),
            fkey_ddls: Vec::from([
                BenchmarkStmt { sql: "ALTER TABLE nation ADD CONSTRAINT fk_nation_region FOREIGN KEY (n_regionkey) REFERENCES region(r_regionkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE supplier ADD CONSTRAINT fk_supplier_nation FOREIGN KEY (s_nationkey) REFERENCES nation(n_nationkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE partsupp ADD CONSTRAINT fk_partsupp_part FOREIGN KEY (ps_partkey) REFERENCES part(p_partkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE partsupp ADD CONSTRAINT fk_partsupp_supplier FOREIGN KEY (ps_suppkey) REFERENCES supplier(s_suppkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE customer ADD CONSTRAINT fk_customer_nation FOREIGN KEY (c_nationkey) REFERENCES nation(n_nationkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE orders ADD CONSTRAINT fk_orders_customer FOREIGN KEY (o_custkey) REFERENCES customer(c_custkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE lineitem ADD CONSTRAINT fk_lineitem_orders FOREIGN KEY (l_orderkey) REFERENCES orders(o_orderkey)".to_string() },
                BenchmarkStmt { sql: "ALTER TABLE lineitem ADD CONSTRAINT fk_lineitem_partsupp FOREIGN KEY (l_partkey, l_suppkey) REFERENCES partsupp(ps_partkey, ps_suppkey)".to_string() },
            ]),
            index_ddls: Vec::from([
                // Key indexes for TPC-H query performance
                BenchmarkStmt { sql: "CREATE INDEX idx_lineitem_shipdate ON lineitem(l_shipdate)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_lineitem_orderkey ON lineitem(l_orderkey)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_lineitem_partkey ON lineitem(l_partkey)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_lineitem_suppkey ON lineitem(l_suppkey)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_lineitem_partsupp ON lineitem(l_partkey, l_suppkey)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_orders_custkey ON orders(o_custkey)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_orders_orderdate ON orders(o_orderdate)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_partsupp_suppkey ON partsupp(ps_suppkey)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_customer_nationkey ON customer(c_nationkey)".to_string() },
                BenchmarkStmt { sql: "CREATE INDEX idx_supplier_nationkey ON supplier(s_nationkey)".to_string() },
            ]),
            vacuum_stmts: Vec::from([
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE region".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE nation".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE part".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE supplier".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE partsupp".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE customer".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE orders".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE lineitem".to_string() },
            ]),
        }
    }

    // Helper function to generate random alphanumeric string
    fn random_string(min_len: usize, max_len: usize) -> String {
        let len = if min_len < max_len {
            rand::thread_rng().gen_range(min_len..=max_len)
        } else {
            max_len
        };
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(len)
            .map(char::from)
            .collect()
    }

    // Generate phone number in format NN-NNN-NNN-NNNN
    fn gen_phone(nation_key: i32) -> String {
        let country_code = 10 + nation_key;
        format!("{}-{:03}-{:03}-{:04}",
            country_code,
            rand::thread_rng().gen_range(100..999),
            rand::thread_rng().gen_range(100..999),
            rand::thread_rng().gen_range(1000..9999))
    }

    // Generate part name from word list
    fn gen_part_name() -> String {
        let words = ["almond", "antique", "aquamarine", "azure", "beige", "bisque", "black", "blanched",
            "blue", "blush", "brown", "burlywood", "burnished", "chartreuse", "chiffon", "chocolate",
            "coral", "cornflower", "cornsilk", "cream", "cyan", "dark", "deep", "dim", "dodger",
            "drab", "firebrick", "floral", "forest", "frosted", "gainsboro", "ghost", "goldenrod",
            "green", "grey", "honeydew", "hot", "indian", "ivory", "khaki", "lace", "lavender",
            "lawn", "lemon", "light", "lime", "linen", "magenta", "maroon", "medium", "metallic",
            "midnight", "mint", "misty", "moccasin", "navajo", "navy", "olive", "orange", "orchid",
            "pale", "papaya", "peach", "peru", "pink", "plum", "powder", "puff", "purple", "red",
            "rose", "rosy", "royal", "saddle", "salmon", "sandy", "seashell", "sienna", "sky",
            "slate", "smoke", "snow", "spring", "steel", "tan", "thistle", "tomato", "turquoise",
            "violet", "wheat", "white", "yellow"];

        let mut rng = rand::thread_rng();
        let selected: Vec<&str> = words.choose_multiple(&mut rng, 5).cloned().collect();
        selected.join(" ")
    }

    // Generate part type
    fn gen_type() -> String {
        let mut rng = rand::thread_rng();
        format!("{} {} {}",
            TYPES_SYLLABLE1.choose(&mut rng).unwrap(),
            TYPES_SYLLABLE2.choose(&mut rng).unwrap(),
            TYPES_SYLLABLE3.choose(&mut rng).unwrap())
    }

    // Generate random date within TPC-H date range (1992-01-01 to 1998-12-31)
    fn gen_date(start_year: i32, end_year: i32) -> NaiveDate {
        let start = NaiveDate::from_ymd_opt(start_year, 1, 1).unwrap();
        let end = NaiveDate::from_ymd_opt(end_year, 12, 31).unwrap();
        let days = (end - start).num_days();
        start + Duration::days(rand::thread_rng().gen_range(0..=days))
    }

    // Populate region table (5 fixed rows)
    pub fn populate_region(client: &mut Client) -> Result<(), String> {
        let mut writer = match client.copy_in("COPY region FROM stdin") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        for (i, name) in REGIONS.iter().enumerate() {
            let comment = Self::random_string(31, 115);
            let line = format!("{}\t{}\t{}\n", i, name, comment);
            if let Err(e) = writer.write_all(line.as_bytes()) {
                return Err(e.to_string());
            }
        }

        writer.finish().map_err(|e| e.to_string())?;
        Ok(())
    }

    // Populate nation table (25 fixed rows)
    pub fn populate_nation(client: &mut Client) -> Result<(), String> {
        let mut writer = match client.copy_in("COPY nation FROM stdin") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        for (i, (name, region_key)) in NATIONS.iter().enumerate() {
            let comment = Self::random_string(31, 114);
            let line = format!("{}\t{}\t{}\t{}\n", i, name, region_key, comment);
            if let Err(e) = writer.write_all(line.as_bytes()) {
                return Err(e.to_string());
            }
        }

        writer.finish().map_err(|e| e.to_string())?;
        Ok(())
    }

    // Populate part table
    pub fn populate_part(client: &mut Client, scalefactor: u32) -> Result<(), String> {
        let n_parts = PARTS_PER_SF * scalefactor;
        let batch_size = 10000u32;
        let n_batches = (n_parts + batch_size - 1) / batch_size;

        for batch in 0..n_batches {
            let mut writer = match client.copy_in("COPY part FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let start = batch * batch_size + 1;
            let end = std::cmp::min((batch + 1) * batch_size, n_parts);

            for p_partkey in start..=end {
                let p_name = Self::gen_part_name();
                let p_mfgr = format!("Manufacturer#{}", rand::thread_rng().gen_range(1..=5));
                let p_brand = format!("Brand#{}{}",
                    rand::thread_rng().gen_range(1..=5),
                    rand::thread_rng().gen_range(1..=5));
                let p_type = Self::gen_type();
                let p_size: i32 = rand::thread_rng().gen_range(1..=50);
                let p_container = CONTAINERS[rand::thread_rng().gen_range(0..CONTAINERS.len())];
                let p_retailprice = (90000 + (p_partkey % 20001) + 100 * (p_partkey % 1000)) as f64 / 100.0;
                let p_comment = Self::random_string(5, 22);

                let line = format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.2}\t{}\n",
                    p_partkey, p_name, p_mfgr, p_brand, p_type, p_size, p_container, p_retailprice, p_comment);
                if let Err(e) = writer.write_all(line.as_bytes()) {
                    return Err(e.to_string());
                }
            }

            writer.finish().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    // Populate supplier table
    pub fn populate_supplier(client: &mut Client, scalefactor: u32) -> Result<(), String> {
        let n_suppliers = SUPPLIERS_PER_SF * scalefactor;
        let batch_size = 5000u32;
        let n_batches = (n_suppliers + batch_size - 1) / batch_size;

        for batch in 0..n_batches {
            let mut writer = match client.copy_in("COPY supplier FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let start = batch * batch_size + 1;
            let end = std::cmp::min((batch + 1) * batch_size, n_suppliers);

            for s_suppkey in start..=end {
                let s_name = format!("Supplier#{:09}", s_suppkey);
                let s_address = Self::random_string(10, 40);
                let s_nationkey: i32 = rand::thread_rng().gen_range(0..25);
                let s_phone = Self::gen_phone(s_nationkey);
                let s_acctbal: f64 = rand::thread_rng().gen_range(-999.99..9999.99);
                let s_comment = Self::random_string(25, 100);

                let line = format!("{}\t{}\t{}\t{}\t{}\t{:.2}\t{}\n",
                    s_suppkey, s_name, s_address, s_nationkey, s_phone, s_acctbal, s_comment);
                if let Err(e) = writer.write_all(line.as_bytes()) {
                    return Err(e.to_string());
                }
            }

            writer.finish().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    // Populate partsupp table
    pub fn populate_partsupp(client: &mut Client, scalefactor: u32) -> Result<(), String> {
        let n_parts = PARTS_PER_SF * scalefactor;
        let n_suppliers = SUPPLIERS_PER_SF * scalefactor;
        let batch_size = 10000u32;
        let n_batches = (n_parts + batch_size - 1) / batch_size;

        for batch in 0..n_batches {
            let mut writer = match client.copy_in("COPY partsupp FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let start = batch * batch_size + 1;
            let end = std::cmp::min((batch + 1) * batch_size, n_parts);

            for ps_partkey in start..=end {
                for i in 0..PARTSUPPS_PER_PART {
                    let ps_suppkey = ((ps_partkey + i * (n_suppliers / 4 + (ps_partkey - 1) / n_suppliers)) % n_suppliers) + 1;
                    let ps_availqty: i32 = rand::thread_rng().gen_range(1..=9999);
                    let ps_supplycost: f64 = rand::thread_rng().gen_range(1.00..=1000.00);
                    let ps_comment = Self::random_string(49, 198);

                    let line = format!("{}\t{}\t{}\t{:.2}\t{}\n",
                        ps_partkey, ps_suppkey, ps_availqty, ps_supplycost, ps_comment);
                    if let Err(e) = writer.write_all(line.as_bytes()) {
                        return Err(e.to_string());
                    }
                }
            }

            writer.finish().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    // Populate customer table
    pub fn populate_customer(client: &mut Client, scalefactor: u32) -> Result<(), String> {
        let n_customers = CUSTOMERS_PER_SF * scalefactor;
        let batch_size = 10000u32;
        let n_batches = (n_customers + batch_size - 1) / batch_size;

        for batch in 0..n_batches {
            let mut writer = match client.copy_in("COPY customer FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let start = batch * batch_size + 1;
            let end = std::cmp::min((batch + 1) * batch_size, n_customers);

            for c_custkey in start..=end {
                let c_name = format!("Customer#{:09}", c_custkey);
                let c_address = Self::random_string(10, 40);
                let c_nationkey: i32 = rand::thread_rng().gen_range(0..25);
                let c_phone = Self::gen_phone(c_nationkey);
                let c_acctbal: f64 = rand::thread_rng().gen_range(-999.99..9999.99);
                let c_mktsegment = SEGMENTS[rand::thread_rng().gen_range(0..SEGMENTS.len())];
                let c_comment = Self::random_string(29, 116);

                let line = format!("{}\t{}\t{}\t{}\t{}\t{:.2}\t{}\t{}\n",
                    c_custkey, c_name, c_address, c_nationkey, c_phone, c_acctbal, c_mktsegment, c_comment);
                if let Err(e) = writer.write_all(line.as_bytes()) {
                    return Err(e.to_string());
                }
            }

            writer.finish().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    // Populate orders and lineitem tables together
    pub fn populate_orders_lineitem(client: &mut Client, scalefactor: u32) -> Result<(), String> {
        let n_orders = ORDERS_PER_SF * scalefactor;
        let n_customers = CUSTOMERS_PER_SF * scalefactor;
        let n_parts = PARTS_PER_SF * scalefactor;
        let n_suppliers = SUPPLIERS_PER_SF * scalefactor;
        let batch_size = 10000u32;
        let n_batches = (n_orders + batch_size - 1) / batch_size;

        for batch in 0..n_batches {
            let mut orders_writer = match client.copy_in("COPY orders FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let start = batch * batch_size + 1;
            let end = std::cmp::min((batch + 1) * batch_size, n_orders);

            // Collect lineitem data while writing orders
            let mut lineitem_data: Vec<String> = Vec::new();

            for o_orderkey in start..=end {
                // Skip orders divisible by 3 - no customer with that key
                let o_custkey = if o_orderkey % 3 == 0 {
                    ((o_orderkey % n_customers) + 1) as i32
                } else {
                    (o_orderkey % n_customers + 1) as i32
                };

                let o_orderdate = Self::gen_date(1992, 1998);
                let o_orderpriority = PRIORITIES[rand::thread_rng().gen_range(0..PRIORITIES.len())];
                let o_clerk = format!("Clerk#{:09}", rand::thread_rng().gen_range(1..=1000));
                let o_shippriority = 0;
                let o_comment = Self::random_string(19, 78);

                // Generate lineitems for this order (1-7 items)
                let n_lineitems: i32 = rand::thread_rng().gen_range(1..=7);
                let mut o_totalprice: f64 = 0.0;
                let mut has_returned = false;

                for l_linenumber in 1..=n_lineitems {
                    let l_partkey: u32 = rand::thread_rng().gen_range(1..=n_parts);
                    // Per TPC-H spec: select one of the 4 suppliers for this part (random 0-3)
                    // Formula must match partsupp exactly
                    let supp_idx: u32 = rand::thread_rng().gen_range(0..4);
                    let l_suppkey = ((l_partkey + supp_idx * (n_suppliers / 4 + (l_partkey - 1) / n_suppliers)) % n_suppliers) + 1;
                    let l_quantity: i32 = rand::thread_rng().gen_range(1..=50);
                    let l_discount: f64 = rand::thread_rng().gen_range(0..=10) as f64 / 100.0;
                    let l_tax: f64 = rand::thread_rng().gen_range(0..=8) as f64 / 100.0;

                    // Calculate extended price
                    let base_price = (90000 + (l_partkey % 20001) + 100 * (l_partkey % 1000)) as f64 / 100.0;
                    let l_extendedprice = base_price * l_quantity as f64;
                    o_totalprice += l_extendedprice * (1.0 - l_discount) * (1.0 + l_tax);

                    // Generate dates relative to order date
                    let l_shipdate = o_orderdate + Duration::days(rand::thread_rng().gen_range(1..=121));
                    let l_commitdate = o_orderdate + Duration::days(rand::thread_rng().gen_range(30..=90));
                    let l_receiptdate = l_shipdate + Duration::days(rand::thread_rng().gen_range(1..=30));

                    // Return flag depends on receipt date
                    let l_returnflag = if l_receiptdate <= NaiveDate::from_ymd_opt(1995, 6, 17).unwrap() {
                        if rand::thread_rng().gen_bool(0.5) { 'R' } else { 'A' }
                    } else {
                        'N'
                    };
                    if l_returnflag == 'R' { has_returned = true; }

                    let l_linestatus = if l_shipdate > NaiveDate::from_ymd_opt(1995, 6, 17).unwrap() { 'O' } else { 'F' };
                    let l_shipinstruct = SHIP_INSTRUCT[rand::thread_rng().gen_range(0..SHIP_INSTRUCT.len())];
                    let l_shipmode = SHIP_MODES[rand::thread_rng().gen_range(0..SHIP_MODES.len())];
                    let l_comment = Self::random_string(10, 43);

                    let lineitem_line = format!("{}\t{}\t{}\t{}\t{}\t{:.2}\t{:.2}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                        o_orderkey, l_partkey, l_suppkey, l_linenumber, l_quantity,
                        l_extendedprice, l_discount, l_tax, l_returnflag, l_linestatus,
                        l_shipdate, l_commitdate, l_receiptdate, l_shipinstruct, l_shipmode, l_comment);
                    lineitem_data.push(lineitem_line);
                }

                // Order status based on lineitems
                let o_orderstatus = if has_returned { 'P' } else if o_orderdate < NaiveDate::from_ymd_opt(1995, 6, 17).unwrap() { 'F' } else { 'O' };

                let order_line = format!("{}\t{}\t{}\t{:.2}\t{}\t{}\t{}\t{}\t{}\n",
                    o_orderkey, o_custkey, o_orderstatus, o_totalprice, o_orderdate,
                    o_orderpriority, o_clerk, o_shippriority, o_comment);
                if let Err(e) = orders_writer.write_all(order_line.as_bytes()) {
                    return Err(e.to_string());
                }
            }

            orders_writer.finish().map_err(|e| e.to_string())?;

            // Now write lineitem data
            let mut lineitem_writer = match client.copy_in("COPY lineitem FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            for line in lineitem_data {
                if let Err(e) = lineitem_writer.write_all(line.as_bytes()) {
                    return Err(e.to_string());
                }
            }

            lineitem_writer.finish().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

// ============================================================================
// TPC-H Query Implementations (Sync with Simple Protocol)
// ============================================================================

impl TPCH {
    /// Q1: Pricing Summary Report - Scans lineitem with date filter
    fn q1_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let delta: i32 = rand::thread_rng().gen_range(60..=120);
        let start = Instant::now();

        let sql = format!(r"
            SELECT l_returnflag, l_linestatus,
                SUM(l_quantity) AS sum_qty,
                SUM(l_extendedprice) AS sum_base_price,
                SUM(l_extendedprice * (1 - l_discount)) AS sum_disc_price,
                SUM(l_extendedprice * (1 - l_discount) * (1 + l_tax)) AS sum_charge,
                AVG(l_quantity) AS avg_qty,
                AVG(l_extendedprice) AS avg_price,
                AVG(l_discount) AS avg_disc,
                COUNT(*) AS count_order
            FROM lineitem
            WHERE l_shipdate <= DATE '1998-12-01' - INTERVAL '{} day'
            GROUP BY l_returnflag, l_linestatus
            ORDER BY l_returnflag, l_linestatus", delta);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q2: Minimum Cost Supplier
    fn q2_sync(client: &mut Client, _scalefactor: u32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let size: i32 = rand::thread_rng().gen_range(1..=50);
        let types = ["TIN", "NICKEL", "BRASS", "STEEL", "COPPER"];
        let type_suffix = types[rand::thread_rng().gen_range(0..types.len())];
        let region = REGIONS[rand::thread_rng().gen_range(0..REGIONS.len())];

        let start = Instant::now();
        let sql = format!(r"
            SELECT s_acctbal, s_name, n_name, p_partkey, p_mfgr, s_address, s_phone, s_comment
            FROM part, supplier, partsupp, nation, region
            WHERE p_partkey = ps_partkey AND s_suppkey = ps_suppkey
                AND p_size = {} AND p_type LIKE '%{}'
                AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey
                AND r_name = '{}'
                AND ps_supplycost = (
                    SELECT MIN(ps_supplycost) FROM partsupp, supplier, nation, region
                    WHERE p_partkey = ps_partkey AND s_suppkey = ps_suppkey
                        AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey
                        AND r_name = '{}'
                )
            ORDER BY s_acctbal DESC, n_name, s_name, p_partkey
            LIMIT 100", size, type_suffix, region, region);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q3: Shipping Priority
    fn q3_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let segment = SEGMENTS[rand::thread_rng().gen_range(0..SEGMENTS.len())];
        let date = Self::gen_date(1995, 1995);

        let start = Instant::now();
        let sql = format!(r"
            SELECT l_orderkey, SUM(l_extendedprice * (1 - l_discount)) AS revenue, o_orderdate, o_shippriority
            FROM customer, orders, lineitem
            WHERE c_mktsegment = '{}' AND c_custkey = o_custkey
                AND l_orderkey = o_orderkey AND o_orderdate < DATE '{}'
                AND l_shipdate > DATE '{}'
            GROUP BY l_orderkey, o_orderdate, o_shippriority
            ORDER BY revenue DESC, o_orderdate
            LIMIT 10", segment, date, date);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q4: Order Priority Checking
    fn q4_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let year: i32 = rand::thread_rng().gen_range(1993..=1997);
        let month: u32 = rand::thread_rng().gen_range(1..=12);
        let date = NaiveDate::from_ymd_opt(year, month, 1).unwrap();

        let start = Instant::now();
        let sql = format!(r"
            SELECT o_orderpriority, COUNT(*) AS order_count
            FROM orders
            WHERE o_orderdate >= DATE '{}' AND o_orderdate < DATE '{}' + INTERVAL '3 month'
                AND EXISTS (
                    SELECT * FROM lineitem WHERE l_orderkey = o_orderkey AND l_commitdate < l_receiptdate
                )
            GROUP BY o_orderpriority
            ORDER BY o_orderpriority", date, date);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q5: Local Supplier Volume
    fn q5_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let region = REGIONS[rand::thread_rng().gen_range(0..REGIONS.len())];
        let year: i32 = rand::thread_rng().gen_range(1993..=1997);
        let date = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();

        let start = Instant::now();
        let sql = format!(r"
            SELECT n_name, SUM(l_extendedprice * (1 - l_discount)) AS revenue
            FROM customer, orders, lineitem, supplier, nation, region
            WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey
                AND l_suppkey = s_suppkey AND c_nationkey = s_nationkey
                AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey
                AND r_name = '{}' AND o_orderdate >= DATE '{}'
                AND o_orderdate < DATE '{}' + INTERVAL '1 year'
            GROUP BY n_name
            ORDER BY revenue DESC", region, date, date);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q6: Forecasting Revenue Change
    fn q6_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let year: i32 = rand::thread_rng().gen_range(1993..=1997);
        let date = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
        let discount: f64 = rand::thread_rng().gen_range(2..=9) as f64 / 100.0;
        let quantity: i32 = rand::thread_rng().gen_range(24..=25);

        let start = Instant::now();
        let sql = format!(r"
            SELECT SUM(l_extendedprice * l_discount) AS revenue
            FROM lineitem
            WHERE l_shipdate >= DATE '{}' AND l_shipdate < DATE '{}' + INTERVAL '1 year'
                AND l_discount BETWEEN {:.2} - 0.01 AND {:.2} + 0.01
                AND l_quantity < {}", date, date, discount, discount, quantity);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q7: Volume Shipping
    fn q7_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let nations: Vec<&str> = NATIONS.iter().map(|(n, _)| *n).collect();
        let nation1 = nations[rand::thread_rng().gen_range(0..nations.len())];
        let mut nation2 = nation1;
        while nation2 == nation1 {
            nation2 = nations[rand::thread_rng().gen_range(0..nations.len())];
        }

        let start = Instant::now();
        let sql = format!(r"
            SELECT supp_nation, cust_nation, l_year, SUM(volume) AS revenue
            FROM (
                SELECT n1.n_name AS supp_nation, n2.n_name AS cust_nation,
                    EXTRACT(YEAR FROM l_shipdate) AS l_year,
                    l_extendedprice * (1 - l_discount) AS volume
                FROM supplier, lineitem, orders, customer, nation n1, nation n2
                WHERE s_suppkey = l_suppkey AND o_orderkey = l_orderkey
                    AND c_custkey = o_custkey AND s_nationkey = n1.n_nationkey
                    AND c_nationkey = n2.n_nationkey
                    AND ((n1.n_name = '{}' AND n2.n_name = '{}')
                        OR (n1.n_name = '{}' AND n2.n_name = '{}'))
                    AND l_shipdate BETWEEN DATE '1995-01-01' AND DATE '1996-12-31'
            ) AS shipping
            GROUP BY supp_nation, cust_nation, l_year
            ORDER BY supp_nation, cust_nation, l_year", nation1, nation2, nation2, nation1);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q8: National Market Share
    fn q8_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let nations: Vec<&str> = NATIONS.iter().map(|(n, _)| *n).collect();
        let nation = nations[rand::thread_rng().gen_range(0..nations.len())];
        let region = REGIONS[rand::thread_rng().gen_range(0..REGIONS.len())];
        let ptype = Self::gen_type();

        let start = Instant::now();
        let sql = format!(r"
            SELECT o_year, SUM(CASE WHEN nation = '{}' THEN volume ELSE 0 END) / SUM(volume) AS mkt_share
            FROM (
                SELECT EXTRACT(YEAR FROM o_orderdate) AS o_year,
                    l_extendedprice * (1 - l_discount) AS volume, n2.n_name AS nation
                FROM part, supplier, lineitem, orders, customer, nation n1, nation n2, region
                WHERE p_partkey = l_partkey AND s_suppkey = l_suppkey
                    AND l_orderkey = o_orderkey AND o_custkey = c_custkey
                    AND c_nationkey = n1.n_nationkey AND n1.n_regionkey = r_regionkey
                    AND r_name = '{}' AND s_nationkey = n2.n_nationkey
                    AND o_orderdate BETWEEN DATE '1995-01-01' AND DATE '1996-12-31'
                    AND p_type = '{}'
            ) AS all_nations
            GROUP BY o_year
            ORDER BY o_year", nation, region, ptype);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q9: Product Type Profit Measure
    fn q9_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let colors = ["almond", "antique", "aquamarine", "azure", "beige", "bisque", "black", "blanched", "blue"];
        let color = colors[rand::thread_rng().gen_range(0..colors.len())];

        let start = Instant::now();
        let sql = format!(r"
            SELECT nation, o_year, SUM(amount) AS sum_profit
            FROM (
                SELECT n_name AS nation, EXTRACT(YEAR FROM o_orderdate) AS o_year,
                    l_extendedprice * (1 - l_discount) - ps_supplycost * l_quantity AS amount
                FROM part, supplier, lineitem, partsupp, orders, nation
                WHERE s_suppkey = l_suppkey AND ps_suppkey = l_suppkey
                    AND ps_partkey = l_partkey AND p_partkey = l_partkey
                    AND o_orderkey = l_orderkey AND s_nationkey = n_nationkey
                    AND p_name LIKE '%{}%'
            ) AS profit
            GROUP BY nation, o_year
            ORDER BY nation, o_year DESC", color);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q10: Returned Item Reporting
    fn q10_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let year: i32 = rand::thread_rng().gen_range(1993..=1995);
        let month: u32 = rand::thread_rng().gen_range(1..=12);
        let date = NaiveDate::from_ymd_opt(year, month, 1).unwrap();

        let start = Instant::now();
        let sql = format!(r"
            SELECT c_custkey, c_name, SUM(l_extendedprice * (1 - l_discount)) AS revenue,
                c_acctbal, n_name, c_address, c_phone, c_comment
            FROM customer, orders, lineitem, nation
            WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey
                AND o_orderdate >= DATE '{}' AND o_orderdate < DATE '{}' + INTERVAL '3 month'
                AND l_returnflag = 'R' AND c_nationkey = n_nationkey
            GROUP BY c_custkey, c_name, c_acctbal, c_phone, n_name, c_address, c_comment
            ORDER BY revenue DESC
            LIMIT 20", date, date);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q11: Important Stock Identification
    fn q11_sync(client: &mut Client, scalefactor: u32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let nations: Vec<&str> = NATIONS.iter().map(|(n, _)| *n).collect();
        let nation = nations[rand::thread_rng().gen_range(0..nations.len())];
        let fraction = 0.0001 / scalefactor as f64;

        let start = Instant::now();
        let sql = format!(r"
            SELECT ps_partkey, SUM(ps_supplycost * ps_availqty) AS value
            FROM partsupp, supplier, nation
            WHERE ps_suppkey = s_suppkey AND s_nationkey = n_nationkey AND n_name = '{}'
            GROUP BY ps_partkey
            HAVING SUM(ps_supplycost * ps_availqty) > (
                SELECT SUM(ps_supplycost * ps_availqty) * {}
                FROM partsupp, supplier, nation
                WHERE ps_suppkey = s_suppkey AND s_nationkey = n_nationkey AND n_name = '{}'
            )
            ORDER BY value DESC", nation, fraction, nation);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q12: Shipping Modes and Order Priority
    fn q12_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let mode1 = SHIP_MODES[rand::thread_rng().gen_range(0..SHIP_MODES.len())];
        let mut mode2 = mode1;
        while mode2 == mode1 {
            mode2 = SHIP_MODES[rand::thread_rng().gen_range(0..SHIP_MODES.len())];
        }
        let year: i32 = rand::thread_rng().gen_range(1993..=1997);
        let date = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();

        let start = Instant::now();
        let sql = format!(r"
            SELECT l_shipmode,
                SUM(CASE WHEN o_orderpriority = '1-URGENT' OR o_orderpriority = '2-HIGH' THEN 1 ELSE 0 END) AS high_line_count,
                SUM(CASE WHEN o_orderpriority <> '1-URGENT' AND o_orderpriority <> '2-HIGH' THEN 1 ELSE 0 END) AS low_line_count
            FROM orders, lineitem
            WHERE o_orderkey = l_orderkey AND l_shipmode IN ('{}', '{}')
                AND l_commitdate < l_receiptdate AND l_shipdate < l_commitdate
                AND l_receiptdate >= DATE '{}' AND l_receiptdate < DATE '{}' + INTERVAL '1 year'
            GROUP BY l_shipmode
            ORDER BY l_shipmode", mode1, mode2, date, date);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q13: Customer Distribution
    fn q13_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let words1 = ["special", "pending", "unusual", "express"];
        let words2 = ["packages", "requests", "accounts", "deposits"];
        let word1 = words1[rand::thread_rng().gen_range(0..words1.len())];
        let word2 = words2[rand::thread_rng().gen_range(0..words2.len())];

        let start = Instant::now();
        let sql = format!(r"
            SELECT c_count, COUNT(*) AS custdist
            FROM (
                SELECT c_custkey, COUNT(o_orderkey) AS c_count
                FROM customer LEFT OUTER JOIN orders ON c_custkey = o_custkey
                    AND o_comment NOT LIKE '%{}%{}%'
                GROUP BY c_custkey
            ) AS c_orders
            GROUP BY c_count
            ORDER BY custdist DESC, c_count DESC", word1, word2);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q14: Promotion Effect
    fn q14_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let year: i32 = rand::thread_rng().gen_range(1993..=1997);
        let month: u32 = rand::thread_rng().gen_range(1..=12);
        let date = NaiveDate::from_ymd_opt(year, month, 1).unwrap();

        let start = Instant::now();
        let sql = format!(r"
            SELECT 100.00 * SUM(CASE WHEN p_type LIKE 'PROMO%' THEN l_extendedprice * (1 - l_discount) ELSE 0 END)
                / SUM(l_extendedprice * (1 - l_discount)) AS promo_revenue
            FROM lineitem, part
            WHERE l_partkey = p_partkey AND l_shipdate >= DATE '{}'
                AND l_shipdate < DATE '{}' + INTERVAL '1 month'", date, date);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q15: Top Supplier (uses CTE instead of view)
    fn q15_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let year: i32 = rand::thread_rng().gen_range(1993..=1997);
        let month: u32 = rand::thread_rng().gen_range(1..=10);
        let date = NaiveDate::from_ymd_opt(year, month, 1).unwrap();

        let start = Instant::now();
        let sql = format!(r"
            WITH revenue AS (
                SELECT l_suppkey AS supplier_no, SUM(l_extendedprice * (1 - l_discount)) AS total_revenue
                FROM lineitem
                WHERE l_shipdate >= DATE '{}' AND l_shipdate < DATE '{}' + INTERVAL '3 month'
                GROUP BY l_suppkey
            )
            SELECT s_suppkey, s_name, s_address, s_phone, total_revenue
            FROM supplier, revenue
            WHERE s_suppkey = supplier_no
                AND total_revenue = (SELECT MAX(total_revenue) FROM revenue)
            ORDER BY s_suppkey", date, date);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q16: Parts/Supplier Relationship
    fn q16_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let brand = format!("Brand#{}{}", rand::thread_rng().gen_range(1..=5), rand::thread_rng().gen_range(1..=5));
        let ptype = format!("{} {}",
            TYPES_SYLLABLE1[rand::thread_rng().gen_range(0..TYPES_SYLLABLE1.len())],
            TYPES_SYLLABLE2[rand::thread_rng().gen_range(0..TYPES_SYLLABLE2.len())]);
        let sizes: Vec<i32> = (0..8).map(|_| rand::thread_rng().gen_range(1..=50)).collect();

        let start = Instant::now();
        let sql = format!(r"
            SELECT p_brand, p_type, p_size, COUNT(DISTINCT ps_suppkey) AS supplier_cnt
            FROM partsupp, part
            WHERE p_partkey = ps_partkey AND p_brand <> '{}'
                AND p_type NOT LIKE '{}%'
                AND p_size IN ({}, {}, {}, {}, {}, {}, {}, {})
                AND ps_suppkey NOT IN (
                    SELECT s_suppkey FROM supplier WHERE s_comment LIKE '%Customer%Complaints%'
                )
            GROUP BY p_brand, p_type, p_size
            ORDER BY supplier_cnt DESC, p_brand, p_type, p_size",
            brand, ptype, sizes[0], sizes[1], sizes[2], sizes[3], sizes[4], sizes[5], sizes[6], sizes[7]);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q17: Small-Quantity-Order Revenue
    fn q17_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let brand = format!("Brand#{}{}", rand::thread_rng().gen_range(1..=5), rand::thread_rng().gen_range(1..=5));
        let container = CONTAINERS[rand::thread_rng().gen_range(0..CONTAINERS.len())];

        let start = Instant::now();
        let sql = format!(r"
            SELECT SUM(l_extendedprice) / 7.0 AS avg_yearly
            FROM lineitem, part
            WHERE p_partkey = l_partkey AND p_brand = '{}' AND p_container = '{}'
                AND l_quantity < (
                    SELECT 0.2 * AVG(l_quantity) FROM lineitem WHERE l_partkey = p_partkey
                )", brand, container);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q18: Large Volume Customer
    fn q18_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let quantity: i32 = rand::thread_rng().gen_range(312..=315);

        let start = Instant::now();
        let sql = format!(r"
            SELECT c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice, SUM(l_quantity)
            FROM customer, orders, lineitem
            WHERE o_orderkey IN (
                    SELECT l_orderkey FROM lineitem GROUP BY l_orderkey HAVING SUM(l_quantity) > {}
                )
                AND c_custkey = o_custkey AND o_orderkey = l_orderkey
            GROUP BY c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice
            ORDER BY o_totalprice DESC, o_orderdate
            LIMIT 100", quantity);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q19: Discounted Revenue
    fn q19_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let brand1 = format!("Brand#{}{}", rand::thread_rng().gen_range(1..=5), rand::thread_rng().gen_range(1..=5));
        let brand2 = format!("Brand#{}{}", rand::thread_rng().gen_range(1..=5), rand::thread_rng().gen_range(1..=5));
        let brand3 = format!("Brand#{}{}", rand::thread_rng().gen_range(1..=5), rand::thread_rng().gen_range(1..=5));
        let qty1: i32 = rand::thread_rng().gen_range(1..=10);
        let qty2: i32 = rand::thread_rng().gen_range(10..=20);
        let qty3: i32 = rand::thread_rng().gen_range(20..=30);

        let start = Instant::now();
        let sql = format!(r"
            SELECT SUM(l_extendedprice * (1 - l_discount)) AS revenue
            FROM lineitem, part
            WHERE (p_partkey = l_partkey AND p_brand = '{}'
                    AND p_container IN ('SM CASE', 'SM BOX', 'SM PACK', 'SM PKG')
                    AND l_quantity >= {} AND l_quantity <= {} + 10
                    AND p_size BETWEEN 1 AND 5
                    AND l_shipmode IN ('AIR', 'AIR REG')
                    AND l_shipinstruct = 'DELIVER IN PERSON')
                OR (p_partkey = l_partkey AND p_brand = '{}'
                    AND p_container IN ('MED BAG', 'MED BOX', 'MED PKG', 'MED PACK')
                    AND l_quantity >= {} AND l_quantity <= {} + 10
                    AND p_size BETWEEN 1 AND 10
                    AND l_shipmode IN ('AIR', 'AIR REG')
                    AND l_shipinstruct = 'DELIVER IN PERSON')
                OR (p_partkey = l_partkey AND p_brand = '{}'
                    AND p_container IN ('LG CASE', 'LG BOX', 'LG PACK', 'LG PKG')
                    AND l_quantity >= {} AND l_quantity <= {} + 10
                    AND p_size BETWEEN 1 AND 15
                    AND l_shipmode IN ('AIR', 'AIR REG')
                    AND l_shipinstruct = 'DELIVER IN PERSON')",
            brand1, qty1, qty1, brand2, qty2, qty2, brand3, qty3, qty3);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q20: Potential Part Promotion
    fn q20_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let colors = ["almond", "antique", "aquamarine", "azure", "beige", "bisque", "black", "blanched", "blue"];
        let color = colors[rand::thread_rng().gen_range(0..colors.len())];
        let year: i32 = rand::thread_rng().gen_range(1993..=1997);
        let date = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
        let nations: Vec<&str> = NATIONS.iter().map(|(n, _)| *n).collect();
        let nation = nations[rand::thread_rng().gen_range(0..nations.len())];

        let start = Instant::now();
        let sql = format!(r"
            SELECT s_name, s_address
            FROM supplier, nation
            WHERE s_suppkey IN (
                    SELECT ps_suppkey FROM partsupp
                    WHERE ps_partkey IN (SELECT p_partkey FROM part WHERE p_name LIKE '{}%')
                        AND ps_availqty > (
                            SELECT 0.5 * SUM(l_quantity) FROM lineitem
                            WHERE l_partkey = ps_partkey AND l_suppkey = ps_suppkey
                                AND l_shipdate >= DATE '{}' AND l_shipdate < DATE '{}' + INTERVAL '1 year'
                        )
                )
                AND s_nationkey = n_nationkey AND n_name = '{}'
            ORDER BY s_name", color, date, date, nation);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q21: Suppliers Who Kept Orders Waiting
    fn q21_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let nations: Vec<&str> = NATIONS.iter().map(|(n, _)| *n).collect();
        let nation = nations[rand::thread_rng().gen_range(0..nations.len())];

        let start = Instant::now();
        let sql = format!(r"
            SELECT s_name, COUNT(*) AS numwait
            FROM supplier, lineitem l1, orders, nation
            WHERE s_suppkey = l1.l_suppkey AND o_orderkey = l1.l_orderkey
                AND o_orderstatus = 'F' AND l1.l_receiptdate > l1.l_commitdate
                AND EXISTS (
                    SELECT * FROM lineitem l2
                    WHERE l2.l_orderkey = l1.l_orderkey AND l2.l_suppkey <> l1.l_suppkey
                )
                AND NOT EXISTS (
                    SELECT * FROM lineitem l3
                    WHERE l3.l_orderkey = l1.l_orderkey AND l3.l_suppkey <> l1.l_suppkey
                        AND l3.l_receiptdate > l3.l_commitdate
                )
                AND s_nationkey = n_nationkey AND n_name = '{}'
            GROUP BY s_name
            ORDER BY numwait DESC, s_name
            LIMIT 100", nation);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }

    /// Q22: Global Sales Opportunity
    fn q22_sync(client: &mut Client) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        // Generate 7 unique country codes (10-34 corresponding to nations)
        let mut codes: Vec<i32> = Vec::new();
        while codes.len() < 7 {
            let code = rand::thread_rng().gen_range(10..35);
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        let codes_str = codes.iter().map(|c| format!("'{}'", c)).collect::<Vec<_>>().join(", ");

        let start = Instant::now();
        let sql = format!(r"
            SELECT cntrycode, COUNT(*) AS numcust, SUM(c_acctbal) AS totacctbal
            FROM (
                SELECT SUBSTRING(c_phone FROM 1 FOR 2) AS cntrycode, c_acctbal
                FROM customer
                WHERE SUBSTRING(c_phone FROM 1 FOR 2) IN ({})
                    AND c_acctbal > (
                        SELECT AVG(c_acctbal) FROM customer
                        WHERE c_acctbal > 0.00 AND SUBSTRING(c_phone FROM 1 FOR 2) IN ({})
                    )
                    AND NOT EXISTS (SELECT * FROM orders WHERE o_custkey = c_custkey)
            ) AS custsale
            GROUP BY cntrycode
            ORDER BY cntrycode", codes_str, codes_str);

        client.batch_execute(&sql)?;
        Ok(start.elapsed().as_micros())
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

#[async_trait]
impl ReadWrite for TPCH {
    async fn execute_rw_transaction(&self, _conn: &mut PgConnection, _transaction: &BenchmarkTransaction) -> Result<u128, Box<dyn std::error::Error>> {
        // TPC-H queries are read-only, but we use the async path as fallback
        // For now, return an error suggesting to use sync path
        Err(Box::new(TPCHError("TPC-H should use sync path for best performance".into())))
    }
}

impl Benchmark for TPCH {
    fn initialize_schema(&self, client: &mut Client) -> Result<u128, postgres::Error> {
        let start = Instant::now();

        let mut transaction = client.transaction()?;
        for ddl in self.get_table_ddls().iter() {
            transaction.batch_execute(&ddl.sql)?;
        }
        transaction.commit()?;

        Ok(start.elapsed().as_micros())
    }

    fn pre_load_data(&self, client: &mut Client) -> Result<u128, String> {
        let start = Instant::now();

        // Load reference tables (region, nation)
        Self::populate_region(client)?;
        Self::populate_nation(client)?;

        Ok(start.elapsed().as_micros())
    }

    fn load_data(&self, client: &mut Client, ids: Vec<u32>) -> Result<u128, String> {
        let start = Instant::now();

        // TPC-H data is not partitioned like TPC-C warehouses.
        // Only the first job (containing id=1) should load the data.
        // Other jobs skip data loading to avoid duplicates.
        if ids.is_empty() || !ids.contains(&1) {
            return Ok(start.elapsed().as_micros());
        }

        // Load all tables based on scale factor
        Self::populate_part(client, self.scalefactor)?;
        Self::populate_supplier(client, self.scalefactor)?;
        Self::populate_partsupp(client, self.scalefactor)?;
        Self::populate_customer(client, self.scalefactor)?;
        Self::populate_orders_lineitem(client, self.scalefactor)?;

        Ok(start.elapsed().as_micros())
    }

    fn get_default_max_id(&self, client: &mut Client) -> Result<u32, postgres::Error> {
        // For TPC-H, compute scale factor from supplier count (SF * 10,000 suppliers)
        let row = client.query("SELECT COUNT(*) FROM supplier", &[])?;
        let count: i64 = row[0].get(0);
        if count == 0 {
            return Ok(if self.scalefactor > 0 { self.scalefactor } else { 1 });
        }
        // Scale factor = supplier_count / 10,000
        let sf = (count as u32) / SUPPLIERS_PER_SF;
        Ok(if sf > 0 { sf } else { 1 })
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

/// Synchronous implementation using simple protocol for TPC-H
impl ReadWriteSync for TPCH {
    fn supports_sync(&self) -> bool {
        true
    }

    fn prepare_statements(&self, _client: &mut Client) -> Result<PreparedStatements, postgres::Error> {
        // TPC-H uses simple protocol - no prepared statements needed
        Ok(PreparedStatements::new())
    }

    fn execute_rw_transaction_sync(
        &self,
        client: &mut Client,
        _prepared: &PreparedStatements,
        transaction: &BenchmarkTransaction,
    ) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        match transaction.id {
            1 => Self::q1_sync(client),
            2 => Self::q2_sync(client, self.scalefactor),
            3 => Self::q3_sync(client),
            4 => Self::q4_sync(client),
            5 => Self::q5_sync(client),
            6 => Self::q6_sync(client),
            7 => Self::q7_sync(client),
            8 => Self::q8_sync(client),
            9 => Self::q9_sync(client),
            10 => Self::q10_sync(client),
            11 => Self::q11_sync(client, self.scalefactor),
            12 => Self::q12_sync(client),
            13 => Self::q13_sync(client),
            14 => Self::q14_sync(client),
            15 => Self::q15_sync(client),
            16 => Self::q16_sync(client),
            17 => Self::q17_sync(client),
            18 => Self::q18_sync(client),
            19 => Self::q19_sync(client),
            20 => Self::q20_sync(client),
            21 => Self::q21_sync(client),
            22 => Self::q22_sync(client),
            _ => Err("Unknown TPC-H query".into()),
        }
    }
}
