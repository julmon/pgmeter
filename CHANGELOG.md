# Changelog

All notable changes to pgmeter are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0.0, minor versions may change the YAML spec schema, the Lua API, the
CSV layout or the CLI; such changes are listed under "Changed".

## [Unreleased]

First public release.

### Added

- `pgmeter init` creates the schema, loads data in parallel (`--jobs`,
  `--scalefactor`), then adds primary keys, foreign keys (`--no-fkey` to skip),
  indexes, vacuums and checkpoints. `--post-create-table` runs an SQL script
  after table creation.
- `pgmeter run` drives the workload with one connection per client
  (`--client`, `--time`, `--rampup`), prints live progress (`--progress`) and
  a results summary as a table, JSON or CSV (`--format`, `--quiet`).
- Benchmarks are YAML specifications plus Lua (Luau) scripts. Transactions run
  in `simple` mode (SQL templates with Lua bindings) or `script` mode (Lua with
  a database bridge: `exec`, `query`, `query_one`, `begin`, `commit`,
  `rollback`, `copy_*`, `rand_*`). Data loading uses either row templates or
  Lua load scripts, with shared modules and a pre-load script.
- Bundled reference workloads derived from the public TPC specifications:
  PGM-B, PGM-C, PGM-H, PGM-DS and PGM-IoT, plus a minimal `custom-simple`
  template. `pgmeter spec list` shows them and `pgmeter spec new` scaffolds a
  copy to edit.
- Each run writes a result directory with `transaction.log`, `error.log`,
  per-transaction CSV files (throughput over time, response times over time,
  statistics) and a self-contained HTML report with charts, percentiles,
  server and data-set information.
- Connection settings follow libpq conventions: `PGHOST`, `PGPORT`, `PGUSER`,
  `PGDATABASE`, `PGPASSWORD` and `~/.pgpass`.
- Released under the PostgreSQL License; third-party notices for Chart.js and
  Luau in `THIRD_PARTY_NOTICES.md`.
- Minimum supported Rust version 1.85; PostgreSQL 12 or later.

[Unreleased]: https://github.com/julmon/pgmeter/compare/main...HEAD
