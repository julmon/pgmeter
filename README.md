# pgmtr

A next-generation PostgreSQL benchmarking tool written in Rust. pgmtr implements TPC-C-like and TPCx-IoT-like workloads to measure and analyze PostgreSQL database performance.

## Features

- **TPC-C-like Benchmark**: Implements the standard TPC-C benchmark transactions (New-Order, Payment, Order-Status, Delivery, Stock-Level)
- **TPCx-IoT-like Benchmark**: Implements IoT gateway workloads (Data-Ingestion, Scan-Query, Device-Query, Alert-Query, Threshold-Update)
- **Concurrent Clients**: Support for multiple concurrent database connections
- **Ramp-up Period**: Gradual client startup to avoid connection storms
- **Data Aggregation**: Automatic collection and analysis of transaction metrics
- **Performance Statistics**: Response time percentiles (95th, 99th), throughput (TPM/TPS), and error rates
- **Parallel Data Loading**: Multi-threaded database initialization for faster setup

## Requirements

- Rust 1.70 or later
- PostgreSQL 12 or later

## Installation

### From Source

```bash
git clone https://github.com/julmon/pg_meter.git
cd pg_meter
cargo build --release
```

The binary will be available at `target/release/pgmtr`.

## Usage

pgmtr uses PostgreSQL connection parameters similar to standard PostgreSQL tools (psql, pgbench).

### Connection Options

| Option | Environment Variable | Default | Description |
|--------|---------------------|---------|-------------|
| `-H, --host` | `PGHOST` | localhost | Database server host or socket directory |
| `-p, --port` | `PGPORT` | 5432 | Database server port |
| `-U, --username` | `PGUSER` | (current user) | Database user name |
| `-d, --dbname` | `PGDATABASE` | (current user) | Database name to connect to |

The password can be provided via the `PGPASSWORD` environment variable.

## Benchmarks

### TPC-C Benchmark

TPC-C simulates a complete order-entry environment with warehouses, districts, customers, orders, and inventory management.

#### Initialize TPC-C Database

```bash
# Initialize with scale factor 10 (10 warehouses) using 4 parallel jobs
pgmtr -H localhost -d benchmark init tpcc -s 10 -j 4

# Initialize without foreign keys (faster, useful for some tests)
pgmtr init tpcc -s 10 -j 4 --no-fkey
```

#### Run TPC-C Benchmark

```bash
# Run with 8 clients for 60 seconds with 10 second ramp-up
pgmtr -H localhost -d benchmark run tpcc -c 8 -T 60 -r 10

# Run targeting specific warehouse range
pgmtr run tpcc -c 16 -T 300 --min-id 1 --max-id 10
```

#### TPC-C Transaction Mix

| Transaction | Weight | Description |
|-------------|--------|-------------|
| New-Order | 45% | Create new customer orders |
| Payment | 43% | Process customer payments |
| Delivery | 4% | Deliver completed orders |
| Order-Status | 4% | Query order status |
| Stock-Level | 4% | Check warehouse stock levels |

The scale factor determines the database size:
- 1 warehouse ~ 100 MB of data
- Each warehouse adds approximately 100 MB

---

### TPCx-IoT Benchmark

TPCx-IoT simulates an IoT gateway system based on an electric utility operational model. It models power substations containing multiple sensors that generate time-series data.

#### Schema

- **substation**: Power substations (scale factor units)
- **sensor**: IoT sensors belonging to substations (100 sensors per substation)
- **measurement**: Time-series sensor readings
- **alert**: Threshold-based alerts generated from sensor data

#### Initialize TPCx-IoT Database

```bash
# Initialize with scale factor 10 (10 substations) using 4 parallel jobs
pgmtr -H localhost -d benchmark init tpciot -s 10 -j 4

# Initialize without foreign keys
pgmtr init tpciot -s 10 -j 4 --no-fkey
```

#### Run TPCx-IoT Benchmark

```bash
# Run with 8 clients for 60 seconds with 10 second ramp-up
pgmtr -H localhost -d benchmark run tpciot -c 8 -T 60 -r 10

# Run targeting specific substation range
pgmtr run tpciot -c 16 -T 300 --min-id 1 --max-id 10
```

