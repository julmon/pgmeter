use std::error::Error;
use std::fmt;
use std::io::Write;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use postgres::Client;
use sqlx::PgConnection;
use sqlx::Connection;
use rand::{distributions::Alphanumeric, Rng};

use super::benchmark::{
    Benchmark,
    BenchmarkStmt,
    BenchmarkTransaction,
    PreparedStatements,
    ReadWrite,
    ReadWriteSync,
};

/// TPCx-IoT-like benchmark for PostgreSQL
///
/// This benchmark simulates an IoT gateway system based on the TPCx-IoT specification.
/// It models an electric utility operational scenario with power substations containing
/// multiple sensors that generate time-series data.
///
/// Schema:
/// - substation: Power substations (scale factor units)
/// - sensor: IoT sensors belonging to substations
/// - measurement: Time-series sensor readings (the main data table)
/// - alert: Threshold-based alerts generated from measurements
///
/// Workload:
/// - Data Ingestion (70%): Bulk insert of sensor measurements
/// - Scan Query (10%): Time-range aggregation queries
/// - Device Query (10%): Single sensor data retrieval
/// - Alert Query (5%): Alert status queries
/// - Threshold Update (5%): Sensor threshold configuration updates
#[allow(dead_code)]
pub struct TPCIoT {
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
pub struct TPCIoTError(String);

impl fmt::Display for TPCIoTError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for TPCIoTError {}

// Number of sensors per substation
const SENSORS_PER_SUBSTATION: u32 = 100;
// Number of initial measurements per sensor during data loading
const INITIAL_MEASUREMENTS_PER_SENSOR: u32 = 1000;
// Batch size for measurements insertion during workload
const MEASUREMENT_BATCH_SIZE: i32 = 100;

impl TPCIoT {
    pub fn new(scalefactor: u32, min_id: u32, max_id: u32) -> TPCIoT {
        TPCIoT {
            name: "TPCx-IoT-like benchmark".to_string(),
            description: "TPCx-IoT-like benchmark implementation for PostgreSQL.".to_string(),
            scalefactor,
            min_id,
            max_id,
            transactions_rw: Vec::from([
                BenchmarkTransaction {
                    id: 1,
                    weight: 70,
                    name: "Data-Ingestion".to_string(),
                    description: "Bulk insert of sensor measurements".to_string(),
                },
                BenchmarkTransaction {
                    id: 2,
                    weight: 10,
                    name: "Scan-Query".to_string(),
                    description: "Time-range aggregation query".to_string(),
                },
                BenchmarkTransaction {
                    id: 3,
                    weight: 10,
                    name: "Device-Query".to_string(),
                    description: "Single sensor data retrieval".to_string(),
                },
                BenchmarkTransaction {
                    id: 4,
                    weight: 5,
                    name: "Alert-Query".to_string(),
                    description: "Alert status query".to_string(),
                },
                BenchmarkTransaction {
                    id: 5,
                    weight: 5,
                    name: "Threshold-Update".to_string(),
                    description: "Sensor threshold configuration update".to_string(),
                },
            ]),
            table_ddls: Vec::from([
                // Drop sequences if they exist
                BenchmarkStmt {
                    sql: "DROP SEQUENCE IF EXISTS measurement_id_seq CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: "DROP SEQUENCE IF EXISTS alert_id_seq CASCADE".to_string(),
                },
                // Create sequences for concurrent ID generation
                BenchmarkStmt {
                    sql: "CREATE SEQUENCE measurement_id_seq".to_string(),
                },
                BenchmarkStmt {
                    sql: "CREATE SEQUENCE alert_id_seq".to_string(),
                },
                // Substation table - represents power substations
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS substation CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE substation (
                            ss_id INTEGER,
                            ss_name VARCHAR(64),
                            ss_location VARCHAR(128),
                            ss_latitude DOUBLE PRECISION,
                            ss_longitude DOUBLE PRECISION,
                            ss_capacity INTEGER,
                            ss_status VARCHAR(16),
                            ss_created_at TIMESTAMP
                        )".to_string(),
                },
                // Sensor table - IoT sensors in substations
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS sensor CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE sensor (
                            sn_id BIGINT,
                            sn_ss_id INTEGER,
                            sn_type VARCHAR(32),
                            sn_unit VARCHAR(16),
                            sn_min_threshold DOUBLE PRECISION,
                            sn_max_threshold DOUBLE PRECISION,
                            sn_status VARCHAR(16),
                            sn_last_reading DOUBLE PRECISION,
                            sn_last_reading_at TIMESTAMP,
                            sn_created_at TIMESTAMP
                        )".to_string(),
                },
                // Measurement table - time-series sensor readings
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS measurement CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE measurement (
                            m_id BIGINT,
                            m_sn_id BIGINT,
                            m_ss_id INTEGER,
                            m_timestamp TIMESTAMP,
                            m_value DOUBLE PRECISION,
                            m_quality INTEGER
                        )".to_string(),
                },
                // Alert table - threshold violations
                BenchmarkStmt {
                    sql: "DROP TABLE IF EXISTS alert CASCADE".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        CREATE TABLE alert (
                            a_id BIGINT,
                            a_sn_id BIGINT,
                            a_ss_id INTEGER,
                            a_timestamp TIMESTAMP,
                            a_type VARCHAR(16),
                            a_value DOUBLE PRECISION,
                            a_threshold DOUBLE PRECISION,
                            a_acknowledged BOOLEAN,
                            a_acknowledged_at TIMESTAMP
                        )".to_string(),
                },
            ]),
            pkey_ddls: Vec::from([
                BenchmarkStmt {
                    sql: "ALTER TABLE substation ADD PRIMARY KEY (ss_id)".to_string(),
                },
                BenchmarkStmt {
                    sql: "ALTER TABLE sensor ADD PRIMARY KEY (sn_id)".to_string(),
                },
                BenchmarkStmt {
                    sql: "ALTER TABLE measurement ADD PRIMARY KEY (m_id)".to_string(),
                },
                BenchmarkStmt {
                    sql: "ALTER TABLE alert ADD PRIMARY KEY (a_id)".to_string(),
                },
            ]),
            fkey_ddls: Vec::from([
                BenchmarkStmt {
                    sql: r"
                        ALTER TABLE sensor
                        ADD CONSTRAINT fk_sensor_substation
                        FOREIGN KEY (sn_ss_id)
                        REFERENCES substation (ss_id)
                    ".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        ALTER TABLE measurement
                        ADD CONSTRAINT fk_measurement_sensor
                        FOREIGN KEY (m_sn_id)
                        REFERENCES sensor (sn_id)
                    ".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        ALTER TABLE measurement
                        ADD CONSTRAINT fk_measurement_substation
                        FOREIGN KEY (m_ss_id)
                        REFERENCES substation (ss_id)
                    ".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        ALTER TABLE alert
                        ADD CONSTRAINT fk_alert_sensor
                        FOREIGN KEY (a_sn_id)
                        REFERENCES sensor (sn_id)
                    ".to_string(),
                },
                BenchmarkStmt {
                    sql: r"
                        ALTER TABLE alert
                        ADD CONSTRAINT fk_alert_substation
                        FOREIGN KEY (a_ss_id)
                        REFERENCES substation (ss_id)
                    ".to_string(),
                },
            ]),
            index_ddls: Vec::from([
                // Index for time-range queries on measurements
                BenchmarkStmt {
                    sql: "CREATE INDEX i_measurement_timestamp ON measurement (m_timestamp)".to_string(),
                },
                // Index for sensor-specific queries
                BenchmarkStmt {
                    sql: "CREATE INDEX i_measurement_sensor ON measurement (m_sn_id, m_timestamp)".to_string(),
                },
                // Covering index for scan query - enables index-only scan for substation aggregations
                BenchmarkStmt {
                    sql: "CREATE INDEX i_measurement_substation ON measurement (m_ss_id, m_timestamp) INCLUDE (m_sn_id, m_value)".to_string(),
                },
                // Index for unacknowledged alerts
                BenchmarkStmt {
                    sql: "CREATE INDEX i_alert_unack ON alert (a_ss_id, a_acknowledged) WHERE NOT a_acknowledged".to_string(),
                },
                // Index for sensor status
                BenchmarkStmt {
                    sql: "CREATE INDEX i_sensor_substation ON sensor (sn_ss_id)".to_string(),
                },
            ]),
            vacuum_stmts: Vec::from([
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE substation".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE sensor".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE measurement".to_string() },
                BenchmarkStmt { sql: "VACUUM FREEZE ANALYZE alert".to_string() },
                // Set sequences to continue from max IDs after data loading
                BenchmarkStmt { sql: "SELECT setval('measurement_id_seq', COALESCE((SELECT MAX(m_id) FROM measurement), 1))".to_string() },
                BenchmarkStmt { sql: "SELECT setval('alert_id_seq', COALESCE((SELECT MAX(a_id) FROM alert), 1))".to_string() },
            ]),
        }
    }

    /// Data Ingestion transaction - bulk insert of sensor measurements
    /// This is the primary workload representing IoT data stream ingestion
    ///
    /// OPTIMIZED: Uses a single INSERT with UNNEST to batch all measurements
    /// instead of 100 individual INSERTs (reduces round-trips from 100+ to 3)
    pub async fn data_ingestion(conn: &mut PgConnection, substation_id: i32, _min_id: u32, _max_id: u32) -> Result<u128, Box<dyn std::error::Error>> {
        let start = Instant::now();

        // Calculate sensor ID range for this substation
        let sensor_base = (substation_id as i64 - 1) * SENSORS_PER_SUBSTATION as i64;

        // Pre-generate ALL random values before any await points (ThreadRng is not Send)
        let sensor_offset: i64 = rand::thread_rng().gen_range(0..SENSORS_PER_SUBSTATION as i64);
        let sensor_id = sensor_base + sensor_offset + 1;

        // Pre-generate all measurement data for the batch
        let mut values: Vec<f64> = Vec::with_capacity(MEASUREMENT_BATCH_SIZE as usize);
        let mut qualities: Vec<i32> = Vec::with_capacity(MEASUREMENT_BATCH_SIZE as usize);
        let mut ms_offsets: Vec<i32> = Vec::with_capacity(MEASUREMENT_BATCH_SIZE as usize);

        {
            // Scope the RNG so it's dropped before any await
            let mut rng = rand::thread_rng();
            for i in 0..MEASUREMENT_BATCH_SIZE {
                let base_value: f64 = rng.gen_range(50.0..150.0);
                let noise: f64 = rng.gen_range(-5.0..5.0);
                values.push(base_value + noise);
                qualities.push(rng.gen_range(90..100));
                ms_offsets.push(i * 10);
            }
        }

        let last_reading = *values.last().unwrap_or(&100.0);

        // Check for threshold violations before transaction (10% chance)
        let create_alert: bool = rand::thread_rng().gen_range(1..=100) <= 10;
        let alert_value: f64 = rand::thread_rng().gen_range(150.0..200.0);

        let mut transaction = conn.begin().await?;

        // OPTIMIZED: Single bulk INSERT using UNNEST to insert all measurements at once
        sqlx::query(r"
            INSERT INTO measurement (m_id, m_sn_id, m_ss_id, m_timestamp, m_value, m_quality)
            SELECT
                nextval('measurement_id_seq'),
                $1,
                $2,
                NOW() + (ms_offset * interval '1 millisecond'),
                value,
                quality
            FROM UNNEST($3::int[], $4::float8[], $5::int[]) AS t(ms_offset, value, quality)
        ")
        .bind(sensor_id)
        .bind(substation_id)
        .bind(&ms_offsets)
        .bind(&values)
        .bind(&qualities)
        .execute(&mut transaction)
        .await?;

        // Update sensor's last reading
        sqlx::query(
            "UPDATE sensor SET sn_last_reading = $1, sn_last_reading_at = NOW() WHERE sn_id = $2"
        )
        .bind(last_reading)
        .bind(sensor_id)
        .execute(&mut transaction)
        .await?;

        // Create alert if threshold violation occurred
        if create_alert {
            let threshold: f64 = 150.0;
            sqlx::query(r"
                INSERT INTO alert (a_id, a_sn_id, a_ss_id, a_timestamp, a_type, a_value, a_threshold, a_acknowledged, a_acknowledged_at)
                VALUES (nextval('alert_id_seq'), $1, $2, NOW(), 'HIGH', $3, $4, false, NULL)
            ")
            .bind(sensor_id)
            .bind(substation_id)
            .bind(alert_value)
            .bind(threshold)
            .execute(&mut transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    /// Scan Query transaction - time-range aggregation query
    /// Simulates analytical queries over recent sensor data
    pub async fn scan_query(conn: &mut PgConnection, substation_id: i32, _min_id: u32, _max_id: u32) -> Result<u128, Box<dyn std::error::Error>> {
        let start = Instant::now();

        let mut transaction = conn.begin().await?;

        // Query aggregated measurements for the last hour for this substation
        let _rows: Vec<(i64, f64, f64, f64, i64)> = sqlx::query_as(r"
            SELECT
                m_sn_id,
                AVG(m_value) as avg_value,
                MIN(m_value) as min_value,
                MAX(m_value) as max_value,
                COUNT(*) as reading_count
            FROM measurement
            WHERE
                m_ss_id = $1
                AND m_timestamp >= NOW() - INTERVAL '1 hour'
            GROUP BY m_sn_id
            ORDER BY avg_value DESC
            LIMIT 10
        ")
        .bind(substation_id)
        .fetch_all(&mut transaction)
        .await?;

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    /// Device Query transaction - single sensor data retrieval
    /// Simulates device-specific data access pattern
    pub async fn device_query(conn: &mut PgConnection, substation_id: i32, _min_id: u32, _max_id: u32) -> Result<u128, Box<dyn std::error::Error>> {
        let start = Instant::now();

        // Calculate sensor ID range for this substation
        let sensor_base = (substation_id as i64 - 1) * SENSORS_PER_SUBSTATION as i64;
        let sensor_offset: i64 = rand::thread_rng().gen_range(0..SENSORS_PER_SUBSTATION as i64);
        let sensor_id = sensor_base + sensor_offset + 1;

        let mut transaction = conn.begin().await?;

        // Get sensor details
        sqlx::query(r"
            SELECT sn_id, sn_ss_id, sn_type, sn_unit, sn_min_threshold, sn_max_threshold, sn_status
            FROM sensor
            WHERE sn_id = $1
        ")
        .bind(sensor_id)
        .execute(&mut transaction)
        .await?;

        // Get recent measurements for this sensor
        sqlx::query(r"
            SELECT m_id, m_timestamp, m_value, m_quality
            FROM measurement
            WHERE m_sn_id = $1
            ORDER BY m_timestamp DESC
            LIMIT 100
        ")
        .bind(sensor_id)
        .execute(&mut transaction)
        .await?;

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    /// Alert Query transaction - alert status query
    /// Retrieves unacknowledged alerts for a substation
    pub async fn alert_query(conn: &mut PgConnection, substation_id: i32, _min_id: u32, _max_id: u32) -> Result<u128, Box<dyn std::error::Error>> {
        let start = Instant::now();

        let mut transaction = conn.begin().await?;

        // Get unacknowledged alerts for this substation
        sqlx::query(r"
            SELECT a_id, a_sn_id, a_timestamp, a_type, a_value, a_threshold
            FROM alert
            WHERE
                a_ss_id = $1
                AND NOT a_acknowledged
            ORDER BY a_timestamp DESC
            LIMIT 50
        ")
        .bind(substation_id)
        .execute(&mut transaction)
        .await?;

        // Acknowledge the oldest unacknowledged alert (if any)
        sqlx::query(r"
            UPDATE alert
            SET a_acknowledged = true, a_acknowledged_at = NOW()
            WHERE a_id = (
                SELECT a_id FROM alert
                WHERE a_ss_id = $1 AND NOT a_acknowledged
                ORDER BY a_timestamp ASC
                LIMIT 1
            )
        ")
        .bind(substation_id)
        .execute(&mut transaction)
        .await?;

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    /// Threshold Update transaction - sensor threshold configuration update
    /// Updates sensor threshold settings
    pub async fn threshold_update(conn: &mut PgConnection, substation_id: i32, _min_id: u32, _max_id: u32) -> Result<u128, Box<dyn std::error::Error>> {
        let start = Instant::now();

        // Calculate sensor ID range for this substation
        let sensor_base = (substation_id as i64 - 1) * SENSORS_PER_SUBSTATION as i64;
        let sensor_offset: i64 = rand::thread_rng().gen_range(0..SENSORS_PER_SUBSTATION as i64);
        let sensor_id = sensor_base + sensor_offset + 1;

        let mut transaction = conn.begin().await?;

        // Generate new threshold values
        let min_threshold: f64 = rand::thread_rng().gen_range(10.0..50.0);
        let max_threshold: f64 = rand::thread_rng().gen_range(150.0..200.0);

        sqlx::query(r"
            UPDATE sensor
            SET sn_min_threshold = $1, sn_max_threshold = $2
            WHERE sn_id = $3
        ")
        .bind(min_threshold)
        .bind(max_threshold)
        .bind(sensor_id)
        .execute(&mut transaction)
        .await?;

        transaction.commit().await?;

        Ok(start.elapsed().as_micros())
    }

    // Helper function to generate random alphanumeric string
    fn random_alpha_string(min_length: usize, max_length: usize) -> String {
        let mut string_length: usize = max_length;
        if min_length < max_length {
            string_length = rand::thread_rng().gen_range(min_length..=max_length);
        }
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(string_length)
            .map(char::from)
            .collect()
    }

    // Sensor types used in the benchmark
    fn get_sensor_types() -> Vec<(&'static str, &'static str)> {
        vec![
            ("VOLTAGE", "V"),
            ("CURRENT", "A"),
            ("POWER", "kW"),
            ("TEMPERATURE", "C"),
            ("FREQUENCY", "Hz"),
            ("PRESSURE", "PSI"),
            ("HUMIDITY", "%"),
            ("VIBRATION", "mm/s"),
        ]
    }

    /// Populate the substation table
    pub fn populate_substation(client: &mut Client, substation_id: u32) -> Result<(), String> {
        let mut writer = match client.copy_in("COPY substation FROM stdin") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        let ss_name = format!("Substation-{:05}", substation_id);
        let ss_location = format!("Location-{}", TPCIoT::random_alpha_string(8, 16));
        let ss_latitude: f64 = rand::thread_rng().gen_range(-90.0..90.0);
        let ss_longitude: f64 = rand::thread_rng().gen_range(-180.0..180.0);
        let ss_capacity: u32 = rand::thread_rng().gen_range(1000..10000);
        let ss_status = "ACTIVE";
        let ss_created_at = format!("{}", Utc::now().format("%Y-%m-%d %H:%M:%S"));

        let line = format!(
            "{}\t{}\t{}\t{:.6}\t{:.6}\t{}\t{}\t{}\n",
            substation_id, ss_name, ss_location, ss_latitude, ss_longitude, ss_capacity, ss_status, ss_created_at
        );

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

    /// Populate sensors for a substation
    pub fn populate_sensors(client: &mut Client, substation_id: u32) -> Result<(), String> {
        let batch_size = 100;
        let n_sensors = SENSORS_PER_SUBSTATION;
        let n_batch = ((n_sensors as f64 / batch_size as f64) as f64).ceil() as u32;

        let sensor_types = TPCIoT::get_sensor_types();
        let sensor_base_id = (substation_id - 1) * SENSORS_PER_SUBSTATION;
        let created_at = format!("{}", Utc::now().format("%Y-%m-%d %H:%M:%S"));

        for b in 1..=n_batch {
            let mut writer = match client.copy_in("COPY sensor FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > n_sensors {
                batch_end = n_sensors;
            }

            for sensor_offset in batch_start..=batch_end {
                let sensor_id = sensor_base_id + sensor_offset;
                let (sn_type, sn_unit) = sensor_types[sensor_offset as usize % sensor_types.len()];
                let sn_min_threshold: f64 = rand::thread_rng().gen_range(10.0..50.0);
                let sn_max_threshold: f64 = rand::thread_rng().gen_range(150.0..200.0);
                let sn_status = "ACTIVE";
                let sn_last_reading: f64 = rand::thread_rng().gen_range(50.0..150.0);

                let line = format!(
                    "{}\t{}\t{}\t{}\t{:.2}\t{:.2}\t{}\t{:.4}\t{}\t{}\n",
                    sensor_id, substation_id, sn_type, sn_unit, sn_min_threshold, sn_max_threshold,
                    sn_status, sn_last_reading, created_at, created_at
                );

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

    /// Populate initial measurements for sensors in a substation
    pub fn populate_measurements(client: &mut Client, substation_id: u32, m_id_start: u64) -> Result<u64, String> {
        let batch_size = 500u32;
        let n_sensors = SENSORS_PER_SUBSTATION;
        let measurements_per_sensor = INITIAL_MEASUREMENTS_PER_SENSOR;
        let total_measurements = n_sensors * measurements_per_sensor;
        let n_batch = ((total_measurements as f64 / batch_size as f64) as f64).ceil() as u32;

        let sensor_base_id = (substation_id - 1) * SENSORS_PER_SUBSTATION;
        let base_timestamp = Utc::now() - chrono::Duration::hours(24);

        let mut m_id = m_id_start;
        let mut sensor_offset = 1u32;
        let mut measurement_offset = 0u32;

        for b in 1..=n_batch {
            let mut writer = match client.copy_in("COPY measurement FROM stdin") {
                Ok(w) => w,
                Err(e) => return Err(e.to_string()),
            };

            let batch_start = (b * batch_size - batch_size + 1) as u32;
            let mut batch_end = (b * batch_size) as u32;
            if batch_end > total_measurements {
                batch_end = total_measurements;
            }

            for _ in batch_start..=batch_end {
                let sensor_id = sensor_base_id + sensor_offset;

                // Calculate timestamp based on measurement offset
                let ts = base_timestamp + chrono::Duration::seconds((measurement_offset * 60) as i64);

                // Generate realistic sensor value
                let value: f64 = rand::thread_rng().gen_range(50.0..150.0);
                let quality: i32 = rand::thread_rng().gen_range(90..100);

                let line = format!(
                    "{}\t{}\t{}\t{}\t{:.4}\t{}\n",
                    m_id, sensor_id, substation_id, ts.format("%Y-%m-%d %H:%M:%S"), value, quality
                );

                match writer.write_all(line.as_bytes()) {
                    Ok(_) => (),
                    Err(e) => return Err(e.to_string()),
                }

                m_id += 1;
                measurement_offset += 1;

                // Move to next sensor after all measurements for current sensor
                if measurement_offset >= measurements_per_sensor {
                    sensor_offset += 1;
                    measurement_offset = 0;
                }
            }

            match writer.finish() {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }
        }

        Ok(m_id)
    }

    /// Populate initial alerts (about 1% of measurements trigger alerts)
    pub fn populate_alerts(client: &mut Client, substation_id: u32, a_id_start: u64) -> Result<u64, String> {
        let n_sensors = SENSORS_PER_SUBSTATION;
        let alerts_per_substation = n_sensors / 10; // ~10% of sensors have an alert
        let sensor_base_id = (substation_id - 1) * SENSORS_PER_SUBSTATION;

        let mut writer = match client.copy_in("COPY alert FROM stdin NULL AS ''") {
            Ok(w) => w,
            Err(e) => return Err(e.to_string()),
        };

        let mut a_id = a_id_start;
        let base_timestamp = Utc::now() - chrono::Duration::hours(12);

        for i in 0..alerts_per_substation {
            let sensor_id = sensor_base_id + (i * 10) + 1;
            let ts = base_timestamp + chrono::Duration::minutes(i as i64 * 30);
            let alert_type = if rand::thread_rng().gen_bool(0.5) { "HIGH" } else { "LOW" };
            let value: f64 = if alert_type == "HIGH" {
                rand::thread_rng().gen_range(150.0..200.0)
            } else {
                rand::thread_rng().gen_range(0.0..10.0)
            };
            let threshold: f64 = if alert_type == "HIGH" { 150.0 } else { 10.0 };
            let acknowledged = rand::thread_rng().gen_bool(0.3);
            let ack_at = if acknowledged {
                format!("{}", (ts + chrono::Duration::hours(1)).format("%Y-%m-%d %H:%M:%S"))
            } else {
                "".to_string()
            };

            let line = format!(
                "{}\t{}\t{}\t{}\t{}\t{:.4}\t{:.2}\t{}\t{}\n",
                a_id, sensor_id, substation_id, ts.format("%Y-%m-%d %H:%M:%S"),
                alert_type, value, threshold, acknowledged, ack_at
            );

            match writer.write_all(line.as_bytes()) {
                Ok(_) => (),
                Err(e) => return Err(e.to_string()),
            }

            a_id += 1;
        }

        match writer.finish() {
            Ok(_) => (),
            Err(e) => return Err(e.to_string()),
        }

        Ok(a_id)
    }
}

#[async_trait]
impl ReadWrite for TPCIoT {
    async fn execute_rw_transaction(&self, conn: &mut PgConnection, transaction: &BenchmarkTransaction) -> Result<u128, Box<dyn std::error::Error>> {
        // Generate the substation id we are going to hit
        let substation_id: i32 = rand::thread_rng()
            .gen_range(self.min_id..=self.max_id) as i32;

        match transaction.id {
            1 => {
                match TPCIoT::data_ingestion(conn, substation_id, self.min_id, self.max_id).await {
                    Ok(duration) => Ok(duration),
                    Err(e) => Err(Box::new(TPCIoTError(e.to_string()))),
                }
            },
            2 => {
                match TPCIoT::scan_query(conn, substation_id, self.min_id, self.max_id).await {
                    Ok(duration) => Ok(duration),
                    Err(e) => Err(Box::new(TPCIoTError(e.to_string()))),
                }
            },
            3 => {
                match TPCIoT::device_query(conn, substation_id, self.min_id, self.max_id).await {
                    Ok(duration) => Ok(duration),
                    Err(e) => Err(Box::new(TPCIoTError(e.to_string()))),
                }
            },
            4 => {
                match TPCIoT::alert_query(conn, substation_id, self.min_id, self.max_id).await {
                    Ok(duration) => Ok(duration),
                    Err(e) => Err(Box::new(TPCIoTError(e.to_string()))),
                }
            },
            5 => {
                match TPCIoT::threshold_update(conn, substation_id, self.min_id, self.max_id).await {
                    Ok(duration) => Ok(duration),
                    Err(e) => Err(Box::new(TPCIoTError(e.to_string()))),
                }
            },
            0 | 6..=u16::MAX => todo!(),
        }
    }
}

impl Benchmark for TPCIoT {
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
        // No pre-load operations needed for TPCx-IoT
        Ok(0)
    }

    fn load_data(&self, client: &mut Client, substation_ids: Vec<u32>) -> Result<u128, String> {
        let start = Instant::now();

        for substation_id in substation_ids {
            // Calculate starting IDs for this substation
            let m_id_start = ((substation_id - 1) as u64) * (SENSORS_PER_SUBSTATION as u64) * (INITIAL_MEASUREMENTS_PER_SENSOR as u64) + 1;
            let a_id_start = ((substation_id - 1) as u64) * ((SENSORS_PER_SUBSTATION / 10) as u64) + 1;

            // Populate tables
            TPCIoT::populate_substation(client, substation_id)?;
            TPCIoT::populate_sensors(client, substation_id)?;
            TPCIoT::populate_measurements(client, substation_id, m_id_start)?;
            TPCIoT::populate_alerts(client, substation_id, a_id_start)?;
        }

        Ok(start.elapsed().as_micros())
    }

    fn get_default_max_id(&self, client: &mut Client) -> Result<u32, postgres::Error> {
        let row = client.query("SELECT MAX(ss_id) AS max_ss_id FROM substation", &[])?;
        let max_ss_id: i32 = row[0].get("max_ss_id");

        Ok(max_ss_id as u32)
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

/// Synchronous implementation for high-performance TPCx-IoT execution
impl ReadWriteSync for TPCIoT {
    fn supports_sync(&self) -> bool {
        true
    }

    fn prepare_statements(&self, _client: &mut Client) -> Result<PreparedStatements, postgres::Error> {
        Ok(PreparedStatements::new())
    }

    fn execute_rw_transaction_sync(
        &self,
        client: &mut Client,
        _prepared: &PreparedStatements,
        transaction: &BenchmarkTransaction,
    ) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let substation_id: i32 = rand::thread_rng()
            .gen_range(self.min_id..=self.max_id) as i32;

        match transaction.id {
            1 => Self::data_ingestion_sync(client, substation_id),
            2 => Self::scan_query_sync(client, substation_id),
            3 => Self::device_query_sync(client, substation_id),
            4 => Self::alert_query_sync(client, substation_id),
            5 => Self::threshold_update_sync(client, substation_id),
            _ => Err("Unknown transaction".into()),
        }
    }
}

impl TPCIoT {
    /// Synchronous Data Ingestion - bulk insert using simple protocol
    fn data_ingestion_sync(client: &mut Client, substation_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let sensor_base = (substation_id as i64 - 1) * SENSORS_PER_SUBSTATION as i64;
        let sensor_offset: i64 = rand::thread_rng().gen_range(0..SENSORS_PER_SUBSTATION as i64);
        let sensor_id = sensor_base + sensor_offset + 1;

        // Pre-generate measurement data
        let mut values: Vec<f64> = Vec::with_capacity(MEASUREMENT_BATCH_SIZE as usize);
        let mut qualities: Vec<i32> = Vec::with_capacity(MEASUREMENT_BATCH_SIZE as usize);

        {
            let mut rng = rand::thread_rng();
            for _ in 0..MEASUREMENT_BATCH_SIZE {
                let base_value: f64 = rng.gen_range(50.0..150.0);
                let noise: f64 = rng.gen_range(-5.0..5.0);
                values.push(base_value + noise);
                qualities.push(rng.gen_range(90..100));
            }
        }

        let last_reading = *values.last().unwrap_or(&100.0);
        let create_alert = rand::thread_rng().gen_range(1..=100) <= 10;
        let alert_value: f64 = rand::thread_rng().gen_range(150.0..200.0);

        let start = Instant::now();
        let mut tx = client.transaction()?;

        // Bulk insert using UNNEST - much faster than individual inserts
        // Build arrays as SQL literals
        let values_str: Vec<String> = values.iter().map(|v| format!("{:.4}", v)).collect();
        let qualities_str: Vec<String> = qualities.iter().map(|q| q.to_string()).collect();
        let offsets_str: Vec<String> = (0..MEASUREMENT_BATCH_SIZE).map(|i| (i * 10).to_string()).collect();

        let sql = format!(
            "INSERT INTO measurement (m_id, m_sn_id, m_ss_id, m_timestamp, m_value, m_quality) \
             SELECT nextval('measurement_id_seq'), {0}, {1}, NOW() + (ms_offset * interval '1 millisecond'), value, quality \
             FROM UNNEST(ARRAY[{2}]::int[], ARRAY[{3}]::float8[], ARRAY[{4}]::int[]) AS t(ms_offset, value, quality)",
            sensor_id, substation_id,
            offsets_str.join(","),
            values_str.join(","),
            qualities_str.join(",")
        );
        tx.batch_execute(&sql)?;

        // Update sensor last reading
        tx.execute(
            "UPDATE sensor SET sn_last_reading = $1, sn_last_reading_at = NOW() WHERE sn_id = $2",
            &[&last_reading, &sensor_id]
        )?;

        // Create alert if threshold violation (10% chance)
        if create_alert {
            let threshold: f64 = 150.0;
            tx.execute(
                "INSERT INTO alert (a_id, a_sn_id, a_ss_id, a_timestamp, a_type, a_value, a_threshold, a_acknowledged, a_acknowledged_at) \
                 VALUES (nextval('alert_id_seq'), $1, $2, NOW(), 'HIGH', $3, $4, false, NULL)",
                &[&sensor_id, &substation_id, &alert_value, &threshold]
            )?;
        }

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous Scan Query
    fn scan_query_sync(client: &mut Client, substation_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let start = Instant::now();
        let mut tx = client.transaction()?;

        tx.query(
            "SELECT m_sn_id, AVG(m_value), MIN(m_value), MAX(m_value), COUNT(*) \
             FROM measurement WHERE m_ss_id = $1 AND m_timestamp >= NOW() - INTERVAL '1 hour' \
             GROUP BY m_sn_id ORDER BY AVG(m_value) DESC LIMIT 10",
            &[&substation_id]
        )?;

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous Device Query
    fn device_query_sync(client: &mut Client, substation_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let sensor_base = (substation_id as i64 - 1) * SENSORS_PER_SUBSTATION as i64;
        let sensor_id = sensor_base + rand::thread_rng().gen_range(1..=SENSORS_PER_SUBSTATION as i64);

        let start = Instant::now();
        let mut tx = client.transaction()?;

        // Get sensor info
        tx.query(
            "SELECT sn_id, sn_type, sn_unit, sn_min_threshold, sn_max_threshold, sn_status, sn_last_reading, sn_last_reading_at \
             FROM sensor WHERE sn_id = $1",
            &[&sensor_id]
        )?;

        // Get recent measurements
        tx.query(
            "SELECT m_timestamp, m_value, m_quality FROM measurement \
             WHERE m_sn_id = $1 AND m_timestamp >= NOW() - INTERVAL '15 minutes' \
             ORDER BY m_timestamp DESC LIMIT 100",
            &[&sensor_id]
        )?;

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous Alert Query
    fn alert_query_sync(client: &mut Client, substation_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let start = Instant::now();
        let mut tx = client.transaction()?;

        // Get unacknowledged alerts
        let rows = tx.query(
            "SELECT a_id, a_sn_id, a_timestamp, a_type, a_value, a_threshold \
             FROM alert WHERE a_ss_id = $1 AND a_acknowledged = false \
             ORDER BY a_timestamp DESC LIMIT 20",
            &[&substation_id]
        )?;

        // Acknowledge oldest alert if any exist
        if !rows.is_empty() {
            let alert_id: i64 = rows[rows.len() - 1].get(0);
            tx.execute(
                "UPDATE alert SET a_acknowledged = true, a_acknowledged_at = NOW() WHERE a_id = $1",
                &[&alert_id]
            )?;
        }

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }

    /// Synchronous Threshold Update
    fn threshold_update_sync(client: &mut Client, substation_id: i32) -> Result<u128, Box<dyn std::error::Error + Send + Sync>> {
        let sensor_base = (substation_id as i64 - 1) * SENSORS_PER_SUBSTATION as i64;
        let sensor_id = sensor_base + rand::thread_rng().gen_range(1..=SENSORS_PER_SUBSTATION as i64);

        let new_min_threshold: f64 = rand::thread_rng().gen_range(5.0..15.0);
        let new_max_threshold: f64 = rand::thread_rng().gen_range(145.0..155.0);

        let start = Instant::now();
        let mut tx = client.transaction()?;

        tx.execute(
            "UPDATE sensor SET sn_min_threshold = $1, sn_max_threshold = $2 WHERE sn_id = $3",
            &[&new_min_threshold, &new_max_threshold, &sensor_id]
        )?;

        tx.commit()?;
        Ok(start.elapsed().as_micros())
    }
}
