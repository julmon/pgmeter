# pgmtr

A next-generation PostgreSQL benchmarking tool written in Rust. pgmtr runs
**user-defined benchmarks described in YAML + Lua**: you declare your schema,
data loading, and transaction mix in a spec file, and pgmtr initializes the
database and drives the workload — no Rust code or recompilation required.

Reference TPC-B, TPC-C, TPCx-IoT, and TPC-H workloads ship as ready-to-run
specs under [`samples/`](samples/).

## Features

- **YAML-defined benchmarks**: Describe schema, indexes, data loading, and the
  transaction mix declaratively in a single spec file.
- **Lua scripting**: Implement transaction logic and data generation in Lua
  (Luau), with a built-in PostgreSQL bridge (`exec`, `query`, `copy_*`,
  transaction control) and fast random-data helpers.
- **Bundled reference specs**: TPC-B, TPC-C, TPCx-IoT, and TPC-H, validated to
  match hand-written native implementations.
- **Concurrent clients** with a configurable **ramp-up period**.
- **Data aggregation & statistics**: Response-time percentiles (95th, 99th),
  throughput (TPM/TPS), and error rates.
- **Parallel data loading** for faster initialization.

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

pgmtr uses PostgreSQL connection parameters similar to standard PostgreSQL tools
(psql, pgbench). Every workload is driven by a YAML spec passed with `--spec`.

### Connection Options

| Option | Environment Variable | Default | Description |
|--------|---------------------|---------|-------------|
| `-H, --host` | `PGHOST` | localhost | Database server host or socket directory |
| `-p, --port` | `PGPORT` | 5432 | Database server port |
| `-U, --username` | `PGUSER` | (current user) | Database user name |
| `-d, --dbname` | `PGDATABASE` | (current user) | Database name to connect to |

The password can be provided via the `PGPASSWORD` environment variable.

### Workflow

A benchmark is run in two phases — initialize the database, then run the
workload:

```bash
# 1. Initialize: create schema and load data (scale factor 10, 4 loader threads)
pgmtr -H localhost -d benchmark init --spec samples/tpc-b.yaml -s 10 -j 4

# 2. Run: 8 clients for 60 seconds with a 10 second ramp-up
pgmtr -H localhost -d benchmark run --spec samples/tpc-b.yaml -c 8 -T 60 -r 10
```

Swap the `--spec` file to run a different workload — the same `init`/`run` flow
applies to every spec, including your own.

### Bundled Reference Specs

| Spec | Workload | Scale unit |
|------|----------|------------|
| `samples/tpc-b.yaml` | Banking debit/credit (single transaction) | branch |
| `samples/tpc-c.yaml` | Order-entry (New-Order, Payment, Order-Status, Delivery, Stock-Level) | warehouse |
| `samples/tpc-iot.yaml` | IoT gateway (Data-Ingestion, Scan/Device/Alert queries, Threshold-Update) | substation |
| `samples/tpc-h.yaml` | Decision-support (22 analytical queries) | scale factor |
| `samples/custom-simple.yaml` | Minimal example of a user-defined workload | tenant |
| `samples/tpc-c-embedded.yaml` | TPC-C with Lua embedded inline instead of external files | warehouse |

### Writing Your Own Benchmark

The full authoring guide — every spec field, the row-template language, both
data-loading styles, the simple/script transaction modes, and the complete Lua
API reference — lives in [`docs/yaml-benchmarks.md`](docs/yaml-benchmarks.md).
It includes a worked from-scratch example.

## Command Options

### Init Options (`init`)

| Option | Default | Description |
|--------|---------|-------------|
| `--spec` | (required) | Path to the YAML benchmark specification file |
| `-s, --scalefactor` | 1 | Scale factor (meaning is defined by the spec's `scaling.unit`) |
| `-j, --jobs` | 1 | Number of parallel threads for data loading |
| `--no-fkey` | false | Skip foreign key creation |
| `--post-create-table` | — | Path to a SQL script run after table creation |

### Run Options (`run`)

| Option | Default | Description |
|--------|---------|-------------|
| `--spec` | (required) | Path to the YAML benchmark specification file |
| `-c, --client` | 1 | Number of concurrent database clients |
| `-T, --time` | 1 | Duration of benchmark in seconds |
| `-r, --rampup` | 0 | Ramp-up period in seconds (clients start gradually) |

## Example Workflow

```bash
# Set connection password
export PGPASSWORD='your_password'

# Create a dedicated benchmark database
psql -h localhost -U postgres -c "CREATE DATABASE benchmark;"

# Initialize TPC-C with 5 warehouses using 4 loader threads
pgmtr -H localhost -U postgres -d benchmark init --spec samples/tpc-c.yaml -s 5 -j 4

# Run a 5-minute benchmark with 10 clients and a 30 second ramp-up
pgmtr -H localhost -U postgres -d benchmark run --spec samples/tpc-c.yaml -c 10 -T 300 -r 30
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
- Average, minimum, maximum response time (ms)
- Standard deviation (ms)
- 95th and 99th percentiles (ms)

## Development

### Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release
```

### Project Structure

```
src/
├── main.rs                    # Application entry point
├── args.rs                    # CLI argument parsing
├── executor.rs                # Benchmark executor (module root)
└── executor/
    ├── benchmark.rs           # Benchmark traits and types
    ├── yaml_types.rs          # Serde structs for the YAML spec
    ├── yaml_benchmark.rs      # YAML-driven Benchmark implementation
    ├── yaml_lua.rs            # Lua VM, DB bridge, binding/template compilation
    ├── data_agg.rs            # Data aggregation and statistics
    ├── terminal.rs            # Terminal output formatting
    └── txmessage.rs           # Inter-thread messaging
samples/                       # Reference and example benchmark specs
docs/yaml-benchmarks.md        # Benchmark authoring guide
```

## License

This project is open source. See the LICENSE file for details.

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests.
