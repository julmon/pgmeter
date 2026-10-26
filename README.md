# pgmeter

[![CI](https://github.com/julmon/pgmeter/actions/workflows/ci.yml/badge.svg)](https://github.com/julmon/pgmeter/actions/workflows/ci.yml)
[![Docs](https://github.com/julmon/pgmeter/actions/workflows/docs.yml/badge.svg)](https://julmon.github.io/pgmeter/)

A PostgreSQL benchmarking tool written in Rust. A benchmark is a **YAML
specification plus Lua scripts**: the spec declares the schema, how to load
the data and the transaction mix, and pgmeter initializes the database and
drives the workload. Adding a workload never means writing Rust or
recompiling.

Six reference workloads ship inside the binary: PGM-B, PGM-C and its
client-driven variant, PGM-H, PGM-DS and PGM-IoT, derived from the TPC-B,
TPC-C, TPC-H, TPC-DS and TPCx-IoT specifications. They are not audited TPC benchmarks; see the
[trademark notice](#trademark-notice).

**Documentation: [julmon.github.io/pgmeter](https://julmon.github.io/pgmeter/)**:
getting started, the user guide, how to write your own benchmark, and the
command-line, Lua API and output file reference.

## Features

- Schema, indexes, data loading and the workload in one spec file; start from
  a bundled workload with `pgmeter spec new` and edit.
- Lua (Luau) scripting for transactions and loaders, with a database bridge
  (`exec`, `query`, `query_one`, transaction control, `COPY` streaming) and
  fast random-data helpers.
- Concurrent clients with a ramp-up window excluded from the statistics;
  specs made of plain SQL transactions are driven from a pool of worker
  threads, like pgbench's `-j`.
- Throughput in the unit the spec defines, response-time percentiles, error
  rates, live progress, JSON and CSV output, and a self-contained HTML report
  with charts ([sample](https://julmon.github.io/pgmeter/sample-report.html)).
- Parallel `COPY`-based data loading.

## Requirements

- Rust 1.87 or later (declared as `rust-version` in `Cargo.toml`)
- PostgreSQL 12 or later
- Linux or macOS (developed and tested on Linux; Windows is not supported)

## Installation

```bash
git clone https://github.com/julmon/pgmeter.git
cd pgmeter
cargo build --release
```

The binary is `target/release/pgmeter`; it has no runtime dependencies.

## Quick start

```bash
export PGPASSWORD=secret
psql -h db -U postgres -c "CREATE DATABASE benchmark"

# Write an editable copy of the bundled PGM-B spec into ./pgm-b
pgmeter spec new pgm-b -o .

# Load it at scale factor 10 with 4 loader threads (drops and recreates its tables)
pgmeter -H db -U postgres -d benchmark init --spec pgm-b -s 10 -j 4

# 8 clients for 60 seconds after a 10 second ramp-up
pgmeter -H db -U postgres -d benchmark run --spec pgm-b -c 8 -T 60 -r 10
```

pgmeter prints the throughput, committed and failed transactions and the
response-time statistics, and writes a results directory with the raw logs,
per-second CSVs and `report.html`.

![pgmeter HTML report: charts](docs/src/images/report-charts.png)

The [documentation](https://julmon.github.io/pgmeter/) covers every option,
the bundled workloads and the spec format.

## Trademark notice

pgmeter ships workloads (PGM-B, PGM-C, PGM-H, PGM-DS, PGM-IoT) **derived from** the
corresponding TPC and TPCx benchmark specifications. They implement a
representative subset of each specification for relative comparison (for
example, comparing two table access methods under the same workload). **They are
not audited, full-specification TPC benchmarks, and their results must not be
compared with, or represented as, official TPC results.**

TPC, TPC Benchmark, TPC-B, TPC-C, TPC-H, TPC-DS, TPCx-IoT, tpmC, QphH and QphDS
are trademarks of the Transaction Processing Performance Council. See
[www.tpc.org](https://www.tpc.org).

## License

pgmeter is released under the [PostgreSQL License](LICENSE).

The binary and the generated reports embed third-party components (Chart.js,
Luau) under their own licenses. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Contributing

Contributions are welcome: issues and pull requests at
[github.com/julmon/pgmeter](https://github.com/julmon/pgmeter). The
[contributing page](https://julmon.github.io/pgmeter/project/contributing.html)
describes the build, the checks and the project layout.
