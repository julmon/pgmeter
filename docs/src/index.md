# pgmeter

pgmeter is a PostgreSQL benchmarking tool written in Rust. A benchmark is a
**YAML specification plus Lua scripts**: the spec declares the schema, how to
load the data and the transaction mix, and pgmeter initializes the database
and drives the workload. Adding a workload never means writing Rust or
recompiling anything.

Five reference workloads ship inside the binary, ready to run: PGM-B, PGM-C,
PGM-H, PGM-DS and PGM-IoT, derived from the TPC-B, TPC-C, TPC-H, TPC-DS and
TPCx-IoT specifications. They are not audited TPC benchmarks and their results
are not comparable to official TPC results; see
[License and trademarks](project/license.md).

## What it does

- **Benchmarks as data.** Schema, indexes, data loading and the workload live
  in one spec file. Start from a bundled workload with `pgmeter spec new`, or
  from a one-table template, and edit.
- **Lua where logic is needed.** Transactions and loaders can be Lua (Luau)
  scripts with a database bridge (`exec`, `query`, `query_one`, transaction
  control, `COPY` streaming) and fast random-data helpers.
- **Two execution engines.** Specs made of plain SQL transactions run on an
  async engine that drives every client from a small pool of worker threads,
  like pgbench; specs with Lua transactions run one thread per client.
- **Measured properly.** Concurrent clients with a ramp-up window that is
  excluded from the statistics, throughput in the unit the spec defines (TPM,
  TPS, queries per hour), response-time percentiles, error rates, live
  progress, and a self-contained HTML report with charts.
- **Fast loading.** Parallel `COPY`-based data loading with per-job id ranges.

## Where to start

- [Installation](getting-started/installation.md), then
  [your first benchmark](getting-started/first-benchmark.md): a PGM-B run
  from an empty database to the report in five minutes.
- [The user guide](guide/connecting.md) for every option of `init` and `run`,
  the results directory and the bundled workloads.
- [Writing benchmarks](writing/index.md) when the bundled workloads are not
  what you need: every spec field, both loading styles, both transaction
  modes and a worked example.
- [The reference](reference/cli.md) for the command line, the Lua API and the
  output file formats.

## A taste

```bash
export PGPASSWORD=secret
psql -h db -U postgres -c "CREATE DATABASE benchmark"

# Write an editable copy of the bundled PGM-B spec into ./pgm-b
pgmeter spec new pgm-b -o .

# Load it at scale factor 10 with 4 loader threads
pgmeter -H db -U postgres -d benchmark init --spec pgm-b -s 10 -j 4

# 8 clients for 60 seconds after a 10 second ramp-up
pgmeter -H db -U postgres -d benchmark run --spec pgm-b -c 8 -T 60 -r 10
```

pgmeter prints the throughput, the committed and failed transactions and the
response-time statistics of the steady-state window, and writes a results
directory with the raw logs, per-second CSVs and `report.html`.

[View a sample report](sample-report.html): a PGM-C run with 10 warehouses and
8 clients.