#### TPCx-IoT Transaction Mix

| Transaction | Weight | Description |
|-------------|--------|-------------|
| Data-Ingestion | 70% | Bulk insert of sensor measurements (100 readings per transaction) |
| Scan-Query | 10% | Time-range aggregation queries over recent sensor data |
| Device-Query | 10% | Single sensor data retrieval with recent measurements |
| Alert-Query | 5% | Query and acknowledge threshold violation alerts |
| Threshold-Update | 5% | Update sensor threshold configuration |

The scale factor determines the database size:
- 1 substation = 100 sensors
- Initial data: 1,000 measurements per sensor + alerts

---

## Command Options

### Init Options

| Option | Default | Description |
|--------|---------|-------------|
| `-s, --scalefactor` | 1 | Scale factor (warehouses for TPC-C, substations for TPCx-IoT) |
| `-j, --jobs` | 1 | Number of parallel threads for data loading |
| `--no-fkey` | false | Skip foreign key creation |

### Run Options

| Option | Default | Description |
|--------|---------|-------------|
| `-c, --client` | 1 | Number of concurrent database clients |
| `-T, --time` | 1 | Duration of benchmark in seconds |
| `-r, --rampup` | 0 | Ramp-up period in seconds (clients start gradually) |
| `--min-id` | 1 | Minimum ID to target (warehouse/substation) |
| `--max-id` | 0 | Maximum ID (0 = auto-detect from data) |

## Example Workflows

### TPC-C Example

```bash
# Set connection password
export PGPASSWORD='your_password'

# Create a dedicated benchmark database
psql -H localhost -U postgres -c "CREATE DATABASE benchmark;"

# Initialize with 5 warehouses
pgmtr -H localhost -U postgres -d benchmark init tpcc -s 5 -j 4

# Run a 5-minute benchmark with 10 clients
pgmtr -H localhost -U postgres -d benchmark run tpcc -c 10 -T 300 -r 30
```

### TPCx-IoT Example

```bash
# Set connection password
export PGPASSWORD='your_password'

# Create a dedicated benchmark database
psql -H localhost -U postgres -c "CREATE DATABASE iot_benchmark;"

# Initialize with 20 substations (2000 sensors total)
pgmtr -H localhost -U postgres -d iot_benchmark init tpciot -s 20 -j 4

# Run a 10-minute IoT workload simulation with 16 clients
pgmtr -H localhost -U postgres -d iot_benchmark run tpciot -c 16 -T 600 -r 60
```

## Output

### Results Directory

Each benchmark run creates a timestamped directory (e.g., `pgmtr-2024-01-15T10:30:00/`) containing:

- `transaction.log` - Raw transaction data (timestamp, client, transaction type, response time)
- `error.log` - Error details for failed transactions
- `pgmtr-tpm-*.csv` - Transactions per minute over time for each transaction type
- `pgmtr-response-time-*.csv` - Response times over time
- `pgmtr-stats-*.csv` - Statistical summary (mean, std, min, max, percentiles)

### Console Output

After benchmark completion, pgmtr displays:

**Results Summary**
- Transaction name
- Number of committed transactions
- Number of errors
- Error rate (%)
- Transactions per minute (TPM)
- Transactions per second (TPS)

**Response Time Statistics**
- Average response time (ms)
- Minimum response time (ms)
- Maximum response time (ms)
- Standard deviation (ms)
- 95th percentile (ms)
- 99th percentile (ms)

## Development

### Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release
```

### Running Tests

```bash
cargo test
```

### Project Structure

```
src/
├── main.rs                    # Application entry point
├── args.rs                    # CLI argument parsing
└── executor/
    ├── mod.rs                 # Benchmark executor
    ├── benchmark.rs           # Benchmark traits and types
    ├── benchmark_wrapper.rs   # Enum wrapper for multiple benchmarks
    ├── tpcc.rs                # TPC-C implementation
    ├── tpciot.rs              # TPCx-IoT implementation
    ├── data_agg.rs            # Data aggregation and statistics
    ├── terminal.rs            # Terminal output formatting
    └── txmessage.rs           # Inter-thread messaging
```

## License

This project is open source. See the LICENSE file for details.

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests.
